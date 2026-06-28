# app / viewer リファクタリング候補

v0.6.1現状の問題を整理し、手を入れる順序と具体的な修正案を記録する。

---

## 候補 D: `viewer.show()` のメソッド分割

### 現状

`show()` の内部で責任が異なるフェーズが順番に並んでいるだけ（約410行、viewer.rs:456〜867行）。

```
show()（410行）
├── 456–467行  早期リターン・変数初期化
├── 466–490行  FrameInput 収集 + アニメーション状態更新
├── 492–515行  テクスチャ更新・outer_pos 記録・OS最大化検知
├── 517–577行  入力変数展開・ページモード切り替え・タイトル更新
├── 579–657行  スロットバー描画（通常 + フルスクリーン時）
├── 659–751行  左エントリリスト + CentralPanel（画像描画 + オーバーレイ）
├── 753–800行  ファイル間・ページ間ナビゲーション処理
├── 802–846行  zoom / フルスク / クローズ処理
└── 848–864行  トースト期限チェック
```

### 修正案（後方→前方の順に抽出、借用競合が少ない順）

```rust
impl ViewerState {
    // Step 1: トースト期限チェック（self のみ、競合なし）
    fn tick_toast(&mut self, ctx: &egui::Context, time: f64) { }

    // Step 2: zoom / フルスク / クローズ処理（close_self を返す）
    fn process_misc_input(
        &mut self,
        ctx: &egui::Context,
        input: &FrameInput,
        is_spread: bool,
        double_clicked: bool,
    ) -> bool { }

    // Step 3: ページ間・ファイル間ナビゲーション（ViewerNav を返す）
    fn process_navigation(
        &mut self,
        input: &FrameInput,
        is_spread: bool,
        step: i32,
        total: usize,
    ) -> ViewerNav { }

    // Step 4: CentralPanel 描画（RenderFrame で引数をまとめる）
    fn draw_central_panel(&mut self, ui: &mut egui::Ui, frame: &RenderFrame) -> bool { }

    // Step 5: スロットバー + FSソートバー（save_slots を返す）
    fn draw_top_bar(
        &mut self,
        ui: &mut egui::Ui,
        input: &FrameInput,
        style: &egui::Style,
    ) -> Option<[Option<WindowSlot>; 4]> { }

    // Step 6: アニメーション状態更新（(animating, t) を返す）
    fn update_animation(&mut self, ctx: &egui::Context, dt: f32) -> (bool, f32) { }
}
```

`show()` 本体は呼び出しの連鎖だけになる。

### 補助型: `RenderFrame`

Step 4 の `draw_central_panel` に渡すフレームローカルな描画値。
`self` から先に全部ローカルに落としてから渡すことで `&mut self` の借用競合を回避する。

```rust
struct RenderFrame {
    tex_lo:      Option<egui::TextureHandle>,
    tex_hi:      Option<egui::TextureHandle>,
    prev_tex_lo: Option<egui::TextureHandle>,
    prev_tex_hi: Option<egui::TextureHandle>,
    animating:   bool,
    t:           f32,
    anim_dir:    i32,
    monitor:     Option<egui::Vec2>,
}
```

### 借用競合への対処方針

`&mut self` メソッドを呼ぶ前に必要な値を全部ローカル変数に落とす。これを統一ルールとする。

```rust
// show() 内でローカルに確定してから渡す
let is_spread    = self.page_mode != PageMode::Single;
let step         = if is_spread { 2i32 } else { 1i32 };
let total        = self.entries.len();
let double_clicked = self.draw_central_panel(ui, &frame); // 戻り値で受け取る
self.process_navigation(&input, is_spread, step, total);
self.process_misc_input(&ctx, &input, is_spread, double_clicked);
```

### 注意点

- `process_navigation` 内で `key_next` / `key_prev` の派生計算（space/down/up の合成）も行う
- `draw_top_bar` のスロット保存ロジックは `self.slots` を書き換えるが、`save_slots` を返り値に出してフラグ代替とする
- `update_animation` の戻り値 `(animating, t)` は `RenderFrame` 組み立てに使う

### 各ステップ後の検証

```sh
cargo build   # コンパイルが通れば構造は正しい
cargo run     # ページ送り・フルスク・クローズを目視確認
```

機能ロジックは一切変えないため、コンパイルが通れば動作は保証される。

### コスト: 中（機能の変更は一切なし、シグネチャ整理のみ）

---

## 変更しない方針のもの

- `PageCache` の `Arc<Mutex<>>` 共有：deferred viewport と app の間で渡すには現状の設計が現実的
- `viewer_nav_deferred: Arc<Mutex<ViewerNav>>`：deferred viewport からの戻り値受け渡しとして維持する（egui の deferred viewport は戻り値を直接返せない制約がある）
- ファイルのモジュール分割（`viewer/render.rs` など）：今の行数では不要。候補 D 完了後に再評価する
