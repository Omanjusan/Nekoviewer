# ウィンドウ位置・ビューアー復帰の自動テスト（Xephyr + 制御口）

Wayland では窓の位置を取得・指定できず、手動でも確認できない。そこで入れ子の X サーバー **Xephyr** の中で実アプリを
起動し、テスト専用の制御口から操作・観測する。対象は「ウィンドウ位置」メニュー（記録・展開・削除・自動展開）、
ビューアー復帰、画面外補正。機能の全体像は [features/viewer-toolbar.md](features/viewer-toolbar.md)。

## 実行

```
cargo test --features test-control --test window_pos_e2e -- --ignored --test-threads=1
```

- 前提: `Xephyr` が入っていること（無ければ各テストは何もせず成功で抜ける）。GPU 描画（RADV など）が使えること。
- 既定では ignore。通常の `cargo test` では動かない。`--test-threads=1` を付けること（Xephyr を1つずつ立てる）。
- 実データには触れない。HOME / XDG_* / XDG_RUNTIME_DIR は一時ディレクトリ（`std::env::temp_dir()`）へ向ける。
  失敗時の調査用に残したいときは `NEKO_E2E_KEEP=1`。一時ディレクトリの `app.log` にアプリの標準エラーが入る。
- テスト側が Xephyr とアプリを子プロセスとして起動・終了する。手で kill する必要はない。

## 仕組み

- Cargo feature `test-control`（既定OFF）。通常ビルドにはコードが入らない。実装は [test_control.rs](../src/test_control.rs)。
- `NEKO_TEST_CONTROL=<dir>` を付けて起動すると、`<dir>/cmd.txt`（1行1コマンド）を50msごとに確認して実行し、削除する。
  - `move x y w h` — ビューアー窓を物理座標で動かす（画面外へ飛ばす用）
  - `recover` — メニューバーの「ビューアー復帰」と同じ
  - `close` — ビューアー窓の OS クローズ（Xボタン）と同じ処理
  - `launchx11 1|0` — 「次回起動時に X11（XWayland）で起動する」の保存値を変える（メニューのチェックと同じ保存経路）
  - `apply N` / `record N` / `delete N` — スロット操作（N=1〜4、F5=1）。メニューと同じ関数を通る
  - `auto N` / `auto none` — 「開いたとき自動展開」
  - `dump` — 状態が落ち着いてから `<dir>/dump.json` に書く（窓の位置・サイズ、モニター一覧、スロット、自動展開）
- `NEKO_FAKE_MONITORS="x,y,w,h;x,y,w,h"` でモニター一覧を差し替える（論理座標）。
  Xephyr はモニターを複数に見せられないため、複数モニターの補正はこれで擬似的に確かめる。
- `NEKO_FAKE_BACKEND=wayland|xwayland|unavailable` でメニュー最上段の表示だけを擬似的に切り替える（実バックエンドは変えない）。
  Xephyr は X11 セッションで、実際の Wayland 起動は再現できない。

## テストの内容（[tests/window_pos_e2e.rs](../tests/window_pos_e2e.rs)）

| テスト | 確かめること |
|---|---|
| `recover_brings_an_offscreen_viewer_back_to_the_center` | 画面外の窓が中央へ戻る。サイズは維持。画面より大きい窓は収まるよう縮む |
| `closing_the_viewer_destroys_its_os_window` | ビューアーを閉じると OS 窓が破棄される（残ると描画もイベント処理もされず固まる） |
| `record_apply_delete_roundtrip` | 記録した位置・サイズへ展開できる。削除後の展開は何も起きない |
| `apply_pulls_offscreen_slots_in_and_keeps_second_monitor_positions` | 2枚目モニター上の記録はそのまま展開。モニター外の記録は近いモニターの内側へ寄る |
| `next_launch_backend_choice_is_persisted_and_the_checkbox_means_a_wish_to_switch` | 次回起動モードが保存され、再起動後も残る。XWayland 起動中に外すと false に戻る |
| `auto_apply_persists_across_restart_and_survives_deleting_its_slot` | 自動展開が再起動後も効く。指定スロットを削除してもチェックは外れず空のまま。「なし」も永続する |

## 限界

- 実際の Wayland ⇔ XWayland の切り替え（起動時の `with_x11()`）は、この Xephyr のテストでは確かめられない。
  実機の Wayland セッションで、保存値 true なら X11 バックエンドになり窓の位置が取れること、
  保存値 false なら取れないこと、保存値 true で `DISPLAY` が無くても起動できることは、
  一時ディレクトリを使った手動実行で確認した（実データには触れていない）。

- 実際のモニター一覧の取得（`available_monitors`）と、複数モニターでの「モニターN」表示は、このテストでは確かめられない。
  Xephyr の RANDR は仮想モニターを受け付けない。
- 窓マネージャーが無いので、枠やタイトルバーを含む外寸の扱いは確かめられない（外寸=内寸として扱われる）。
- Xephyr 上の winit は、サイズ0や1x1のダミーモニターを返すことがある。アプリ側は極小のモニターを無視する
  （`ViewerState::slot_monitors`、`recover_viewer_window`）。
