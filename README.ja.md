# Nekoviewer

ZIP / CBZ / TAR / 7z 形式のマンガアーカイブを快適に閲覧するための、シングルバイナリのデスクトップビューアーです。

[English README](README.md)

---

## 目的

- 本棚のように並んだフォルダを掘り下げながら、アーカイブ内の画像ファイル群を開いて読むという一連操作をアプリ内ビューアーウィンドウで素早く行う
- アーカイブを収集する人、整理している人向けビューアー
- 製作者が使用目的に合ったビューアーを自作している
- AIコーディングでどこまでやれるかの実験

## 特徴

- Linux / Windows 両対応
- ファイルシステム直接参照 : 外部サービス、外部サーバーへの依存なし
- アーカイブスコアリング機能搭載。アーカイブ巻末まで読むと自動的に★0.5〜5点までの評価オーバーレイが出て採点が可能。何もせずに閉じれば未評価のまま
- 自動で記録される閲覧回数でのソートが可能。メインソート条件であるファイル名・日付・ファイルサイズと合わせて複合ソート可能。閲覧回数順/スコア順の切り替えも可能。たとえば「スコア大きい順を維持しつつ、ファイルサイズの大きい方からみたい」というフィルタリングが可能
- SMBのネットワーク越しのフォルダ参照も想定。キャッシュはローカルに保存するのでネットワークでパスが特徴的なことになっても動作可能
- GIF, WebP, AVIFのアニメーションファイル対応。高解像度で出力されるAI生成アニメーションに特に有効
- お気に入りファイル設定対応、1つの項目で多数のお気に入りフォルダフラグ設定可能
- 限定的検索機能 : サムネイルをキャッシュしたファイル群のみを対象とした検索機能付き
- アーカイブごとに見開きモードの設定保存ができる。設定後は自動復元
- アーカイブごとにソート条件を任意保存できる。再表示時に復元し、ビューアー離脱時に変更を自動保存
- 移動・削除・コピーのようなファイラー由来の機能は触れず、ビューアー機能に徹している。その分誤操作に強い（今後、コピーには対応する予定）
- 多言語対応(ja/en/cn)
- 宣伝無し、テレメトリ無し

デモンストレーションGIF
<p align="center">
  <img width="500" height="298" alt="Image" src="https://github.com/user-attachments/assets/6b986484-74ce-47cf-82d0-f5a329be600a" />
</p>

---

## インストール

### Windows

[GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) から最新の `nekoviewer.exe` をダウンロードして任意のフォルダに置いてください。インストール不要ですが、専用フォルダ内に入れて運用するのをおすすめします。

### Linux

Linux版はAppImageとFlatpakファイルを [GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) で配布しています（Flathub等の公式リポジトリ経由ではありません）。確実に動作させたい場合は、後述の[ビルド](#ビルド)を参照してソースからビルドしてください。

#### Flatpak版

```bash
flatpak install --user ./Nekoviewer-*-x86_64.flatpak
flatpak run io.github.Omanjusan.Nekoviewer
```

Flatpak版はHOME配下とマウント済みドライブを読み取り専用で閲覧します。書き込むのは `~/.var/app/io.github.Omanjusan.Nekoviewer/` 配下の設定とキャッシュだけです。

#### AppImage版

[GitHub Releases](https://github.com/Omanjusan/Nekoviewer/releases/latest) から `Nekoviewer-*-x86_64.AppImage` をダウンロードし、実行権限を付けて起動します。

```bash
chmod +x ./Nekoviewer-*-x86_64.AppImage
./Nekoviewer-*-x86_64.AppImage
```

---

## 使い方

### Windows版の起動時の注意

SmartScreenの警告が表示されますが、詳細ボタンを押して実行するを選択すると起動します。これはリリース毎に起こります。

### 起動

```
Windows: nekoviewer.exe
Linux: nekoviewer
```

[フォルダパス]を引数として受け付けていますが、基本的に何も指定せずの起動でOKです。

### 基本操作のおすすめ設定

- キーアサイン機能を実装しています。おすすめの設定手順は次の通りです。
  1. エクスプローラーのメニューバーにある「ツールボックス」ボタンをONに切り替える
  2. 適当な画像またはアーカイブを開き、ビューアーウィンドウ内に表示されるツールボックスのマスを右クリックして、各種機能をボタン化する
  3. ボタン化したマスをさらに右クリックして「キー割当」を行う

  この手順で、WASDキーでのページ送り・戻り、ファイル送り・戻りなどを実現できます。
- ビューアー位置&サイズ固定機能が利用可能です。解像度が高いモニタでは、ビューアー部を固定して表示しておくのをおすすめします（ただしWayland環境は非対応）。
- 本アプリはエクスプローラー部とビューアー部が1対1の状態を維持します。ファイルを開き直しても、ビューアー部が存在していればビューアーのレイアウトを維持し、デスクトップ上のウィンドウ配置を崩しません。

### 対応フォーマット

**アーカイブ:** ZIP, CBZ, 7Z, CB7, TAR, CBT, tar.gz/tgz, tar.zst/tzst（読み込み可能な単独画像ファイルにも対応）
(tar.xz は未対応、rar は検討中。詳細は [docs/formats.md](docs/formats.md) を参照)

**画像:** JPEG, PNG, WebP, GIF, BMP, AVIF, TIFF

**アニメーション再生対応:** AVIF, WebP, GIF, (APNGまだ未定)

## ビルド

### 初回（ソースビルド）

ソースからビルドする場合は Rust toolchain（`cargo`）と `make` が必要です。

```bash
git clone https://github.com/Omanjusan/Nekoviewer.git
cd Nekoviewer
make release
./target/release/nekoviewer
```

`make release` は初回実行時に不足している依存パッケージ（`nasm`、`dav1d` 等）のインストールを案内します。

### Linuxアップデート時

```bash
git pull
make release
./target/release/nekoviewer
```

迷ったときは `make help` でヘルプを表示できます。

### 開発者向け: Flatpak開発ビルド

一般の方はここは読み飛ばしてもらって構いません。公式Flathub BuilderとRust SDK拡張を導入してから `make flatpak` を実行します。

```bash
flatpak install --user flathub org.flatpak.Builder org.freedesktop.Sdk.Extension.rust-stable//25.08
make flatpak
```

---

## セキュリティポリシー

マルウェアスキャンの実施内容や、問題の報告方法は [SECURITY.ja.md](SECURITY.ja.md) を参照してください。

## プライバシーポリシー

本アプリがユーザーのデータを収集することはありません。生成されたサムネイル、閲覧回数やスコアなどの保存はすべてローカルDBに登録しアプリの機能内においてのみ活用されます。その他テレメトリーも一切含まれていません。翻訳機能のみ、ローカルLLMと通信するためにネットワーク通信を利用します。ユーザーが設定画面でURLを入力しない限り、通信は行われません。

## AI サポートについて

このプロジェクトは **Claude（Anthropic）** による AI アシスタントのサポートのもとで開発しています。

設計・実装の議論、コードのレビュー、リファクタリング提案などに活用しており、開発判断の最終的な責任は人間（作者）が持ちます。

---

## ライセンス

MIT License — 全文は [LICENSE](LICENSE) を参照してください。

---

## サードパーティライセンス

本ソフトウェアが使用しているサードパーティライブラリ（Rustクレート、および dav1d・libavif・libwebp など静的リンクしているネイティブライブラリ）のライセンス表記は [THIRDPARTYNOTICES.md](THIRDPARTYNOTICES.md) を参照してください。
