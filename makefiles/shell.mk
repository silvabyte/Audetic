# Find all shell scripts
SHELL_SCRIPTS := $(shell find scripts release f -name '*.sh' 2>/dev/null)

# Lint shell scripts with ShellCheck and checkbashisms
.PHONY: shell-lint
shell-lint:
	@if ! command -v shellcheck >/dev/null 2>&1; then \
		echo "❌ ShellCheck not installed. Run: make install-shell-tools"; \
		exit 1; \
	fi
	@echo "Running ShellCheck..."
	@shellcheck $(SHELL_SCRIPTS)
	@if command -v checkbashisms >/dev/null 2>&1; then \
		echo "Running checkbashisms..."; \
		checkbashisms $(SHELL_SCRIPTS) || true; \
	else \
		echo "⚠️  checkbashisms not installed (optional), skipping..."; \
	fi
	@echo "✅ Shell script linting completed"

# Format shell scripts with shfmt
.PHONY: shell-fmt
shell-fmt:
	@if ! command -v shfmt >/dev/null 2>&1; then \
		echo "❌ shfmt not installed. Run: make install-shell-tools"; \
		exit 1; \
	fi
	@echo "Formatting shell scripts with shfmt..."
	@shfmt -w -i 2 -ci -bn $(SHELL_SCRIPTS)
	@echo "✅ Shell script formatting completed"

# Check shell scripts (lint without auto-fix)
.PHONY: shell-check
shell-check:
	@if ! command -v shellcheck >/dev/null 2>&1; then \
		echo "❌ ShellCheck not installed. Run: make install-shell-tools"; \
		exit 1; \
	fi
	@echo "Checking shell scripts with ShellCheck..."
	@shellcheck $(SHELL_SCRIPTS)
	@echo "✅ Shell script checking completed"

# Install shell script quality tools
.PHONY: install-shell-tools
install-shell-tools:
	@./scripts/install-shell-tools.sh
