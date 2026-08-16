.PHONY: build release release-musl setup help

help:
	@if echo "$$LANG" | grep -qi "ja"; then \
		echo "使い方: make [ターゲット]"; \
		echo ""; \
		echo "  build         デバッグビルド (cargo build)"; \
		echo "  release       リリースビルド (cargo build --release)"; \
		echo "  release-musl  musl静的リンクのリリースビルド（配布用単一バイナリ）"; \
		echo "  setup         依存パッケージのセットアップのみ実行"; \
		echo "  help          このヘルプを表示"; \
	else \
		echo "Usage: make [target]"; \
		echo ""; \
		echo "  build         Debug build (cargo build)"; \
		echo "  release       Release build (cargo build --release)"; \
		echo "  release-musl  Statically-linked musl release build (single-binary distribution)"; \
		echo "  setup         Run dependency setup only"; \
		echo "  help          Show this help"; \
	fi

build: setup
	cargo build

release: setup
	cargo build --release

release-musl:
	@./setup.sh --musl
	PKG_CONFIG_PATH=/usr/local/musl/lib/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 CC_x86_64_unknown_linux_musl=musl-gcc \
		cargo build --release --target x86_64-unknown-linux-musl

setup:
	@./setup.sh
