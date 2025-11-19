#!/bin/bash
set -euo pipefail

# Parse command line arguments
CLEAN_INSTALL=false
SKIP_DEPS=false
SKIP_WHISPER=false
FORCE_REBUILD=false

while [[ $# -gt 0 ]]; do
  case $1 in
    --clean)
      CLEAN_INSTALL=true
      shift
      ;;
    --skip-deps)
      SKIP_DEPS=true
      shift
      ;;
    --skip-whisper)
      SKIP_WHISPER=true
      shift
      ;;
    --rebuild)
      FORCE_REBUILD=true
      shift
      ;;
    --help)
      echo "Audetic Installation Script for Arch Linux"
      echo ""
      echo "Usage: $0 [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --clean         Clean install (remove existing installations)"
      echo "  --skip-deps     Skip system dependency installation"
      echo "  --skip-whisper  Skip whisper.cpp build"
      echo "  --rebuild       Force rebuild Audetic even if binary exists"
      echo "  --help          Show this help message"
      echo ""
      echo "Examples:"
      echo "  $0                    # Normal install with smart detection"
      echo "  $0 --clean            # Fresh install from scratch"
      echo "  $0 --skip-whisper     # Update only Audetic"
      echo "  $0 --rebuild          # Force rebuild Audetic"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      echo "Use --help for usage information"
      exit 1
      ;;
  esac
done

# Colors for output - check if terminal supports colors
if [ -t 1 ] && [ -n "${TERM}" ] && [ "${TERM}" != "dumb" ]; then
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[1;33m'
  BLUE='\033[0;34m'
  NC='\033[0m' # No Color
else
  RED=''
  GREEN=''
  YELLOW=''
  BLUE=''
  NC=''
fi

# Print colored output
print_step() {
  printf "${BLUE}==>${NC} %s\n" "$1"
}

print_success() {
  printf "${GREEN}✓${NC} %s\n" "$1"
}

print_error() {
  printf "${RED}✗${NC} %s\n" "$1"
}

print_warning() {
  printf "${YELLOW}!${NC} %s\n" "$1"
}

# Get the directory where the script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Audetic project root is one level up from scripts directory
AUDETIC_DIR="$(dirname "$SCRIPT_DIR")"

# Configuration variables
WHISPER_DIR="$HOME/.local/share/audetic/whisper"
CONFIG_DIR="$HOME/.config/audetic"
INSTALL_DIR="/usr/local/bin"
SOURCE_BACKUP_DIR="$HOME/.local/share/audetic/source"

# Check if running on Arch Linux
if [ ! -f /etc/arch-release ]; then
  os_name=$(grep '^NAME=' /etc/os-release | cut -d'=' -f2)
  print_error "This script is designed for Arch Linux. Detected: $os_name"
  exit 1
fi

print_step "Starting Audetic installation for Arch Linux"

# Clean install check
if [ "$CLEAN_INSTALL" = true ]; then
  print_warning "Performing clean install - removing existing installations"
  [ -d "$WHISPER_DIR" ] && rm -rf "$WHISPER_DIR"
  [ -f "$INSTALL_DIR/audetic" ] && sudo rm -f "$INSTALL_DIR/audetic"
  [ -d "$SOURCE_BACKUP_DIR" ] && rm -rf "$SOURCE_BACKUP_DIR"
  print_success "Clean install prepared"
fi

# Step 1: Install system dependencies
if [ "$SKIP_DEPS" = false ]; then
  print_step "Checking system dependencies..."
  MISSING_DEPS=()
  for dep in rust ydotool wtype wl-clipboard alsa-lib curl cmake make gcc; do
    if ! pacman -Qi "$dep" &>/dev/null; then
      MISSING_DEPS+=("$dep")
    fi
  done

  if [ ${#MISSING_DEPS[@]} -eq 0 ]; then
    print_success "All system dependencies already installed"
  else
    print_step "Installing missing dependencies: ${MISSING_DEPS[*]}"
    if ! sudo pacman -S --needed --noconfirm "${MISSING_DEPS[@]}"; then
      print_error "Failed to install system dependencies"
      exit 1
    fi
    print_success "System dependencies installed"
  fi
else
  print_warning "Skipping system dependency check (--skip-deps)"
fi

# Step 1.5: Setup ydotool service
print_step "Checking ydotool service..."
if systemctl --user is-active --quiet ydotool.service; then
  print_success "ydotool service is already running"
else
  print_step "Setting up ydotool service..."
  if ! systemctl --user enable --now ydotool.service; then
    print_warning "Failed to enable ydotool service - you may need to start it manually"
    print_warning "Run: systemctl --user enable --now ydotool.service"
  else
    print_success "ydotool service enabled and started"
  fi
fi

# Step 2: Clone and build optimized whisper.cpp
if [ "$SKIP_WHISPER" = false ]; then
  mkdir -p "$(dirname "$WHISPER_DIR")"

  # Check if whisper is already installed and working
  if [ -d "$WHISPER_DIR" ] && [ -f "$WHISPER_DIR/build/bin/whisper-cli" ] && [ -f "$WHISPER_DIR/models/ggml-large-v3-turbo-q5_1.bin" ]; then
    print_success "Whisper already installed with model"
    print_warning "Use --clean to reinstall whisper from scratch"
  else
    print_step "Setting up optimized whisper.cpp..."

    if [ -d "$WHISPER_DIR" ]; then
      print_warning "Incomplete whisper installation found. Removing..."
      rm -rf "$WHISPER_DIR"
    fi

    print_step "Cloning optimized whisper.cpp fork..."
    if ! git clone https://github.com/matsilva/whisper.git "$WHISPER_DIR"; then
      print_error "Failed to clone whisper repository"
      exit 1
    fi

    cd "$WHISPER_DIR"
    print_step "Building whisper-cli with large-v3-turbo model (this may take a while)..."
    if ! ./build.sh; then
      print_error "Failed to build whisper"
      exit 1
    fi
    print_success "Whisper built successfully"

    # Verify whisper-cli exists
    if [ ! -f "$WHISPER_DIR/build/bin/whisper-cli" ]; then
      print_error "whisper-cli binary not found at expected location"
      exit 1
    fi
  fi
else
  print_warning "Skipping whisper installation (--skip-whisper)"
fi

# Step 3: Build Audetic
cd "$AUDETIC_DIR"

# Check if we need to rebuild
BUILD_NEEDED=false
if [ ! -f "target/release/audetic" ]; then
  BUILD_NEEDED=true
  print_step "Audetic binary not found, building..."
elif [ "$FORCE_REBUILD" = true ]; then
  BUILD_NEEDED=true
  print_step "Force rebuild requested, building Audetic..."
else
  # Check if source files are newer than binary
  newer_files=$(find src -newer target/release/audetic -print -quit 2>/dev/null || true)
  if [ -n "$newer_files" ]; then
    BUILD_NEEDED=true
    print_step "Source files changed, rebuilding Audetic..."
  else
    print_success "Audetic binary is up to date"
  fi
fi

if [ "$BUILD_NEEDED" = true ]; then
  if ! cargo build --release; then
    print_error "Failed to build Audetic"
    exit 1
  fi
  print_success "Audetic built successfully"
fi

# Step 4: Install Audetic binary
print_step "Installing Audetic binary..."

# Check if service is running and stop it to avoid "Text file busy" error
SERVICE_WAS_RUNNING=false
if systemctl --user is-active --quiet audetic.service; then
  SERVICE_WAS_RUNNING=true
  print_warning "Audetic service is running, stopping it temporarily..."
  systemctl --user stop audetic.service
  sleep 1 # Give it a moment to fully stop
fi

# Try to copy the binary
if ! sudo cp target/release/audetic "$INSTALL_DIR/"; then
  print_error "Failed to install Audetic binary"
  # If service was running, start it again even on failure
  if [ "$SERVICE_WAS_RUNNING" = true ]; then
    systemctl --user start audetic.service
  fi
  exit 1
fi
sudo chmod +x "$INSTALL_DIR/audetic"
print_success "Audetic installed to $INSTALL_DIR"

# If service was running before, start it again
if [ "$SERVICE_WAS_RUNNING" = true ]; then
  print_step "Restarting Audetic service..."
  systemctl --user start audetic.service
  print_success "Audetic service restarted"
fi

# Step 4.5: Keep source for updates and install update scripts
print_step "Setting up update mechanism..."
mkdir -p "$SOURCE_BACKUP_DIR"

# Copy source files for future updates
if ! cp -r "$AUDETIC_DIR"/.git "$SOURCE_BACKUP_DIR/" 2>/dev/null; then
  print_warning "Git repository not found. Updates will require manual installation."
else
  # Copy essential files including update scripts
  cp -r "$AUDETIC_DIR"/* "$SOURCE_BACKUP_DIR/" 2>/dev/null || true
  # Ensure update scripts are in backup location
  cp "$AUDETIC_DIR/scripts/update-audetic.sh" "$SOURCE_BACKUP_DIR/" 2>/dev/null || true
  cp "$AUDETIC_DIR/scripts/audetic-update" "$SOURCE_BACKUP_DIR/" 2>/dev/null || true
  print_success "Source backed up for future updates"
fi

# Make update scripts executable
chmod +x "$AUDETIC_DIR/scripts/update-audetic.sh"
chmod +x "$AUDETIC_DIR/scripts/audetic-update"

# Install system-wide update command
if [ -f "$AUDETIC_DIR/scripts/audetic-update" ]; then
  # Update the wrapper with correct source directory
  sed -i "s|AUDETIC_SOURCE_DIR:-.*}|AUDETIC_SOURCE_DIR:-$SOURCE_BACKUP_DIR}|" "$AUDETIC_DIR/scripts/audetic-update"

  if ! sudo cp "$AUDETIC_DIR/scripts/audetic-update" "$INSTALL_DIR/"; then
    print_warning "Failed to install update wrapper"
  else
    sudo chmod +x "$INSTALL_DIR/audetic-update"
    print_success "Update command installed: audetic-update"
  fi
fi

# Step 5: Create configuration
print_step "Creating configuration..."
mkdir -p "$CONFIG_DIR"

cat >"$CONFIG_DIR/config.toml" <<EOF
[whisper]
provider = "whisper-cpp"
model = "large-v3-turbo"
language = "en"
command_path = "$WHISPER_DIR/build/bin/whisper-cli"
model_path = "$WHISPER_DIR/models/ggml-large-v3-turbo-q5_1.bin"

[ui]
notification_color = "rgb(ff1744)"

[wayland]
input_method = "ydotool"

[behavior]
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
audio_feedback = true
EOF

print_success "Configuration created at $CONFIG_DIR/config.toml"

# Step 6: Create systemd user service
print_step "Creating systemd user service..."
mkdir -p ~/.config/systemd/user

# Get number of CPU threads for whisper
num_threads=$(nproc)

cat >~/.config/systemd/user/audetic.service <<EOF
[Unit]
Description=Audetic Voice Transcription Service
After=graphical-session.target

[Service]
Type=simple
WorkingDirectory=$AUDETIC_DIR
ExecStart=$INSTALL_DIR/audetic
Restart=always
RestartSec=5
Environment="RUST_LOG=info"
Environment="HOME=$HOME"
Environment="PATH=/usr/local/bin:/usr/bin:/bin"
# Memory settings - adjust based on your system
MemoryMax=8G
MemorySwapMax=12G
# CPU settings - let whisper use multiple cores
# Remove CPUQuota to allow full CPU usage
# Set thread count for whisper (adjust based on your CPU)
Environment="OMP_NUM_THREADS=$num_threads"

[Install]
WantedBy=default.target
EOF

systemctl --user daemon-reload
print_success "Systemd service created"

# Step 7: Print Hyprland keybind instructions
print_step "Installation complete!"
echo
print_warning "Next steps:"
echo
printf "1. Start Audetic service:\n"
printf "   %bmake start%b\n" "${GREEN}" "${NC}"
echo
printf "2. Add this keybind to your Hyprland config (~/.config/hypr/hyprland.conf):\n"
printf "   %bbindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle%b\n" "${GREEN}" "${NC}"
printf "   or for Omarchy:\n"
printf "   %bbindd = SUPER, R, Audetic, exec, \$terminal -e curl -X POST http://127.0.0.1:3737/toggle%b\n" "${GREEN}" "${NC}"
echo
printf "3. Check service status:\n"
printf "   %bmake status%b\n" "${GREEN}" "${NC}"
echo
printf "4. View logs if needed:\n"
printf "   %bmake logs%b\n" "${GREEN}" "${NC}"
echo
printf "5. Common development commands:\n"
printf "   %bmake help%b      # Show all available commands\n" "${GREEN}" "${NC}"
printf "   %bmake restart%b   # Restart the service\n" "${GREEN}" "${NC}"
printf "   %bmake build%b     # Rebuild Audetic\n" "${GREEN}" "${NC}"
echo
printf "6. Update Audetic anytime with:\n"
printf "   %bmake update%b     # Update Audetic only\n" "${GREEN}" "${NC}"
printf "   %bmake update-all%b # Update both Audetic and whisper.cpp\n" "${GREEN}" "${NC}"
printf "   Note: If 'audetic-update' command not found, run: %bhash -r%b\n" "${GREEN}" "${NC}"
echo
print_success "Audetic is ready to use! Press Super+R to start recording."
