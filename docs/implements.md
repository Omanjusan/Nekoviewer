# Nekoviewer 実装メモ

v.1.1.0

構造・ファイル別役割・技術スタックは [architecture.md](architecture.md) を参照。本ドキュメントは
UI/操作仕様、キャッシュ以外の各種挙動の仕様メモ。

## 概要

Rust + egui（生winit + egui-wgpu、eframe不使用）で作る画像ビューアアプリ。
アーカイブ（ZIP / CBZ / 7Z / CB7 / TAR系。詳細は[formats.md](formats.md)）内の画像をサムネイル
グリッドで一覧し、選択するとOSネイティブの独立ウィンドウでビューアを開く。

---

## UI レイアウト

```
┌──────────┬────────────────────────────┐
│          │  サムネイルグリッド            │
│  フォルダ  │  [img] [img] [img] ...     │
│  ツリー   │  [img] [img] [img] ...     │
│ (左ペイン) │  (右ペイン)                 │
└──────────┴────────────────────────────┘
```

- 左ペイン: エクスプローラー型フォルダツリー
- 右ペイン: アーカイブファイルのサムネイルグリッド
- ファイルをダブルクリック → 独立したOSネイティブウィンドウでビューアを開く（同時に複数開ける）
- ビューア画面には**サムネイルバー**を重畳表示できる（上下左右いずれかの位置、アイドル時自動非表示、`gui_config.rs`の`ThumbbarPos`で設定）。アーカイブ内の全ページを小さいサムネイル列で表示し、現在地をマーカーで示す

---

## 対応フォーマット

対応拡張子・使用ライブラリの一覧は [formats.md](formats.md) を参照。非アーカイブの生画像
ファイルも閲覧できる。

---

## サムネイル仕様

### 生成ロジック

- アーカイブ内で最初に見つかった画像1枚を採用する（複数枚のサンプリングや輝度判定は行わない）
- ZIPは Local File Header を先頭から順読みし、最初にデコード成功した画像を採用（`fs/archive/zip.rs::load_first_image_sequential`）
- 7z/tarは最初の画像が見つかった時点でブロック展開を打ち切る（ソリッド圧縮の全展開を避けるため）
- リサイズ: 長辺256px固定、JPEGエンコードして保存（フォーマット選択は無い）

### アスペクト比

- **1 : √2**（A4など出版物の縦横比）でグリッドセル幅を計算

### グリッド表示サイズ

- 設定ダイアログのスライダーで64〜512pxの範囲で変更（`view_gui_config.rs`）

---

## サムネイルDB

対象ディレクトリには一切書き込まない方式。`config.cache_root()`配下（`local`=実行ファイル隣の`cache/`、`xdg`=`~/.local/share/nekoview/cache/`、Windowsは`%LOCALAPPDATA%`）に、監視対象ディレクトリの絶対パスをSHA256ハッシュ化した名前のサブディレクトリを作り、その中の`cache.redb`（redbデータベース）にファイル名をキー、(mtime, JPEGバイト列)を値として保存する（`neko_dir.rs`）。

---

## ソート

| 軸 | 昇順 / 降順 |
| --- | --- |
| ファイル名 | ✓ |
| 保存日付 | ✓ |
| ファイルサイズ | ✓ |

- 保存先はディレクトリごとではなく、`nekoviewer.state`にグローバル単一設定として保存する

---

## ビューア操作

| 操作 | 動作 |
| --- | --- |
| `←` `→` `↑` `↓` / `Space` | ページ送り |
| マウスホイール | ページ送り（蓄積式） |
| `Home` / `End` | 先頭 / 末尾ページへジャンプ |
| `1` `2` `3` | 単ページ / 見開き(左綴じ) / 見開き(右綴じ) 切替 |
| `4` `5` | 見開き時のページ送り方向制御 |
| `Shift`+`↑`/`↓`、`Shift`+ホイール | 前後のアーカイブファイルへ移動 |
| `Enter` | ズーム（実寸 ⇔ フィット）切替 |
| `Alt`+`Enter` | フルスクリーン切替（擬似フルスクリーン） |
| `Esc` | クローズ（フルスクリーン解除も兼ねる） |
| `F5`〜`F8` | ウィンドウ位置・サイズのスロット適用/保存 |

- マルチウィンドウ対応: 各ビューアが独立したOSネイティブウィンドウとして開く（生winit採用によって実現）

---

## サムネイル再生成タイミング

バッチ的な差分検出は行わない。グリッド表示のたびに各アーカイブのサムネイルをワーカーへ要求し、DB内のmtimeと現在のファイルmtimeを突合、不一致なら再生成する遅延方式（`neko_dir.rs`の読み出し処理）。

---

## 画像リサイズアルゴリズム

`fast_image_resize`クレート経由で以下を選択可能（サムネイル生成・ビューア表示で独立設定）。

- `nearest` — Nearest Neighbor — 最速・品質最低
- `triangle` — Bilinear — 速い
- `catmullrom` — CatmullRom — バランス型
- `lanczos3` — Lanczos3 — 高品質・遅い

デフォルト: サムネイル生成=`triangle`、ビューア表示=`catmullrom`

---

## 設定ファイル

TOMLではなく独自の`key = value` + `[section]`形式。実行ファイル隣に2ファイルに分離して保存する。

- `nekoviewer.conf`（`config.rs`、起動時のみ読込）: `[startup]`（use_last_dir/fixed_dir）、`[viewer]`（filter/default_slot）、`[thumbnail]`（filter）、`[grid]`（thumb_size）、`[worker]`（decode_threads）、`[cache]`（storage/cache_total_mb/anim_*）、`[log]`（perf/key/common）
- `nekoviewer.state`（`gui_config.rs`、実行中に随時上書き保存）: last_dir/window_size/sort_key/sort_ascending/lang/viewer_zoom/fullscreen/thumbbar設定等

---

## スレッド数設定

`rayon`は使用せず`std::thread`ベース。`[worker] decode_threads`（既定0=自動）で、0のときは`std::thread::available_parallelism() / 2`（最低1）をワーカースレッド数として使う。

---

## 起動方法

```sh
# 指定パスから開始
nekoviewer /path/to/dir

# キャッシュ予算の明示指定
nekoviewer --cache-max-mb 2048

# 引数なしは use_last_dir → fixed_dir → ホームディレクトリ → ルートの順にフォールバック
nekoviewer

# 多重起動禁止を無効化（デバッグ用。複数プロセスを並行起動したいとき）
NEKOVIEWER_ALLOW_MULTI=1 nekoviewer
```

### 多重起動禁止

2つ目以降のプロセスは起動処理を行わず、先発プロセスへ ping を送って即終了する
（起動パス引数は無視される）。実装は [single_instance.rs](../src/single_instance.rs)。

- **検知**: Windows は Named Mutex、Unix系は flock。いずれもOSがプロセス終了時に
  自動解放するため、異常終了時の残骸掃除は不要
- **通知**: 先発プロセスへ named pipe（Windows）/ Unixドメインソケット（Unix系）で
  ping。受信するとエクスプローラー窓へ `focus_window()` + `request_user_attention()`
  を試みる（ベストエフォート）
- **Wayland上の既知の制限**: GNOME/Mutter で動作確認したところ、compositor の
  フォーカス盗み防止ポリシーにより `focus_window()` は効かない。通知（タスクバー/
  ドックのアテンション表示）は届くので、ユーザーがそれをクリックすれば前面に出る。
  X11 / Windows は素直にフォーカスが移る見込み（Windows は実機未検証）
- 先発プロセスがロックを握ったまま ping に応答しない場合（フリーズ等）、後発
  プロセスは救済せずエラー終了する

---

プロジェクト全体の方針（対応OS・シンプル優先・実装言語等）は `.claude/CLAUDE.md` を参照。
