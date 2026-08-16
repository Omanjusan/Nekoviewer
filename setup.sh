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

MUSL=false
if [ "${1:-}" = "--musl" ]; then
    MUSL=true
fi

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

# musl 静的リンクビルド（配布用単一バイナリ、--musl 指定時のみ）
if $MUSL; then
    echo ""
    echo "=== musl 静的ビルド セットアップ ==="
    echo ""

    if [ "$PM" = "apk" ]; then
        warn "Alpine（musl ネイティブ環境）では --musl は不要です。'make release' でそのまま静的寄りのバイナリになります"
    else
        if ! command -v musl-gcc &>/dev/null; then
            if ask "musl-tools をインストールしますか？"; then
                pkg_install musl-tools
            else
                error "musl-gcc が必要です"
            fi
        fi

        if ! rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl; then
            info "musl ターゲットを追加します"
            rustup target add x86_64-unknown-linux-musl
        else
            info "musl ターゲット（rustup）"
        fi

        # dav1d を musl-gcc で静的ビルドし、/usr/local/musl にインストール
        # （通常の dav1d インストール先 /usr/local とは別系統。pkg-config の解決先を
        #   PKG_CONFIG_PATH で切り替えて、glibc/musl 両方のビルドを共存させる）
        MUSL_PREFIX=/usr/local/musl
        if [ -f "$MUSL_PREFIX/lib/pkgconfig/dav1d.pc" ]; then
            info "dav1d（musl 静的ライブラリ）"
        else
            echo "dav1d を musl-gcc で静的ビルドします（インストール先: $MUSL_PREFIX）"
            if ask "続けますか？"; then
                BUILD_DIR=$(mktemp -d)
                git clone --depth 1 https://code.videolan.org/videolan/dav1d.git "$BUILD_DIR/dav1d"
                CC=musl-gcc meson setup "$BUILD_DIR/build" "$BUILD_DIR/dav1d" \
                    --default-library=static \
                    --buildtype=release \
                    --libdir=lib \
                    --prefix="$MUSL_PREFIX"
                ninja -C "$BUILD_DIR/build"
                sudo ninja -C "$BUILD_DIR/build" install
                info "dav1d（musl）インストール完了"
            else
                error "musl 向け dav1d が必要です"
            fi
        fi

        echo ""
        info "musl セットアップ完了！以下のコマンドで静的バイナリをビルドできます:"
        echo "  PKG_CONFIG_PATH=$MUSL_PREFIX/lib/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 CC_x86_64_unknown_linux_musl=musl-gcc \\"
        echo "    cargo build --release --target x86_64-unknown-linux-musl"
    fi
fi
