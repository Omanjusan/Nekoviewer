# Nekoviewer States

## エクスプローラー部
- ファイル・フォルダツリーのブラウジング。ビューアーで閲覧する中心ファイルの確定用途
- アプリケーションの親ウィンドウ。ビューアー（子）は独立ウィンドウとして表示する

### 状態
- Init: 起動直後
- DirReady: 前回DIR復帰 or フォールバック位置に移動済み
- Browsing: サムネイル列挙・描画中（lazy描画のためEventReadyと並行して入力受付可能）
- EventReady: 現在フォルダのサムネイル描画完了。ユーザー入力待機中

### 正常な遷移
- Init → DirReady: 前回ディレクトリ復帰 or フォールバック
- DirReady → Browsing: サムネイル列挙開始
- Browsing → EventReady: 描画完了（ただしBrowsing中もイベント受付可）
- Browsing / EventReady → （ビューアー部へ）: サムネイルダブルクリック

---

## ビューアー部
- エクスプローラー部で確定されたアーカイブ or 画像ファイルを受取り、独立ウィンドウとして表示
- 終了はアプリ終了ではなくウィンドウクローズ。完了後エクスプローラー部にフォーカス移行

### 状態
- Hidden: 非表示（未初期化 or クローズ済み）
- Initializing: 新規初期化中
- Displayed: 画像表示中
  - サブ状態 WaitingEvent: キーイベント待機中（描画完了前でも遷移可）
  - 属性 displaymode: fullscreen / window（トグル。OS最大化はfullscreenに集約、タイトルバーなし）
- Closing: クローズ処理中

### 正常な遷移
- （エクスプローラーからダブルクリック）→
  - Hidden → Initializing → Displayed: 新規初期化して表示
  - Displayed → Displayed: 画像差し替え
- Displayed(WaitingEvent) で受け付けるイベント:
  - displaymode切替: fullscreen ↔ window トグル
  - navigation: ← → / Shift+↑↓ / スクロール → ビューアー内ファイル前後移動
  - movepage: ↑↓ / スクロール → アーカイブ内ページ移動
  - ESC → Closing
- Closing → Hidden: クローズ完了

### 異常（バグとみなす）
- Hidden 中にユーザー入力イベント
- Initializing 中にダブルクリックイベント（多重起動）
- Closing 中に画像差し替えイベント