#!/usr/bin/env bash
# musl静的バイナリからAppImageを組み立てる。
# 前提: make release-musl 等で target/x86_64-unknown-linux-musl/release/nekoviewer が
#       ビルド済みであること（このスクリプト自体はビルドを行わない）。
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKG_DIR="$ROOT_DIR/packaging/appimage"
BIN_PATH="$ROOT_DIR/target/x86_64-unknown-linux-musl/release/nekoviewer"
APPDIR="$ROOT_DIR/target/appimage/Nekoviewer.AppDir"
OUT_DIR="$ROOT_DIR/target/appimage"

if [ ! -f "$BIN_PATH" ]; then
    echo "エラー: $BIN_PATH が無い。先に 'make release-musl' でビルドしてください" >&2
    exit 1
fi

echo "=== AppDir 構築 ==="
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
cp "$BIN_PATH" "$APPDIR/usr/bin/nekoviewer"
cp "$PKG_DIR/nekoviewer.desktop" "$APPDIR/nekoviewer.desktop"
cp "$PKG_DIR/nekoviewer.png" "$APPDIR/nekoviewer.png"
cp "$PKG_DIR/AppRun" "$APPDIR/AppRun"
chmod +x "$APPDIR/AppRun" "$APPDIR/usr/bin/nekoviewer"

echo "=== appimagetool 準備 ==="
APPIMAGETOOL="$OUT_DIR/appimagetool.AppImage"
if [ ! -f "$APPIMAGETOOL" ]; then
    mkdir -p "$OUT_DIR"
    curl -L -o "$APPIMAGETOOL" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    chmod +x "$APPIMAGETOOL"
fi

echo "=== AppImage 生成 ==="
VERSION="${NEKOVIEWER_VERSION:-$(grep -m1 '^version' "$ROOT_DIR/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')}"
OUT_FILE="$OUT_DIR/Nekoviewer-${VERSION}-x86_64.AppImage"
# CI(コンテナ/sandbox)ではFUSEが無いことが多いため --appimage-extract-and-run を使う。
ARCH=x86_64 "$APPIMAGETOOL" --appimage-extract-and-run "$APPDIR" "$OUT_FILE"

echo ""
echo "生成完了: $OUT_FILE"
