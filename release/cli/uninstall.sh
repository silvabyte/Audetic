#!/usr/bin/env bash
set -euo pipefail

# Audetic uninstaller
# Usage:
#   curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash
#   curl -fsSL ... | bash -s -- --dry-run
#   curl -fsSL ... | bash -s -- --keep-database

BLUE="\033[34m"
GREEN="\033[32m"
YELLOW="\033[33m"
RED="\033[31m"
BOLD="\033[1m"
DIM="\033[2m"
RESET="\033[0m"

# Options
DRY_RUN=false
YES=false
KEEP_CONFIG=false
KEEP_DATABASE=false
KEEP_UPDATES=false
REMOVE_TEMP=false

# Paths — must match the layout produced by `audetic install`.
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/audetic"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/audetic"
BIN_DIR="$DATA_DIR/bin"
SERVICE_NAME="audetic.service"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

# Discovered artifacts
declare -a ARTIFACTS_TO_REMOVE=()
declare -a ARTIFACTS_KEPT=()
TOTAL_SIZE=0

log() {
  local level="$1"
  shift
  case "$level" in
    info) echo -e "${BLUE}==>${RESET} $*" ;;
    success) echo -e "${GREEN}✓${RESET} $*" ;;
    warn) echo -e "${YELLOW}!${RESET} $*" ;;
    error) echo -e "${RED}✗${RESET} $*" ;;
    title) echo -e "${BOLD}$*${RESET}" ;;
    dim) echo -e "${DIM}  $*${RESET}" ;;
    *) echo "$@" ;;
  esac
}

die() {
  log error "$*"
  exit 1
}

usage() {
  cat <<'EOF'
Audetic uninstaller

Options:
  --dry-run          Show what would be removed without making changes
  -y, --yes          Skip confirmation prompt
  --keep-config      Preserve ~/.config/audetic (config.toml, update_state.json)
  --keep-database    Preserve transcription history database
  --keep-updates     Preserve update cache and backup binaries
  --remove-temp      Also clean /tmp/audetic_* files (default: no)
  --help             Show this message

Examples:
  # Standard uninstall (removes binary, service, config, data)
  curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash

  # Preview what would be removed
  curl -fsSL ... | bash -s -- --dry-run

  # Uninstall but keep transcription history
  curl -fsSL ... | bash -s -- --keep-database

  # Full cleanup including temp files
  curl -fsSL ... | bash -s -- --remove-temp
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        DRY_RUN=true
        shift
        ;;
      -y | --yes)
        YES=true
        shift
        ;;
      --keep-config)
        KEEP_CONFIG=true
        shift
        ;;
      --keep-database)
        KEEP_DATABASE=true
        shift
        ;;
      --keep-updates)
        KEEP_UPDATES=true
        shift
        ;;
      --remove-temp)
        REMOVE_TEMP=true
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

systemctl_available() {
  command -v systemctl >/dev/null 2>&1
}

human_size() {
  local bytes="$1"
  if ((bytes >= 1073741824)); then
    echo "$(awk "BEGIN {printf \"%.1f\", $bytes/1073741824}")G"
  elif ((bytes >= 1048576)); then
    echo "$(awk "BEGIN {printf \"%.1f\", $bytes/1048576}")M"
  elif ((bytes >= 1024)); then
    echo "$(awk "BEGIN {printf \"%.1f\", $bytes/1024}")K"
  else
    echo "${bytes}B"
  fi
}

get_size() {
  local path="$1"
  if [[ -e "$path" ]]; then
    if [[ -d "$path" ]]; then
      du -sb "$path" 2>/dev/null | awk '{print $1}' || echo 0
    else
      stat -c%s "$path" 2>/dev/null || stat -f%z "$path" 2>/dev/null || echo 0
    fi
  else
    echo 0
  fi
}

add_artifact() {
  local path="$1"
  local description="$2"
  local size
  size=$(get_size "$path")
  ARTIFACTS_TO_REMOVE+=("$path|$description|$size")
  TOTAL_SIZE=$((TOTAL_SIZE + size))
}

add_kept() {
  local path="$1"
  local reason="$2"
  ARTIFACTS_KEPT+=("$path|$reason")
}

# Discover all installed artifacts.
#
# Layout produced by `audetic install`:
#   ~/.local/share/audetic/bin/             — binary + auto-update .bak files
#   ~/.local/share/audetic/audetic.db{,-wal,-shm}
#   ~/.local/share/audetic/updates/         — staged update archives
#   ~/.local/share/audetic/update.lock
#   ~/.local/share/audetic/meetings/        — meeting recordings + transcripts
#   ~/.local/share/audetic/keybind-backups/
#   ~/.config/audetic/                       — config.toml, update_state.json
#   ~/.config/systemd/user/audetic.service
discover_artifacts() {
  log info "Scanning for Audetic artifacts..."

  # Binary directory — always removed (it's the program itself).
  if [[ -d "$BIN_DIR" ]]; then
    add_artifact "$BIN_DIR" "Binary directory"
  fi

  # Service file
  local service_path="$SYSTEMD_USER_DIR/$SERVICE_NAME"
  if [[ -f "$service_path" ]]; then
    add_artifact "$service_path" "Systemd service unit"
  fi

  # Config directory
  if [[ -d "$CONFIG_DIR" ]]; then
    if $KEEP_CONFIG; then
      add_kept "$CONFIG_DIR" "--keep-config"
    else
      add_artifact "$CONFIG_DIR" "Config directory"
    fi
  fi

  # Data directory: itemize so we can honor --keep-database / --keep-updates
  # without orphaning the binary (which lives under $DATA_DIR/bin).
  if [[ -d "$DATA_DIR" ]]; then
    local db_path="$DATA_DIR/audetic.db"
    local db_wal="$DATA_DIR/audetic.db-wal"
    local db_shm="$DATA_DIR/audetic.db-shm"
    local updates_dir="$DATA_DIR/updates"
    local update_lock="$DATA_DIR/update.lock"
    local meetings_dir="$DATA_DIR/meetings"
    local keybind_backups="$DATA_DIR/keybind-backups"

    if $KEEP_DATABASE; then
      [[ -f "$db_path" ]] && add_kept "$db_path" "--keep-database"
      [[ -f "$db_wal" ]] && add_kept "$db_wal" "--keep-database"
      [[ -f "$db_shm" ]] && add_kept "$db_shm" "--keep-database"
    else
      [[ -f "$db_path" ]] && add_artifact "$db_path" "Transcription database"
      [[ -f "$db_wal" ]] && add_artifact "$db_wal" "Database WAL"
      [[ -f "$db_shm" ]] && add_artifact "$db_shm" "Database SHM"
    fi

    if $KEEP_UPDATES; then
      [[ -d "$updates_dir" ]] && add_kept "$updates_dir" "--keep-updates"
      [[ -f "$update_lock" ]] && add_kept "$update_lock" "--keep-updates"
    else
      [[ -d "$updates_dir" ]] && add_artifact "$updates_dir" "Update cache"
      [[ -f "$update_lock" ]] && add_artifact "$update_lock" "Update lock file"
    fi

    [[ -d "$meetings_dir" ]] && add_artifact "$meetings_dir" "Meeting recordings"
    [[ -d "$keybind_backups" ]] && add_artifact "$keybind_backups" "Keybind backups"
  fi

  # Temp files (only with --remove-temp)
  if $REMOVE_TEMP; then
    for tmp in /tmp/audetic_*.wav; do
      [[ -e "$tmp" ]] && add_artifact "$tmp" "Temp audio file"
    done
  fi
}

print_plan() {
  echo ""
  if [[ ${#ARTIFACTS_TO_REMOVE[@]} -eq 0 ]]; then
    log success "No Audetic artifacts found to remove"
    if [[ ${#ARTIFACTS_KEPT[@]} -gt 0 ]]; then
      echo ""
      log info "Preserved artifacts:"
      for item in "${ARTIFACTS_KEPT[@]}"; do
        IFS="|" read -r path reason <<<"$item"
        log dim "$path ($reason)"
      done
    fi
    exit 0
  fi

  log title "The following will be removed:"
  echo ""
  for item in "${ARTIFACTS_TO_REMOVE[@]}"; do
    IFS="|" read -r path description size <<<"$item"
    local human
    human=$(human_size "$size")
    printf "  ${RED}✗${RESET} %-35s %s ${DIM}(%s)${RESET}\n" "$description" "$path" "$human"
  done
  echo ""
  log info "Total size: $(human_size "$TOTAL_SIZE")"

  if [[ ${#ARTIFACTS_KEPT[@]} -gt 0 ]]; then
    echo ""
    log title "The following will be preserved:"
    echo ""
    for item in "${ARTIFACTS_KEPT[@]}"; do
      IFS="|" read -r path reason <<<"$item"
      printf "  ${GREEN}✓${RESET} %s ${DIM}(%s)${RESET}\n" "$path" "$reason"
    done
  fi
  echo ""
}

confirm_uninstall() {
  if $YES; then
    return 0
  fi

  if $DRY_RUN; then
    return 1
  fi

  echo -n "Proceed with uninstall? [y/N] "
  read -r response </dev/tty
  case "$response" in
    [yY] | [yY][eE][sS]) return 0 ;;
    *) return 1 ;;
  esac
}

stop_service() {
  if ! systemctl_available; then
    log dim "systemctl not available, skipping service stop"
    return 0
  fi

  if systemctl --user is-active "$SERVICE_NAME" >/dev/null 2>&1; then
    log info "Stopping $SERVICE_NAME..."
    systemctl --user stop "$SERVICE_NAME" || log warn "Failed to stop service"
  else
    log dim "Service not running"
  fi
}

disable_service() {
  if ! systemctl_available; then
    return 0
  fi

  if systemctl --user is-enabled "$SERVICE_NAME" >/dev/null 2>&1; then
    log info "Disabling $SERVICE_NAME..."
    systemctl --user disable "$SERVICE_NAME" >/dev/null 2>&1 || log warn "Failed to disable service"
  fi
}

remove_path() {
  local path="$1"
  local description="$2"

  if [[ ! -e "$path" ]]; then
    return 0
  fi

  # Everything Audetic writes lives under $HOME — no sudo needed.
  if rm -rf "$path"; then
    log success "Removed $description"
    return 0
  else
    log error "Failed to remove $path"
    return 1
  fi
}

perform_uninstall() {
  log title "Uninstalling Audetic..."
  echo ""

  # Stop and disable service first
  stop_service
  disable_service

  # Remove artifacts
  local failed=0
  for item in "${ARTIFACTS_TO_REMOVE[@]}"; do
    IFS="|" read -r path description _ <<<"$item"
    remove_path "$path" "$description" || ((failed++))
  done

  # Reload systemd if we removed the service file
  if systemctl_available; then
    systemctl --user daemon-reload >/dev/null 2>&1 || true
  fi

  # Clean up empty parent directories.
  # $DATA_DIR is itemized (bin/, db, updates/, etc.) so it may be empty
  # after removal — rmdir leaves it alone if anything was kept via flags.
  rmdir "$DATA_DIR" 2>/dev/null || true
  rmdir "$SYSTEMD_USER_DIR" 2>/dev/null || true
  rmdir "$(dirname "$SYSTEMD_USER_DIR")" 2>/dev/null || true

  echo ""
  if ((failed > 0)); then
    log warn "Uninstall completed with $failed error(s)"
    exit 1
  else
    log success "Audetic has been uninstalled"
    if [[ ${#ARTIFACTS_KEPT[@]} -gt 0 ]]; then
      log info "Some files were preserved (use flags to control):"
      for item in "${ARTIFACTS_KEPT[@]}"; do
        IFS="|" read -r path _ <<<"$item"
        log dim "$path"
      done
    fi
  fi
}

# Main
parse_args "$@"

if $DRY_RUN; then
  log title "Audetic uninstaller (dry run)"
else
  log title "Audetic uninstaller"
fi

discover_artifacts
print_plan

if $DRY_RUN; then
  log info "Dry run complete. No changes were made."
  exit 0
fi

if ! confirm_uninstall; then
  log info "Uninstall cancelled"
  exit 0
fi

perform_uninstall
