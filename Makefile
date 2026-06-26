.PHONY: build release setup help

help:
	@if echo "$$LANG" | grep -qi "ja"; then \
		echo "使い方: make [ターゲット]"; \
		echo ""; \
		echo "  build    デバッグビルド (cargo build)"; \
		echo "  release  リリースビルド (cargo build --release)"; \
		echo "  setup    依存パッケージのセットアップのみ実行"; \
		echo "  help     このヘルプを表示"; \
	else \
		echo "Usage: make [target]"; \
		echo ""; \
		echo "  build    Debug build (cargo build)"; \
		echo "  release  Release build (cargo build --release)"; \
		echo "  setup    Run dependency setup only"; \
		echo "  help     Show this help"; \
	fi

build: setup
	cargo build

release: setup
	cargo build --release

setup:
	@./setup.sh
