# docs/ 索引

初めて触る人・しばらく空けて戻ってきた人向けの読む順。プロジェクト全体の基礎ルール
（対応OS、コミット規約、unsafe方針等）は [`.claude/CLAUDE.md`](../.claude/CLAUDE.md) にある。

## 1. 全体像

- [architecture.md](architecture.md) — 技術スタック、ディレクトリ/ファイル構成、ファイル別役割、
  主要な設計規則。まずここ
- [cache-design.md](cache-design.md) — キャッシュ層（ページ/ファイル/サムネDB）の設計
- [formats.md](formats.md) — 対応フォーマット一覧（拡張子・ライブラリ・ライセンス）
- [buildoptions.md](buildoptions.md) — Cargo feature によるビルド時の形式切り替え

## 2. 仕様

- [implements.md](implements.md) — UI レイアウト、サムネイル仕様、ソート、ビューア操作、
  設定ファイル、起動方法など挙動レベルの仕様メモ
- [states.md](states.md) — エクスプローラー部/ビューアー部の状態遷移

## 3. 機能仕様

- [features/favorite-files-dirs.md](features/favorite-files-dirs.md) — お気に入り機能

## 4. 決定記録（過去の経緯・完了済み計画）

内容は実施当時のスナップショット。現状の設計を知りたい場合は上記1〜3を参照し、
「なぜこうなっているか」を知りたい場合にここを見る。

- [decisions/winit-migration.md](decisions/winit-migration.md) — eframe → 自前winitループへの移行
- [decisions/thumbbar-7z-cache.md](decisions/thumbbar-7z-cache.md) — サムネバー速度改善・7z重複展開解消

## 5. 未着手・懸念事項

- [todo.md](todo.md)
