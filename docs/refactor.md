# app / viewer リファクタリング候補

v0.6.1現状の問題を整理し、手を入れる順序と具体的な修正案を記録する。

---

## 候補 C: `app.ui()` のメソッド分割

### 現状

`ui()` の内部で責任が異なるフェーズが順番に並んでいるだけで、メソッドとして分離されていない（app.rs:443〜1209行、約760行）。

```
ui()（760行）
├── 443–531行  バックグラウンドワーカーポーリング（5種）
├── 533–557行  ページ先読みスライディングウィンドウ
├── 559–671行  deferred viewport 結果処理 + viewer ウィンドウ生成
├── 673–741行  エクスプローラーキーナビゲーション
├── 743–857行  メニューバー描画
├── 860–952行  左パネル描画（ツリー + ドライブ）
├── 955–1151行 中央パネル描画（サムネグリッド）
└── 1153–1176行 トースト描画
```

### 修正案

```rust
impl NekoviewApp {
    fn poll_workers(&mut self, ctx: &egui::Context) { /* ワーカーポーリング */ }
    fn prefetch_pages(&self) { /* スライディングウィンドウ先読み */ }
    fn draw_viewer_viewport(&mut self, ctx: &egui::Context) { /* deferred viewport 生成 */ }
    fn handle_explorer_keys(&mut self, ctx: &egui::Context) { /* キーナビゲーション */ }
    fn draw_menu_bar(&mut self, ui: &mut egui::Ui) { /* メニューバー */ }
    fn draw_folder_panel(&mut self, ui: &mut egui::Ui) { /* 左パネル */ }
    fn draw_central_panel(&mut self, ui: &mut egui::Ui) { /* サムネグリッド */ }
    fn draw_toast(&self, ctx: &egui::Context) { /* トースト */ }
}
```

`ui()` 本体は呼び出しの連鎖だけになる。

### 注意点

- `draw_central_panel` 内のサムネグリッドが最も長い（約200行）。分割後でもここは大きいが、他のフェーズと分離されるだけで十分読みやすくなる
- `poll_workers` は `ctx` を参照しないので `&mut self` だけで書ける部分が多い
- `&mut self` の借用競合が起きた箇所はローカル変数に退避して対処する

### コスト: 中（機能の変更は一切なし、シグネチャ整理のみ）

---

## 変更しない方針のもの

- `PageCache` の `Arc<Mutex<>>` 共有：deferred viewport と app の間で渡すには現状の設計が現実的
- `viewer_nav_deferred: Arc<Mutex<ViewerNav>>`：deferred viewport からの戻り値受け渡しとして維持する（egui の deferred viewport は戻り値を直接返せない制約がある）
- ファイルのモジュール分割（`viewer/render.rs` など）：今の行数では不要。候補 C 完了後に再評価する
