# Nekoviewer

ZIP / CBZ 形式のマンガアーカイブを快適に閲覧するための、シングルバイナリのデスクトップビューアです。

[English README](README.md)

---

## 目的

本棚のように並んだフォルダを掘り下げながら、アーカイブ内の画像ファイル群を開いて読むという一連操作をアプリ内ビューアーウィンドウで素早く行う。

- たぶんLinux/Windows両対応
- ファイルシステム直接参照 — 専用データベースや外部サービスへの依存なし
- 軽量動作 — サムネイルキャッシュによる再表示コスト削減、RUST採用によるリーク0と軽量高速化
- シングルバイナリ — インストール不要。実行ファイルと設定ファイルだけで完結。ただし実行フォルダは掘ってそこで動作させるほうがいい/キャッシュフォルダを本体バイナリ配下に置く時はなおさら推奨
- アンインストール不要。レジストリ参照していないのでEXEとその配下の自動生成フォルダを全消しすればクリーンに
- SMBのネットワーク越しのフォルダ参照も想定。キャッシュはローカルに保存するのでネットワークでパスが特徴的なことになっても動作可能

---

## インストール / ビルド

### Windows

[GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) から最新の `nekoviewer.exe` をダウンロードして任意のフォルダに置いてください。インストール不要です。

セキュリティソフトに誤検知される場合は [VirusTotal チェックページ](https://www.virustotal.com/gui/url/883c1d800c90c40c2ef478fbe8a2ad0627a8d780e3e7b825794864cb23c2b473) を参照してください（v0.2.0 時点）

### Linux

Rust toolchain（`cargo`）と `make` が必要です。

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
make release
```

`make release` は初回実行時に不足している依存パッケージ（`nasm`、`dav1d` 等）のインストールを案内します。
`make help` ヘルプ表示。迷ったらこれで。

---

## 使い方

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
| `Home` | 先頭ページへ(未実装) |
| `End` | 末尾ページへ(未実装) |
| `1` | 単ページ表示 |
| `2` | 見開き（左綴じ） |
| `3` | 見開き（右綴じ） |
| `4` | 見開き時のオフセット調整-1(オフセットは-1~+1の間で制限される) |
| `5` | 見開き時のオフセット調整+1(オフセットは-1~+1の間で制限される) |
| `F5`〜`F8` | ウィンドウ位置・サイズのスロット記憶 / 呼び出し |
| `Enter` / 左ダブルクリック | 原寸表示・ウィンドウサイズに合わせるを切り替え
| `ALT+ENTER` / CMB | フルスクリーン-ウィンドウモード切り替え
| `ESC` | ビューアーウィンドウ閉じ

### 対応フォーマット

**アーカイブ:** ZIP, CBZ, (読み込み可能な単独画像ファイル)

**画像:** JPEG, PNG, WebP, GIF, PNG, BMP
**アニメーション再生対応:** WebP, GIF, (APNGまだ未定)

### 設定ファイル（`nekoviewer.conf`）

実行ファイルと同じフォルダに自動生成されます。初回起動後に編集してください。主な設定項目：

```conf
[startup]
# true にすると前回開いたフォルダから起動
use_last_dir = false
# 固定の起動フォルダ（空欄はホームディレクトリ）
fixed_dir =

[cache]
# local : 実行ファイル配下の cache/ に保存（開発・確認用）
# xdg   : %LOCALAPPDATA%/nekoview/cache/ に保存（本番推奨だが評価版状態なのでlocalで問題なし）
storage = local
# ページキャッシュの上限メモリ（MB）。省略時はシステムRAMの30%
# max_mb = 

[worker]
# デコードスレッド数。0 = 自動（論理コア数の半分）
decode_threads = 0

[thumbnail]
# nearest / triangle / catmullrom / lanczos3
filter = triangle

[viewer]
# nearest / triangle / catmullrom / lanczos3
filter = catmullrom

[grid]
# サムネイル長辺サイズ（px）。64〜512
thumb_size = 256
```

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
