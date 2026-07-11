include makefiles/shell.mk

# Distribution is clone-and-build only — there are no hosted releases and no
# auto-updater. `make install` is the single supported install path, and since
# it's idempotent it doubles as the upgrade path: `git pull && make install`.
# See docs/adr/0001-source-only-distribution.md.

# Service identity, per platform. Both are per-user — nothing here needs sudo.
SERVICE_NAME  ?= audeticd.service
LAUNCH_LABEL  ?= ai.audetic.daemon

.PHONY: help build release check test clean run lint fmt fix quality \
        install install-linux install-macos uninstall \
        logs start restart stop status \
        ui-install ui-dev ui-build ui-preview ui-typecheck ui-lint codegen \
        macos-sign macos-sign-release macos-app macos-app-debug macos-menubar

# Default target
help:
	@echo "🦀 Audetic Development Commands"
	@echo ""
	@echo "  make install   - Build from source and install as a service (also the upgrade path)"
	@echo "  make uninstall - Stop the service and remove what install put on disk"
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
	@echo "  make logs      - Follow service logs"
	@echo "  make restart   - Restart service"
	@echo "  make stop      - Stop service"
	@echo "  make status    - Check service status"
	@echo ""
	@echo "  make clean     - Clean build artifacts"
	@echo ""
	@echo "  Web UI (apps/web-ui — bundled into the daemon binary):"
	@echo "  make ui-install        - Install web UI dependencies (bun)"
	@echo "  make ui-dev            - Run the web UI in dev mode (vite at :5173, proxies /api to :3737)"
	@echo "  make ui-build          - Build the web UI to static files (dist/)"
	@echo "  make ui-preview        - Preview the production build locally"
	@echo "  make ui-typecheck      - Typecheck the web UI"
	@echo "  make ui-lint           - Lint the web UI (ESLint) + run custom rule tests"
	@echo "  make codegen           - Regenerate apps/web-ui TS types from the daemon's OpenAPI spec"
	@echo ""
	@echo "  macOS bundle:"
	@echo "  make macos-app         - Build + ad-hoc sign target/release/Audetic.app"
	@echo "  make macos-menubar     - Build the SwiftUI menu-bar agent"

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
	cd apps/web-ui && bun run lint
	cd apps/web-ui && bun run test
	@echo "✓ quality checks passed (rust + web-ui)"

# ---------------------------------------------------------------------------
# Install / uninstall
#
# `audeticd install` owns the platform-specific work (systemd user unit on
# Linux, LaunchAgents on macOS). Make's only job is to build the right artifact
# and hand off. macOS goes through the .app bundle because TCC attributes the
# Microphone / Screen Recording grants to the bundle's code signature — a bare
# binary can never be granted them.
# ---------------------------------------------------------------------------

install:
	@case "$$(uname -s)" in \
	  Darwin) $(MAKE) install-macos ;; \
	  Linux)  $(MAKE) install-linux ;; \
	  *) echo "✗ Unsupported platform: $$(uname -s)"; exit 1 ;; \
	esac

install-linux: release
	./target/release/audeticd install

install-macos: macos-app
	"$(MACOS_APP_DIR)/Contents/MacOS/audeticd" install

# Teardown runs from the *installed* daemon when there is one, so `make
# uninstall` works on a checkout that was never built.
#
# The candidate must actually support the subcommand: a daemon installed before
# `uninstall` existed would otherwise die on an unrecognized argument. Probe
# with `uninstall --help` and fall back to building one that does.
#
# Pass flags through: `make uninstall ARGS="--dry-run"`, or `--keep-database`
# to preserve transcription history. `audeticd uninstall --help` lists them.
ARGS ?=

uninstall:
	@case "$$(uname -s)" in \
	  Darwin) candidates="$$HOME/Applications/Audetic.app/Contents/MacOS/audeticd $(MACOS_APP_DIR)/Contents/MacOS/audeticd" ;; \
	  Linux)  candidates="$$HOME/.local/share/audetic/bin/audeticd ./target/release/audeticd ./target/debug/audeticd" ;; \
	  *) echo "✗ Unsupported platform: $$(uname -s)"; exit 1 ;; \
	esac; \
	bin=""; \
	for c in $$candidates; do \
	  if [ -x "$$c" ] && "$$c" uninstall --help >/dev/null 2>&1; then bin="$$c"; break; fi; \
	done; \
	if [ -z "$$bin" ]; then \
	  echo "→ No audeticd with an \`uninstall\` subcommand found; building one"; \
	  cargo build --release -p audetic || exit 1; \
	  bin=./target/release/audeticd; \
	fi; \
	"$$bin" uninstall $(ARGS)

# ---------------------------------------------------------------------------
# Service management. systemd on Linux, launchd on macOS — one vocabulary.
# ---------------------------------------------------------------------------

run:
	RUST_LOG=info cargo run --release -p audetic

start:
	@case "$$(uname -s)" in \
	  Darwin) launchctl bootstrap "gui/$$(id -u)" "$$HOME/Library/LaunchAgents/$(LAUNCH_LABEL).plist" 2>/dev/null || true; \
	          launchctl kickstart -k "gui/$$(id -u)/$(LAUNCH_LABEL)" ;; \
	  Linux)  systemctl --user enable --now $(SERVICE_NAME) ;; \
	esac
	@echo "✓ Service enabled and started"

stop:
	@case "$$(uname -s)" in \
	  Darwin) launchctl bootout "gui/$$(id -u)/$(LAUNCH_LABEL)" ;; \
	  Linux)  systemctl --user stop $(SERVICE_NAME) ;; \
	esac
	@echo "✓ Service stopped"

restart:
	@case "$$(uname -s)" in \
	  Darwin) launchctl kickstart -k "gui/$$(id -u)/$(LAUNCH_LABEL)" ;; \
	  Linux)  systemctl --user restart $(SERVICE_NAME) ;; \
	esac
	@echo "✓ Service restarted"

logs:
	@case "$$(uname -s)" in \
	  Darwin) tail -f "$$HOME/Library/Logs/Audetic/audetic.log" ;; \
	  Linux)  journalctl --user -u $(SERVICE_NAME) -f ;; \
	esac

# Two independent signals, because they can disagree: the supervisor may call a
# crash-looping unit "active" while the daemon never actually binds the port.
status:
	@case "$$(uname -s)" in \
	  Darwin) launchctl print "gui/$$(id -u)/$(LAUNCH_LABEL)" >/dev/null 2>&1 \
	            && echo "✓ Service is loaded" || echo "✗ Service is not loaded" ;; \
	  Linux)  systemctl --user is-active $(SERVICE_NAME) >/dev/null 2>&1 \
	            && echo "✓ Service is running" || echo "✗ Service is not running" ;; \
	esac
	@curl -s http://127.0.0.1:3737/api/status 2>/dev/null | python3 -m json.tool || echo "✗ API not responding"

# Web UI (apps/web-ui) — current SPA. Daemon must be running for ui-dev.
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

ui-lint:
	cd apps/web-ui && bun run lint
	cd apps/web-ui && bun run test

# Emit the spec from a freshly built binary rather than scraping a daemon on
# :3737 — otherwise codegen captures whatever stale version happens to be
# listening, which is exactly how the UI drifts from the API.
codegen:
	cargo build -p audetic --bin audeticd
	./target/debug/audeticd openapi > target/openapi.json
	cd apps/web-ui && bun run codegen

# macOS code-signing. Ad-hoc-signs the local binary with the hardened runtime
# and entitlements so the OS associates the embedded Info.plist with this
# specific binary path and shows the Microphone / Screen Recording prompts.
# Without this step, TCC sees an unsigned binary and either uses the wrong
# identity or silently skips the prompt entirely.
#
# Ad-hoc (`-`) is the only signing path carried: builds are local, so there is
# no Developer ID or notarization story to maintain. The cost is that the TCC
# identity is the cdhash, so macOS may re-prompt for permissions after a
# rebuild.
SIGN_IDENTITY ?= -
MACOS_BINARY  ?= target/debug/audeticd
MACOS_ENTITLEMENTS ?= crates/audetic/macos/audetic.entitlements

macos-sign:
	@echo "→ codesign ($(SIGN_IDENTITY)) $(MACOS_BINARY)"
	codesign --force --sign "$(SIGN_IDENTITY)" \
		--options runtime \
		--entitlements $(MACOS_ENTITLEMENTS) \
		--timestamp=none \
		$(MACOS_BINARY)
	@echo "✓ signed. Verify with: codesign -dv --verbose=2 $(MACOS_BINARY)"

macos-sign-release: MACOS_BINARY=target/release/audeticd
macos-sign-release: macos-sign

# macOS app bundle. Produces target/<profile>/Audetic.app containing the
# daemon binary, Info.plist, and PkgInfo. Signed in place.
#
#   make macos-app                 # release bundle, ad-hoc signed
#   make macos-app-debug           # debug bundle for quick iteration
#
# `make install` on macOS goes through macos-app, so this is usually a step you
# get for free rather than one you run directly.
MACOS_APP_PROFILE ?= release
MACOS_APP_DIR     ?= target/$(MACOS_APP_PROFILE)/Audetic.app

# macOS menu-bar agent ("Audetic Menu Bar.app"). A SwiftUI MenuBarExtra app
# (apps/menubar-macos) that shows daemon status, offers point-and-click
# dictation/meeting toggles, and registers user-customizable global keyboard
# shortcuts. It is an independent HTTP consumer of the daemon — the macOS
# analog of the Hyprland keybind. It gets embedded inside Audetic.app's
# LoginItems by _macos-app-build so a single artifact carries everything.
MENUBAR_DIR     ?= apps/menubar-macos
MENUBAR_APP_DIR ?= $(MENUBAR_DIR)/.build/Audetic Menu Bar.app

macos-menubar:
	@command -v swift >/dev/null 2>&1 || { echo "✗ swift toolchain not found (install Xcode CLT)"; exit 1; }
	@echo "→ swift build -c release ($(MENUBAR_DIR))"
	cd $(MENUBAR_DIR) && swift build -c release
	@echo "→ Assembling $(MENUBAR_APP_DIR)"
	@rm -rf "$(MENUBAR_APP_DIR)"
	@mkdir -p "$(MENUBAR_APP_DIR)/Contents/MacOS"
	@mkdir -p "$(MENUBAR_APP_DIR)/Contents/Resources"
	@cp $(MENUBAR_DIR)/macos/Info.plist "$(MENUBAR_APP_DIR)/Contents/Info.plist"
	@cp $(MENUBAR_DIR)/.build/release/AudeticMenuBar "$(MENUBAR_APP_DIR)/Contents/MacOS/AudeticMenuBar"
	@printf 'APPL????' > "$(MENUBAR_APP_DIR)/Contents/PkgInfo"
	@echo "→ codesign ($(SIGN_IDENTITY)) $(MENUBAR_APP_DIR)"
	codesign --force --sign "$(SIGN_IDENTITY)" \
		--options runtime \
		--timestamp=none \
		"$(MENUBAR_APP_DIR)"
	@echo "✓ $(MENUBAR_APP_DIR)"

macos-app: release macos-menubar
	@$(MAKE) _macos-app-build MACOS_APP_PROFILE=release

macos-app-debug: build macos-menubar
	@$(MAKE) _macos-app-build MACOS_APP_PROFILE=debug

# Internal: assemble + sign the bundle. Don't call directly — go through
# macos-app / macos-app-debug so the underlying cargo build runs first.
_macos-app-build:
	@echo "→ Assembling $(MACOS_APP_DIR)"
	@rm -rf $(MACOS_APP_DIR)
	@mkdir -p $(MACOS_APP_DIR)/Contents/MacOS
	@mkdir -p $(MACOS_APP_DIR)/Contents/Resources
	@cp crates/audetic/macos/Info.plist $(MACOS_APP_DIR)/Contents/Info.plist
	@cp target/$(MACOS_APP_PROFILE)/audeticd $(MACOS_APP_DIR)/Contents/MacOS/audeticd
	@# Ship the slim `audetic` CLI inside the bundle too; `audeticd install`
	@# copies it onto PATH. Keeping it inside the bundle means the single
	@# built artifact still yields both the daemon and the CLI.
	@cp target/$(MACOS_APP_PROFILE)/audetic $(MACOS_APP_DIR)/Contents/MacOS/audetic
	@# Embed the menu-bar agent as a login item. `audeticd install` registers
	@# it as a per-user LaunchAgent so it starts on login alongside the daemon.
	@mkdir -p "$(MACOS_APP_DIR)/Contents/Library/LoginItems"
	@if [ -d "$(MENUBAR_APP_DIR)" ]; then \
		echo "  · Embedding $(MENUBAR_APP_DIR) → Contents/Library/LoginItems/"; \
		cp -R "$(MENUBAR_APP_DIR)" "$(MACOS_APP_DIR)/Contents/Library/LoginItems/Audetic Menu Bar.app"; \
	else \
		echo "  ✗ $(MENUBAR_APP_DIR) missing — run \`make macos-menubar\` first"; exit 1; \
	fi
	@printf 'APPL????' > $(MACOS_APP_DIR)/Contents/PkgInfo
	@# Sign nested code inside-out so the outer bundle validates: the menu-bar
	@# app first, then the CLI (no mic/screen entitlements — neither captures),
	@# then the bundle itself.
	codesign --force --sign "$(SIGN_IDENTITY)" \
		--options runtime \
		--timestamp=none \
		"$(MACOS_APP_DIR)/Contents/Library/LoginItems/Audetic Menu Bar.app"
	codesign --force --sign "$(SIGN_IDENTITY)" \
		--options runtime \
		--timestamp=none \
		$(MACOS_APP_DIR)/Contents/MacOS/audetic
	@echo "→ codesign ($(SIGN_IDENTITY)) $(MACOS_APP_DIR)"
	codesign --force --sign "$(SIGN_IDENTITY)" \
		--options runtime \
		--entitlements $(MACOS_ENTITLEMENTS) \
		--timestamp=none \
		$(MACOS_APP_DIR)
	@echo "✓ $(MACOS_APP_DIR)"
	@codesign -dv --verbose=2 $(MACOS_APP_DIR) 2>&1 | grep -E 'Identifier|Format|Signature|TeamIdentifier|Info.plist'

# Cleanup
clean:
	cargo clean
	rm -f /tmp/audetic_*.wav
