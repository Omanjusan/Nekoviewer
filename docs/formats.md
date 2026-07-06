# 対応フォーマット一覧

v.1.1.0

拡張子ごとの対応内容・使用ライブラリ・ライセンスの一覧。feature によるビルド時の有効/無効切り替えは
[buildoptions.md](./buildoptions.md) を参照。

## アーカイブ

| 拡張子 | 対応内容 | ライブラリ | ライセンス |
| --- | --- | --- | --- |
| `.zip` / `.cbz` | 常時有効の基幹形式。中央ディレクトリを使いランダムアクセスで読み込む | `zip` | MIT |
| `.7z` / `.cb7` | feature `fmt-7z`。ソリッド圧縮前提で、開いた時点で画像を一括展開して保持する | `sevenz-rust2` | Apache-2.0 |
| `.tar` / `.cbt`（無圧縮） | feature `fmt-tar`。中央ディレクトリを持たないため全体走査し、一括展開して保持する | `tar` | MIT OR Apache-2.0 |
| `.tar.gz` / `.tgz`（gzip） | feature `fmt-tar`。マジック `1F 8B` を検出し、tar読み込み前に透過解凍する | `flate2` | MIT OR Apache-2.0 |
| `.tar.zst` / `.tzst`（zstd） | feature `tar-zstd`（`fmt-tar` を暗黙有効化）。マジック `28 B5 2F FD` を検出し透過解凍する。純Rust実装でC依存なし | `ruzstd` | MIT |
| `.tar.xz`（未実装・枠のみ） | feature `tar-xz`（default-off）。liblzma（C依存）が必要なため現状デコード未実装 | - | - |

## 画像

| 拡張子 | 対応内容 | ライブラリ | ライセンス |
| --- | --- | --- | --- |
| `.jpg` / `.jpeg` | 静止画デコード | `image`（jpeg feature） | MIT OR Apache-2.0 |
| `.png` | 静止画デコード。APNGは `is_apng()` 判定でアニメーション扱いにする | `image`（png feature） | MIT OR Apache-2.0 |
| `.gif` | アニメーション対応（`anim.rs` のリングバッファ方式で再生） | `image`（gif feature） | MIT OR Apache-2.0 |
| `.bmp` | 静止画デコード | `image`（bmp feature） | MIT OR Apache-2.0 |
| `.webp` | 静止画・アニメーション両対応。先頭バイトのマジック判定で `image` crateより優先して専用デコーダを使う | `webp`（内部で libwebp にバインド）+ `libwebp-sys` | webp: MIT OR Apache-2.0 / libwebp-sys: MIT（libwebp本体: BSD-3-Clause） |
| `.avif` | 静止画・アニメーション対応。ftypボックスのブランド（avif/avis）でシグネチャ判定してデコード | `libavif` + `libavif-sys`（AV1デコーダとして `local/libdav1d-sys` 経由の dav1d を使用） | libavif/libavif-sys: BSD-2-Clause（dav1d本体: BSD-2-Clause） |
