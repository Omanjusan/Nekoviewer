(C) 拡縮
    - ズーム・パン。手動回転（実装済み、[docs/features/image-orientation.md](features/image-orientation.md)）
      と合わせてページの見た目調整の一群として扱う。
(F) コマフォーカス移動
(H) コピペ（当アプリからのコピーだけでいい書き込みをみる系はやらない。エクスプローラー代替にはならない）OS側のエクスプローラーを呼ぶのはあってもいいかも
(I) キーボードオペレーション対応をエクスプローラー部でも行う(詳細、メニューアクセスとか)
(J) キーアサイン機能 残作業
    - display_name()のi18n化（現状ReaderAction/ExplorerActionの表示名が日本語ハードコード。i18n::t()経由の多言語対応パターンに合流させる）
    - Explorer側のマウス割り当て対応（現状キーボードのみ）
    - 「削除」ボタン（既定値に戻すのではなく、そのスロットを完全に無効化する機能）。ActionBindingの型（`Option<KeyCombo>`で未設定＝既定値）では3値目の「無効化」状態を表現できないため設計変更が必要

## ファイルID管理の残件（[features/file-identity.md](features/file-identity.md)）

- キャッシュ整理: 「見つからないまま保持日数（設定→その他、1〜120日・既定60）を過ぎたID」のDBからの削除と、手動の
  「キャッシュ整理」。削除したIDの記録・サムネ行・旧v1の行の扱いを合わせて決める。解決UIの「削除まで残り○日」はこの削除の予告
- 手動紐付け画面: 自動復旧できなかった記録（バックフィルが間に合わなかった旧データ等）を、ユーザーが手で紐付ける。
  解決UI（[fingerprint-resolution-ui.md](features/fingerprint-resolution-ui.md)）の「候補から選ぶ」操作と画面を共通化できる
- 製品版の自動バックアップ: `dev_db_backup::ensure_pre_migration_backup`（DBを開く前に1回きりコピー）は起動処理へ接続済み。
  失敗時の確認ダイアログとID層の無効化（`file_identity::set_enabled`）も実装済み。設定画面の「バックアップフォルダを開く」も実装済み。残り: 試験。開発用のバックアップ/リストアUIは実リリース時に削除（[dev-db-backup.md](dev-db-backup.md)）
- お気に入りの横断一覧（IDレコードの全件走査）が、数十万ファイル規模でどれだけかかるかの確認

