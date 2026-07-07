# eframe → 自前 winit ループ 移行（決定記録）

> **状態: 移行完了**。本ドキュメントは実施当時の計画・検証記録であり、以後の変更は反映されない。
> 現在のアーキテクチャは [architecture.md](../architecture.md) を参照。

eframe を捨て、egui(UIライブラリ) は残したまま **egui + egui-winit + egui-wgpu を自前の winit
イベントループで束ねる**マルチウィンドウ構成へ移行した計画・記録。

対象ブランチ（当時）: `experiment/winit-multiwindow`

---

## 1. 背景：なぜ移行するか

eframe(0.35) は「**ROOT ウィンドウは 1 つだけ。ビューアーもステータスも `show_viewport_deferred`
で ROOT のフレームに相乗りする子ウィンドウ**」という構造を持つ。ビューアーにフォーカスが移ると
ROOT が背面でアイドル化し、**モデル更新・イベント処理そのものが止まる**。結果、ROOT 側に集約した
キー処理（`handle_viewer_keys`, commit 86eca4b）が走らず、**ビューアー前面時にファイルナビゲーション
が一切効かない**。これは egui の即時モードの問題ではなく eframe の設計制約で、workaround を複数試して
全て失敗した（3 日）。

→ 乗り換え先に必要な唯一の条件は「**アプリのモデル更新ループが、どの窓にフォーカスがあっても回ること**」。

## 2. PoC 検証結果（合格）

`poc/winit-multiwindow/`（独立クレート）で最小再現を作り、ログ実測で 2 点を確認済み。

| 検証点 | 結果 |
|---|---|
| (1) フォーカス非依存の更新＋描画 | ✅ 非フォーカス窓も `painted/s=85`（フォーカス窓と同レート）、共有 `counter` も継続 |
| (2) 窓ごとのキー配送 → 共有モデル | ✅ VIEWER 窓の Left/Right が `viewer_page` を ±1（1→10→-10 を実測） |

**PoC で得た重要な学び**：
- winit/Windows では **非フォーカス窓の `request_redraw()` → `RedrawRequested` が間引かれる**。
  描画は `request_redraw` 頼みにせず、**ループ本体から必要な窓を直接 `render` する**こと。
- eframe の「背面でモデルが止まる」とは別物。自前ループでは**描画駆動を自分で決められる**のが勝因。

## 3. 基本方針

- **egui は残す**（UI 描画コードはほぼそのまま流用）。**eframe だけ捨てる**。
- バックエンドは **wgpu**（Win/Wayland 両対応・画像向き、glow より堅い）。`egui_wgpu::winit::Painter`
  が複数サーフェスを viewport_id で管理できるので、これを 1 つ持って全窓で共有する。
- **窓ごとに独立した `egui::Context` + `egui_winit::State`** を持つ（ROOT 結合を作らない）。
- **キーは窓ごとに配送**。winit の `WindowEvent { window_id, .. }` で「どの窓のキーか」が最初から判る
  ので、86eca4b の「ROOT 集約キー処理」を**窓ごと分岐**へ戻す。
- **render-on-demand**：PoC の毎フレーム 85Hz 全描画は省電力的に本番不可。再描画が必要な窓だけを
  ループ本体から `render` する（§7）。
- 単一バイナリ維持（CLAUDE.md）。wgpu はネイティブで単一バイナリ可。

## 4. 依存関係の変更（Cargo.toml）

```toml
# 削除
eframe = "0.35"
# 追加（PoC で検証済みのバージョン）
egui-winit = "0.35"
egui-wgpu  = { version = "0.35", features = ["winit"] }
winit      = "0.30"
wgpu       = "29"      # egui-wgpu 0.35 が要求
pollster   = "0.4"     # Painter::new / set_window は async
# egui = "0.35" は維持
```

注意：egui 0.35 では `egui::Context::run` は **`run_ui`** に改名されている。

## 5. eframe 結合点の棚卸し → winit 置換 対応表

| 現状（eframe） | 場所 | 置換先（winit 自前ループ） |
|---|---|---|
| `eframe::run_native` / `NativeOptions` | [main.rs:37-58](../../src/main.rs#L37) | `EventLoop` + `ApplicationHandler`。`NativeOptions.viewport.with_inner_size` は `Window::default_attributes().with_inner_size` へ |
| `eframe::App::logic()` | [view_explorer.rs:474](../../src/view_explorer.rs#L474) | ループ本体（`about_to_wait` 等）の「常時走る処理」へ移設。viewer_closing 消費・status 駆動はここ |
| `eframe::App::ui()` | [view_explorer.rs:498](../../src/view_explorer.rs#L498) | ROOT(エクスプローラー)窓の `render` クロージャ内へ。中の egui パネル描画はそのまま流用 |
| `eframe::App::on_exit()` | [view_explorer.rs:494](../../src/view_explorer.rs#L494) | `LoopExiting` または CloseRequested 時の終了処理へ |
| `show_viewport_deferred`（ビューアー） | [view_explorer.rs:717](../../src/view_explorer.rs#L717) | 2 枚目の OS ウィンドウとして winit `create_window`。`draw_viewer_viewport` の中身は viewer 窓の `render` へ |
| `show_viewport_deferred`（ステータス, debug） | [view_status.rs:163](../../src/view_status.rs#L163) | 3 枚目の OS ウィンドウ（debug ビルド時のみ生成） |
| 日本語フォント設定 `cc.egui_ctx` | [main.rs:48-54](../../src/main.rs#L48) | 各窓の `egui::Context` 生成直後に `setup_japanese_font` を適用（窓ごとに Context があるため全窓へ） |
| `cc.egui_ctx.clone()` を `NekoviewApp::new` へ | [main.rs:56](../../src/main.rs#L56) | ワーカー起床用の ctx は「ROOT 窓の Context」または専用の `EventLoopProxy` へ（§7） |

### ViewportCommand → winit Window メソッド 対応表

現在ウィンドウ制御は全面的に `send_viewport_cmd` 依存。winit では対象 `Window` のメソッド直呼びに変える。

| ViewportCommand | 場所 | winit 置換 |
|---|---|---|
| `Title(..)` | [view_reader.rs:503](../../src/view_reader.rs#L503) | `window.set_title(..)` |
| `Decorations(bool)` | view_reader.rs:453,810,818,831 | `window.set_decorations(bool)` |
| `OuterPosition(..)` | [view_reader.rs:571](../../src/view_reader.rs#L571) | `window.set_outer_position(..)` |
| `InnerSize(..)` | [view_reader.rs:574](../../src/view_reader.rs#L574) | `window.request_inner_size(..)` |
| `Fullscreen(bool)` | view_reader.rs:806,814,827 | `window.set_fullscreen(Some(Borderless(None)) / None)` |
| `Maximized(bool)` | view_reader.rs:809,817,830 | `window.set_maximized(bool)` |
| `Minimized(true)` | [view_reader.rs:836](../../src/view_reader.rs#L836) | `window.set_minimized(true)` |
| `Focus` | [view_explorer.rs:722](../../src/view_explorer.rs#L722) | `window.focus_window()` |
| `Close` | view_explorer.rs:807, view_status.rs:181 | 当該 `Window` を drop（管理 Map から除去）。Wayland フルスクリーン中の Close 無視問題（[view_explorer.rs:687](../../src/view_explorer.rs#L687)）も「先に fullscreen 解除 → 窓除去」で素直に表現できる |

## 6. 流用境界（触らないもの）

以下はフレームワーク非依存。**100% そのまま残す**（むしろ移行を楽にする本体）。

- `model.rs` / `controller.rs`（純ロジック・メッセージ型）
- `cache.rs`（PageCache / サムネ / ワーカー。`Arc<Mutex<..>>` 共有も維持）
- `fs/`（dir / archive / mount）
- `config.rs` / `anim.rs` / `i18n.rs` / `spread_offset.rs` / `neko_dir.rs`
- `view_reader.rs` / `view_explorer.rs` の **egui パネル描画の中身**（ウィジェット構築コード）

差し替えるのは「eframe glue（ループ・窓生成・ViewportCommand）」だけ。MVC リファクタ資産は丸ごと活きる。

## 7. render-on-demand 駆動設計

PoC は毎フレーム全描画だったが、本番は「再描画が必要な窓だけ」描く。再描画トリガ：

| トリガ | 対象窓 | 備考 |
|---|---|---|
| 入力イベント | その窓 | winit が `WindowEvent` で配送 |
| アニメーション再生中（GIF/WebP/AVIF・ページ送りスライド） | ビューアー窓 | 次フレーム時刻でループを起こす |
| ページ読込中（テクスチャ待ち） | ビューアー窓 | 100ms 間隔 |
| ワーカー結果到着（ページ/サムネ/ファイルキャッシュ/サマリ/ディレクトリスキャン） | エクスプローラー窓 | 現状 `ctx.request_repaint()` で起こしている（[cache.rs:157](../../src/cache.rs#L157) が手本）。winit では **`EventLoopProxy::send_event` でループを叩き起こす**形に統一 |
| ステータス窓 1Hz（debug） | ステータス窓 | タイマーで定期 |

設計ポイント：
- 各ワーカーは `ctx` の代わりに `EventLoopProxy<UserEvent>` を持ち、結果送信後に `send_event` で起床。
  `fs/dir.rs` は egui 非依存を保つため `wake: impl Fn()` コールバック越しに渡す（CLAUDE.md のレイヤ方針）。
- ループの `ControlFlow` は基本 `Wait`、再描画予定がある窓があれば `WaitUntil(最短時刻)`。
  （PoC の `Poll` 常時回しはアイドル時 CPU を食うので本番では使わない）
- 「次にいつ再描画するか」を窓ごとに保持し、`about_to_wait` で最短を計算して `ControlFlow` を設定。

## 8. 段階実装プラン

各段で `cargo build`（debug/release 両方）を通す。

1. **土台**：Cargo.toml 差し替え。`EventLoop` + `ApplicationHandler` + `Painter` の最小骨格を `main.rs`
   に置き、**エクスプローラー窓 1 枚だけ**で既存 `ui()` の中身を表示できるところまで。
2. **ワーカー起床の付け替え**：`request_repaint` 依存を `EventLoopProxy` 起床へ。`ControlFlow::Wait`
   ＋ render-on-demand へ移行（アイドル時 CPU が下がることを確認）。
3. **ビューアー窓**：2 枚目の OS ウィンドウとして追加。`view_reader.rs` の描画中身を viewer 窓の
   `render` に載せる。`ViewportCommand` 群を §5 の winit メソッドへ置換。
4. **窓ごとキー配送**：86eca4b の ROOT 集約キー処理を撤去し、`window_event` で窓ごとに分岐
   （エクスプローラーの←→ / ビューアーの←→ を自然に使い分け）。
5. **ステータス窓**（debug）：3 枚目として追加。
6. **ナビゲーション貫通の実機確認**：ビューアー前面で前/次ファイルが効くこと（当初の本丸）を確認。
7. **クリーンアップ**：PoC の暫定ログ撤去、不要になった `viewer_closing` 等の eframe 由来 workaround
   の整理。`poc/` は役目を終えたら削除 or `docs` 参照用に残すか判断。

## 9. リスク・未解決ポイント（実装中に詰める）

- **Wayland マルチウィンドウ＋フルスクリーン**：現コードに「Wayland フルスクリーン中は Close 無視」
  の回避コメントあり（[view_explorer.rs:687](../../src/view_explorer.rs#L687)）。winit 直制御で素直になる見込みだが要実機確認。
- **クリップボード / IME / DPI / マルチモニタ**：eframe が肩代わりしていた部分。`egui-winit` が大半を
  担う（`State::on_window_event` が処理）が、初期土台で配線確認が要る。
- **テクスチャの窓間共有**：窓ごとに `egui::Context` が別なので、同じ画像を両窓で出すならテクスチャは
  窓ごとに登録が要る（現状はビューアーが主に使うので影響は限定的）。`PageCache` のデコード結果（生
  ピクセル）は共有のままでよい。
- **ウィンドウ位置・サイズ復帰**：マイルストーンの「デフォルトビューアー位置・サイズ復帰」と整合する
  形で `WindowSlot` 永続化を winit の `outer_position`/`inner_size` ベースに。
- **`Painter` の context 引数**：`Painter::new(context, ..)` に渡す Context は描画の実体には使われない
  （`paint_and_update_textures` は primitives を明示的に受け取る）。窓ごと Context とは別に専用の
  `Context::default()` を 1 つ渡せばよい（PoC で確認済み）。

---

## 付録：PoC の要点（参考記録）

PoC クレート `poc/winit-multiwindow/` は §2 の 2 検証点を確認して役目を終えたため、
段階7（クリーンアップ）で削除済み。設計が本実装（`src/winit_app.rs`）へ移ったため記録のみ残す。核心は：
- 1 つの `Painter` を全窓で共有、`set_window(viewport_id, window)` でサーフェス登録
- 窓ごとに `egui::Context` + `egui_winit::State`
- `render(painter, win, model)`：`take_egui_input` → `ctx.run_ui` → `tessellate` →
  `painter.paint_and_update_textures`
- 描画は**ループ本体から直接**（`RedrawRequested` 頼みにしない）
