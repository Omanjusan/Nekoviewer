# Nekoview MVP 実装メモ

## 方針

- シングルバイナリ（`nekoview` 1本）
- egui multi-viewport でエクスプローラーとビューアを同一プロセス内の別ウィンドウとして開く
- キャッシュ・設定・ソート永続化はすべて後回し
- ZIP / CBZ のみ対応（7Z は MVP 後）
- サムネイルはキャッシュなし・即時生成（`image` crate のみ、並列化なし）

---

## スコープ外（MVP では実装しない）

| 機能 | 理由 |
|------|------|
| 7Z サポート | ZIP/CBZ で評価十分 |
| SQLite キャッシュ (`index.db`) | 速度評価は後 |
| `.neko/` ディレクトリ構造 | キャッシュ不要なら不要 |
| `sort.toml` 永続化 | ソートUIごと後回し |
| 読み取り専用ディレクトリ対応 | エッジケース |
| `config.toml` / 設定UI | デフォルト値をハードコード |
| `Ctrl+ホイール` グリッドズーム | 後回し |
| サムネイル輝度チェック（白/黒除外） | 先頭1枚固定で代替 |
| rayon 並列サムネイル生成 | シングルスレッドで十分 |
| ソート（ファイル名・日付・サイズ） | 後回し |

---

## ファイル構成

```
nekoview/
├── Cargo.toml
├── docs/
│   ├── implements.md
│   └── mvp.md          ← このファイル
└── src/
    ├── main.rs          # 引数解析・eframe 起動
    ├── app.rs           # メインウィンドウ（フォルダツリー + サムネイルグリッド）
    ├── viewer.rs        # ビューアウィンドウ（ZIP 内画像のページ送り）
    └── fs/
        ├── mod.rs
        ├── dir.rs       # ディレクトリ内の ZIP/CBZ 列挙
        └── archive.rs   # ZIP/CBZ からの画像読み出し
```

---

## 依存クレート（Cargo.toml）

```toml
[dependencies]
eframe = "0.29"
egui = "0.29"
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "webp", "gif", "bmp"] }
zip = "2"
walkdir = "2"
```

---

## 各モジュールの責務

### `main.rs`

- `std::env::args()` で第1引数をパス取得（なければ `$HOME`）
- `eframe::run_native()` でアプリを起動

### `app.rs` — `NekoviewApp`

**状態:**

```rust
struct NekoviewApp {
    current_dir: PathBuf,           // 現在表示中のディレクトリ
    dir_entries: Vec<PathBuf>,      // フォルダツリー用（サブディレクトリ一覧）
    archives: Vec<ArchiveEntry>,    // 右ペインのアーカイブ一覧
    thumbnails: HashMap<PathBuf, egui::TextureHandle>, // 生成済みサムネイル
    viewer: Option<ViewerState>,    // 開いているビューア（None = 未開）
}

struct ArchiveEntry {
    path: PathBuf,
    file_name: String,
}
```

**UI:**

- `egui::SidePanel::left` — `dir_entries` をリスト表示、クリックで `current_dir` 更新
- `egui::CentralPanel` — `archives` をグリッド表示（`egui_extras::TableBuilder` or 手動グリッド）
  - サムネイル未生成のものは灰色の placeholder
  - ダブルクリックで `viewer` を開く

**サムネイル生成:**

- `update()` 内で未生成のアーカイブを1件ずつ処理（フレームあたり1件でUIブロックを避ける）
- `fs::archive::load_first_image(path)` → `image` crate でリサイズ（長辺 256px）→ `egui::ColorImage` → `TextureHandle`

### `viewer.rs` — `ViewerState`

**状態:**

```rust
struct ViewerState {
    archive_path: PathBuf,
    image_paths: Vec<String>,  // ZIP 内の画像エントリ名（ソート済み）
    current_index: usize,
    texture: Option<egui::TextureHandle>,
}
```

**操作:**

| 入力 | 動作 |
|------|------|
| `→` / `Space` / ホイール下 | 次ページ |
| `←` / ホイール上 | 前ページ |

**描画:**

- `egui::Window::new()` で別ウィンドウとして表示
- `current_index` が変わったら ZIP から画像を読み直し、テクスチャ更新
- 画像をウィンドウサイズに合わせてアスペクト比維持でリサイズ表示

### `fs/dir.rs`

```rust
// current_dir の直下にある ZIP/CBZ ファイルを列挙
pub fn list_archives(dir: &Path) -> Vec<PathBuf>

// current_dir の直下にあるサブディレクトリを列挙（1階層のみ）
pub fn list_subdirs(dir: &Path) -> Vec<PathBuf>
```

### `fs/archive.rs`

```rust
// ZIP/CBZ の先頭画像1枚をデコードして返す（サムネイル用）
pub fn load_first_image(path: &Path) -> Option<image::DynamicImage>

// ZIP/CBZ 内の画像エントリ名を自然順ソートで返す
pub fn list_images(path: &Path) -> Vec<String>

// ZIP/CBZ から指定エントリの画像をデコードして返す（ビューア用）
pub fn load_image(path: &Path, entry_name: &str) -> Option<image::DynamicImage>
```

---

## サムネイル仕様（MVP 簡略版）

- 先頭の画像エントリ1枚を無条件採用（輝度チェックなし）
- 長辺 256px にリサイズ（`FilterType::Triangle`）
- キャッシュなし（起動のたびに再生成）

---

## 実装順序

1. `Cargo.toml` — 依存クレートを追加
2. `fs/dir.rs` — `list_archives` / `list_subdirs`
3. `fs/archive.rs` — `list_images` / `load_image` / `load_first_image`
4. `main.rs` — 引数解析と `eframe` 起動の骨格
5. `app.rs` — フォルダツリー + アーカイブ一覧（テキスト表示から始める）
6. `app.rs` — サムネイル生成・表示を追加
7. `viewer.rs` — ZIP を開いてページ送りできる最小ビューア

---

## 評価ポイント（MVP で確認したいこと）

- フォルダツリーの操作感（クリックでの移動が自然か）
- サムネイルグリッドのレスポンス（生成遅延が許容範囲か）
- ビューアのページ送りの快適さ
- egui の全体的な描画パフォーマンス
