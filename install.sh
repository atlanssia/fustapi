#!/bin/sh
# FustAPI Deterministic Installer
# Security-first, no heuristics, checksum-verified.

set -e

# Configuration
REPO="atlanssia/fustapi"
INSTALL_DIR="/usr/local/bin"
BINARY_NAME="fustapi"
GITHUB_API="https://api.github.com/repos/$REPO"
GITHUB_DOWNLOAD="https://github.com/$REPO/releases/download"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

# Flag variables
DRY_RUN=false
VERIFY_ONLY=false

# Help message and supported matrix
print_supported_matrix() {
    printf "${RED}Error:${NC} Unsupported OS/Architecture combination.\n\n"
    printf "FustAPI currently supports the following targets:\n"
    printf "  - Linux (x86_64)  -> x86_64-unknown-linux-gnu\n"
    printf "  - Linux (ARM64)   -> aarch64-unknown-linux-gnu\n"
    printf "  - macOS (x86_64)  -> x86_64-apple-darwin\n"
    printf "  - macOS (ARM64)   -> aarch64-apple-darwin\n"
    printf "  - Windows (x64)   -> x86_64-pc-windows-msvc\n\n"
}

# Parse arguments
for arg in "$@"; do
    case $arg in
        --dry-run) DRY_RUN=true ;;
        --verify-only) VERIFY_ONLY=true ;;
        --help|-h)
            printf "FustAPI Installer\n\n"
            printf "Usage: install.sh [options]\n\n"
            printf "Options:\n"
            printf "  --dry-run      Show what would be done without downloading\n"
            printf "  --verify-only  Download and verify checksum without installing\n"
            printf "  --help, -h     Show this help message\n"
            exit 0
            ;;
    esac
done

printf "${BLUE}==>${NC} Resolving target platform...\n"

# 1. Deterministic Target Resolution
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

TARGET=""
EXTENSION="tar.gz"

case "$OS-$ARCH" in
    linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
    linux-aarch64|linux-arm64) TARGET="aarch64-unknown-linux-gnu" ;;
    darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
    darwin-aarch64|darwin-arm64) TARGET="aarch64-apple-darwin" ;;
    mingw*|cygwin*|msys*) 
        TARGET="x86_64-pc-windows-msvc" 
        EXTENSION="zip"
        ;;
    *)
        print_supported_matrix
        exit 1
        ;;
esac

printf "${BLUE}==>${NC} Resolved Target: ${GREEN}$TARGET${NC}\n"

# 2. Version Resolution
printf "${BLUE}==>${NC} Fetching latest release version...\n"
RELEASE_JSON=$(curl -s "$GITHUB_API/releases/latest")
VERSION=$(echo "$RELEASE_JSON" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')

if [ -z "$VERSION" ]; then
    printf "${RED}Error:${NC} Failed to resolve version from GitHub API.\n"
    exit 1
fi

FILENAME="fustapi-$VERSION-$TARGET.$EXTENSION"
DOWNLOAD_URL="$GITHUB_DOWNLOAD/$VERSION/$FILENAME"
CHECKSUM_URL="$GITHUB_DOWNLOAD/$VERSION/checksums-sha256.txt"

if [ "$DRY_RUN" = true ]; then
    printf "${YELLOW}[DRY RUN]${NC} Would download: $DOWNLOAD_URL\n"
    printf "${YELLOW}[DRY RUN]${NC} Would verify against: $CHECKSUM_URL\n"
    exit 0
fi

# 3. Artifact Manifest Validation (Pre-download check)
printf "${BLUE}==>${NC} Validating artifact existence...\n"
if ! echo "$RELEASE_JSON" | grep -q "$FILENAME"; then
    printf "${RED}Error:${NC} Artifact $FILENAME not found in release assets.\n"
    exit 1
fi

# 4. Mandatory Verification and Download
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

printf "${BLUE}==>${NC} Downloading checksums...\n"
curl -sL "$CHECKSUM_URL" -o "$TEMP_DIR/checksums-sha256.txt"

printf "${BLUE}==>${NC} Downloading $FILENAME...\n"
curl -sL "$DOWNLOAD_URL" -o "$TEMP_DIR/$FILENAME"

# 5. SHA256 Verification (MANDATORY)
printf "${BLUE}==>${NC} Verifying SHA256 integrity...\n"

# Extract expected hash from checksums file
# Robustly find the 64-char hex string in the line containing the filename
EXPECTED_HASH=$(grep "$FILENAME" "$TEMP_DIR/checksums-sha256.txt" | grep -oE '[0-9a-fA-F]{64}' | head -n 1)

if [ -z "$EXPECTED_HASH" ]; then
    printf "${RED}Error:${NC} No checksum found for $FILENAME in manifest.\n"
    exit 1
fi

# Calculate actual hash
if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL_HASH=$(sha256sum "$TEMP_DIR/$FILENAME" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL_HASH=$(shasum -a 256 "$TEMP_DIR/$FILENAME" | awk '{print $1}')
else
    printf "${RED}Error:${NC} No sha256 calculation tool found (sha256sum or shasum).\n"
    exit 1
fi

if [ "$ACTUAL_HASH" != "$EXPECTED_HASH" ]; then
    printf "${RED}Error:${NC} Checksum mismatch!\n"
    printf "  Expected: $EXPECTED_HASH\n"
    printf "  Actual:   $ACTUAL_HASH\n"
    exit 1
fi

printf "${GREEN}OK:${NC} Checksum verified successfully.\n"

if [ "$VERIFY_ONLY" = true ]; then
    printf "${GREEN}Success:${NC} Artifact verified at $TEMP_DIR/$FILENAME\n"
    exit 0
fi

# 6. Safe Extraction
printf "${BLUE}==>${NC} Extracting $BINARY_NAME...\n"
if [ "$EXTENSION" = "zip" ]; then
    unzip -q "$TEMP_DIR/$FILENAME" -d "$TEMP_DIR/out"
else
    mkdir -p "$TEMP_DIR/out"
    tar -xzf "$TEMP_DIR/$FILENAME" -C "$TEMP_DIR/out"
fi

# Locate binary (handle .exe for windows)
RAW_BINARY="$TEMP_DIR/out/$BINARY_NAME"
if [ "$TARGET" = "x86_64-pc-windows-msvc" ]; then
    RAW_BINARY="$TEMP_DIR/out/$BINARY_NAME.exe"
fi

if [ ! -f "$RAW_BINARY" ]; then
    printf "${RED}Error:${NC} Binary not found in extracted archive.\n"
    exit 1
fi

# 7. Install to target directory
printf "${BLUE}==>${NC} Installing to $INSTALL_DIR (may require sudo)...\n"
if [ -w "$INSTALL_DIR" ]; then
    mv "$RAW_BINARY" "$INSTALL_DIR/$BINARY_NAME"
    chmod +x "$INSTALL_DIR/$BINARY_NAME"
else
    sudo mv "$RAW_BINARY" "$INSTALL_DIR/$BINARY_NAME"
    sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
fi

printf "\n${GREEN}Success:${NC} FustAPI $VERSION installed to $INSTALL_DIR/$BINARY_NAME\n"
printf "Run '${BLUE}fustapi serve${NC}' to start the gateway.\n"
