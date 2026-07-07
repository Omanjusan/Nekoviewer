# アーキテクチャ

v1.1.0（winit移行後）。移行の経緯・理由は [decisions/winit-migration.md](decisions/winit-migration.md) を参照。

## 技術スタック

| 用途 | クレート |
| --- | --- |
| GUI | `egui` + `egui-winit` + `egui-wgpu` + `winit` + `pollster`（**eframe不使用**） |
| 画像デコード | `image`（jpeg/png/webp/gif/bmp/tiff）+ `libavif`/`libavif-sys`（AVIF）+ `webp`/`libwebp-sys` |
| リサイズ | `fast_image_resize` |
| ZIP / CBZ | `zip`（常時有効） |
| 7Z / CB7・TAR系 | `sevenz-rust2` / `tar` + `flate2` + `ruzstd`（feature切替、[buildoptions.md](buildoptions.md)参照） |
| 永続化（サムネDB・見開き状態・お気に入り） | `redb` |
| SHA256 ハッシュ | `sha2` |
| 設定ファイル | 独自iniパーサ（自前実装、TOML不使用） |
| ディレクトリ走査 | `std::fs::read_dir`（walkdir不使用） |
| 並列処理 | `std::thread`（rayon不使用） |
| システム情報 | `sysinfo`（キャッシュ予算の自動算出用） |

## MVCに準じた構造

egui の即時描画モデルのため、View が Controller を呼ぶのではなく View が戻り値
（`ViewerOutput`）で意図を返し、App（`NekoviewApp`）が処理する。ウィンドウ・イベントループの
自前化（eframe不使用）は `winit_app.rs` が担い、`NekoviewApp` 自体はフレームワーク非依存。

```
main.rs
  └─ winit_app.rs                 ← 自前 winit イベントループ（複数OS窓の生成・render-on-demand駆動）
       └─ NekoviewApp (view_explorer.rs)  ← App / Explorer View / Controller 統合
            ├─ types.rs                   ← 純粋ドメイン状態・共有型（テクスチャなし）
            ├─ controller.rs              ← ナビゲーション純粋ロジック + メッセージ型
            ├─ view_reader.rs             ← Reader Window（ViewerState）
            ├─ cache.rs                   ← ページ/ファイル/サムネイルキャッシュ・ワーカー
            ├─ config.rs / gui_config.rs  ← 起動時設定 / 実行時state永続化
            ├─ view_gui_config.rs         ← 設定ダイアログUI
            ├─ favorites.rs               ← お気に入り（redb）
            ├─ spread_state.rs            ← 見開き状態の保存/復帰（redb）
            ├─ fs/                        ← ファイルシステム抽象（dir, archive, mount）
            ├─ anim.rs                    ← GIF/WebP/AVIF アニメーション再生（リングバッファ）
            ├─ i18n.rs                    ← 多言語対応（日本語/英語）
            ├─ neko_dir.rs                ← サムネイルDB（redb）のディレクトリ解決
            ├─ spread_offset.rs           ← 見開きオフセット値型
            └─ model_innerlog.rs / view_innerlog.rs / view_status.rs ← アプリ内ログ・ステータス窓（debug）
```

## ファイル別役割

### main.rs
エントリポイント。config/state ロード、winit `EventLoop` 起動、日本語フォント設定。
変更頻度：低。フォント候補の追加以外はほぼ触らない。

### winit_app.rs
eframe を捨てた後の自前 winit イベントループ。`ApplicationHandler` を実装し、エクスプローラー
(ROOT)・ビューアー・ステータス（debugのみ）の複数OS窓を `EguiWindow`（独立した `egui::Context` +
`egui_winit::State`）として管理する。描画はループ本体（`about_to_wait`）から直接行う
render-on-demand方式（非フォーカス窓の `request_redraw` は当てにしない）。旧 `eframe::App` の
`logic()`/`ui()`/`on_exit()` 相当の呼び出しもここから行う。`ViewportCommand`（Title/Fullscreen/
Position/Size等）は `egui_winit::process_viewport_commands` で対象 `Window` へ橋渡しするため、
`view_reader.rs` 側は eframe時代のAPI呼び出しのまま変更不要。

### types.rs
純粋なドメイン状態・共有型のみ。UI・テクスチャ・非同期状態は一切持たない。
`PageMode`（Single/SpreadLeft/SpreadRight）、`ReaderSortKey`、`ExplorerSortKey`、`ViewerEntry`
などを定義。
変更時の注意：ここに副作用・egui 依存を混入させない。

> **注意**: 本ファイルには `ArchiveModel`/`DirModel`/`AppModel`（旧MVC設計での「唯一の真実の源」
> 想定構造体）が残っているが、現状の実装では未使用（`NekoviewApp` が直接状態を保持している）。
> 削除するか実際に配線するかは未決定 — 触る前に要相談。

### controller.rs
ナビゲーションの純粋ロジック。ファイルリストの prev/next 計算（`find_next_file`）。
View → App 間のメッセージ型：`ViewerNav`（None/PrevFile/NextFile）、
`ViewerOutput`（nav + close_requested + save_slots）。
副作用なし・egui 依存なし。テスト容易。

### view_explorer.rs
App の中核。`NekoviewApp` 構造体を定義する。
責務：
- Explorer UI（ファイルツリー・サムネイルグリッド・ソートヘッダー）の描画
- ViewerState（view_reader）の生成・破棄・ナビゲーション処理
- サムネイルワーカーへのリクエスト送信・結果受信
- ドライブ/GVFS マウント一覧の表示
- ウィンドウサイズ・ソート状態の永続化

egui のツリー描画ヘルパー（`show_tree_node`）、`TreeAction` enum もここ。
最も大きく複雑なファイル。機能追加時は責務の混入に注意。

### view_reader.rs
Reader ウィンドウの描画と入力処理。`ViewerState` 構造体。
重要な設計点：`FrameInput::collect()` で `ctx.input()` をフレームに1回だけ呼び出し、以降は
そのスナップショットを参照する（複数回呼び出しによるバグ防止）。`show()` が `ViewerOutput` を
返し、呼び出し元（view_explorer）が処理。
責務：ページ表示（Single/Spread）、スクロール、フルスクリーン切り替え、キー入力・マウス入力の
処理、アニメーション更新、`PageCache` からのテクスチャ取得、`WindowSlot`（F5–F8）の保存・復元。

### cache.rs
ページキャッシュ（`PageCache`）、ファイルキャッシュ（`FileCache`）、サムネイルワーカー、各種
ワーカーを持つ。バックグラウンドスレッドで画像デコードを行い、メインスレッドに mpsc で結果を
返す。設計の詳細は [cache-design.md](cache-design.md) を参照。

### config.rs / gui_config.rs
`config.rs`: `AppConfig`（起動時設定 `nekoviewer.conf`）の読み書き。
`gui_config.rs`: 実行中に随時上書きされる state（`nekoviewer.state`：ウィンドウ位置・ソート順・
言語・ビューア設定等）の永続化。

### view_gui_config.rs
設定ダイアログの egui 描画部分のみ。データ永続化は `gui_config.rs`/`config.rs` が担当。

### favorites.rs
お気に入りフォルダ・お気に入りファイルのメンバーシップを redb で管理。仕様の詳細は
[features/favorite-files-dirs.md](features/favorite-files-dirs.md) を参照。

### spread_state.rs
見開き状態（page_mode, spread_offset）の保存・復帰を redb で管理。

### fs/
- `dir.rs`：ディレクトリスキャン・ファイル種別判定（archive か raw image か）
- `archive/`：アーカイブ抽象化層。`mod.rs` がフォーマット判定に基づき `zip.rs`/`sevenz.rs`/
  `tar.rs` へディスパッチする。共通のメモリ見積もりロジック・`decode.rs`（画像バイト列デコード）・
  `detect.rs`（マジックバイト判定）もここ
- `mount.rs`：GVFS SMB マウント・ローカルドライブ一覧取得

### anim.rs
GIF・アニメーションWebP・アニメーションAVIFの再生制御。全フレーム一括デコードではなく、
`SequentialAnimDecoder`（1フレームずつ逐次デコード）+ `FrameRingBuffer`（容量固定のリング
バッファ）方式。ランダムアクセス不可・前進のみの制約を前提に、ループ境界でのみ `restart()`
してデコーダを作り直す。

### i18n.rs
日本語/英語の UI 文字列を静的に定義。`i18n::t()` で取得。`state.lang` に基づいて起動時に設定。

### neko_dir.rs
redb ベースのサムネイルディスクキャッシュ（ディレクトリ単位）の管理。

### spread_offset.rs
見開き表示のオフセット（−1 〜 +1）を型で表現する小さなモジュール。

### model_innerlog.rs / view_innerlog.rs / view_status.rs
アプリ内ログのグローバルリングバッファとその描画（ステータス窓の一部）。debugビルドでは
独立OS窓、releaseではROOT内のフローティング窓として表示。

### local/libdav1d-sys/
vcpkg 経由でビルドした dav1d（AV1デコーダ）の Rust バインディング。AVIF デコードに使用。
通常は触らない。

### .github/workflows/
Windows（vcpkg + MSVC）と Linux（musl 静的リンク）の CI/CD。リリース時に `nekoviewer.exe` と
Linux バイナリを自動ビルド・公開。

### .cargo/
Cargo のローカル設定（`config.toml`）。musl ターゲット指定等。

## 主要な設計規則

- `ctx.input()` は `view_reader.rs` の `FrameInput::collect()` で1回だけ呼ぶ。他の場所で
  `ctx.input()` を追加しない（キーイベント消費の競合が起きる）
- `types.rs` に egui・非同期・IO を混入させない
- `controller.rs` の関数は副作用なしの純粋関数として保つ（テスト容易性）
- `unsafe` は原則禁止（`local/libdav1d-sys` のFFI境界、`anim.rs` の逐次デコーダが持つ生ポインタ
  周りを除く）
- エラーは `anyhow::Result` で統一
