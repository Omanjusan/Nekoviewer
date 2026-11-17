# キャッシュ設計

v1.1.0時点のキャッシュ層（`cache.rs` / `anim.rs` / `neko_dir.rs`）の全体設計。個別作業の経緯・
検討過程は [decisions/thumbbar-7z-cache.md](decisions/thumbbar-7z-cache.md) を参照。

## 全体像

キャッシュは3層に分かれる。いずれもバックグラウンドスレッドでデコード/展開を行い、
メインスレッドへ mpsc で結果を返す（`ctx.request_repaint()` で描画を起こす）。

| 層 | 役割 | 保持データ |
| --- | --- | --- |
| `FileCache` | アーカイブ1件分の「開くコスト」を再利用 | 生バイト列 or 7z/tar展開済み画像一式 |
| `PageCache` | 表示中ページのデコード結果 | RGBAピクセル（静止画）/ `RingAnimation`（アニメ） |
| サムネDB（`neko_dir.rs`） | ディレクトリ単位のサムネイル | JPEGバイト列（redb, ディスク永続化） |

## 予算計算（`resolve_cache_budgets`）

- 合計予算 = 指定値（`--cache-max-mb` / `[cache] cache_total_mb`）省略時はシステムRAMの30%
  （`sysinfo`取得失敗時は500MBにフォールバック）
- `PAGE_CACHE_SHARE_PCT : FILE_CACHE_SHARE_PCT = 70 : 30` で分配（`page_max`, `file_max`）
- `page_min = page_max * 40%`。PageCacheは`page_max`超過時に`page_min`まで古いエントリをevictする
  （LRU、フォルダ距離ベース）
- アニメ1本のリングバッファ予算は `page_max` の25%（`ANIM_RING_BUDGET_PCT`）。フェーズ2の
  メモリ見積もり（`fs/archive/mod.rs::estimate_archive_memory`）も同じ値を使い、開く前の判定と
  実際のリング容量算出を整合させている

## FileCache: ZIPとソリッド圧縮形式（7z/tar）の非対称設計

`FileCacheEntry` は `Raw(Arc<[u8]>)` と `Extracted(Arc<HashMap<String, Vec<u8>>>)` の2種類。

- **ZIP/CBZ**: 生バイト列（`Raw`）を共有し、各デコードスレッドが自分用の軽量 `ZipArchive` を
  開いて並列にデコードする。`ZipArchive::by_name` は1エントリだけの個別解凍が可能な真の
  ランダムアクセスを持つため、生バイト列の共有だけで十分並列性が出る
- **FileCache にまだ載っていない zip（ディスク直読み）**: `OpenArchive::Fast`（`FastZip`）が、
  `ZipArchive::new` を通さず中央ディレクトリだけで開く。判断できない zip は従来の `ZipArchive`
  （`OpenArchive::Disk`）に戻る。冷たいHDDの初回を速くするための経路
  （[features/archive-open-perf.md](features/archive-open-perf.md)）
- **FileCache の zip 丸読み**: 2MBずつ読み、ページ読み等の画面表示用のディスク読みが動いている間は
  次の区切りを待つ（HDDで、ページ送りと同じディスクを取り合わないため。同ドキュメント §4.1）
- **7z/tar（ソリッド圧縮）**: ランダムアクセスができないため、`FileCache` 側で1回だけ全画像を
  展開し（`Extracted`）、各デコードスレッドは読み取り専用でロックなしに参照する
- あえて非対称にしている理由: もしZIPも「生きたアーカイブハンドルを共有」する形にすると
  `by_name` が `&mut self` を要求するためデコードが直列化し性能が落ちる

`FileCache::insert` には `PageCache` の bypass 相当（1エントリが予算超過時の退避）が無い。
`Extracted` が `max_bytes` を単体で超えるケースは無条件evictループが空になった上で予算超過状態
のまま挿入される。実害は未確認だが既知の制約。

## PageCache と `PageContent`

`PageContent` は静止画（RGBAバッファ）とアニメーション（`RingAnimation`）を持つ enum。
アニメーションは全フレーム一括保持ではなく、`anim.rs` の `SequentialAnimDecoder`（1フレームずつ
逐次デコード）+ `FrameRingBuffer`（容量固定リング、古いフレームから自動エビクション）で保持
する。GIF/APNG/AVIF/WebPいずれもこの方式に統一されている。

- ランダムアクセス不可・前進のみが前提（フレーム間差分合成方式のフォーマット制約）。
  ループ境界（最終フレーム→先頭）でのみ `restart()` でデコーダを元データから作り直す
  （この再デコードによる一瞬のフリーズは許容）
- リング容量は `resolve_ring_capacity(frame_bytes, budget_bytes, min_frames, max_frames)` で
  1フレームサイズと予算から算出（下限/上限は `[cache] anim_ring_min_frames`/
  `anim_ring_max_frames`、既定4/32）
- 1フレームでもハードリミット（`ANIM_HARD_LIMIT_BYTES`）を超える場合は静止画にフォールバック

## サムネイル

- ディレクトリブラウザのグリッド用サムネは `spawn_thumb_worker`、アーカイブ内サムネバー用は
  `spawn_entry_thumb_worker`（別ワーカー）
- ディスク永続化は対象ディレクトリに一切書き込まない方式。`config.cache_root()` 配下
  （`local`=実行ファイル隣の`cache/`、`xdg`=`~/.local/share/nekoview/cache/`、Windowsは
  `%LOCALAPPDATA%`）に、監視対象ディレクトリの絶対パスをSHA256ハッシュ化した名前の
  サブディレクトリを作り、その中の `cache.redb`（`neko_dir.rs`）に
  ファイル名→`(mtime, JPEGバイト列)` を保存する
- 再生成判定はバッチ差分検出ではなく、グリッド表示のたびにDB内mtimeと現在のファイルmtimeを
  突合する遅延方式
- 非画像ZIP等は `INVALID_TABLE` にマーカーを記録し、毎回の再スキャンを避ける
- **消えたファイルのサムネ行は、スキャン時に削除しない**（`sync_thumbnail_records` は検索用の索引と非画像マーカーだけ掃除する）。
  ファイルが移動・リネーム・複製されたとき、ファイルID解決（[features/file-identity.md](features/file-identity.md)）が
  元のDBの生成済み（Current）のサムネ行を新しい名前（別フォルダのDB可）へコピーするため
  （`take_thumbnail_rows` / `put_thumbnail_rows`。移動先で生成中・生成済みなら上書きしない）。
  ID解決待ちのファイルは、解決が済むまでサムネ生成を始めない。残ったサムネ行の掃除は、別ブランチ予定のキャッシュ整理の役割

### サムネの一括破棄（`neko_dir::clear_folder_thumbs`）

フォルダ単位で、貯めたサムネをすべて破棄する API。サムネの拡縮アルゴリズムやサイズの設定を変えたあと、
作り込み済みのサムネを作り直す入口として、UI から呼べるようにしてある（UI の導線は未実装）。

- `clear_folder_thumbs(db)`: そのフォルダのキャッシュDBから、サムネ本体と生成元・サイズ・フィルタ・状態・希望ソースの
  全行を1トランザクションで消し、消したファイル数を返す。ファイルの記録・ページ数・評価などは残す。UI からは、
  アプリが開いている `cache_db` のハンドルを渡す
- `clear_dir_thumbs(dir, config)`: フォルダのパスから DB を引いて同じことをする（DB が無ければ何もしない）。
  対象は直下のみ（サブフォルダは別DB）
- 起動引数 `--clear-thumbs` が、起動前（ワーカーが動く前）に `clear_dir_thumbs` を呼ぶ。対象は起動するフォルダ
  （位置引数があればその場所、無ければ最後に訪れたフォルダ）。標準エラーに破棄件数を出す
  （[implements.md](implements.md) の「起動方法」）

### 飛び地一覧（お気に入り・検索・タグ検索・類似）のサムネ参照

飛び地一覧（表示中フォルダ以外のファイルが並ぶ一覧）のサムネ要求は `ThumbRequest.db = None` で送る
（表示中フォルダのDBへ誤って保存しないため）。以前はこのとき、プローブがDBを見ずに毎回元アーカイブから
生成していた（HDDで30〜700ms・結果は保存されない）。

- プローブレーン（`cache.rs::probe_foreign_thumb`）が、そのファイル自身の**親フォルダのサムネDB**
  （既存のものだけ・新規には作らない）を**読み取り専用**で引く。開いたDBは親フォルダ単位で使い回す
- 当たれば暫定表示を出し、続けて mtime と生成条件（サイズ・フィルタ・登録サムネ）を
  `neko_dir::is_thumbnail_current` で検証する（書き込みなし）。一致なら完了、食い違えば旧画像を出したまま
  メモリ上だけで再生成する。**飛び地からDBへは書かない**
- 外れ（DB無し・行無し）は従来どおりメモリ上だけで生成する
- `neko_dir::open_cache_db` は、スキーマ版が現行で中断中のprocessingが無ければ、書き込みトランザクションと
  THUMBS_TABLE の全キー走査を省く（`schema_is_settled`）。THUMBS_TABLE はJPEG本体を抱えるので、全キー走査は
  DBファイル全体の読み出しになる
- 計測: 設定の perf ログON時に `[perf/foreign-thumb]` を出す（open・stat・現行判定）。実測は、当たり1件
  約0.3ms・stat 0.0ms・再生成なし（stat のコールド時の値は未確認。必要なら再起動直後に再計測）

## 見積もり（開く前のメモリ超過チェック）

`fs/archive/mod.rs::estimate_archive_memory` がページ数に応じてサンプリング
（3枚以下→先頭1枚、4〜10枚→先頭・末尾、11枚以上→先頭・中間・末尾）し、サンプル平均×総ページ数
が予算を超えるかを判定する。7z（ソリッド圧縮）はサンプリングでの軽量見積もりが成立しない
ため、`estimate_archive_memory_7z` に委譲し一括展開結果を使った全件厳密判定を行う。
zip のサンプル読みは `FastZip`（中央ディレクトリの近道。`ZipArchive::new` を通さない）で行い、
使えない zip だけ従来の `ZipArchive` に戻る（[features/archive-open-perf.md](features/archive-open-perf.md)）。
