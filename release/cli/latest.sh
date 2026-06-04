#!/usr/bin/env bash
#
# Audetic installer (user-local, no sudo).
#
#   curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
#
# Downloads the audetic daemon and hands off to `audetic install`. On Linux
# the daemon is a raw binary registered as a systemd user service. On macOS
# it ships as Audetic.app and registers as a LaunchAgent. Both flows end
# with the web UI opening on http://127.0.0.1:3737/ — everything under $HOME,
# no sudo.
#
# Source of truth lives in the repo at release/cli/latest.sh; `make
# installer-lint` checks it.

set -euo pipefail

BASE_URL="${AUDETIC_INSTALL_URL:-https://install.audetic.ai}"
CHANNEL="${AUDETIC_CHANNEL:-stable}"
VERSION="${AUDETIC_VERSION:-}"
NO_LAUNCH=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --channel)
      CHANNEL="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --no-launch)
      NO_LAUNCH=true
      shift
      ;;
    --help | -h)
      cat <<EOF
Audetic installer (user-local)

Options:
  --channel <name>   Release channel (default: stable)
  --version <v>      Pin a specific version
  --no-launch        Don't open the UI in a browser after install
  --help             Show this message

Environment:
  AUDETIC_INSTALL_URL   Override the release host (default: https://install.audetic.ai)
  AUDETIC_CHANNEL       Default channel
  AUDETIC_VERSION       Default version
EOF
      exit 0
      ;;
    *)
      echo "Unknown option: $1 (use --help)" >&2
      exit 1
      ;;
  esac
done

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

require_cmd curl
require_cmd tar

# `sha256sum` ships with Linux coreutils; macOS only has `shasum -a 256`.
# Pick whichever is on PATH and warn if neither is.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "No sha256 tool found (need sha256sum or shasum)" >&2
    exit 1
  fi
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64) echo "linux-x86_64-gnu" ;;
        aarch64 | arm64) echo "linux-aarch64-gnu" ;;
        *)
          echo "Unsupported Linux architecture: $arch" >&2
          exit 1
          ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64 | aarch64) echo "macos-aarch64" ;;
        x86_64) echo "macos-x86_64" ;;
        *)
          echo "Unsupported macOS architecture: $arch" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "Unsupported OS: $os" >&2
      exit 1
      ;;
  esac
}

TARGET="$(detect_target)"

if [[ -z "$VERSION" ]]; then
  version_file="version"
  [[ "$CHANNEL" != "stable" ]] && version_file="version-${CHANNEL}"
  VERSION="$(curl -fsSL "$BASE_URL/cli/$version_file" | tr -d '[:space:]')"
  [[ -z "$VERSION" ]] && {
    echo "Could not fetch version from $BASE_URL/cli/$version_file" >&2
    exit 1
  }
fi

echo "==> Audetic $VERSION ($TARGET)"

TMP="$(mktemp -d -t audetic-install.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

ARCHIVE="audetic-${VERSION}-${TARGET}.tar.gz"
ARCHIVE_URL="$BASE_URL/cli/releases/$VERSION/$ARCHIVE"

echo "==> Downloading $ARCHIVE_URL"
curl -fsSL "$ARCHIVE_URL" -o "$TMP/$ARCHIVE"

EXPECTED_SHA="$(curl -fsSL "$ARCHIVE_URL.sha256" | awk '{print $1}')"
GOT_SHA="$(sha256 "$TMP/$ARCHIVE")"
if [[ "$EXPECTED_SHA" != "$GOT_SHA" ]]; then
  echo "Checksum mismatch: expected $EXPECTED_SHA, got $GOT_SHA" >&2
  exit 1
fi
echo "==> Verified sha256"

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

# The archive ships two binaries: the daemon `audeticd` and the standalone
# CLI `audetic`. We hand off to `audeticd install`, which sets up the service
# AND places the `audetic` CLI on PATH. On macOS both live inside the
# Audetic.app bundle (Contents/MacOS/{audeticd,audetic}); `audeticd install`
# must run from inside the bundle so it can derive the bundle root via
# current_exe.
case "$TARGET" in
  macos-*)
    BUNDLE="$(find "$TMP" -maxdepth 3 -type d -name 'Audetic.app' | head -n1)"
    [[ -d "$BUNDLE" ]] || {
      echo "Archive missing Audetic.app" >&2
      exit 1
    }
    BINARY="$BUNDLE/Contents/MacOS/audeticd"
    ;;
  *)
    BINARY="$(find "$TMP" -maxdepth 3 -type f -name 'audeticd' -perm -u+x | head -n1)"
    ;;
esac

[[ -x "$BINARY" ]] || {
  echo "Archive missing audeticd binary" >&2
  exit 1
}

# Hand off to `audeticd install` — the daemon owns service setup, the readiness
# probe, placing the `audetic` CLI on PATH, and opening the UI in the browser.
INSTALL_ARGS=()
$NO_LAUNCH && INSTALL_ARGS+=(--no-launch)
"$BINARY" install "${INSTALL_ARGS[@]}"
