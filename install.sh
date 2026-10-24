#!/bin/bash
#
# Peko Installer - Download from GitHub Releases
# Usage: curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/main/install.sh | bash
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
GITHUB_REPO="ConekoAI/peko-runtime"
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
PEKO_HOME_DIR="${PEKO_HOME:-$HOME/.peko}"

# Detect architecture and OS
detect_platform() {
    local arch=$(uname -m)
    local os=$(uname -s | tr '[:upper:]' '[:lower:]')
    
    case "$arch" in
        x86_64|amd64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
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
    local asset_name="peko-${platform}.tar.gz"
    
    echo -e "${BLUE}Downloading Peko v${version} for ${platform}...${NC}"
    
    # Try GitHub releases first
    local download_url="https://github.com/${GITHUB_REPO}/releases/download/v${version}/${asset_name}"
    
    echo -e "   ${BLUE}Source: ${download_url}${NC}"
    
    if ! curl -fsSL --progress-bar "$download_url" -o "${tmpdir}/peko.tar.gz" 2>/dev/null; then
        echo -e "${YELLOW}Release asset not found, trying alternative...${NC}"
        
        # Try without 'v' prefix
        download_url="https://github.com/${GITHUB_REPO}/releases/download/${version}/${asset_name}"
        
        if ! curl -fsSL --progress-bar "$download_url" -o "${tmpdir}/peko.tar.gz" 2>/dev/null; then
            echo -e "${RED}Failed to download binary${NC}"
            echo -e "${YELLOW}Please check that the release exists:${NC}"
            echo -e "   ${BLUE}https://github.com/${GITHUB_REPO}/releases${NC}"
            rm -rf "$tmpdir"
            exit 1
        fi
    fi
    
    echo -e "${BLUE}Extracting...${NC}"
    tar -xzf "${tmpdir}/peko.tar.gz" -C "$tmpdir" 2>/dev/null || {
        echo -e "${RED}Failed to extract archive${NC}"
        rm -rf "$tmpdir"
        exit 1
    }
    
    # The archive holds two binaries. `peko` starts the daemon from the
    # `peko-daemon` beside it, so both must land in the same directory.
    local bin
    for bin in peko peko-daemon; do
        if [ ! -f "${tmpdir}/${bin}" ]; then
            echo -e "${RED}Could not find ${bin} in the archive${NC}"
            rm -rf "$tmpdir"
            exit 1
        fi
        chmod +x "${tmpdir}/${bin}"
    done
    
    echo -e "${BLUE}Installing to ${INSTALL_DIR}...${NC}"
    if [ -w "$INSTALL_DIR" ]; then
        mv "${tmpdir}/peko" "${tmpdir}/peko-daemon" "${INSTALL_DIR}/"
    else
        echo -e "${YELLOW}Requesting sudo access to install to ${INSTALL_DIR}${NC}"
        sudo mv "${tmpdir}/peko" "${tmpdir}/peko-daemon" "${INSTALL_DIR}/"
    fi
    
    rm -rf "$tmpdir"
    echo -e "${GREEN}✓ Peko v${version} installed to ${INSTALL_DIR}/peko (and peko-daemon)${NC}"
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
    
    # A template unit: `peko@<user>` runs the daemon as that user. The
    # daemon stays in the foreground so systemd supervises the process
    # itself rather than a launcher that exits at once.
    local unit
    unit=$(cat <<EOF
[Unit]
Description=Peko Agent Runtime (%i)
Documentation=https://github.com/ConekoAI/peko-runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=%i
ExecStart=${INSTALL_DIR}/peko daemon start --foreground
ExecStop=${INSTALL_DIR}/peko daemon stop
Restart=always
RestartSec=10
StartLimitInterval=60s
StartLimitBurst=3

[Install]
WantedBy=multi-user.target
EOF
)
    if [ -w "/etc/systemd/system" ]; then
        printf '%s\n' "$unit" > /etc/systemd/system/peko@.service
    else
        echo -e "${YELLOW}Requesting sudo access to install systemd service${NC}"
        printf '%s\n' "$unit" | sudo tee /etc/systemd/system/peko@.service > /dev/null
    fi
    
    sudo systemctl daemon-reload 2>/dev/null || true
    echo -e "${GREEN}✓ Systemd service installed${NC}"
    echo ""
    echo -e "${YELLOW}To enable and start Peko as a service:${NC}"
    echo "  sudo systemctl enable peko@\$USER"
    echo "  sudo systemctl start peko@\$USER"
}

# Create the runtime home (models catalog, vault, pekos live here)
setup_directories() {
    echo -e "${BLUE}Setting up directories...${NC}"
    mkdir -p "$PEKO_HOME_DIR"
    echo -e "${GREEN}✓ Runtime home: ${PEKO_HOME_DIR}${NC}"
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
    echo -e "${GREEN}  Peko installed successfully!${NC}"
    echo -e "${GREEN}═══════════════════════════════════════════════════${NC}"
    echo ""
    echo -e "${BLUE}Quick Start:${NC}"
    echo ""
    echo "  1. Add a model to the catalog and store its API key in the vault:"
    echo "     peko model add --id my-model --api-format <format> \\"
    echo "         --base-url <endpoint-url> --model <wire-model-id> --key \"<your-api-key>\""
    echo "     (<format>: anthropic_messages | openai_completions | openai_responses;"
    echo "      use --no-key for local endpoints — see \`peko model add --help\`)"
    echo ""
    echo "  2. Start the daemon:"
    echo "     peko daemon start"
    echo ""
    echo "  3. Create your first peko and talk to it:"
    echo "     peko create my-peko --model my-model"
    echo "     peko send my-peko \"Hello\""
    echo "     peko log my-peko"
    echo ""
    echo -e "${BLUE}Configuration:${NC}"
    echo "  Runtime home: ${PEKO_HOME_DIR} (models.toml, vault, pekos)"
    echo "  Optional tuning: see config.example.toml in the repo"
    echo ""
    if ! command -v python3 >/dev/null 2>&1; then
        echo -e "${YELLOW}Note:${NC} python3 is not on PATH. The Workflow tool needs it;"
        echo "  everything else works without it."
        echo ""
    fi
    echo -e "${BLUE}Documentation:${NC}"
    echo "  https://github.com/ConekoAI/peko-runtime#readme"
    echo ""
}

# Main installation flow
main() {
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo -e "${BLUE}  Peko Installer${NC}"
    echo -e "${BLUE}  github.com/ConekoAI/peko-runtime${NC}"
    echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
    echo ""
    
    # Check if already installed
    if command -v peko >/dev/null 2>&1; then
        local current_version
        current_version=$(peko --version 2>/dev/null | grep -o '[0-9]\+\.[0-9]\+\.[0-9]\+' || echo "unknown")
        echo -e "${YELLOW}Peko is already installed (v${current_version})${NC}"
        echo ""
        # Under `curl ... | bash`, stdin is this script, so ask on the
        # terminal. With no terminal at all (CI, containers), go ahead.
        if [ -t 0 ]; then
            read -p "Reinstall/Update anyway? (y/N) " -n 1 -r
            echo
        elif { : </dev/tty; } 2>/dev/null; then
            read -p "Reinstall/Update anyway? (y/N) " -n 1 -r </dev/tty
            echo
        else
            echo "No terminal to ask on; reinstalling."
            REPLY=y
        fi
        if [[ ! $REPLY =~ ^[Yy]$ ]]; then
            echo "Installation cancelled"
            exit 0
        fi
    fi
    
    check_dependencies
    
    local platform
    platform=$(detect_platform)
    local version
    version="${VERSION:-$(get_latest_version)}"
    
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
            echo "  PEKO_HOME           Runtime home (default: ~/.peko)"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

main
