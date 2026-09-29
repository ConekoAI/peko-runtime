#!/bin/bash
#
# Peko Install from Source
# Usage: curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/main/install-from-source.sh | bash
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Configuration
REPO_URL="https://github.com/ConekoAI/peko-runtime.git"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
CLONE_DIR="${CLONE_DIR:-/tmp/peko-build}"
PEKO_HOME_DIR="${PEKO_HOME:-$HOME/.peko}"

# Check dependencies
check_dependencies() {
    local deps=("git" "curl")
    local missing=()
    
    for dep in "${deps[@]}"; do
        if ! command -v "$dep" >/dev/null 2>&1; then
            missing+=("$dep")
        fi
    done
    
    if [ ${#missing[@]} -ne 0 ]; then
        echo -e "${RED}Missing dependencies: ${missing[*]}${NC}"
        if [[ " ${missing[*]} " =~ " rustc " ]]; then
            echo -e "${YELLOW}Install Rust: https://rustup.rs/${NC}"
        fi
        exit 1
    fi
    
    # Check for Rust
    if ! command -v cargo >/dev/null 2>&1; then
        echo -e "${RED}Rust/Cargo not found${NC}"
        echo -e "${YELLOW}Install Rust:${NC}"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
    
    echo -e "${GREEN}✓ Dependencies OK${NC}"
}

# Clone repository
clone_repo() {
    echo -e "${BLUE}Cloning repository...${NC}"
    
    # Clean up old clone if exists
    if [ -d "$CLONE_DIR" ]; then
        echo -e "${YELLOW}  Removing old build directory...${NC}"
        rm -rf "$CLONE_DIR"
    fi
    
    git clone --depth 1 "$REPO_URL" "$CLONE_DIR"
    cd "$CLONE_DIR"
    
    echo -e "${GREEN}✓ Cloned to ${CLONE_DIR}${NC}"
}

# Build from source
build() {
    echo -e "${BLUE}Building Peko from source...${NC}"
    echo -e "${YELLOW}  This may take a few minutes...${NC}"
    
    cd "$CLONE_DIR"
    
    # Build release binary
    cargo build --release
    
    if [ ! -f "target/release/peko" ]; then
        echo -e "${RED}Build failed - binary not found${NC}"
        exit 1
    fi
    
    echo -e "${GREEN}✓ Build complete${NC}"
}

# Install binary
install_binary() {
    echo -e "${BLUE}Installing binary...${NC}"
    
    local binary_path="${CLONE_DIR}/target/release/peko"
    
    if [ -w "$INSTALL_DIR" ]; then
        cp "$binary_path" "${INSTALL_DIR}/peko"
        chmod +x "${INSTALL_DIR}/peko"
    else
        echo -e "${YELLOW}Requesting sudo access to install to ${INSTALL_DIR}${NC}"
        sudo cp "$binary_path" "${INSTALL_DIR}/peko"
        sudo chmod +x "${INSTALL_DIR}/peko"
    fi
    
    echo -e "${GREEN}✓ Installed to ${INSTALL_DIR}/peko${NC}"
}

# Setup directories
setup_directories() {
    echo -e "${BLUE}Setting up directories...${NC}"
    mkdir -p "$PEKO_HOME_DIR"
    echo -e "${GREEN}✓ Runtime home: ${PEKO_HOME_DIR}${NC}"
}

# Install systemd service (Linux only)
install_systemd_service() {
    if [ "$(uname -s)" != "Linux" ]; then
        return 0
    fi
    
    if ! command -v systemctl >/dev/null 2>&1; then
        echo -e "${YELLOW}systemd not detected, skipping service installation${NC}"
        return 0
    fi
    
    echo -e "${BLUE}Installing systemd service...${NC}"
    
    local service_file="/etc/systemd/system/peko.service"
    
    if [ -w "/etc/systemd/system" ]; then
        cat > "$service_file" <<EOF
[Unit]
Description=Peko Agent Runtime
After=network-online.target

[Service]
Type=simple
User=%I
ExecStart=${INSTALL_DIR}/peko daemon start
ExecStop=${INSTALL_DIR}/peko daemon stop
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
    else
        echo -e "${YELLOW}Requesting sudo for systemd service${NC}"
        sudo tee "$service_file" > /dev/null <<EOF
[Unit]
Description=Peko Agent Runtime
After=network-online.target

[Service]
Type=simple
User=%I
ExecStart=${INSTALL_DIR}/peko daemon start
ExecStop=${INSTALL_DIR}/peko daemon stop
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
    fi
    
    sudo systemctl daemon-reload 2>/dev/null || true
    echo -e "${GREEN}✓ Systemd service installed${NC}"
    echo -e "${YELLOW}  Enable: sudo systemctl enable peko@\$USER${NC}"
    echo -e "${YELLOW}  Start:  sudo systemctl start peko@\$USER${NC}"
}

# Print post-install info
print_post_install() {
    echo ""
    echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  Peko installed from source!${NC}"
    echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "${BLUE}Quick Start:${NC}"
    echo ""
    echo "  1. Add a model to the catalog and store its API key in the vault:"
    echo "     peko model add --template openai --model gpt-4o --key \"\$OPENAI_API_KEY\""
    echo "     (templates: openai, anthropic, kimi, ... — see \`peko model add --help\`)"
    echo ""
    echo "  2. Start the daemon:"
    echo "     peko daemon start"
    echo ""
    echo "  3. Create your first peko and talk to it:"
    echo "     peko create my-peko"
    echo "     peko send my-peko \"Hello\""
    echo "     peko log my-peko"
    echo ""
    echo -e "${BLUE}Development:${NC}"
    echo "  Source: ${CLONE_DIR}"
    echo "  Build:  cd ${CLONE_DIR} && cargo build --release"
    echo ""
    echo -e "${BLUE}Configuration:${NC}"
    echo "  Runtime home: ${PEKO_HOME_DIR} (models.toml, vault, pekos)"
    echo "  Optional tuning: see config.example.toml in the repo"
    echo ""
    echo -e "${YELLOW}To update later, just re-run this script${NC}"
}

# Clean up build directory
cleanup() {
    echo -e "${BLUE}Cleaning up...${NC}"
    rm -rf "$CLONE_DIR"
    echo -e "${GREEN}✓ Cleaned up ${CLONE_DIR}${NC}"
}

# Main flow
main() {
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo -e "${BLUE}  Peko Installer (from source)${NC}"
    echo -e "${BLUE}  github.com/ConekoAI/peko-runtime${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo ""
    
    check_dependencies
    clone_repo
    build
    install_binary
    setup_directories
    install_systemd_service
    cleanup
    print_post_install
}

# Handle flags
while [[ $# -gt 0 ]]; do
    case $1 in
        --keep-source)
            KEEP_SOURCE=1
            shift
            ;;
        --install-dir)
            INSTALL_DIR="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: install-from-source.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --keep-source       Keep the source directory after install"
            echo "  --install-dir DIR   Install to custom directory"
            echo "  --help, -h          Show this help"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

main

# Keep source if requested
if [ "${KEEP_SOURCE:-0}" -eq 1 ]; then
    echo ""
    echo -e "${YELLOW}Source kept at: ${CLONE_DIR}${NC}"
fi
