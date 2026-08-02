#!/bin/bash
# build_vpk.sh - Build OpenNOW Vita VPK package
# This script builds the PS Vita VPK for OpenNOW Vita using cargo-vita

set -e  # Exit on any error

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "========================================"
echo "OpenNOW Vita VPK Build Script"
echo "========================================"
echo ""

# Source cargo environment if it exists
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

# Check if VITASDK is set
if [ -z "$VITASDK" ]; then
    echo -e "${RED}ERROR: VITASDK environment variable is not set.${NC}"
    echo "Please set VITASDK to your VitaSDK installation path."
    echo "Example: export VITASDK=/usr/local/vitasdk"
    exit 1
fi

echo -e "${GREEN}✓${NC} VITASDK found at: $VITASDK"

# Check if VitaSDK binaries exist
if [ ! -f "$VITASDK/bin/arm-vita-eabi-gcc" ]; then
    echo -e "${RED}ERROR: VitaSDK binaries not found in $VITASDK/bin${NC}"
    exit 1
fi

echo -e "${GREEN}✓${NC} VitaSDK binaries found"

# Check if rustup is installed
if ! command -v rustup &> /dev/null; then
    echo -e "${RED}ERROR: rustup is not installed.${NC}"
    echo ""
    echo "Please install Rust using rustup:"
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    echo ""
    echo "After installation, run:"
    echo "  source \$HOME/.cargo/env"
    echo "  ./build_vpk.sh"
    exit 1
fi

echo -e "${GREEN}✓${NC} rustup found: $(rustup --version | head -1)"

# Check if nightly toolchain is installed
if ! rustup toolchain list | grep -q nightly; then
    echo -e "${YELLOW}Installing Rust nightly toolchain...${NC}"
    rustup toolchain install nightly
fi

echo -e "${GREEN}✓${NC} Rust nightly toolchain available"

# Check if rust-src component is installed (required for cross-compilation)
if ! rustup component list --toolchain nightly | grep -q "rust-src.*installed"; then
    echo -e "${YELLOW}Installing rust-src component (required for cross-compilation)...${NC}"
    rustup component add rust-src --toolchain nightly
fi

echo -e "${GREEN}✓${NC} rust-src component installed"

# Check if cargo-vita is installed
if ! cargo +nightly vita --version &> /dev/null; then
    echo -e "${YELLOW}cargo-vita not found. Installing...${NC}"
    cargo +nightly install cargo-vita
fi

echo -e "${GREEN}✓${NC} cargo-vita found: $(cargo +nightly vita --version 2>&1 | head -1)"

# Check if pkg-config is available
if ! command -v pkg-config &> /dev/null; then
    echo -e "${YELLOW}WARNING: pkg-config not found. This may cause build issues.${NC}"
    echo "Install with: sudo apt install pkg-config (Debian/Ubuntu)"
    echo "           or: brew install pkg-config (macOS)"
fi

echo ""
echo "========================================"
echo "Building VPK..."
echo "========================================"
echo ""

# Set RUSTFLAGS to disable NEON (required for this target)
export RUSTFLAGS="-C target-feature=-neon"

# Build the VPK using cargo-vita (note: armv7-sony-vita-newlibeabihf target
# doesn't have prebuilt artifacts, so cargo-vita uses -Z build-std to build
# the standard library from source, which is why rust-src is required)
cargo +nightly vita build vpk --release

# Check if build succeeded
VPK_PATH="target/armv7-sony-vita-newlibeabihf/release/opennow-vita.vpk"

if [ -f "$VPK_PATH" ]; then
    VPK_SIZE=$(du -h "$VPK_PATH" | cut -f1)
    echo ""
    echo "========================================"
    echo -e "${GREEN}✓ BUILD SUCCESS!${NC}"
    echo "========================================"
    echo ""
    echo "VPK location: $VPK_PATH"
    echo "VPK size: $VPK_SIZE"
    echo ""
    echo "To install on your PS Vita:"
    echo "  1. Transfer the VPK to your Vita via USB or FTP"
    echo "  2. Open VitaShell and navigate to the VPK"
    echo "  3. Press X to install"
    echo ""
    echo "Or use the Makefile targets:"
    echo "  make desktop              # Copy VPK to Desktop"
    echo "  make ftp VITA_IP=<ip>     # Upload via FTP"
    exit 0
else
    echo ""
    echo "========================================"
    echo -e "${RED}✗ BUILD FAILED${NC}"
    echo "========================================"
    echo "VPK file was not created at expected location."
    exit 1
fi
