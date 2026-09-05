# Audetic Documentation

Welcome to the Audetic documentation. This directory contains detailed guides for configuring and using Audetic.

## Available Documentation

### Installation & Setup

- [Installation Guide](./installation.md) - Complete installation instructions for all platforms
- [macOS Install Guide](./macos-install.md) - Build-from-source setup for macOS (Apple Silicon)
- [Text Injection Setup](./text-injection-setup.md) - Set up automatic text injection methods for different environments
- [**Configuration Guide**](./configuration.md) - Complete configuration reference covering all Audetic settings including providers, audio, UI, and behavior
- [Waybar Integration](./waybar-integration.md) - Add Audetic status indicators to your Waybar

### CLI Commands

Audetic includes built-in commands for managing transcription providers:

```bash
# Provider management
audetic provider show        # Show current provider configuration
audetic provider configure   # Interactive provider setup wizard
audetic provider test        # Validate provider without recording
```

Audetic is installed from source and has no auto-updater — `git pull && make
install` is the upgrade path. See
[ADR 0001](./adr/0001-source-only-distribution.md).

See the [Configuration Guide](./configuration.md#provider-cli-helpers) for detailed provider command documentation.

### Development

- [**Adding Providers**](./adding-providers/README.md) - Step-by-step guide for adding new transcription providers
- [CoreAudio device churn proof](./coreaudio-device-churn-proof.md) - Manual macOS hot-swap verification harness

Audetic includes a Makefile for common development tasks:

```bash
make help       # Show all available commands
make build      # Build debug binary
make release    # Build optimized release
make test       # Run tests
make lint       # Run clippy linter
make fmt        # Check formatting
make start      # Enable and start service
make logs       # Show service logs
make status     # Check service status
```

### Coming Soon

- Keyboard Shortcuts - Setting up custom keybindings
- Troubleshooting Guide - Common issues and solutions
- API Reference - HTTP API endpoints and usage

## Quick Links

- [Main README](../README.md) - Project overview and quick start
- [GitHub Repository](https://github.com/silvabyte/Audetic) - Source code and issue tracker
