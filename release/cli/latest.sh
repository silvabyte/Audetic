#!/usr/bin/env bash
set -euo pipefail

# Audetic installer & updater bootstrapper
# Usage:
#   curl -fsSL https://install.audetic.ai/cli/latest.sh | bash

BLUE="\033[34m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
BOLD="\033[1m"
RESET="\033[0m"

BASE_URL="${AUDETIC_INSTALL_URL:-https://install.audetic.ai}"
CHANNEL="${AUDETIC_CHANNEL:-stable}"
INSTALL_PREFIX="${AUDETIC_PREFIX:-/usr/local}"
REQUESTED_VERSION=""
SYSTEM_MODE=false
NO_START=false
FORCE_REINSTALL=false
CLEAN_INSTALL=false
DRY_RUN=false
UNINSTALL_ONLY=false
MINISIGN_PUBKEY="${AUDETIC_MINISIGN_PUBKEY:-}"

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/audetic"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/audetic"
BIN_DIR="$INSTALL_PREFIX/bin"
STATE_FILE="$CONFIG_DIR/update_state.json"
SERVICE_NAME="audetic.service"

REPO_RAW_BASE="${AUDETIC_GITHUB_RAW:-https://raw.githubusercontent.com/silvabyte/Audetic/main}"

TMP_ROOT=""

log() {
  local level="$1"
  shift
  case "$level" in
    info) echo -e "${BLUE}==>${RESET} $*" ;;
    success) echo -e "${GREEN}✓${RESET} $*" ;;
    warn) echo -e "${YELLOW}!${RESET} $*" ;;
    error) echo -e "${RED}✗${RESET} $*" ;;
    title) echo -e "${BOLD}$*${RESET}" ;;
    *) echo "$@" ;;
  esac
}

die() {
  log error "$*"
  exit 1
}

cleanup() {
  [[ -n "$TMP_ROOT" && -d "$TMP_ROOT" ]] && rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

usage() {
  cat <<'EOF'
Audetic installer

Options:
  --prefix <path>     Install prefix (default: /usr/local)
  --channel <name>    Release channel (default: stable)
  --version <v>       Install a specific version
  --system            Install system-wide service (/etc/systemd/system)
  --user              Force user service mode (default)
  --no-start          Do not enable/start the service after install
  --force-reinstall   Reinstall even if the version matches
  --clean             Remove previous binaries/services before reinstall; with --uninstall also removes config/cache
  --dry-run           Show what would happen without changing the system
  --uninstall         Remove Audetic using the same options (combine with --clean to purge config/cache)
  --help              Show this message

Environment overrides:
  AUDETIC_INSTALL_URL   Base URL for release artifacts (default: https://install.audetic.ai)
  AUDETIC_CHANNEL       Default channel
  AUDETIC_PREFIX        Default install prefix
  AUDETIC_MINISIGN_PUBKEY  Minisign public key for signature verification

Examples:
  curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
  curl -fsSL ... | bash -s -- --prefix "$HOME/.local" --no-start
  AUDETIC_CHANNEL=beta bash latest.sh --dry-run
EOF
}

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || die "Missing required command: $cmd"
}

require_sudo() {
  command -v sudo >/dev/null 2>&1 || die "Need elevated privileges; install sudo or use --prefix within \$HOME"
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --prefix)
        INSTALL_PREFIX="$2"
        BIN_DIR="$INSTALL_PREFIX/bin"
        shift 2
        ;;
      --channel)
        CHANNEL="$2"
        shift 2
        ;;
      --version)
        REQUESTED_VERSION="$2"
        shift 2
        ;;
      --system)
        SYSTEM_MODE=true
        shift
        ;;
      --user)
        SYSTEM_MODE=false
        shift
        ;;
      --no-start)
        NO_START=true
        shift
        ;;
      --force-reinstall)
        FORCE_REINSTALL=true
        shift
        ;;
      --clean)
        CLEAN_INSTALL=true
        shift
        ;;
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      --uninstall)
        UNINSTALL_ONLY=true
        shift
        ;;
      --help | -h)
        usage
        exit 0
        ;;
      *)
        die "Unknown option: $1 (use --help)"
        ;;
    esac
  done
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
          die "Unsupported Linux architecture: $arch"
          ;;
      esac
      ;;
    Darwin)
      case "$arch" in
        arm64) echo "macos-aarch64" ;;
        x86_64) echo "macos-x86_64" ;;
        *)
          die "Unsupported macOS architecture: $arch"
          ;;
      esac
      ;;
    *)
      die "Unsupported operating system: $os"
      ;;
  esac
}

trim() {
  tr -d ' \t\r\n'
}

fetch_version() {
  if [[ -n "$REQUESTED_VERSION" ]]; then
    echo "$REQUESTED_VERSION"
    return
  fi

  local version_file="version"
  if [[ "$CHANNEL" != "stable" ]]; then
    version_file="version-${CHANNEL}"
  fi

  local url="$BASE_URL/cli/$version_file"
  curl -fsSL "$url" | trim
}

download_manifest() {
  local version="$1"
  local url="$BASE_URL/cli/releases/$version/manifest.json"
  local dst="$TMP_ROOT/manifest.json"
  curl -fsSL "$url" -o "$dst" || die "Unable to download manifest: $url"
  echo "$dst"
}

extract_target_metadata() {
  local manifest="$1"
  local target="$2"
  python3 - "$manifest" "$target" <<'PY'
import json, sys, pathlib
manifest_path = pathlib.Path(sys.argv[1])
target_id = sys.argv[2]
data = json.loads(manifest_path.read_text())
targets = data.get("targets", {})
if target_id not in targets:
    sys.exit(1)
t = targets[target_id]
archive = t.get("archive")
sha256 = t.get("sha256")
sig = t.get("sig")
size = t.get("size", 0)
print(f"{archive}|{sha256}|{'' if sig in (None, '') else sig}|{size}")
PY
}

verify_sha256() {
  local file="$1"
  local expected="$2"
  local got
  if command -v sha256sum >/dev/null 2>&1; then
    got="$(sha256sum "$file" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    got="$(shasum -a 256 "$file" | awk '{print $1}')"
  else
    die "Need sha256sum or shasum for checksum verification"
  fi

  if [[ "$got" != "$expected" ]]; then
    die "Checksum mismatch for $(basename "$file"): expected $expected got $got"
  fi
}

verify_signature() {
  local file="$1"
  local signature_path="$2"
  local sig_url="$3"

  [[ -z "$signature_path" ]] && return 0
  if [[ -z "$MINISIGN_PUBKEY" ]]; then
    log warn "Signature provided but AUDETIC_MINISIGN_PUBKEY not set; skipping signature verification"
    return 0
  fi
  require_cmd minisign
  local sig_file
  sig_file="$TMP_ROOT/$(basename "$signature_path")"
  curl -fsSL "$sig_url" -o "$sig_file" || die "Unable to download signature: $sig_url"
  minisign -Vm "$file" -P "$MINISIGN_PUBKEY" -x "$sig_file" >/dev/null 2>&1 || die "Signature verification failed"
}

current_installed_version() {
  local bin="$1"
  if [[ -x "$bin" ]]; then
    "$bin" --version 2>/dev/null | awk '{print $2}' | head -n1 | trim || true
  fi
}

ensure_dir() {
  local dir="$1"
  mkdir -p "$dir"
}

install_with_permissions() {
  local src="$1" dest="$2" mode="$3"
  if install -m "$mode" "$src" "$dest" 2>/dev/null; then
    return 0
  fi
  require_sudo
  sudo install -m "$mode" "$src" "$dest"
}

remove_with_permissions() {
  local path="$1"
  [[ ! -e "$path" ]] && return 0
  if rm -f "$path" 2>/dev/null; then
    return 0
  fi
  require_sudo
  sudo rm -f "$path"
}

systemctl_available() {
  command -v systemctl >/dev/null 2>&1
}

stop_service() {
  systemctl_available || return 0
  if $SYSTEM_MODE; then
    sudo systemctl stop "$SERVICE_NAME" >/dev/null 2>&1 || true
  else
    systemctl --user stop "$SERVICE_NAME" >/dev/null 2>&1 || true
  fi
}

disable_service() {
  systemctl_available || return 0
  if $SYSTEM_MODE; then
    sudo systemctl disable "$SERVICE_NAME" >/dev/null 2>&1 || true
  else
    systemctl --user disable "$SERVICE_NAME" >/dev/null 2>&1 || true
  fi
}

install_service_unit() {
  local source="$1"
  if ! systemctl_available; then
    log warn "systemctl not found; skipping service installation. Start audetic manually."
    return 0
  fi
  if $SYSTEM_MODE; then
    local target="/etc/systemd/system/$SERVICE_NAME"
    install_with_permissions "$source" "$target" 0644
    sudo systemctl daemon-reload
    if ! $NO_START; then
      sudo systemctl enable --now "$SERVICE_NAME"
    else
      sudo systemctl enable "$SERVICE_NAME"
    fi
  else
    local systemd_user_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    ensure_dir "$systemd_user_dir"
    local target="$systemd_user_dir/$SERVICE_NAME"
    install -m 0644 "$source" "$target"
    systemctl --user daemon-reload
    if ! $NO_START; then
      systemctl --user enable --now "$SERVICE_NAME"
    else
      systemctl --user enable "$SERVICE_NAME" >/dev/null 2>&1 || true
    fi
  fi
}

write_update_state() {
  ensure_dir "$CONFIG_DIR"
  cat >"$STATE_FILE" <<JSON
{
  "current_version": "$1",
  "channel": "$CHANNEL",
  "last_check": null,
  "auto_update": true
}
JSON
}

install_config_template() {
  ensure_dir "$CONFIG_DIR"
  local config_path="$CONFIG_DIR/config.toml"
  if [[ -f "$config_path" ]]; then
    log info "Config already exists at $config_path (leaving untouched)"
    return
  fi

  local template="$1"
  if [[ -n "$template" && -f "$template" ]]; then
    cp "$template" "$config_path"
    log success "Copied default config to $config_path"
    return
  fi

  local fallback_url="$REPO_RAW_BASE/example_config.toml"
  curl -fsSL "$fallback_url" -o "$config_path"
  log success "Fetched default config to $config_path"
}

perform_uninstall() {
  log title "Audetic uninstall"
  stop_service || true
  disable_service || true

  local service_target
  if $SYSTEM_MODE; then
    service_target="/etc/systemd/system/$SERVICE_NAME"
  else
    service_target="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$SERVICE_NAME"
  fi
  remove_with_permissions "$service_target"
  remove_with_permissions "$BIN_DIR/audetic"

  if $CLEAN_INSTALL; then
    rm -rf "$CONFIG_DIR" "$DATA_DIR"
    log info "Removed config and data directories"
  fi

  log success "Audetic uninstalled"
  exit 0
}

parse_args "$@"

require_cmd curl
require_cmd tar
require_cmd python3
require_cmd install

if $UNINSTALL_ONLY; then
  perform_uninstall
fi

TMP_ROOT="$(mktemp -d -t audetic-install.XXXXXX)"

TARGET_TRIPLE="$(detect_target)"
VERSION="$(fetch_version)"
[[ -z "$VERSION" ]] && die "Could not determine version for channel $CHANNEL"

log title "Audetic installer"
log info "Channel       : $CHANNEL"
log info "Version       : $VERSION"
log info "Install prefix: $INSTALL_PREFIX"
log info "Target        : $TARGET_TRIPLE"
mode_type=$([[ $SYSTEM_MODE == true ]] && echo system || echo user)
log info "Mode          : $mode_type"

MANIFEST_PATH="$(download_manifest "$VERSION")"
TARGET_METADATA="$(extract_target_metadata "$MANIFEST_PATH" "$TARGET_TRIPLE" || true)"
[[ -z "$TARGET_METADATA" ]] && die "Target $TARGET_TRIPLE not available in manifest"
IFS="|" read -r ARCHIVE_NAME EXPECTED_SHA SIGNATURE_PATH _ <<<"$TARGET_METADATA"
[[ -z "$ARCHIVE_NAME" || -z "$EXPECTED_SHA" ]] && die "Manifest missing archive or checksum for $TARGET_TRIPLE"

ARCHIVE_URL="$BASE_URL/cli/releases/$VERSION/$ARCHIVE_NAME"
ARCHIVE_PATH="$TMP_ROOT/$ARCHIVE_NAME"

log info "Downloading artifact: $ARCHIVE_NAME"
curl -fsSL "$ARCHIVE_URL" -o "$ARCHIVE_PATH" || die "Failed to download $ARCHIVE_URL"

CHECKSUM_URL="$ARCHIVE_URL.sha256"
REMOTE_SHA="$(curl -fsSL "$CHECKSUM_URL" 2>/dev/null | awk '{print $1}' || true)"
if [[ -n "$REMOTE_SHA" ]]; then
  EXPECTED_SHA="$REMOTE_SHA"
  log info "Verified checksum via $CHECKSUM_URL"
else
  log warn "Unable to fetch checksum file at $CHECKSUM_URL; falling back to manifest hash"
fi

verify_sha256 "$ARCHIVE_PATH" "$EXPECTED_SHA"
verify_signature "$ARCHIVE_PATH" "$SIGNATURE_PATH" "$BASE_URL/cli/releases/$VERSION/$SIGNATURE_PATH"

EXTRACT_DIR="$TMP_ROOT/extracted"
mkdir -p "$EXTRACT_DIR"
tar -xzf "$ARCHIVE_PATH" -C "$EXTRACT_DIR"

STAGING_DIR="$(find "$EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
if [[ -z "$STAGING_DIR" ]]; then
  STAGING_DIR="$EXTRACT_DIR"
fi

BIN_SOURCE="$(find "$STAGING_DIR" -maxdepth 2 -type f -name 'audetic' -perm -111 | head -n1)"
[[ -z "$BIN_SOURCE" ]] && die "Archive does not contain audetic binary"

SERVICE_SOURCE="$(find "$STAGING_DIR" -maxdepth 3 -type f -name "$SERVICE_NAME" | head -n1)"
[[ -z "$SERVICE_SOURCE" ]] && die "Archive missing $SERVICE_NAME"

CONFIG_TEMPLATE="$(find "$STAGING_DIR" -maxdepth 3 -type f -name 'example_config.toml' | head -n1)"

INSTALLED_VERSION="$(current_installed_version "$BIN_DIR/audetic")"
if [[ "$INSTALLED_VERSION" == "$VERSION" && $FORCE_REINSTALL == false && $CLEAN_INSTALL == false ]]; then
  log success "Audetic $VERSION already installed; use --force-reinstall or --clean to reinstall"
  exit 0
fi

if $DRY_RUN; then
  log info "Dry run requested; verified artifact integrity. No changes applied."
  exit 0
fi

if $CLEAN_INSTALL; then
  log info "Removing existing installation before reinstall"
  stop_service || true
  remove_with_permissions "$BIN_DIR/audetic"
fi

log info "Installing binary to $BIN_DIR"
ensure_dir "$BIN_DIR"
install_with_permissions "$BIN_SOURCE" "$BIN_DIR/audetic" 0755

log info "Installing systemd unit"
install_service_unit "$SERVICE_SOURCE"

install_config_template "$CONFIG_TEMPLATE"

ensure_dir "$DATA_DIR"
write_update_state "$VERSION"

log success "Audetic $VERSION installed successfully"
log info "Binary location: $BIN_DIR/audetic"
service_mode=$([[ $SYSTEM_MODE == true ]] && echo system || echo user)
log info "Service mode   : $service_mode"

if $NO_START; then
  log warn "Service start skipped (--no-start). Run 'systemctl --user start $SERVICE_NAME' when ready."
fi

log info "Use 'audetic update' to trigger manual update checks once CLI wiring is available."
