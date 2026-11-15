# 検索タブの自動テスト（Xephyr + 制御口）

検索タブ（新モード・旧モード・履歴・移動アイコン・マーカー）を、入れ子の X サーバー **Xephyr** の中で実アプリを起動して確かめる。
仕組みは [e2e-window-pos.md](e2e-window-pos.md) と同じ（Cargo feature `test-control`、環境変数 `NEKO_TEST_CONTROL` の制御口）。
機能の仕様は [features/search-tab.md](features/search-tab.md)・[features/search-history.md](features/search-history.md)。

## 実行

```
cargo test --features test-control --test search_e2e -- --ignored --test-threads=1
```

- 前提・注意は [e2e-window-pos.md](e2e-window-pos.md) と同じ（Xephyr と GPU 描画が必要。既定では ignore。`--test-threads=1`。
  HOME / XDG_* は一時ディレクトリで、実データには触れない。失敗時に残したいときは `NEKO_E2E_KEEP=1`）。
- 1つだけ走らせる: 末尾にテスト名の一部を足す（例: `... --test-threads=1 goto_virtual`）。
- 台帳（ファイルID）は、テストが作ったフォルダを、実アプリが**開いたときに**振る（`open <フォルダ>` の後に、台帳の件数が増えるのを待つ）。
  実際の経路どおりで、テスト専用のデータ投入は無い。
- 通常の `cargo test`（制御口なし）には含まれない。単体テストは `cargo test`。

## 制御口に足したコマンド（`cmd.txt`、1行1コマンド）

エクスプローラー側の操作。どれも、画面の操作と同じ関数を通る（[test_control.rs](../src/test_control.rs) の `ExploreCmd`）。

| コマンド | 内容 |
|---|---|
| `open <パス>` | 実フォルダを開く（パス中の空白もそのまま使える） |
| `tab real／favorites／search／virtual／tag` | 左ペインのタブを切り替える |
| `view-mode list／card` | 表示モード |
| `search-mode new／legacy` | 検索モード |
| `search-text <文字列>` | 新モードの検索文字列（行の残り全部。空も可） |
| `search-size <最小> <最大>` | 新モードのサイズ（MB）。空は `-` |
| `search-start` / `search-cancel` | 開始／中断（条件が空なら始まらない） |
| `search-select <番号>` | 履歴の番号（0始まり、全体の並びの添字）の行を選ぶ |
| `search-notice` | 注意点ダイアログを開く |
| `goto-real <番号>` | 一覧の番号のファイルについて、「実」アイコンを押したのと同じ移動 |
| `goto-virtual <番号> [選択肢]` | 同じく「仮想」。登録が複数でメニューが出たら、選択肢（0始まり）を押したことにする |
| `virtual-add <パス>` | 実フォルダを仮想ルート直下に登録する（登録済みなら、その子として足す） |
| `sim-threshold <70〜100>` | 「サムネ一致閾値」を変える |

## 観測（`dump.json` の `app.search`）

`dump` で書き出される。履歴・表示・移動・マーカー・照合の状態:

- 表示: `tab`、`mode`、`viewing`（表示中の履歴の行）、`selected`、`archives`（一覧の件数）、`history_visible`（いまのモードの行の添字）、
  `history`（各行: `label`・`hits`・`running`・`cancelled`・`unconfirmed`・`mode`・`progress`・`restored`・`saved_at`）。
- 実行: `job_running`、`any_running`、`form_text`、`notice_open`、`loading_shown`（中央に「読み込み中」を出すか）、`scan_state`。
- 移動: `current_dir`、`cursor_file`（カーソルが乗っているファイル名）、`virtual_node`、`virtual_ids`、`goto_menu`（選択メニューの項目数）。
- 台帳・マーカー: `ledger_total`、`dir_markers`、`cross_markers`（検索結果で重複マーカーが出るファイル → アイコン名）、
  `hash_rows`（保存済みハッシュの行数）、`cross_sims`（「？」が出るファイル → 類似度・件数・消去済）、`sim_inflight`、`sim_done`。

## テストの内容（[tests/search_e2e.rs](../tests/search_e2e.rs)）

| テスト | 確かめること |
|---|---|
| `new_search_finds_files_by_name_and_narrows_by_size` | 既定が新。名前のOR、サイズだけ、文字列とサイズのAND、履歴の行の選択 |
| `new_search_keeps_collecting_while_another_tab_is_shown` | 検索の直後に別タブへ移っても、結果は履歴の行へ溜まる。そのタブの一覧は汚れない。戻って行を選ぶと出る |
| `cancel_stops_the_search_and_keeps_the_row_without_a_running_job` | 中断の経路。中断後に次の検索を始められる |
| `switching_search_mode_keeps_each_modes_input_and_hides_the_tree` | 新旧の入力が独立している |
| `the_search_tab_never_shows_a_stuck_loading_label` | 検索タブに「読み込み中」が残らない。フォルダタブへ戻ると一覧が戻る |
| `goto_real_opens_the_files_folder_in_the_folders_tab_and_selects_the_file` | 「実」の移動。ファイルが選択される。検索タブへ戻ると結果がそのまま出る |
| `goto_virtual_opens_the_registered_virtual_folder_and_chooses_among_several` | 「仮想」の移動。登録が無ければ何も起きない。複数なら選択メニュー。戻ると結果が出る |
| `search_conditions_are_saved_and_a_restored_row_rescans_after_a_restart` | 条件だけが新しい順に保存される。再起動で履歴が戻り、行を選ぶと再走査され、先頭へ移る |
| `a_search_without_any_condition_cannot_start_in_either_mode` | 空の条件では、新旧とも始まらず、履歴にも残らない |
| `each_search_mode_shows_only_its_own_history_and_clears_the_results_on_switching` | 履歴の一覧がモードごと。切り替えで結果がクリアされる |
| `search_results_show_saved_duplicate_markers_read_only` | 同じ内容のファイルに、再取得／旧ファイル側が出る。重複の無いファイルには付かない |
| `search_results_show_a_lazily_matched_similar_marker_from_saved_hashes` | 似た画像の2枚だけに「？」が出る。しきい値を変えると計算し直される |

## 限界

- マウスの操作（アイコンのクリック・ホバー説明・ドラッグ）は再現できない。`goto-real` などは、押したときと同じ関数を直接呼ぶ。
  当たり判定・見た目・ホバーは、[features/search-tab.md](features/search-tab.md) の手動テスト手順で確かめる。
- 窓マネージャーが無い、GPU 描画が要る、といった Xephyr の制約は [e2e-window-pos.md](e2e-window-pos.md) と同じ。
- 数十万件規模の速度は確かめていない（計算量の実測は [features/similar-image-marker.md](features/similar-image-marker.md) の「検索結果での表示」）。
