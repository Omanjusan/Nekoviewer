# ビルドオプション（Cargo features）

アーカイブ各形式のバックエンドは Cargo feature で切り替えられる。不要な形式をビルドから外すことで
バイナリサイズと依存クレートを削減できる。設定は `Cargo.toml` の `[features]` にある。

## 一覧

| feature | 既定 | 内容 | 追加される依存 |
| --- | :---: | --- | --- |
| `fmt-7z` | ✅ on | 7Z / CB7 の読み込み | `sevenz-rust2` |
| `fmt-tar` | ✅ on | TAR / CBT（raw）と tar.gz / tgz（gzip）の読み込み | `tar`（gzip は既存の `flate2`） |
| `tar-zstd` | ✅ on | tar.zst / tzst の読み込み | `ruzstd`（純 Rust, C依存なし） |
| `tar-xz` | ⬜ off | tar.xz 対応の**枠のみ**（未実装） | 実装時に liblzma（C依存） |

`default = ["fmt-7z", "fmt-tar", "tar-zstd"]`。何も指定しなければ 7z と tar（raw + gzip + zstd）が有効。
`tar-zstd` は tar バックエンドを前提とするため、有効化すると `fmt-tar` も自動的に有効になる。

### ZIP / CBZ は常時有効（feature ではない）

zip/cbz は画像・コミックビューアの基幹形式であり、キャッシュ層（`cache.rs` の `OpenArchive::Disk/Mem`
など）が zip 型に依存しているため、feature 化せず常にコンパイルする。したがって zip を外したビルドは
提供しない。

### tar-zstd は純 Rust で実装済み

`tar-zstd` は `ruzstd`（純 Rust の zstd 実装）でデコードするため、C ライブラリを引き込まず
**単一バイナリ + musl 静的リンク**という配布方針（`.claude/CLAUDE.md` 参照）と両立できる。
そのため既定で有効にしている。判定は `src/fs/archive/detect.rs` のマジックバイト `28 B5 2F FD` を最優先、
拡張子 `.tar.zst` / `.tzst` をフォールバックとする。解凍は `src/fs/archive/tar.rs` の `open_reader()` が
`ruzstd::decoding::StreamingDecoder` で透過的に行い、以降は raw/gzip tar と同じ一括展開経路に相乗りする。

### tar-xz は「枠」のみ

`tar-xz` は現状 `[features]` に定義してあるだけで、デコード経路は未実装。liblzma（C 依存）を引き込み、
上記の配布方針と相性が悪いため既定で無効にしている。実装する場合は、当該 feature に `dep:` を割り当て、
`open_reader()` に先頭 `FD 37 7A 58 5A 00` のマジックバイト判定と liblzma デコーダの差し込みを追加する。

- `.zst` / `.xz` 単体（アーカイブでない単一圧縮ファイル）は別スコープで、tar 系 feature には含めない

## ビルド例

```sh
# 既定（zip + 7z + tar（raw/gzip/zstd））
cargo build --release

# zip のみ（最小構成。7z/tar とその依存を除外）
cargo build --release --no-default-features

# zip + 7z のみ（tar を除外）
cargo build --release --no-default-features --features fmt-7z

# zip + tar（raw/gzip のみ、zstd 非対応）
cargo build --release --no-default-features --features fmt-tar

# zip + tar（zstd 込み）。tar-zstd は fmt-tar を自動で引き込む
cargo build --release --no-default-features --features tar-zstd
```

## 挙動への影響

- 無効化した形式のファイルは `fs/dir.rs::list_archives` の列挙対象から外れ、ブラウザ上に表示されない
  （例: `fmt-7z` 無効時は `.7z` / `.cb7` を一覧しない。`tar-zstd` 無効・`fmt-tar` のみ有効時は
  `.tar.zst` / `.tzst` を一覧しない）。
- 形式判定（`fs/archive/detect.rs::detect_format`）は、無効な形式のマジックバイト・拡張子を判定候補から
  除外する。有効なバックエンドが無いファイルは Zip とみなされ、内容が zip でなければ空として扱われる。
- 実行時の対応形式はマジックバイト優先で判定するため、拡張子偽装（`.cbz` の中身が実は 7z 等）でも
  有効なバックエンドがあれば正しく開ける。

## CI / リリース

`.github/workflows/` のリリースビルドは既定 features（zip + 7z + tar（raw/gzip/zstd）、いずれも純 Rust 依存）
でビルドする。C 依存を伴う `tar-xz` を有効化する場合は、対応するツールチェーン整備が別途必要になる。
