# Nekoviewer

ZIP / CBZ 形式のマンガアーカイブを快適に閲覧するための、シングルバイナリのデスクトップビューアです。

[English README](README.md)

---

## 目的

本棚のように並んだフォルダを掘り下げながら、アーカイブ内の画像ファイル群を開いて読むという一連操作をアプリ内ビューアーウィンドウで素早く行う。

- Linux / Windows 両対応
- ファイルシステム直接参照 : 外部サービス、外部サーバーへの依存なし
- 軽量動作 : サムネイルキャッシュによる再表示コスト削減、RUST採用によるリーク0と軽量高速化
- シングルバイナリ : インストール不要。実行ファイルと設定ファイルだけで完結。ただし実行フォルダは掘ってそこで動作させるほうがいい / キャッシュフォルダを本体バイナリ配下に置く時はなおさら推奨
- アンインストール不要。レジストリ参照していないのでEXEとその配下の自動生成フォルダを全消しすればクリーンに
- SMBのネットワーク越しのフォルダ参照も想定。キャッシュはローカルに保存するのでネットワークでパスが特徴的なことになっても動作可能
- メニューバーに多言語対応スイッチ搭載(configfileは作業中)

---

## インストール / ビルド

### Windows

[GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) から最新の `nekoviewer.exe` をダウンロードして任意のフォルダに置いてください。インストール不要ですが専用フォルダ内に入れて運用するのをおすすめします

### Linux

Rust toolchain（`cargo`）と `make` が必要です。

#### 初回

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
make release
./target/release/nekoviewer
```

`make release` は初回実行時に不足している依存パッケージ（`nasm`、`dav1d` 等）のインストールを案内します。

#### Linuxアップデート時

```bash
git pull
make release
./target/release/nekoviewer
```

`make help` ヘルプ表示。迷ったらこれで。

---

## 使い方


### Windows / Linux 共通のアップデート時

exe（Linuxでは実行バイナリ）を配置しているフォルダ内のstateファイルとconfは念の為消しておいてください
自動的に新規項目が不足しているからセーブデータも更新しますみたいな機能はまだありません

### 起動

```
nekoviewer [フォルダパス]
```

引数を省略した場合は `nekoviewer.conf` の設定、または前回開いたフォルダから起動。
基本的に引数無しでOK

### 基本操作

#### メインウィンドウ

| 操作 | 動作 |
|------|------|
| フォルダクリック | そのフォルダのアーカイブ一覧に移動 |
| Enter / サムネイルダブルクリック | セレクターがいる位置のファイルをビューアウィンドウで開く |
| ソートヘッダクリック | ファイル名 / 日付 / サイズでソート切り替え |
| カーソルキー / サムネイルアイテムクリック | アイテムセレクターの移動 |

セレクターはアーカイブファイルの場合は青、単独画像ファイルの場合は赤で表示される

#### ビューアウィンドウ

| 操作 | 動作 |
|------|------|
| `↓` / `Space` / ホイール下 | 次のページへ |
| `↑` / ホイール上 | 前のページへ |
| `SHIFT+↓` / `SHIFT+ホイール下` | 巻末時、次のファイルへ |
| `SHIFT+↑` / `SHIFT+ホイール上` | 冒頭時、前のファイルへ |
| `←` | 次のファイルへ(1P目にジャンプ) 現在ページ地点に影響されない |
| `→` | 前のファイルへ(1P目にジャンプ) 現在ページ地点に影響されない |
| `Home` | 先頭ページへ(未実装) |
| `End` | 末尾ページへ(未実装) |
| `1` | 単ページ表示 |
| `2` | 見開き（左綴じ） |
| `3` | 見開き（右綴じ） |
| `4` | 見開き時のオフセット調整-1(オフセットは-1~+1の間で制限される) |
| `5` | 見開き時のオフセット調整+1(オフセットは-1~+1の間で制限される) |
| `F5`〜`F8` | ウィンドウ位置・サイズのスロット記憶 / 呼び出し(waylandでは機能しません) |
| `Enter` / 左ダブルクリック | 原寸表示・ウィンドウサイズに合わせるを切り替え
| `ALT+ENTER` / 中央マウスボタンクリック | フルスクリーン-ウィンドウモード切り替え
| `ESC` | ビューアーウィンドウ閉じ

### 対応フォーマット

**アーカイブ:** ZIP, CBZ, 7Z, CB7, TAR, CBT, tar.gz/tgz, tar.zst/tzst（読み込み可能な単独画像ファイルにも対応）
(tar.xz は未対応、rar は検討中。詳細は [docs/formats.md](docs/formats.md) を参照)

**画像:** JPEG, PNG, WebP, GIF, BMP, AVIF, TIFF
**アニメーション再生対応:** AVIF, WebP, GIF, (APNGまだ未定)

### 設定ファイル（`nekoviewer.conf`）

主な設定項目はアプリ内のGUI設定画面に移行済み。設定ファイルは初回起動時の生成・高度な設定用途のみで、日常的な変更はGUI設定画面から行う。

---

## AI サポートについて

このプロジェクトは **Claude（Anthropic）** による AI アシスタントのサポートのもとで開発しています。

設計・実装の議論、コードのレビュー、リファクタリング提案などに活用しており、開発判断の最終的な責任は人間（作者）が持ちます。

---

## ライセンス

MIT License

Copyright (c) 2025 Omanjusan

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

---

## サードパーティライセンス

本ソフトウェアは以下のサードパーティライブラリを使用しています。

- **[redb](https://github.com/cberner/redb)** — サムネイルディスクキャッシュに使用する組み込みキーバリューデータベース。MIT OR Apache-2.0 ライセンス。
