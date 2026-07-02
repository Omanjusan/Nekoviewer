<!-- markdownlint-disable -->
フェーズ１は実装済みです。今のブランチの内容を確認してください


フェーズ2: メモリ見積もりとダイアログ
一番の懸念は「サンプリングした数枚の解像度が代表値として信頼できるか」です。複数枚サンプリングするとはいえ、悪意なく作られた変則的なZIP(解像度がフレームごとに頻繁に変わるもの)では見積もりが安定しません。これは完璧を狙わず「外れたらフェーズ5や動的再判定で拾う」と割り切る前提を明文化しておかないと、後で「なぜ見積もりが外れたのか」のデバッグで時間を溶かしそうです。
もう一つは、見積もり計算自体のコスト(ZIP内の数枚のヘッダだけ読むとはいえ、ZIP解凍ライブラリによっては「該当エントリだけシーク」が苦手で全エントリを舐める実装になっている場合がある)です。anim.rsの既存デコード経路がランダムアクセス可能な実装になっているか確認が要ります。

フェーズ1.5(先行対応): 現状のguard_anim_size(cache.rs)は、from_gif/from_webp/from_apng/from_avifが全フレームをRGBAデコードし終えた後に合計サイズをチェックする「後追い」の実装になっている。1枚で数GBに展開されるアニメーションが来た場合、ガードが発火する前にピークメモリがそこまで到達してしまう。フェーズ2のサンプリングも同じデコード経路を通るため、この抜け穴を先に塞ぐ。対応はフレームごとにインクリメンタルに合計サイズを見て、閾値超過時点で残りフレームのデコードを打ち切る形にする。AVIFは既存のwhile avifDecoderNextImageループにチェックを1行足すだけで済むが、GIF/APNGはcollect_frames()での一括収集をやめて手動ループに変更する必要があり、WebPはAnimDecoder::decode()の時点でlibwebp内部が既に全フレームをデコード済みのため、フレーム単位アクセスAPIの有無を個別に確認する必要がある。

フェーズ2 決定事項:
メモリ予算はフェーズ3のリングバッファ予算と統一し、新規の設定値は追加しない。既存のresolve_cache_budgets(cache.rs)が返す予算(page_max)をそのまま見積もりゲートの閾値として流用する。
サンプリングは冒頭1枚だけを高速抽出する既存のload_first_image_sequential系関数(archive.rs、ネットワークパス向けに中央ディレクトリを読まず先頭から順次パースする実装)を全体スキャンに拡張しない。代わりに、list_imagesが既に持っているentry_name一覧に対してZipArchive::by_name(中央ディレクトリ経由のO(1)ランダムアクセス)で個別エントリだけを展開する専用のサンプリング関数を新設する。
サンプル数はページ数に応じて段階的に決める: 3枚以下なら1枚(先頭)、4〜10枚なら2枚(先頭・末尾)、11枚以上なら3枚(先頭・中間・末尾)。
各サンプルのデコードには上記フェーズ1.5のインクリメンタルガードを必ず適用する。1枚のサンプル単体でハードリミットを突破する場合はその時点でガードに引っかけてよい(残りのサンプリングを打ち切り、即座に「予算超過」と判定してよい)。冗長ではあるが、サンプル単体チェックと最終的な合計見積もり値でのチェックの両方を行うことでガードの網羅性を担保する。
見積もりが予算超過と判定した場合のダイアログは、現時点では「展開に十分なメモリが確保できません」という確認のみとし、OKボタン以外の追加動作(原寸展開の続行や自動縮小)は設けない。原寸表示やウィンドウサイズ追従時の基準サイズをどう置くかが未決定のため、選択肢を増やすのはその方針が固まってからにする。

フェーズ3: リングバッファ(GIF/APNG/AVIF)
対象は1アニメーションPageContent内のフレーム間(ページ間の先読みとは別)。現在フレームから「距離」を測る基準が、連番なのか実際の再生順(逆再生やループ境界をどう扱うか)なのかを先に決めておく必要があります。特にループ再生中、最終フレーム再生中の「次」は先頭フレームに戻るので、単純な |index - current| だと境界付近で誤判定(本来近いのに遠いと判定してエビクトしてしまう)が起きえます。circular distance の計算式にしておかないと、ループ再生のたびに先頭付近フレームが無駄に再デコードされる事態になりそうです。
決定事項: 再生は前進のみ・ループはモジュロという現状の単純さを踏まえ、「デコーダを1個持ち回して次の1フレームだけ逐次デコードし、リングバッファ容量超過分は古いフレームから破棄、ループ境界(最終→先頭)でのみデコーダを先頭から作り直す」方式で実装する(素直に実装し、ループ時の再デコードによる一瞬のフリーズは許容する)。リングバッファの容量(先読み枚数)は当面固定値の定数とし、設定ファイルからの変更は将来対応(TODO)。
WebPはlibwebp内部の一括デコード(webpクレートのAnimDecoder::decode())しか使えず、フレーム単位ストリーミングにはlibwebp-sysのFFIでWebPAnimDecoderGetNext/Resetを直接叩く追加検証が要るため、フェーズ3.5として分離する（GIF/APNG/AVIFの実装を先に完了させ、WebPは別途対応）。

フェーズ3.5: リングバッファ(WebP) — 実装完了
libwebp-sys(0.9)のFFIシグネチャを直接確認し、WebPAnimDecoderGetNext(1フレームずつ取得)・WebPAnimDecoderHasMoreFrames・WebPAnimDecoderReset(先頭巻き戻し)がAVIF実装と同型で組めることを確認した上で実装。実アニメWebPのテストフィクスチャがリポジトリに無く`webp`クレート(0.3.1)にもアニメエンコード機能が無いため、往復の自動テストは見送り（フェーズ3.6の結合テストに合流）。WebPも含め全フォーマットがリングバッファ方式に統一されたため、cache.rsの`AnimatedContent`enum(Ring/Full二重構造)を撤去し`PageContent::Animated(RingAnimation)`に単純化した。

フェーズ3.6: 結合テスト(GIF/APNG/AVIF/WebP) — GIF・WebP実施済み
`test/nouka.gif`（640x360, 1316フレーム、全展開なら約1.2GB）を使い、cache.rsに`ring_integration_tests`を追加。
- `ring_anim_stays_bounded_on_real_large_gif`: 200フレーム分再生をシミュレートしても`resident_bytes()`が全フレーム分(1.2GB)の1/4未満、かつリング容量(32枚)相当に収まることを確認。
- `ring_anim_restart_replays_from_head_on_real_gif`: 終端(1316番目)がNoneになること、`restart()`後に先頭フレームへ戻れることを確認。

WebP版も追加（960x1376, 243フレーム, 全展開なら約1.28GBのアニメWebP、ユーザー実機の`/tmp/testwebp.zip`内エントリを使用、パスが環境依存のため`#[ignore]`）。
- `ring_anim_webp_stays_bounded_on_real_large_file` / `ring_anim_webp_restart_replays_from_head`: GIF版と同じ観点を確認。実装時に「前進専用のためエビクト済みフレームは再取得できない」という設計通りの挙動をテストコード側が誤解していたバグ(frame0のサイズをエビクト後に取得しようとしていた)を発見・修正。

release/debugとも全パス（release 1.8~1.5秒、debug 27秒）。APNG/AVIF分の実物ファイルでの結合テストは未実施。

**フェーズ2見積もりロジックの不整合を発見・修正(2026-07-01)**: `/tmp/testwebp.zip`(20エントリ、大きいものは960x1376/243フレームのアニメWebP)を実機の`cargo run --release`で開くと、サムネイル閲覧時にもメモリ不足ガードに引っかかるとの報告。調査の結果、`estimate_anim_sample_bytes`(フェーズ2)が「アニメを全フレームデコードしたら何バイトになるか」を見積もっており、フェーズ3/3.5のリングバッファ化(実際のランタイムは常にリング容量`DEFAULT_RING_CAPACITY`(32枚)分に収まる)を反映していなかったことが判明。診断テストで実測: 修正前は1サンプルあたり406〜1224MB(全フレーム分)、平均×20エントリ≈19GBが予算15.4GBを超えOverBudget誤判定。修正後はリング容量分だけデコードする方式に変更し、94〜161MBに縮小、正しくOkと判定されることを確認。既存のGIF単体テストの期待値とも矛盾しないことを確認済み。

フェーズ4: 動的先読み幅 — 実装完了(2026-07-01)
avg_frame_sizeを都度再計算するコストと、頻繁な先読み幅変動によるデコードのチラつき(先読み幅が縮んだ瞬間に直前までキャッシュされていたフレームが急にエビクトされ、ちょっと進んだだけで再デコードが走る)が懸念だったが、ユーザーとの相談の結果「アニメ開始時にフレーム0のバイト数から1回だけ算出し、再生中は固定」という方式に決定。GIF/APNG/AVIF/WebPいずれもimage crate等のフル合成済みキャンバスを返す実装のため、フレーム0のサイズがアニメ全体で共通という前提が成立し、都度再計算・ヒステリシスは不要になった。

決定事項:
- 予算: `page_max`(既存`resolve_cache_budgets`)の固定割合(`ANIM_RING_BUDGET_PCT`=25%、cache.rs)をアニメ1本のリング予算とする。新規設定値は追加しない。
- 容量算出: `resolve_ring_capacity(frame_bytes, budget_bytes, min_frames, max_frames)`(anim.rs)という純関数で `(budget_bytes / frame_bytes).clamp(min_frames, max_frames)` を返す。`RingAnimation::from_source`内でフレーム0デコード直後に1回だけ呼ぶ。
- 上下限: `AppConfig`に`anim_ring_min_frames`/`anim_ring_max_frames`を追加(config.rs、`[cache]`セクション)。空欄・不正値は既定(下限4/上限32)にフォールバック。
- フェーズ2の見積もりゲート(`estimate_anim_sample_bytes`, fs/archive.rs)も同じ`ANIM_RING_BUDGET_PCT`と`resolve_ring_capacity`を使うよう統一し、実際のリング容量とサンプリング見積もりの整合を取った。
- 既知の別課題(PageCacheの会計が`insert()`時点のスナップショットのみでリング成長を追跡していない)は今回のスコープ外として先送り。

`resolve_ring_capacity`の単体テスト5件(通常範囲・下限クランプ・上限クランプ・ゼロ入力フォールバック・min>max不正設定)を追加。既存のring_integration_tests(GIF/WebP実機データ)は固定32枚決め打ちだった期待値を、テスト用の大きい予算(10GB)で容量が上限32にクランプされる前提の`TEST_RING_BOUNDS`に置き換え、release/debugとも全パス確認。実機の`/tmp/testwebp.zip`(page_max≈15.4GB環境)でも`debug_probe_testwebp_zip`が正しくOkと判定することを確認済み。

フェーズ5: ハードエラー/フォールバック — 実装完了(2026-07-02)
対象は「同一アニメ内でフレームごとに元解像度が異なる変則ファイルで、再生中(中間フレーム)にframe0より大幅に大きいフレームに遭遇するケース」。表示ターゲットサイズの可変化(窓サイズ追従)はフェーズ6のスコープとして今回は対象外。

決定事項:
- 旧`ANIM_HARD_LIMIT_BYTES`(2GB固定)を廃止し、`anim_frame_hard_limit_mb`という設定値に置き換えた(config.rs、`[cache]`セクション、既存の`anim_ring_min_frames`等と同じ`UsizeDefault<N>`パターン)。空欄・不正値は既定100(MB)にフォールバック。
- 閾値は実行環境のRAM/VRAMに連動させず、一般的なアニメ解像度から逆算した固定値とした。FHD(1920x1080)は約7.9MB/フレーム、4K系列最大(DCI 4K 4096x2160)で約33.75MB/フレーム、5K(5120x2880)で約56.3MB/フレームになる計算から、「4K級までは通し、それより明確に大きい変則フレームだけ弾く」より緩めのガードレールとして100MBを既定値に採用。
- 超過フレームへの対処は、フレーム単体を都度縮小して再生を継続する方式(打ち切りや静止画への丸ごとフォールバックはしない)。frame0とフレーム2枚目以降で挙動が非対称だった旧実装(frame0超過時のみアニメーション自体を諦めて静止画1枚にフォールバック)も統一し、frame0超過時も縮小してアニメーションとして続行するようにした。
- 通知は`eprintln!`による簡易ログのみ("MUGIさんのFrameInput/EventLogデバッグ基盤"への連携は、当該基盤がリポジトリ内に存在しないことを確認した上で見送り。将来実装されたら接続する)。

実装:
- `cache.rs`に`RingAnimation::guard_frame_size(frame, hard_limit_bytes, filter, index)`を新設。生デコードサイズ(リサイズ前、w×h×4)がhard_limitを超える場合、面積ベースの縮小率(`sqrt(limit/raw)`)で当該フレームだけ縮小する。`from_source`のframe0/frame1、および`with_frame`のデコードループ内の中間フレーム、両方から共通で呼ぶ。
- `frame_hard_limit_bytes`は`spawn_worker`から`RingAnimation::from_source`まで、既存の`ring_bounds`と並ぶ新しいパラメータとして配線した(フェーズ2の見積もりゲート`estimate_anim_sample_bytes`は対象外、あちらは`ring_budget_bytes`ベースの別基準のため変更なし)。
- ユニットテスト2件追加: フレームごとに解像度が異なる合成GIFで、(1)中間フレームが超過してもhard_limit以内に縮小され後続フレームへ普通に進行できること、(2)frame0が超過しても静止画に丸ごとフォールバックせず縮小した上でアニメーションとして続行することを確認。既存のring_integration_tests(実機データ)は新パラメータの配線のみで期待値に変更なく全パス。

フェーズ6: 再デコードトグルとデバウンス
最大の懸念はWayland環境でのリサイズイベント発火パターンの不安定さです(MUGIさんの環境はZorin OS/Waylandとのことなので)。前回話した通り、デバウンスの300ms閾値が環境依存で最適値がずれる可能性があり、実機での実測調整が必須になります。あわせて、トグルONのままアプリを終了→再起動した際にトグル状態を永続化するかどうか(設定ファイルに保存するか、毎回OFFから始まるか)も地味に決めておくべき仕様です。
全体を通じての懸念は、フェーズ3のリングバッファとフェーズ2の事前見積もりが「予算」という同じ概念(memory_budget_bytes vs cache_budget_bytes)を別々に持つ可能性がある点です。これが2つの別設定値のままだと、ユーザーから見て「全体メモリ上限」と「キャッシュ上限」の関係がわかりにくくなるので、できれば単一の予算値を両フェーズで共有する設計にしておいた方が後々の整合性が取りやすいと思います。

（2026-07-02 追記）コード調査の結果、`memory_budget_bytes`という識別子は実コード中に存在せず、ドキュメント上の仮称のみだったことを確認。実装は既に`cache_budget_bytes`（`resolve_cache_budgets`の`page_max`）に一本化されており、この懸念は解消済みと判断。

フェーズ6を以下のサブフェーズに分割して実装する。

- 6-A: 設定・永続化・UI
- 6-B: デバウンス＋世代管理
- 6-C: 静止画の再デコード配線
- 6-D: アニメの再デコード
- 6-E: 実機調整・結合確認

決定事項（ユーザーとの相談による）:
- 対象は静止画・アニメ両方。トリガーはウィンドウリサイズおよびzoom_actual（実寸⇔フィット）切替の両方。
- トグルボタン・デバウンス値サイクルボタンは、ビューアー個別ではなく全アニメーション共通の設定であるため、ビューアー窓ではなく親のエクスプローラー窓のトップバーに配置する。デバウンス値は300ms/400ms/.../1000ms/100msと100ms刻みでループするサイクルボタン方式（固定値ではなくクリックで調整可能）。
- アニメ再生中にリサイズ（再デコード）が発生した場合、再生位置は先頭に巻き戻す。フェーズ5の「フリーズは許容」という前例と一貫させ、実装をシンプルに保つ判断。
- 連続リサイズ中に前回の再デコードリクエストが処理中でも、常に最新の要求のみを反映し古い結果は破棄する（世代カウンタ方式）。

フェーズ6-A/6-B 実装完了（2026-07-02）:
- `config.rs`: `ViewerConfig`に`redecode_on_resize: bool`（既定false）、`resize_debounce_ms: u64`（既定300、100刻みで100〜1000をループ）、`redecode_trigger_seq: u64`（非永続・実行時のみの世代カウンタ）を追加。前2つは`zoom_actual`/`fullscreen`と同じstateファイル永続化パターンに乗せた（`redecode_on_resize=`/`resize_debounce_ms=`行を追加、読み込み時は不正値・範囲外を既定へフォールバック）。サイクル計算は`next_debounce_ms()`という純関数で切り出した。
- `i18n.rs`: `redecode_on()`/`redecode_off()`/`redecode_debounce_label(ms)`を日英中3言語で追加。
- `view_explorer.rs`: トップバーの`[?]`ボタン列にトグルボタンとデバウンスサイクルボタンを追加（クリックのたびに`viewer_cfg`を更新して`save_state`を呼ぶ、既存の言語切替ボタンと同じ書き方）。`NekoviewApp`に`resize_redecode_last_seq`/`resize_redecode_deadline`を追加し、`logic()`から毎フレーム呼ばれる`poll_resize_redecode()`で`redecode_trigger_seq`の変化を検知→デバウンス期限をセット→期限超過で発火、という流れを実装。`notify_viewer_resized()`という公開メソッドをwinit_app.rs側の通知窓口として新設。
- `winit_app.rs`: `WindowEvent::Resized`のうちビューアー窓(is_viewer)分のみ`app.notify_viewer_resized()`を呼び、`redecode_trigger_seq`をインクリメントする配線を追加（エクスプローラー窓のリサイズは対象外）。
- `view_reader.rs`: zoom_actual切替箇所(`process_misc_input`)でも同様に`redecode_trigger_seq`をインクリメントし、リサイズと同じデバウンス経路に合流させた。
- 世代カウンタとデバウンス発火は「基盤」のみで、実際の再デコードリクエスト発行（フェーズ6-C/6-D）はまだ接続していない。発火時は`log_common!`でログのみ出す(`[resize-redecode] debounce fired (generation=N)`)。`cargo build`／`cargo test`とも既存テスト全パス（新規ユニットテストはUI寄りのため今回は追加せず、ビルド確認のみ）。
- 6-C（静止画の再デコード配線）・6-D（アニメの再デコード）は規模が大きいため別チャットで着手する。6-E（実機調整）は6-C/6-Dの完了具合を見てから判断する。

フェーズ6-C/6-D 実装完了（2026-07-02）:

着手前にユーザーと確認した設計方針: 再デコード時の目標解像度は「ビューアー窓の実サイズ（物理px）」を基準にする（zoom_actual時は無制限=原寸）。従来のデコードは全ページ固定1920×1080上限（`MAX_DISPLAY_W/H`、cache.rs）でウィンドウサイズを一切見ておらず、リサイズ再デコードが意味を持つにはこの上限を可変化する必要があったため。

- `cache.rs`: `LoadRequest`に`target_size: Option<(u32,u32)>`を追加（Noneは無制限=原寸）。`spawn_worker`のワーカーループから`load_page_content`/`decode_anim_from_ext`/`decode_ring_anim`/`load_raw_content_from_bytes`/`load_raw_file_content`まで貫通させ、末端の`resize_for_display`と`RingAnimation::from_source`が従来の固定`MAX_DISPLAY_W/H`直書きではなく引数の`target_size`でスケール計算するよう書き換えた（Noneのときは`to_rgba8()`のみでリサイズ自体をスキップ=原寸）。起動直後などまだ一度もリサイズ再デコードが発火していない状態向けに`DEFAULT_DECODE_TARGET`(=旧固定値と同じ1920×1080)を新設。`PageCache`に`remove(path, index)`を追加（bypassスロット・known_bypassも含めて破棄、再デコード結果を次のinsert()で入れ直す前提）。既存テスト(ring_integration_tests)は`decode_ring_anim`呼び出しに`Some((1920,1080))`を明示的に渡す形へ更新し、期待値は変更なし。
- `view_reader.rs`: `ViewerState`に`content_px: (u32,u32)`を追加。`show()`の毎フレーム冒頭で`ctx.content_rect().size() * ctx.pixels_per_point()`から算出し保持する（egui 0.35では`screen_rect()`が`content_rect()`に改名されている点に注意）。`visible_original_indices()`（見開き時は2枚、単ページ時は1枚のoriginal_indexを返す）、`current_decode_target(zoom_actual)`（zoom_actual時はNone=無制限、それ以外は直近の`content_px`）、`invalidate_pages()`（指定ページのテクスチャ・アニメ再生状態(`anim_states`)を破棄）を新設。
- `view_explorer.rs`: `NekoviewApp`に`decode_target: Option<(u32,u32)>`（既定`DEFAULT_DECODE_TARGET`）を追加し、`prefetch_pages()`が送る`LoadRequest`にも同じ値を使う（先読みページも表示中ページと同じ解像度で揃える）。`poll_resize_redecode()`のデバウンス発火部を`fire_resize_redecode()`呼び出しに置き換え、実処理を実装: (1) `viewer_cfg.zoom_actual`と`viewer.current_decode_target()`から新ターゲットを算出し`decode_target`を更新、(2) 表示中ページ(`visible_original_indices()`)ぶんだけ`page_cache.remove()`で既存デコード結果を破棄、(3) 同じページに対して新ターゲット付きの`LoadRequest`を再送、(4) `viewer.invalidate_pages()`でテクスチャ・アニメ状態を破棄。静止画・アニメーションとも同じ`decode_ring_anim`/`resize_for_display`経路を通るため分岐は不要（RingAnimationは新規に作られるため、フェーズ6-A決定どおり自動的に先頭フレームから再生し直される）。
- `cargo build`／`cargo test --release`とも全パス（30 passed / 4 ignored、既存ignoredテストは実機パス依存のため従来通り）。UIの実機確認（Wayland環境でのリサイズイベント発火・デバウンス300msの妥当性・実際に解像度が変わって見えるか）は未実施、6-Eで対応する。

フェーズ6-E 実装完了（2026-07-02、ユーザー実機Zorin OS/Waylandでの結合確認）:

ユーザーの実機（Zorin OS/GNOME/Wayland、DISPLAY自体は起動しているがxdotool等の自動操作ツールが無い環境）でアプリを実際に起動してもらい、ビューアーのリサイズ・トグル操作を試してもらう形で確認した。1回目の確認で「デバウンスが働いているのか微妙、ON/OFF・ウィンドウサイズに関わらず常に画像がきれいに見える」との報告があり、ログ(`/tmp/neko_run.log`)を確認したところ`[resize-redecode]`が一度も発火していないことが判明。原因調査の結果、6-Bの実装に2つのバグがあった:

1. **`poll_resize_redecode()`がエクスプローラー窓の描画パスからしか呼ばれていなかった**（`winit_app.rs::render_due_windows()`で`app.logic(&ctx)`はエクスプローラー窓用のクロージャ内でのみ呼ばれ、ビューアー窓用の`app.render_viewer(ui)`からは呼ばれていなかった）。ビューアー窓だけを操作している間はエクスプローラー窓が再描画されないため、`redecode_trigger_seq`の変化検知自体が走らなかった。
2. **デバウンス待機中に将来のフレームが明示的に予約されていなかった**。`resize_redecode_deadline`をセットしても`ctx.request_repaint_after()`を呼んでいなかったため、静止画表示中などアニメの継続描画要因が無い窓では、リサイズ直後の1フレーム限りで再評価の機会が失われ、デッドラインが「二度と評価されないまま放置」される状態になっていた（連続リサイズ中は各Resizedイベントで即座再描画されるため一見動いているように見えるが、リサイズを止めた後の最終デバウンス発火が起きない）。

修正: `NekoviewApp::poll_resize_redecode()`を`view_explorer.rs::render_viewer()`の冒頭でも呼ぶようにし（エクスプローラー窓の再描画タイミングに依存しないようにする）、かつ`&egui::Context`を受け取って待機中は毎回`ctx.request_repaint_after(残り時間)`を呼ぶよう変更した。修正後、実機ログで複数回のリサイズ操作に対して`[resize-redecode] fired (generation=N, target=Some((w,h)), pages=1)`が正しく発火し、`target`がリサイズ後の実ウィンドウサイズに追従し、連続リサイズ中は世代カウンタが進んで最後の1回だけ発火する（設計通り「最新の要求のみ反映」）ことをユーザーに確認してもらった。

なお、ユーザーの最初の「見た目がいつも綺麗」という報告自体は、`test/`ディレクトリのサンプル画像がいずれも1920×1080を大きく下回る解像度で、原寸のままデコードされるためウィンドウサイズを変えても見た目の変化が出ないという、テスト素材側の限界による面もある（1920×1080超の高解像度画像かつウィンドウを1920px超に広げた場合にのみ「リサイズ後にシャープになる」効果が視認できる設計のため）。この点は今回のバグ修正確認では検証できておらず、必要なら別途高解像度画像で確認する。

`cargo build`／`cargo test --release`とも全パス（30 passed / 4 ignored）。デバウンス既定値(300ms)自体の体感調整（早い/遅い）は、UIのサイクルボタンで100ms単位に手動調整できる設計のため、追加のコード変更は行わず据え置きとした。これでフェーズ6（再デコードトグル+デバウンス）は全サブフェーズ完了。