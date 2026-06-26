#!/usr/bin/env bash
set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { echo -e "${GREEN}[✓]${NC} $*"; }
warn()  { echo -e "${YELLOW}[!]${NC} $*"; }
error() { echo -e "${RED}[✗]${NC} $*"; exit 1; }

ask() {
    read -rp "$1 [y/N] " yn
    case $yn in [Yy]*) return 0;; *) return 1;; esac
}

echo "=== Nekoviewer セットアップ ==="
echo ""

# パッケージマネージャ検出
if command -v apt-get &>/dev/null; then
    PM=apt
elif command -v apk &>/dev/null; then
    PM=apk
elif command -v brew &>/dev/null; then
    PM=brew
else
    error "対応するパッケージマネージャが見つかりません（apt / apk / brew）"
fi

pkg_install() {
    case $PM in
        apt)  sudo apt-get install -y "$@" ;;
        apk)  sudo apk add --no-cache "$@" ;;
        brew) brew install "$@" ;;
    esac
}

MISSING=()

check_tool() {
    local name=$1 pkg_apt=${2:-$1} pkg_apk=${3:-$1} pkg_brew=${4:-$1}
    if command -v "$name" &>/dev/null; then
        info "$name"
    else
        warn "$name が見つかりません"
        case $PM in
            apt)  MISSING+=("$pkg_apt") ;;
            apk)  MISSING+=("$pkg_apk") ;;
            brew) MISSING+=("$pkg_brew") ;;
        esac
    fi
}

check_tool nasm
check_tool cmake
check_tool meson
# ninja は distro によってコマンド名が違う
if command -v ninja &>/dev/null || command -v ninja-build &>/dev/null; then
    info "ninja"
else
    warn "ninja が見つかりません"
    case $PM in
        apt)  MISSING+=("ninja-build") ;;
        apk)  MISSING+=("ninja") ;;
        brew) MISSING+=("ninja") ;;
    esac
fi
# pkg-config も distro によって名前が違う
if command -v pkg-config &>/dev/null || command -v pkgconf &>/dev/null; then
    info "pkg-config"
else
    warn "pkg-config が見つかりません"
    case $PM in
        apt)  MISSING+=("pkg-config") ;;
        apk)  MISSING+=("pkgconf") ;;
        brew) MISSING+=("pkg-config") ;;
    esac
fi

echo ""

if [ ${#MISSING[@]} -gt 0 ]; then
    echo "インストールが必要なパッケージ: ${MISSING[*]}"
    if ask "インストールしますか？"; then
        pkg_install "${MISSING[@]}"
    else
        error "必要なパッケージがインストールされていません"
    fi
fi

# dav1d チェック（静的リンクが必要）
echo ""
DAV1D_OK=false
if pkg-config --libs dav1d &>/dev/null; then
    # 静的ライブラリが存在するか確認
    DAV1D_LIB=$(pkg-config --variable=libdir dav1d 2>/dev/null || true)
    if [ -n "$DAV1D_LIB" ] && ls "$DAV1D_LIB"/libdav1d.a &>/dev/null; then
        info "dav1d（静的ライブラリ）"
        DAV1D_OK=true
    else
        warn "dav1d の動的ライブラリは見つかりましたが、静的ライブラリ（.a）がありません"
    fi
else
    warn "dav1d が見つかりません"
fi

if ! $DAV1D_OK; then
    echo "dav1d をソースからビルドして静的インストールします"
    if ask "続けますか？"; then
        BUILD_DIR=$(mktemp -d)
        git clone --depth 1 https://code.videolan.org/videolan/dav1d.git "$BUILD_DIR/dav1d"
        meson setup "$BUILD_DIR/build" "$BUILD_DIR/dav1d" \
            --default-library=static \
            --buildtype=release \
            --prefix=/usr/local
        ninja -C "$BUILD_DIR/build"
        sudo ninja -C "$BUILD_DIR/build" install
        # ldconfig で認識させる（Linux のみ）
        if command -v ldconfig &>/dev/null; then
            sudo ldconfig
        fi
        info "dav1d インストール完了"
    else
        error "dav1d が必要です"
    fi
fi

echo ""
info "セットアップ完了！以下のコマンドでビルドできます:"
echo "  cargo build"
