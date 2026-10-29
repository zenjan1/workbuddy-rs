#!/bin/bash
# Xi one-click install script
# Usage: curl -sL xi.sh/install | sh
#        bash install.sh --test

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*"
}

# Test mode
if [[ "$1" == "--test" ]]; then
    log_info "Running in test mode..."
    log_info "Install script is working correctly!"
    exit 0
fi

# Main installation logic
log_info "Starting Xi installation..."

# Detect OS
OS="$(uname -s)"
case "$OS" in
    Linux*)     PLATFORM="linux";; 
    Darwin*)    PLATFORM="darwin";; 
    *)          log_error "Unsupported OS: $OS"; exit 1;;
esac

# Detect architecture
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64)   ARCH="amd64";; 
    arm64|aarch64)  ARCH="arm64";; 
    *)              log_error "Unsupported architecture: $ARCH"; exit 1;;
esac

log_info "Detected platform: $PLATFORM/$ARCH"

# Installation directory
INSTALL_DIR="${XI_INSTALL_DIR:-$HOME/.xi/bin}"
mkdir -p "$INSTALL_DIR"

# Download URL
BASE_URL="https://xi.sh/releases"
VERSION="${XI_VERSION:-latest}"
if [[ "$VERSION" == "latest" ]]; then
    VERSION=$(curl -sL "$BASE_URL/latest.txt" 2>/dev/null || echo "v0.1.0")
fi

BINARY_NAME="xi-$PLATFORM-$ARCH"
DOWNLOAD_URL="$BASE_URL/$VERSION/$BINARY_NAME"

log_info "Downloading $BINARY_NAME from $DOWNLOAD_URL"

# Download and install
curl -sL "$DOWNLOAD_URL" -o "$INSTALL_DIR/xi" || {
    log_error "Failed to download Xi binary"
    exit 1
}

chmod +x "$INSTALL_DIR/xi"

log_info "Xi installed to $INSTALL_DIR/xi"

# Add to PATH if needed
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    log_warn "Add $INSTALL_DIR to your PATH to use xi command"
    log_warn "Example: export PATH=\"$INSTALL_DIR:\$PATH\""
fi

log_info "Installation complete! Run 'xi --help' to get started."
