#!/usr/bin/env bash
#
# Install shell script quality tools:
# - ShellCheck (shell script linter)
# - shfmt (shell script formatter)
# - checkbashisms (portability checker)
#

set -euo pipefail

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo "🔧 Installing Shell Script Quality Tools"
echo ""

# Detect OS
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
  OS="linux"
elif [[ "$OSTYPE" == "darwin"* ]]; then
  OS="macos"
else
  OS="unknown"
fi

echo "Detected OS: $OS"
echo ""

# Check if running as root
if [ "$EUID" -eq 0 ]; then
  echo -e "${RED}❌ Please run this script as a normal user (not root)${NC}"
  echo "   The script will prompt for sudo password when needed"
  exit 1
fi

# Function to check if a command exists
command_exists() {
  command -v "$1" &>/dev/null
}

# Install ShellCheck
install_shellcheck() {
  if command_exists shellcheck; then
    echo -e "${GREEN}✓ ShellCheck already installed${NC}"
    shellcheck --version | head -n 1
  else
    echo "📦 Installing ShellCheck..."

    if [ "$OS" = "linux" ]; then
      if command_exists pacman; then
        sudo pacman -S --noconfirm shellcheck
      elif command_exists apt-get; then
        sudo apt-get update && sudo apt-get install -y shellcheck
      elif command_exists dnf; then
        sudo dnf install -y ShellCheck
      else
        echo -e "${YELLOW}⚠️  Could not detect package manager${NC}"
        echo "   Please install ShellCheck manually:"
        echo "   https://github.com/koalaman/shellcheck#installing"
        return 1
      fi
    elif [ "$OS" = "macos" ]; then
      if command_exists brew; then
        brew install shellcheck
      else
        echo -e "${YELLOW}⚠️  Homebrew not found${NC}"
        echo "   Please install Homebrew first: https://brew.sh"
        echo "   Then run: brew install shellcheck"
        return 1
      fi
    fi

    if command_exists shellcheck; then
      echo -e "${GREEN}✅ ShellCheck installed successfully${NC}"
      shellcheck --version | head -n 1
    fi
  fi
}

# Install shfmt
install_shfmt() {
  if command_exists shfmt; then
    echo -e "${GREEN}✓ shfmt already installed${NC}"
    shfmt --version
  else
    echo "📦 Installing shfmt..."

    if [ "$OS" = "linux" ]; then
      if command_exists pacman; then
        sudo pacman -S --noconfirm shfmt
      elif command_exists apt-get; then
        # shfmt not in standard apt repos, use snap or download binary
        if command_exists snap; then
          sudo snap install shfmt
        else
          echo "   Downloading shfmt binary..."
          local version="v3.8.0"
          local arch
          arch=$(uname -m)
          if [ "$arch" = "x86_64" ]; then
            arch="amd64"
          elif [ "$arch" = "aarch64" ]; then
            arch="arm64"
          fi
          curl -Lo /tmp/shfmt "https://github.com/mvdan/sh/releases/download/${version}/shfmt_${version}_linux_${arch}"
          chmod +x /tmp/shfmt
          sudo mv /tmp/shfmt /usr/local/bin/shfmt
        fi
      elif command_exists dnf; then
        sudo dnf install -y shfmt
      else
        echo -e "${YELLOW}⚠️  Could not detect package manager${NC}"
        echo "   Please install shfmt manually:"
        echo "   https://github.com/mvdan/sh#shfmt"
        return 1
      fi
    elif [ "$OS" = "macos" ]; then
      if command_exists brew; then
        brew install shfmt
      else
        echo -e "${YELLOW}⚠️  Homebrew not found${NC}"
        echo "   Please install Homebrew first: https://brew.sh"
        echo "   Then run: brew install shfmt"
        return 1
      fi
    fi

    if command_exists shfmt; then
      echo -e "${GREEN}✅ shfmt installed successfully${NC}"
      shfmt --version
    fi
  fi
}

# Install checkbashisms
install_checkbashisms() {
  if command_exists checkbashisms; then
    echo -e "${GREEN}✓ checkbashisms already installed${NC}"
    checkbashisms --version 2>&1 | head -n 1 || echo "checkbashisms (version unknown)"
  else
    echo "📦 Installing checkbashisms..."

    if [ "$OS" = "linux" ]; then
      if command_exists pacman; then
        # checkbashisms not available in standard Arch repos
        echo -e "${YELLOW}⚠️  checkbashisms not available in Arch Linux standard repos${NC}"
        echo "   It can be installed from AUR: yay -S checkbashisms"
        echo "   Or skip it - it's optional (ShellCheck is the main tool)"
        return 0 # Don't fail - it's optional
      elif command_exists apt-get; then
        sudo apt-get update && sudo apt-get install -y devscripts
      elif command_exists dnf; then
        sudo dnf install -y devscripts
      else
        echo -e "${YELLOW}⚠️  Could not detect package manager${NC}"
        echo "   checkbashisms is part of the 'devscripts' package"
        echo "   It's optional - ShellCheck is the main tool"
        return 0 # Don't fail - it's optional
      fi
    elif [ "$OS" = "macos" ]; then
      echo -e "${YELLOW}⚠️  checkbashisms not readily available on macOS${NC}"
      echo "   You can install it via cpan:"
      echo "   sudo cpan Debian::Devscripts::Checkbashisms"
      echo "   Or skip it - it's optional (ShellCheck is the main tool)"
      return 0 # Don't fail - it's optional
    fi

    if command_exists checkbashisms; then
      echo -e "${GREEN}✅ checkbashisms installed successfully${NC}"
    else
      echo -e "${YELLOW}⚠️  checkbashisms not installed (optional)${NC}"
    fi
  fi
}

# Install all tools
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

install_shellcheck
echo ""

install_shfmt
echo ""

install_checkbashisms
echo ""

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo -e "${GREEN}✨ Installation complete!${NC}"
echo ""
echo "You can now run:"
echo "  make all.shell-lint  - Lint all shell scripts"
echo "  make all.shell-fmt   - Format all shell scripts"
echo "  make all.shell-check - Check all shell scripts"
echo ""
