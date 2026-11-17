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
- [features/image-orientation.md](features/image-orientation.md) — Exif自動回転・手動回転・
  Exif ON/OFF設定
- [features/archive-sort-state.md](features/archive-sort-state.md) — アーカイブ単位のソート条件保存
- [features/sort-menu.md](features/sort-menu.md) — エクスプローラーのソート展開メニュー（基本キー・評価キー・ビューアーのデフォルト）とキー操作
- [features/card-band.md](features/card-band.md) — サムネカード下部の帯の設定（従来方式／カスタム書式・透過度）
- [features/koma-mode.md](features/koma-mode.md) — 疑似コマ送りモード（拡大表示の上のサブモード）
- [features/archive-rating.md](features/archive-rating.md) — アーカイブ評価（★半星刻み）・訪問回数・
  サムネ評価帯・評価フィルタ
- [features/file-identity.md](features/file-identity.md) — ファイルID（フィンガープリント）管理。移動・リネーム・
  コピー後も評価・しおり・見開き・ソート・登録サムネ・タグ・お気に入り・サムネを引き継ぐ仕組み
- [features/fingerprint-resolution-ui.md](features/fingerprint-resolution-ui.md) — 「DB内の重複の解決」画面のレイアウト・文面仕様
- [features/file-identity-test-checklist.md](features/file-identity-test-checklist.md) — ファイルID管理の統合テスト手順書
- [features/window-position-menu.md](features/window-position-menu.md) — ウィンドウ位置メニュー（記録/展開/削除/自動展開）・画面外補正・ビューアー復帰
- [e2e-window-pos.md](e2e-window-pos.md) — ウィンドウ位置・ビューアー復帰の自動テスト（Xephyr＋制御口）の実行手順
- [e2e-search.md](e2e-search.md) — 検索タブの自動テスト（Xephyr＋制御口）の実行手順・コマンド・観測項目
- [features/main-tag-search.md](features/main-tag-search.md) — 主要タグ検索（右クリックのワンアクション検索・取得状況の表示）と、右クリックメニューのテーマ分け
- [features/explorer-view-modes.md](features/explorer-view-modes.md) — エクスプローラーの表示モード（カード／リスト）と選択の規則、手動テスト手順
- [features/duplicate-marker.md](features/duplicate-marker.md) — 重複マーカー（同じ内容のファイルの警告）と類似ファイル一覧・完全比較、手動テスト手順
- [features/list-attr-column.md](features/list-attr-column.md) — リストの「エラー列」「属性列」（固定スロット）と、★・訪問回数の＋－編集（ロック付き）、手動テスト手順
- [features/perf-fixture.md](features/perf-fixture.md) — 速度計測フィクスチャー（開発専用）。サムネのみ／フルサポートの処理時間比較、DBクリアと復元、手動確認項目
- [features/tag-scan-perf.md](features/tag-scan-perf.md) — タグ自動抽出の高速化（zip の中央ディレクトリだけを末尾から読む近道・マジック判定の省略・従来経路への戻り条件）、計測ログの見方、手動テスト手順
- [features/archive-open-perf.md](features/archive-open-perf.md) — アーカイブを開く速度（冷たいHDD）。開く経路から `ZipArchive::new` を外す近道・FastZip・同じディスクを取り合わせない仕組み（FileCache の譲り・手すき生成・タグプレビューのデバウンス・サムネバーのページ優先と同時2本）、計測ログの見方、手動テスト手順
- [features/viewer-decode-perf.md](features/viewer-decode-perf.md) — ビューアーの静止画デコード・縮小の速度。真グレー(L8)の縮小経路の切り替え（タップ数8の境目）、等倍に近い縮小をGPUに任せる設定（既定ON・縮小率0.85以上）、縮小の内訳ログ、開発用ベンチ、見送った案（WebP高速化・libjpeg-turbo ほか）、手動テスト手順
- [features/idle-thumb.md](features/idle-thumb.md) — 手すき時間のサムネ生成（操作が3秒止まっている間に、画面外のサムネをDBへ先回りして貯める）、手動確認項目
- [features/tag-search.md](features/tag-search.md) — タグ検索タブ（フォルダ横断のタグ絞り込み、単一選択カテゴリ、左ペイン幅の可変）と手動テスト手順
- [features/search-tab.md](features/search-tab.md) — 検索タブ（新=台帳全件／旧=基点フォルダ）、検索文字列の書き方、進捗・中断・他タブでの継続、
  場所列と「実」「仮想」の移動アイコン、結果のマーカー（読み取り専用）、手動テスト手順
- [features/search-history.md](features/search-history.md) — 検索条件の履歴（条件だけを JSON Lines で保存、新旧で別ファイル、各50件、再起動後は再走査）と、空条件の禁止
- [features/virtual-folder-register-subs.md](features/virtual-folder-register-subs.md) — 仮想フォルダの
  「実サブフォルダを一括登録」（未登録分だけを子として追加。子孫まで見るかは選択）
- [features/reveal-in-file-manager.md](features/reveal-in-file-manager.md) — 選択した項目を維持したままファイラーで開く（Windows COM / Linux FileManager1、Nautilusの制限）
- [dev-db-backup.md](dev-db-backup.md) — 開発用DBバックアップ/リストア（デバッグタブ。実リリース時に削除）

## 4. 決定記録（過去の経緯・完了済み計画）

内容は実施当時のスナップショット。現状の設計を知りたい場合は上記1〜3を参照し、
「なぜこうなっているか」を知りたい場合にここを見る。

- [decisions/winit-migration.md](decisions/winit-migration.md) — eframe → 自前winitループへの移行
- [decisions/thumbbar-7z-cache.md](decisions/thumbbar-7z-cache.md) — サムネバー速度改善・7z重複展開解消

## 5. 未着手・懸念事項

- [todo.md](todo.md)
