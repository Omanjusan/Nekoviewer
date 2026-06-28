# app / viewer リファクタリング候補

v0.6.1現状の問題を整理し、手を入れる順序と具体的な修正案を記録する。

---

## 現状の問題点サマリー

| ファイル | 行数 | 主な問題 |
|--------|------|---------|
| `app.rs` | 1241 | `ui()` 700行超。描画・ポーリング・ナビゲーション・ビューア管理が混在 |
| `viewer.rs` | 1026 | `show()` 550行超。テクスチャ管理・入力処理・UI描画・アニメーションが混在 |

---

## 候補 A: `ViewerOutput` 型の導入（viewer → app 通信の一本化）

### 現状

viewer から app への通信が 3 ルートに散らばっている。

| 通信経路 | 内容 | 場所 |
|---------|------|------|
| `show()` 戻り値 `ViewerNav` | ファイル間移動要求 | viewer.rs:815–819 |
| `v.open = false` フラグ直書き | ウィンドウ閉鎖 | viewer.rs:872 |
| `v.save_requested = true` フラグ直書き | スロット永続化要求 | viewer.rs:612 |

app 側では `v.open` と `v.save_requested` を毎フレーム直接チェックしている（app.rs:556–571）。

### 修正案

`show()` の戻り値を `ViewerOutput` 構造体に変更し、フラグ経由の通信を廃止する。

```rust
// viewer.rs に追加
pub struct ViewerOutput {
    pub nav: ViewerNav,
    pub close_requested: bool,
    /// Some(_) のとき app 側で永続化する
    pub save_slots: Option<[Option<WindowSlot>; 4]>,
}

impl ViewerOutput {
    fn none() -> Self {
        Self { nav: ViewerNav::None, close_requested: false, save_slots: None }
    }
}
```

`show()` のシグネチャ変更:
```rust
pub fn show(&mut self, ui: &mut egui::Ui, page_cache: &PageCache) -> ViewerOutput
```

app.rs 側は `output.close_requested` / `output.save_slots` で判断し、`viewer` の pub フィールドを直接読まない。

### 影響範囲

- `viewer.rs`: `show()` 戻り値の組み立て方法を変更、`open` / `save_requested` は `pub` 廃止
- `app.rs`: deferred viewport コールバック内の `nav_arc` 型を変更、フラグチェックブロック（556–571行）を `output` 処理に統一

### コスト: 低〜中

---

## 候補 B: `ViewerState` pub フィールドの非公開化

### 現状

`ViewerState` の pub フィールド一覧（viewer.rs:100–144）とapp.rs側のアクセス:

| フィールド | app.rs でのアクセス | 適切な扱い |
|-----------|------------------|----------|
| `archive_path` | 読み取り（cur_path 取得など） | 読み取り専用アクセサ |
| `entries` | 読み取り（prefetch ループ） | 読み取り専用アクセサ |
| `is_raw_file` | 読み取り（viewer 開封時の分岐） | 読み取り専用アクセサ |
| `page_mode` | 読み取り（メニューバーのラベル） | 読み取り専用アクセサ |
| `open` | 読み取り＋書き込み（閉鎖判定） | 候補 A で廃止 |
| `save_requested` | 読み取り＋書き込み（永続化） | 候補 A で廃止 |
| `first_frame` | 読み取り＋書き込み（サイズ指定） | `pub(crate)` に格下げ、またはアクセサ |
| `slots` | 読み取り（スロット取り出し） | 候補 A の `save_slots` 経由に移動 |
| `textures` | 直接アクセスなし（現状不使用） | 非公開化で問題なし |
| `fullscreen` | 直接アクセスなし | 非公開化で問題なし |

### 修正案

```rust
impl ViewerState {
    pub fn archive_path(&self) -> &PathBuf { &self.archive_path }
    pub fn entries(&self) -> &[ViewerEntry] { &self.entries }
    pub fn is_raw_file(&self) -> bool { self.is_raw_file }
    pub fn page_mode(&self) -> PageMode { self.page_mode }
    // first_frame は app.rs 内でのみ必要なので pub(crate) で十分
    pub(crate) fn take_first_frame(&mut self) -> bool {
        let f = self.first_frame;
        self.first_frame = false;
        f
    }
}
```

`slots` は候補 A の `ViewerOutput::save_slots` 経由になるので `pub` 不要になる。

### コスト: 低（候補 A の後に実施すると自然に整理される）

---

## 候補 C: `app.ui()` のメソッド分割

### 現状

`ui()` の内部で責任が異なるフェーズが順番に並んでいるだけで、メソッドとして分離されていない（app.rs:428–1129）。

```
ui()（700行）
├── 430–518行  バックグラウンドワーカーポーリング（5種）
├── 520–544行  ページ先読みスライディングウィンドウ
├── 546–618行  deferred viewport 結果処理 + viewer ウィンドウ生成
├── 620–693行  エクスプローラーキーナビゲーション
├── 695–808行  メニューバー描画
├── 810–904行  左パネル描画（ツリー + ドライブ）
├── 907–1103行 中央パネル描画（サムネグリッド）
└── 1105–1128行 トースト描画
```

### 修正案

```rust
impl NekoviewApp {
    fn poll_workers(&mut self, ctx: &egui::Context) { /* 430–518行 */ }
    fn prefetch_pages(&self) { /* 520–544行 */ }
    fn process_viewer_output(&mut self, output: ViewerOutput) { /* 546–573行 */ }
    fn draw_viewer_viewport(&mut self, ctx: &egui::Context) { /* 576–618行 */ }
    fn handle_explorer_keys(&mut self, ctx: &egui::Context) { /* 620–693行 */ }
    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) { /* 695–808行 */ }
    fn draw_folder_panel(&mut self, ui: &mut egui::Ui) { /* 810–904行 */ }
    fn draw_central_panel(&mut self, ui: &mut egui::Ui) { /* 907–1103行 */ }
    fn draw_toast(&self, ctx: &egui::Context) { /* 1105–1128行 */ }
}
```

`ui()` 本体は呼び出しの連鎖だけになる。

### 注意点

- `draw_central_panel` 内のサムネグリッドが最も長い（約200行）。分割後でもここは大きいが、他のフェーズと分離されるだけで十分読みやすくなる
- `poll_workers` は `ctx` を参照しないので `&mut self` だけで書ける部分が多い

### コスト: 中（機能の変更は一切なし、シグネチャ整理のみ）

---

## 候補 D: `viewer.show()` のフェーズ分割

### 現状

`show()` 内の責任:

```
show()（550行）
├── 352–452行  テクスチャ管理（GPUアップロード・ウィンドウ外破棄）
├── 454–459行  アニメーション用テクスチャ取り出し
├── 462–533行  入力読み取り（キー・スクロール）
├── 535–621行  ビューア内メニューバー描画
├── 624–709行  左エントリリスト描画
├── 711–801行  メインビュー描画（通常 + アニメーション）
├── 803–848行  ページ送り・ファイル間ナビゲーション計算
└── 876–894行  トースト期限チェック
```

### 修正案

```rust
impl ViewerState {
    fn update_textures(&mut self, ctx: &egui::Context, page_cache: &PageCache) { /* テクスチャ管理 */ }
    fn read_input(&self, ctx: &egui::Context) -> ViewerInput { /* 入力読み取り */ }
    fn draw_sort_bar(&mut self, ui: &mut egui::Ui) { /* ソートバー（通常 + FS 共通化） */ }
    fn draw_entry_list(&mut self, ui: &mut egui::Ui) { /* 左エントリリスト */ }
    fn draw_main_view(&mut self, ui: &mut egui::Ui, ...) { /* メインビュー */ }
    fn apply_input(&mut self, input: &ViewerInput) -> ViewerOutput { /* ページ送り・ナビ計算 */ }
}
```

**特記: ソートバーの重複（候補 D の中で最コスパが高い）**

通常時ソートバー（viewer.rs:572–621）とフルスクリーン時ソートバー（viewer.rs:641–671）がほぼ同じコードで重複している。`draw_sort_bar()` に切り出すだけで約 50 行削減できる。

### コスト: 中〜高（入力型 `ViewerInput` の設計が伴う）

---

## 実施推奨順序

| 順番 | 候補 | 理由 |
|-----|------|------|
| 1 | **候補 A**: `ViewerOutput` 導入 | フラグ通信をなくすと app/viewer の境界が確定する |
| 2 | **候補 B**: pub フィールド非公開化 | A 完了後に不要な pub が自明になる |
| 3 | **候補 C**: `app.ui()` 分割 | B 完了後に app 側の依存が整理されて分割しやすい |
| 4 | **候補 D (ソートバー重複のみ)** | 小さい割に視認性が上がる。他の D は後回しでよい |

候補 D の残り（`ViewerInput` 型導入など）は候補 A〜C が安定してから着手する。

---

## 変更しない方針のもの

- `PageCache` の `Arc<Mutex<>>` 共有：deferred viewport と app の間で渡すには現状の設計が現実的
- `viewer_nav_deferred: Arc<Mutex<ViewerNav>>`：候補 A 完了後も deferred viewport からの戻り値受け渡しとして維持する（egui の deferred viewport は戻り値を直接返せない制約がある）
- ファイルのモジュール分割（`viewer/render.rs` など）：今の行数では不要。候補 A〜C 完了後に再評価する
