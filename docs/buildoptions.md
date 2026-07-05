# ビルドオプション（Cargo features）

アーカイブ各形式のバックエンドは Cargo feature で切り替えられる。不要な形式をビルドから外すことで
バイナリサイズと依存クレートを削減できる。設定は `Cargo.toml` の `[features]` にある。

## 一覧

| feature | 既定 | 内容 | 追加される依存 |
| --- | :---: | --- | --- |
| `fmt-7z` | ✅ on | 7Z / CB7 の読み込み | `sevenz-rust2` |
| `fmt-tar` | ✅ on | TAR / CBT（raw）と tar.gz / tgz（gzip）の読み込み | `tar`（gzip は既存の `flate2`） |
| `tar-zstd` | ⬜ off | tar.zst 対応の**枠のみ**（未実装） | 実装時に `zstd`（C依存） |
| `tar-xz` | ⬜ off | tar.xz 対応の**枠のみ**（未実装） | 実装時に liblzma（C依存） |

`default = ["fmt-7z", "fmt-tar"]`。何も指定しなければ 7z と tar（raw+gzip）が有効。

### ZIP / CBZ は常時有効（feature ではない）

zip/cbz は画像・コミックビューアの基幹形式であり、キャッシュ層（`cache.rs` の `OpenArchive::Disk/Mem`
など）が zip 型に依存しているため、feature 化せず常にコンパイルする。したがって zip を外したビルドは
提供しない。

### tar-zstd / tar-xz は「枠」のみ

`tar-zstd` / `tar-xz` は現状 `[features]` に定義してあるだけで、デコード経路は未実装。
どちらも C ライブラリ（zstd / liblzma）を引き込み、**単一バイナリ + musl 静的リンク**という配布方針
（`.claude/CLAUDE.md` 参照）と相性が悪いため既定で無効にしている。実装する場合は、当該 feature に対応する
`dep:` を割り当て、`src/fs/archive/tar.rs` の `open_reader()` にマジックバイト判定とデコーダ差し込みを追加する
（`open_reader` 内のコメントに差し込み位置を記載）。

- `tar-zstd`: 先頭 `28 B5 2F FD` を見て `zstd::Decoder` で包む
- `tar-xz`: 先頭 `FD 37 7A 58 5A 00` を見て liblzma デコーダで包む
- `.zst` / `.xz` 単体（アーカイブでない単一圧縮ファイル）は別スコープで、tar 系 feature には含めない

## ビルド例

```sh
# 既定（zip + 7z + tar）
cargo build --release

# zip のみ（最小構成。7z/tar とその依存を除外）
cargo build --release --no-default-features

# zip + 7z のみ（tar を除外）
cargo build --release --no-default-features --features fmt-7z

# zip + tar のみ（7z を除外）
cargo build --release --no-default-features --features fmt-tar
```

## 挙動への影響

- 無効化した形式のファイルは `fs/dir.rs::list_archives` の列挙対象から外れ、ブラウザ上に表示されない
  （例: `fmt-7z` 無効時は `.7z` / `.cb7` を一覧しない）。
- 形式判定（`fs/archive/detect.rs::detect_format`）は、無効な形式のマジックバイト・拡張子を判定候補から
  除外する。有効なバックエンドが無いファイルは Zip とみなされ、内容が zip でなければ空として扱われる。
- 実行時の対応形式はマジックバイト優先で判定するため、拡張子偽装（`.cbz` の中身が実は 7z 等）でも
  有効なバックエンドがあれば正しく開ける。

## CI / リリース

`.github/workflows/` のリリースビルドは既定 features（zip + 7z + tar、いずれも純 Rust 依存）でビルドする。
C 依存を伴う `tar-zstd` / `tar-xz` を有効化する場合は、対応するツールチェーン整備が別途必要になる。
