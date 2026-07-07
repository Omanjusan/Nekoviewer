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

## 見積もり（開く前のメモリ超過チェック）

`fs/archive/mod.rs::estimate_archive_memory` がページ数に応じてサンプリング
（3枚以下→先頭1枚、4〜10枚→先頭・末尾、11枚以上→先頭・中間・末尾）し、サンプル平均×総ページ数
が予算を超えるかを判定する。7z（ソリッド圧縮）はサンプリングでの軽量見積もりが成立しない
ため、`estimate_archive_memory_7z` に委譲し一括展開結果を使った全件厳密判定を行う。
