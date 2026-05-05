include makefiles/shell.mk

VERSION ?=
CHANNEL ?= stable
TARGETS ?= linux-x86_64-gnu
ALLOW_DIRTY ?= 0
DRY_RUN ?= 0
SKIP_TESTS ?= 0
SKIP_TAG ?= 0
USE_CROSS ?= 0
EXTRA_FEATURES ?=
AUTO_COMMIT ?= 1

.PHONY: help build release check test clean install uninstall run logs start restart stop status lint fmt fix deploy deploy-beta deploy-stable \
        ui-install ui-dev ui-build ui-preview ui-typecheck codegen \
        electron-install electron-dev electron-build electron-typecheck electron-codegen \
        electron-package electron-package-arm64 electron-package-mac

# Default target
help:
	@echo "🦀 Audetic Development Commands"
	@echo ""
	@echo "  make build     - Build debug binary"
	@echo "  make release   - Build optimized release binary"
	@echo "  make check     - Run cargo check"
	@echo "  make test      - Run tests"
	@echo "  make lint      - Run clippy linter"
	@echo "  make fmt       - Check formatting"
	@echo "  make fix       - Fix formatting and simple lint issues"
	@echo ""
	@echo "  make run       - Run Audetic directly"
	@echo "  make start     - Enable and start service"
	@echo "  make logs      - Show service logs"
	@echo "  make restart   - Restart service"
	@echo "  make stop      - Stop service"
	@echo "  make status    - Check service status"
	@echo ""
	@echo "  make clean        - Clean build artifacts"
	@echo "  make deploy       - Build/package/publish release artifacts (auto-bumps when VERSION unset;"
	@echo "                      env: VERSION, VERSION_AUTO_BUMP=patch|minor|major|none, TARGETS, CHANNEL, DRY_RUN=1,"
	@echo "                      SKIP_TESTS=1, SKIP_TAG=1, ALLOW_DIRTY=1, USE_CROSS=1, EXTRA_FEATURES, AUTO_COMMIT=0,"
	@echo "                      CONTINUE_ON_ERROR=1)"
	@echo "  make deploy-beta  - Deploy to beta channel (convenience for CHANNEL=beta)"
	@echo "  make deploy-stable- Deploy to stable channel (convenience for CHANNEL=stable)"
	@echo ""
	@echo "  Web UI (apps/web-ui — current):"
	@echo "  make ui-install        - Install web UI dependencies (bun)"
	@echo "  make ui-dev            - Run the web UI in dev mode (vite at :5173)"
	@echo "  make ui-build          - Build the web UI to static files (dist/)"
	@echo "  make ui-preview        - Preview the production build locally"
	@echo "  make ui-typecheck      - Typecheck the web UI"
	@echo "  make codegen           - Regenerate apps/web-ui TS types from daemon /openapi.json"
	@echo ""
	@echo "  Electron UI (apps/ui — legacy, kept until web-ui reaches parity):"
	@echo "  make electron-install        - Install Electron UI dependencies (bun)"
	@echo "  make electron-dev            - Run the Electron UI in dev mode"
	@echo "  make electron-build          - Build the Electron UI (out/)"
	@echo "  make electron-typecheck      - Typecheck the Electron UI"
	@echo "  make electron-codegen        - Regenerate apps/ui TS types from daemon /openapi.json"
	@echo "  make electron-package        - Build daemon + Electron UI -> Linux x64 AppImage + .deb"
	@echo "  make electron-package-arm64  - Same as electron-package but for aarch64"
	@echo "  make electron-package-mac    - Build daemon + Electron UI -> macOS arm64 DMG (signing required, phase 7)"

# Build commands
build:
	cargo build

release:
	cargo build --release

check:
	cargo check

test:
	cargo test

# Code quality
lint:
	cargo clippy --all-targets --all-features -- -D warnings

fmt:
	cargo fmt

fix:
	cargo fix --allow-dirty --allow-staged

deploy:
	@VERSION=$(VERSION) \
	 CHANNEL=$(CHANNEL) \
	 TARGETS="$(TARGETS)" \
	 ALLOW_DIRTY=$(ALLOW_DIRTY) \
	 DRY_RUN=$(DRY_RUN) \
	 SKIP_TESTS=$(SKIP_TESTS) \
	 SKIP_TAG=$(SKIP_TAG) \
	 USE_CROSS=$(USE_CROSS) \
	 EXTRA_FEATURES="$(EXTRA_FEATURES)" \
	 AUTO_COMMIT=$(AUTO_COMMIT) \
	 bun ./scripts/release/deploy.ts

deploy-beta:
	@echo "🚀 Deploying to beta channel..."
	@$(MAKE) deploy CHANNEL=beta

deploy-stable:
	@echo "🚀 Deploying to stable channel..."
	@$(MAKE) deploy CHANNEL=stable

# Service management
run:
	AUDETIC_DISABLE_AUTO_UPDATE=1 RUST_LOG=info cargo run --release

logs:
	journalctl --user -u audetic.service -f

start:
	systemctl --user enable --now audetic.service
	@echo "✓ Service enabled and started"

restart:
	systemctl --user restart audetic.service
	@echo "✓ Service restarted"

stop:
	systemctl --user stop audetic.service
	@echo "✓ Service stopped"

status:
	@systemctl --user is-active audetic.service >/dev/null 2>&1 && echo "✓ Service is running" || echo "✗ Service is not running"
	@curl -s http://127.0.0.1:3737/status 2>/dev/null | python3 -m json.tool || echo "✗ API not responding"

# Web UI (apps/web-ui) — current SPA. Daemon must be running for codegen and dev.
ui-install:
	cd apps/web-ui && bun install

ui-dev:
	cd apps/web-ui && bun run dev

ui-build:
	cd apps/web-ui && bun run build

ui-preview:
	cd apps/web-ui && bun run preview

ui-typecheck:
	cd apps/web-ui && bun run typecheck

codegen:
	cd apps/web-ui && bun run codegen

# Electron UI (apps/ui) — legacy, kept until web-ui reaches parity, then deleted.
electron-install:
	cd apps/ui && bun install

electron-dev:
	cd apps/ui && bun run dev

electron-build:
	cd apps/ui && bun run build

electron-typecheck:
	cd apps/ui && bun run typecheck

electron-codegen:
	cd apps/ui && bun run codegen

# Bundled-binary packaging — Electron-only. Builds the daemon for the
# requested target, stages it under apps/ui/resources/bin/, runs
# electron-vite, then electron-builder. Output goes to apps/ui/release/.
#
#   make electron-package        # native Linux x64 AppImage + .deb
#   make electron-package-arm64  # Linux aarch64 AppImage + .deb
#   make electron-package-mac    # macOS arm64 DMG (phase 7 — needs signing)
electron-package:
	cd apps/ui && bun run package:linux

electron-package-arm64:
	cd apps/ui && bun run package:linux:arm64

electron-package-mac:
	cd apps/ui && bun run package:mac

# Cleanup
clean:
	cargo clean
	rm -f /tmp/audetic_*.wav
