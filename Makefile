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

.PHONY: help build release check test clean install uninstall run logs start restart stop status lint fmt fix quality deploy deploy-beta deploy-stable \
        ui-install ui-dev ui-build ui-preview ui-typecheck codegen \
        installer-lint \
        macos-sign macos-sign-release

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
	@echo "  make quality   - Run all quality checks (rust fmt/clippy/test + web-ui typecheck)"
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
	@echo "  Web UI (apps/web-ui — bundled into the daemon binary):"
	@echo "  make ui-install        - Install web UI dependencies (bun)"
	@echo "  make ui-dev            - Run the web UI in dev mode (vite at :5173, proxies /api to :3737)"
	@echo "  make ui-build          - Build the web UI to static files (dist/)"
	@echo "  make ui-preview        - Preview the production build locally"
	@echo "  make ui-typecheck      - Typecheck the web UI"
	@echo "  make codegen           - Regenerate apps/web-ui TS types from daemon /api/openapi.json"
	@echo ""
	@echo "  Installer:"
	@echo "  make installer-lint    - Lint release/cli/latest.sh"

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

# One-shot gate for both projects: Rust (fmt + clippy + tests) and the
# bun web-ui (typecheck). Run before committing or in CI.
quality:
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test
	cd apps/web-ui && bun run typecheck
	@echo "✓ quality checks passed (rust + web-ui)"

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
	@curl -s http://127.0.0.1:3737/api/status 2>/dev/null | python3 -m json.tool || echo "✗ API not responding"

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

# Lint the user-local installer script (served at install.audetic.ai/cli/latest.sh).
# End-to-end run hits systemd and pulls a real release; do that on a throwaway
# profile, not in CI.
installer-lint:
	bash -n release/cli/latest.sh
	bash -n release/cli/uninstall.sh
	@if command -v shellcheck >/dev/null 2>&1; then shellcheck release/cli/latest.sh release/cli/uninstall.sh; else echo "shellcheck not installed; skipping"; fi
	@echo "✓ release/cli/*.sh ok"

# macOS code-signing. Ad-hoc-signs the local binary with the hardened runtime
# and entitlements so the OS associates the embedded Info.plist with this
# specific binary path and shows the Microphone / Screen Recording prompts.
# Without this step, TCC sees an unsigned binary and either uses the wrong
# identity or silently skips the prompt entirely.
#
# For shareable builds use `make macos-sign-release SIGN_IDENTITY="Developer ID Application: … (Z25737G79K)"`.
SIGN_IDENTITY ?= -
MACOS_BINARY  ?= target/debug/audetic
MACOS_ENTITLEMENTS ?= crates/audetic/macos/audetic.entitlements

macos-sign:
	@echo "→ codesign ($(SIGN_IDENTITY)) $(MACOS_BINARY)"
	codesign --force --sign $(SIGN_IDENTITY) \
		--options runtime \
		--entitlements $(MACOS_ENTITLEMENTS) \
		--timestamp=none \
		$(MACOS_BINARY)
	@echo "✓ signed. Verify with: codesign -dv --verbose=2 $(MACOS_BINARY)"

macos-sign-release: MACOS_BINARY=target/release/audetic
macos-sign-release: macos-sign

# Cleanup
clean:
	cargo clean
	rm -f /tmp/audetic_*.wav
