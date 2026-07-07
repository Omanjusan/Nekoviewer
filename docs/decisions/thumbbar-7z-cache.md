<!-- markdownlint-disable -->
# サムネバー速度改善 / 7z重複展開解消（決定記録）

> キャッシュの現在の全体設計は [cache-design.md](../cache-design.md) を参照。本ドキュメントは
> サムネバー速度改善と7z重複展開解消という個別作業当時の経緯・検討記録。

v1.1.0
サムネバー速度改善(todo.md (A)(B)(C))関連の作業メモ。

フェーズ1: enqueue近傍優先化 — 実装完了。`thumbbar_missing_indices`(view_reader.rs)が全ページ一括ではなく`spread_lo`±`THUMBBAR_ENQUEUE_WINDOW`(暫定固定40)の範囲だけを返すように変更。開いた瞬間・大ジャンプ直後の一括enqueueを解消。

フェーズ2: サムネバー描画の仮想化 — 実装完了。`draw_thumbbar_contents`(view_reader.rs)を`ScrollArea::show_viewport`ベースに変更し、可視範囲＋前後8枚マージンだけ実描画。可視範囲は`thumbbar_visible_range`に記録し、フェーズ1の固定窓の代わりに`thumbbar_missing_indices`がこれを使う(まだ一度も描画されていない最初のフレームだけ固定窓にフォールバック)。現在ページのマーカーが可視範囲外でも位置計算だけで`scroll_to_rect`できるようにした。

7z重複展開の解消 — 実装完了。問題の根本はソリッド圧縮の7zがランダムアクセスできず「開いた時点で全画像を一括展開」という設計にしていたこと自体ではなく、その展開処理がメイン画像ローダ(`spawn_worker`)・サムネワーカー(`spawn_entry_thumb_worker`)それぞれのスレッドローカル変数として独立していたため、デコードスレッド数T本分(最大2T回)重複していたこと。ZIPは`ZipArchive::by_name`で1エントリだけ個別解凍できる真のランダムアクセスがあるため、この問題はそもそも7z特有。

対策は「FileCache拡張」方式を採用。`FileCacheEntry`enum(`Raw(Arc<[u8]>)` / `SevenZExtracted(Arc<HashMap<String,Vec<u8>>>)`)を新設し、既存の単一バックグラウンドスレッド(`spawn_file_cache_worker`)が7zなら展開まで行う。`FileCache`は元々`ViewExplorer`(メインスレッド)が単独所有し、`ensure_file_cached`の`contains`+`pending`チェックで多重リクエストを防いでいたため、新たな排他制御(Mutex/OnceLock等)を作らずに「プロセス全体で展開は実質1回」を実現できた。デコードワーカー側は受け取った`FileCacheEntry`をそのまま使うだけになり、自前の7z展開コードは削除(ディスクミス時の安全弁としてのみ残置)。

7zのFileCache展開待ち中に飛んできたページ/サムネ要求は`deferred_archive_requests`(view_explorer.rs)に保留し、展開完了時にまとめてフラッシュする(`ensure_file_cached`が既に持っていた`file_cache_pending`集合を判定に流用、新規の状態は増やしていない)。これにより「開いた瞬間の数フレームだけ稀に重複」という残存リスクも解消。

ZIP/7zは「FileCacheという同じ入り口を通る」という意味で対称にしたが、中身のアクセス方法はあえて非対称のままにしている: ZIPは生バイト列(`Raw`)をFileCacheに置き、各デコードスレッドがそこから自分用の軽量`ZipArchive`を開いて並列にデコードする(今までの並列性を維持)。7zは展開済み`Arc<HashMap>`を共有し、各スレッドは読み取り専用でロック無しに参照する。もしZIPも「生きたアーカイブハンドルを共有」する形にすると、`by_name`が`&mut self`を要求するためデコードが直列化し性能後退するので、あえて揃えなかった。

**要検討: ファイルキャッシュの予算比重(PAGE_CACHE_SHARE_PCT/FILE_CACHE_SHARE_PCT、cache.rs)**
現在70:30(ページ:ファイル)。この比率は「FileCacheは生バイト列(圧縮されたアーカイブファイルそのもの)を持つだけ」という前提で決めたもの。今回7zは`SevenZExtracted`として「展開済みの画像バイト列一式」を持つようになり、これは同じアーカイブの生ファイルサイズより大きくなる(圧縮前サイズに近い、コミック1冊で数十〜数百MB規模)。ファイルキャッシュの実質的な重要度・データサイズが上がったため、比率をファイルキャッシュ側に厚めに振り直す必要がないか、実機のメモリ計測をしてから判断したい。ユーザーからは「ページキャッシュよりファイルキャッシュの方が重要になっている、ページキャッシュ分から捻出していい」との方針は既にもらっている。

**既知の未対応点**
- `FileCache::insert`には`PageCache`の`bypass`スロットに相当する「1エントリが予算を超えた場合の退避」が無い。7zの展開結果(`SevenZExtracted`)が`max_bytes`を単体で超えるケースは、無条件evictループが空になるまで回った上で、超過したまま`entries`に挿入される(=一時的に予算超過状態になる)。実害は今のところ未確認だが、上記の比率調整と合わせて検討したい。
- 複数の7zアーカイブを行き来する使い方では、FileCacheの通常のフォルダ距離LRU evictionがそのまま働く(7z専用の特別な保持ロジックは無い)。「直前に見た7zに戻ってきたら再展開なし」という恩恵はLRUに乗る範囲でのみ得られる。
