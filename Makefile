.PHONY: build release flatpak appimage-build appimage setup help

help:
	@if echo "$$LANG" | grep -qi "ja"; then \
		echo "使い方: make [ターゲット]"; \
		echo ""; \
		echo "  build           デバッグビルド (cargo build)"; \
		echo "  release         リリースビルド (cargo build --release)"; \
		echo "  flatpak         公式Flathub BuilderでFlatpakをビルド・インストール"; \
		echo "  appimage-build  cargo-zigbuildでglibc 2.17 ABI固定のリリースビルド"; \
		echo "  appimage        appimage-buildの成果物からAppImageを生成"; \
		echo "  setup           依存パッケージのセットアップのみ実行"; \
		echo "  help            このヘルプを表示"; \
	else \
		echo "Usage: make [target]"; \
		echo ""; \
		echo "  build           Debug build (cargo build)"; \
		echo "  release         Release build (cargo build --release)"; \
		echo "  flatpak         Build and install with the official Flathub Builder"; \
		echo "  appimage-build  Release build pinned to glibc 2.17 ABI via cargo-zigbuild"; \
		echo "  appimage        Package the appimage-build output as an AppImage"; \
		echo "  setup           Run dependency setup only"; \
		echo "  help            Show this help"; \
	fi

build: setup
	cargo build

release: setup
	cargo build --release

flatpak:
	flatpak run --command=flathub-build org.flatpak.Builder --install io.github.Omanjusan.Nekoviewer.yml

appimage-build: setup
	@command -v cargo-zigbuild >/dev/null || { \
		echo "エラー: cargo-zigbuild が無い。'cargo install cargo-zigbuild' と zig(https://ziglang.org/download/) の導入が必要" >&2; \
		exit 1; \
	}
	@command -v zig >/dev/null || { \
		echo "エラー: zig が無い。https://ziglang.org/download/ から導入してPATHに追加してください" >&2; \
		exit 1; \
	}
	rustup target add x86_64-unknown-linux-gnu
	cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17

appimage: appimage-build
	@./packaging/appimage/build-appimage.sh

setup:
	@./setup.sh
