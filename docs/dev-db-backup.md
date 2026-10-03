# 開発用DBバックアップ/リストア

FP（フィンガープリント）管理への移行を、旧パス仕様のDBへ何度でも戻して検証するための開発者向けツール。
製品機能ではなく、実リリース時に削除する（下記）。FP管理の全体像は [features/file-identity.md](features/file-identity.md)。ロジックは [dev_db_backup.rs](../src/dev_db_backup.rs)、
UIは [view_explorer/dev_db_tools.rs](../src/view_explorer/dev_db_tools.rs)。

## 使い方（設定 → デバッグタブ末尾「開発用DBツール」）

- デバッグビルドは常時表示。リリースビルドは「リリースビルドでもDBツールを表示」をONにすると表示
  （設定フォルダの `dev_db_tools.visible` で保持）
- **現DBをバックアップ**: `nekoviewer_spread.redb` を `dev_backup/baseline.redb`（1スロット）へコピー。
  既にあれば上書き確認。**FP仕様のDB（ID層の使用開始マーカーあり）はガードダイアログで拒否**する
  （旧パス仕様へ戻すための基準にならないため）
- **バックアップからリストア**: 復元を予約する（`nekoviewer_spread.redb.restore-pending`）。
  起動中のDBは差し替えず、**アプリを閉じて再起動したとき**、DBを開く前に適用される。
  適用時、現DBは `dev_backup/failed/failed-<日時>.redb` へ退避される（バグ再現状態の調査用）。
  退避に失敗したら復元を中止し、現DBと予約を残す
- **テスト用: FP仕様マーカー切替**: ガードの動作確認用。Phase 1 以降はID層が最初にIDを作るときに自動で立つ

保存先はいずれも設定フォルダ（`~/.config/nekoview/` 等）。対象は `nekoviewer_spread.redb` のみで、
ディレクトリ別の `cache.redb`（作り直せる）と `nekoviewer_tags.json` は対象外。

## 世代管理と自動バックアップ導線

`failed/` は、**5ファイル、または合計1GBを超えたら古い順に削除**（最新1件は残す）。

自動バックアップ（`backup_auto/`）は、FP仕様への移行前に取る**1回きり**のコピー。`ensure_pre_migration_backup(root)` が、
`apply_pending_restore` の後・`open_spread_db` の前（`view_explorer/mod.rs` の `spread_db` 初期化）で、
DBファイルを開かずに丸ごとコピーする（開いたDBは Windows でロックされるため）。

- DBファイルが無い（新規インストール）→ 取らない
- `backup_auto/` に `.redb` が1つでもある → 取らない（世代管理・ローテーションはしない）
- 失敗は `Err`。確認ダイアログとID層の無効化は後続フェーズ
- FP仕様のDBでも（`backup_auto/` が空なら）取る。旧パス仕様のDBが保証されるわけではない。旧DBへ戻すのは
  旧版へ戻す場合の手動復元（DBファイルの差し替え）のみで、復元UIは作らない

## 実リリース時に削除するもの

- 「リリースでも表示」チェックボックスと `dev_db_tools.visible` の扱い
- `view_explorer/dev_db_tools.rs`・`NekoviewApp::dev_db_ui`・`view_gui_config.rs` の呼び出し2箇所・
  `view_explorer/mod.rs` の起動フック（`apply_pending_restore`）
- `dev_db_backup.rs` は `cfg(debug_assertions)` へ戻す（自動バックアップ導線を製品へ紐付ける場合はその部分だけ残す）
