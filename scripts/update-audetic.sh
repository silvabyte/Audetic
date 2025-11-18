#!/bin/bash
set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Print colored output
print_step() {
  echo -e "${BLUE}==>${NC} $1"
}

print_success() {
  echo -e "${GREEN}✓${NC} $1"
}

print_error() {
  echo -e "${RED}✗${NC} $1"
}

print_warning() {
  echo -e "${YELLOW}!${NC} $1"
}

# Get the directory where the script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AUDETIC_DIR="$SCRIPT_DIR"

# Configuration variables
WHISPER_DIR="$HOME/.local/share/audetic/whisper"
CONFIG_DIR="$HOME/.config/audetic"
INSTALL_DIR="/usr/local/bin"
BACKUP_DIR="$HOME/.config/audetic/backups"

# Parse command line arguments
UPDATE_WHISPER=false
FORCE_UPDATE=false
CHECK_ONLY=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --whisper)
      UPDATE_WHISPER=true
      shift
      ;;
    --force)
      FORCE_UPDATE=true
      shift
      ;;
    --check)
      CHECK_ONLY=true
      shift
      ;;
    --help)
      echo "Audetic Update Script"
      echo
      echo "Usage: $0 [OPTIONS]"
      echo
      echo "Options:"
      echo "  --whisper    Also update whisper.cpp installation"
      echo "  --force      Force update even if already up to date"
      echo "  --check      Only check for updates, don't install"
      echo "  --help       Show this help message"
      echo
      echo "Examples:"
      echo "  $0                  # Update Audetic only"
      echo "  $0 --whisper        # Update both Audetic and whisper.cpp"
      echo "  $0 --check          # Check for available updates"
      exit 0
      ;;
    *)
      print_error "Unknown option: $1"
      echo "Use --help for usage information"
      exit 1
      ;;
  esac
done

# Function to check for updates
check_for_updates() {
  cd "$1"
  git fetch origin >/dev/null 2>&1
  LOCAL=$(git rev-parse HEAD)
  REMOTE=$(git rev-parse origin/HEAD 2>/dev/null || git rev-parse origin/main 2>/dev/null || git rev-parse origin/master)

  if [ "$LOCAL" != "$REMOTE" ]; then
    return 0 # Updates available
  else
    return 1 # No updates
  fi
}

# Function to get current version
get_version() {
  cd "$1"
  git describe --tags --always 2>/dev/null || git rev-parse --short HEAD
}

print_step "Audetic Update Manager"
echo

# Check if Audetic is installed
if [ ! -f "$INSTALL_DIR/audetic" ]; then
  print_error "Audetic not found in $INSTALL_DIR"
  print_warning "Please run scripts/install-arch.sh first"
  exit 1
fi

# Stop the service before updating
print_step "Checking Audetic service status..."
if systemctl --user is-active --quiet audetic.service; then
  SERVICE_WAS_RUNNING=true
  print_warning "Audetic service is running. It will be restarted after update."
else
  SERVICE_WAS_RUNNING=false
fi

# Check for updates
cd "$AUDETIC_DIR"
CURRENT_VERSION=$(get_version "$AUDETIC_DIR")
print_step "Current Audetic version: $CURRENT_VERSION"

if check_for_updates "$AUDETIC_DIR" || [ "$FORCE_UPDATE" = true ]; then
  AUDETIC_UPDATE_AVAILABLE=true
  NEW_VERSION=$(cd "$AUDETIC_DIR" && git rev-parse --short origin/HEAD 2>/dev/null || echo "latest")
  print_warning "Audetic update available: $CURRENT_VERSION → $NEW_VERSION"
else
  AUDETIC_UPDATE_AVAILABLE=false
  print_success "Audetic is up to date"
fi

# Check whisper updates if requested
if [ "$UPDATE_WHISPER" = true ] && [ -d "$WHISPER_DIR" ]; then
  cd "$WHISPER_DIR"
  WHISPER_CURRENT=$(get_version "$WHISPER_DIR")
  print_step "Current whisper.cpp version: $WHISPER_CURRENT"

  if check_for_updates "$WHISPER_DIR" || [ "$FORCE_UPDATE" = true ]; then
    WHISPER_UPDATE_AVAILABLE=true
    print_warning "whisper.cpp update available"
  else
    WHISPER_UPDATE_AVAILABLE=false
    print_success "whisper.cpp is up to date"
  fi
fi

# If only checking, exit here
if [ "$CHECK_ONLY" = true ]; then
  if [ "$AUDETIC_UPDATE_AVAILABLE" = true ] || [ "${WHISPER_UPDATE_AVAILABLE:-false}" = true ]; then
    print_warning "Updates are available. Run without --check to install."
    exit 0
  else
    print_success "Everything is up to date!"
    exit 0
  fi
fi

# Exit if no updates available (unless forced)
if [ "$AUDETIC_UPDATE_AVAILABLE" = false ] && [ "${WHISPER_UPDATE_AVAILABLE:-false}" = false ] && [ "$FORCE_UPDATE" = false ]; then
  print_success "Nothing to update!"
  exit 0
fi

# Create backup directory
mkdir -p "$BACKUP_DIR"
BACKUP_DATE=$(date +%Y%m%d_%H%M%S)

# Backup configuration
print_step "Backing up configuration..."
if [ -f "$CONFIG_DIR/config.toml" ]; then
  cp "$CONFIG_DIR/config.toml" "$BACKUP_DIR/config_${BACKUP_DATE}.toml"
  print_success "Configuration backed up to $BACKUP_DIR/config_${BACKUP_DATE}.toml"
fi

# Stop service if running
if [ "$SERVICE_WAS_RUNNING" = true ]; then
  print_step "Stopping Audetic service..."
  systemctl --user stop audetic.service
fi

# Update Audetic
if [ "$AUDETIC_UPDATE_AVAILABLE" = true ] || [ "$FORCE_UPDATE" = true ]; then
  print_step "Updating Audetic..."
  cd "$AUDETIC_DIR"

  # Pull latest changes
  DEFAULT_BRANCH=$(git remote show origin | grep 'HEAD branch' | cut -d' ' -f5)
  if ! git pull origin "$DEFAULT_BRANCH"; then
    print_error "Failed to pull latest changes"
    exit 1
  fi

  # Clean and rebuild
  print_step "Building Audetic..."
  cargo clean
  if ! cargo build --release; then
    print_error "Failed to build Audetic"
    exit 1
  fi

  # Install new binary
  print_step "Installing updated binary..."
  if ! sudo cp target/release/audetic "$INSTALL_DIR/"; then
    print_error "Failed to install Audetic binary"
    exit 1
  fi
  sudo chmod +x "$INSTALL_DIR/audetic"
  print_success "Audetic updated successfully"
fi

# Update whisper if requested
if [ "$UPDATE_WHISPER" = true ] && [ "${WHISPER_UPDATE_AVAILABLE:-false}" = true -o "$FORCE_UPDATE" = true ]; then
  print_step "Updating whisper.cpp..."
  cd "$WHISPER_DIR"

  # Pull latest changes
  WHISPER_BRANCH=$(git remote show origin | grep 'HEAD branch' | cut -d' ' -f5)
  if ! git pull origin "$WHISPER_BRANCH"; then
    print_error "Failed to pull whisper.cpp updates"
    exit 1
  fi

  # Clean and rebuild
  print_step "Rebuilding whisper.cpp (this may take a while)..."
  # Clean build directory if it exists
  [ -d build ] && rm -rf build
  if ! ./build.sh; then
    print_error "Failed to build whisper.cpp"
    exit 1
  fi

  print_success "whisper.cpp updated successfully"
fi

# Check for configuration changes
print_step "Checking configuration compatibility..."

# Simply warn user to check for new config options
if [ -f "$CONFIG_DIR/config.toml" ]; then
  print_warning "Please check if new configuration options are available:"
  echo "  Current config: $CONFIG_DIR/config.toml"
  echo "  Backup saved to: $BACKUP_DIR/config_${BACKUP_DATE}.toml"
  echo "  Check documentation at: https://github.com/silvabyte/Audetic/blob/main/docs/"
fi

# Update systemd service if needed
print_step "Updating systemd service..."
systemctl --user daemon-reload

# Restart service if it was running
if [ "$SERVICE_WAS_RUNNING" = true ]; then
  print_step "Starting Audetic service..."
  if systemctl --user start audetic.service; then
    print_success "Audetic service restarted"
  else
    print_error "Failed to start Audetic service"
    print_warning "Check logs with: journalctl --user -u audetic.service -e"
  fi
fi

# Show update summary
echo
print_success "Update completed!"
echo
NEW_VERSION=$(get_version "$AUDETIC_DIR")
echo "Audetic version: $NEW_VERSION"
if [ "$UPDATE_WHISPER" = true ]; then
  WHISPER_VERSION=$(get_version "$WHISPER_DIR" 2>/dev/null || echo "unknown")
  echo "whisper.cpp version: $WHISPER_VERSION"
fi

# Show post-update instructions
echo
print_step "Post-update steps:"
echo "1. Check service status: ${GREEN}systemctl --user status audetic.service${NC}"
echo "2. View logs if needed: ${GREEN}journalctl --user -u audetic.service -f${NC}"
echo "3. Test recording with your keybind (e.g., Super+R)"

# Check for release notes
if [ -f "$AUDETIC_DIR/CHANGELOG.md" ] || [ -f "$AUDETIC_DIR/RELEASES.md" ]; then
  echo
  print_warning "Check release notes for important changes"
fi
