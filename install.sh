#!/bin/bash
#
# Pekobot Installer - Download from GitHub Releases
# Usage: curl -fsSL https://raw.githubusercontent.com/coneko/pekobot/main/install.sh | bash
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
GITHUB_REPO="coneko/pekobot"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
CONFIG_DIR="${CONFIG_DIR:-$HOME/.config/pekobot}"
DATA_DIR="${DATA_DIR:-$HOME/.local/share/pekobot}"

# Detect architecture and OS
detect_platform() {
    local arch=$(uname -m)
    local os=$(uname -s | tr '[:upper:]' '[:lower:]')
    
    case "$arch" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        armv7l) arch="armv7" ;;
        *) echo -e "${RED}Unsupported architecture: $arch${NC}"; exit 1 ;;
    esac
    
    case "$os" in
        linux) os="linux" ;;
        darwin) os="macos" ;;
        *) echo -e "${RED}Unsupported OS: $os${NC}"; exit 1 ;;
    esac
    
    echo "${os}-${arch}"
}

# Get latest release version from GitHub
get_latest_version() {
    local version
    version=$(curl -fsSL "${GITHUB_API}/releases/latest" 2>/dev/null | grep -o '"tag_name": "[^"]*"' | cut -d'"' -f4 | sed 's/^v//')
    
    if [ -z "$version" ]; then
        echo "0.1.0"  # Fallback version
    else
        echo "$version"
    fi
}

# Download and install binary
install_binary() {
    local platform=$1
    local version=$2
    local tmpdir=$(mktemp -d)
    local asset_name="pekobot-${platform}.tar.gz"
    
    echo -e "${BLUE}Downloading Pekobot v${version} for ${platform}...${NC}"
    
    # Try GitHub releases first
    local download_url="https://github.com/${GITHUB_REPO}/releases/download/v${version}/${asset_name}"
    
    echo -e "   ${BLUE}Source: ${download_url}${NC}"
    
    if ! curl -fsSL --progress-bar "$download_url" -o "${tmpdir}/pekobot.tar.gz" 2>/dev/null; then
        echo -e "${YELLOW}Release asset not found, trying alternative...${NC}"
        
        # Try without 'v' prefix
        download_url="https://github.com/${GITHUB_REPO}/releases/download/${version}/${asset_name}"
        
        if ! curl -fsSL --progress-bar "$download_url" -o "${tmpdir}/pekobot.tar.gz" 2>/dev/null; then
            echo -e "${RED}Failed to download binary${NC}"
            echo -e "${YELLOW}Please check that the release exists:${NC}"
            echo -e "   ${BLUE}https://github.com/${GITHUB_REPO}/releases${NC}"
            rm -rf "$tmpdir"
            exit 1
        fi
    fi
    
    echo -e "${BLUE}Extracting...${NC}"
    tar -xzf "${tmpdir}/pekobot.tar.gz" -C "$tmpdir" 2>/dev/null || {
        echo -e "${RED}Failed to extract archive${NC}"
        rm -rf "$tmpdir"
        exit 1
    }
    
    # Find the binary (might be in subdir)
    local binary_path
    if [ -f "${tmpdir}/pekobot" ]; then
        binary_path="${tmpdir}/pekobot"
    elif [ -f "${tmpdir}/target/release/pekobot" ]; then
        binary_path="${tmpdir}/target/release/pekobot"
    else
        binary_path=$(find "$tmpdir" -name "pekobot" -type f | head -1)
    fi
    
    if [ -z "$binary_path" ]; then
        echo -e "${RED}Could not find pekobot binary in archive${NC}"
        rm -rf "$tmpdir"
        exit 1
    fi
    
    # Make executable
    chmod +x "$binary_path"
    
    # Install binary
    echo -e "${BLUE}Installing to ${INSTALL_DIR}...${NC}"
    if [ -w "$INSTALL_DIR" ]; then
        mv "$binary_path" "${INSTALL_DIR}/pekobot"
    else
        echo -e "${YELLOW}Requesting sudo access to install to ${INSTALL_DIR}${NC}"
        sudo mv "$binary_path" "${INSTALL_DIR}/pekobot"
    fi
    
    rm -rf "$tmpdir"
    echo -e "${GREEN}✓ Pekobot v${version} installed to ${INSTALL_DIR}/pekobot${NC}"
}

# Install systemd service (Linux only)
install_systemd_service() {
    if [ "$(uname -s)" != "Linux" ]; then
        return 0
    fi
    
    # Check if systemd is available
    if ! command -v systemctl >/dev/null 2>&1; then
        echo -e "${YELLOW}systemd not detected, skipping service installation${NC}"
        return 0
    fi
    
    echo -e "${BLUE}Installing systemd service...${NC}"
    
    # Create service file
    if [ -w "/etc/systemd/system" ]; then
        cat > /etc/systemd/system/pekobot.service <<EOF
[Unit]
Description=Pekobot Agent Runtime
Documentation=https://github.com/coneko/pekobot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=%I
ExecStart=${INSTALL_DIR}/pekobot daemon start
ExecStop=${INSTALL_DIR}/pekobot daemon stop
Restart=always
RestartSec=10
StartLimitInterval=60s
StartLimitBurst=3

[Install]
WantedBy=multi-user.target
EOF
    else
        echo -e "${YELLOW}Requesting sudo access to install systemd service${NC}"
        sudo tee /etc/systemd/system/pekobot.service > /dev/null <<EOF
[Unit]
Description=Pekobot Agent Runtime
Documentation=https://github.com/coneko/pekobot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=%I
ExecStart=${INSTALL_DIR}/pekobot daemon start
ExecStop=${INSTALL_DIR}/pekobot daemon stop
Restart=always
RestartSec=10
StartLimitInterval=60s
StartLimitBurst=3

[Install]
WantedBy=multi-user.target
EOF
    fi
    
    sudo systemctl daemon-reload 2>/dev/null || true
    echo -e "${GREEN}✓ Systemd service installed${NC}"
    echo ""
    echo -e "${YELLOW}To enable and start Pekobot as a service:${NC}"
    echo "  sudo systemctl enable pekobot@\$USER"
    echo "  sudo systemctl start pekobot@\$USER"
}

# Create directories and config
setup_directories() {
    echo -e "${BLUE}Setting up directories...${NC}"
    
    mkdir -p "$CONFIG_DIR"
    mkdir -p "$DATA_DIR"
    mkdir -p "$DATA_DIR/tools"
    mkdir -p "$DATA_DIR/workspaces"
    
    # Create default config if it doesn't exist
    if [ ! -f "${CONFIG_DIR}/config.toml" ]; then
        cat > "${CONFIG_DIR}/config.toml" <<EOF
# Pekobot Configuration
# Generated by install.sh on $(date -I)

[agent]
name = "default"
provider = "openai"
model = "gpt-4o-mini"

[memory]
type = "sqlite"
path = "${DATA_DIR}/memory.db"

[tools]
registry = "pekohub"
registry_url = "https://tools.coneko.ai"
auto_install = true

[daemon]
enabled = true
poll_interval = 15
EOF
        echo -e "${GREEN}✓ Default config created at ${CONFIG_DIR}/config.toml${NC}"
    fi
}

# Check dependencies
check_dependencies() {
    local deps=("curl" "tar")
    local missing=()
    
    for dep in "${deps[@]}"; do
        if ! command -v "$dep" >/dev/null 2>&1; then
            missing+=("$dep")
        fi
    done
    
    if [ ${#missing[@]} -ne 0 ]; then
        echo -e "${RED}Missing dependencies: ${missing[*]}${NC}"
        echo -e "${YELLOW}Please install them and run again${NC}"
        exit 1
    fi
}

# Print post-install instructions
print_post_install() {
    echo ""
    echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}  Pekobot installed successfully!${NC}"
    echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "${BLUE}Quick Start:${NC}"
    echo ""
    echo "  1. Set your API key:"
    echo "     export OPENAI_API_KEY='your-key-here'"
    echo ""
    echo "  2. Create your first agent:"
    echo "     pekobot agent create my-agent"
    echo ""
    echo "  3. Run the agent:"
    echo "     pekobot agent start my-agent"
    echo ""
    echo "  4. Or run in daemon mode:"
    echo "     pekobot daemon start"
    echo ""
    echo -e "${BLUE}Configuration:${NC}"
    echo "  Config: ${CONFIG_DIR}/config.toml"
    echo "  Data:   ${DATA_DIR}"
    echo ""
    echo -e "${BLUE}Documentation:${NC}"
    echo "  https://github.com/coneko/pekobot#readme"
    echo ""
    echo -e "${YELLOW}Need help? Join our Discord: https://discord.gg/clawd${NC}"
}

# Main installation flow
main() {
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo -e "${BLUE}  Pekobot Installer${NC}"
    echo -e "${BLUE}  github.com/coneko/pekobot${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo ""
    
    # Check if already installed
    if command -v pekobot >/dev/null 2>&1; then
        local current_version
        current_version=$(pekobot --version 2>/dev/null | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' || echo "unknown")
        echo -e "${YELLOW}Pekobot is already installed (v${current_version})${NC}"
        echo ""
        read -p "Reinstall/Update anyway? (y/N) " -n 1 -r
        echo
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            echo "Installation cancelled"
            exit 0
        fi
    fi
    
    check_dependencies
    
    local platform
    platform=$(detect_platform)
    local version
    version=$(get_latest_version)
    
    echo -e "Platform: ${GREEN}${platform}${NC}"
    echo -e "Version:  ${GREEN}v${version}${NC}"
    echo -e "Install:  ${GREEN}${INSTALL_DIR}${NC}"
    echo ""
    
    install_binary "$platform" "$version"
    setup_directories
    install_systemd_service
    
    print_post_install
}

# Handle flags
while [[ $# -gt 0 ]]; do
    case $1 in
        --version)
            VERSION="$2"
            shift 2
            ;;
        --install-dir)
            INSTALL_DIR="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: install.sh [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --version VERSION    Install specific version (default: latest)"
            echo "  --install-dir DIR    Install to custom directory (default: /usr/local/bin)"
            echo "  --help, -h          Show this help"
            echo ""
            echo "Environment:"
            echo "  INSTALL_DIR         Installation directory"
            echo "  CONFIG_DIR          Config directory (default: ~/.config/pekobot)"
            echo "  DATA_DIR            Data directory (default: ~/.local/share/pekobot)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

main
