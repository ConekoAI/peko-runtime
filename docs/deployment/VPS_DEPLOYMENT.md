# VPS Deployment Guide

Deploy Pekobot to a VPS for 24/7 operation.

## Quick Start

```bash
# One-line installer from GitHub
curl -fsSL https://raw.githubusercontent.com/coneko/pekobot/main/install.sh | bash

# Or with custom options
curl -fsSL https://raw.githubusercontent.com/coneko/pekobot/main/install.sh | bash -s -- --install-dir /opt/pekobot
```

## Manual Installation

### 1. System Requirements

- **OS**: Ubuntu 22.04+ / Debian 12+ / CentOS 9+
- **RAM**: 512MB minimum (1GB recommended)
- **Disk**: 100MB for Pekobot + space for tools/data
- **Network**: Outbound HTTPS (443) required

### 2. Download Binary

```bash
# Get latest release version
VERSION=$(curl -fsSL https://api.github.com/repos/coneko/pekobot/releases/latest | grep '"tag_name":' | cut -d'"' -f4 | sed 's/^v//')

# Download for your platform
PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m | sed 's/x86_64/x86_64/;s/aarch64/aarch64/')
curl -fsSL -o pekobot.tar.gz \
  "https://github.com/coneko/pekobot/releases/download/v${VERSION}/pekobot-${PLATFORM}.tar.gz"

# Extract
tar -xzf pekobot.tar.gz
sudo mv pekobot /usr/local/bin/
sudo chmod +x /usr/local/bin/pekobot
```

### 3. Create User (Recommended)

```bash
# Create dedicated user
sudo useradd -r -s /bin/false pekobot
sudo mkdir -p /home/pekobot
sudo chown pekobot:pekobot /home/pekobot
```

### 4. Set Up Directories

```bash
# Create directories
sudo mkdir -p /etc/pekobot
sudo mkdir -p /var/lib/pekobot/{tools,workspaces}
sudo mkdir -p /var/log/pekobot

# Set ownership
sudo chown -R pekobot:pekobot /var/lib/pekobot
sudo chown -R pekobot:pekobot /var/log/pekobot
```

### 5. Configure Pekobot

```bash
# Create config
sudo tee /etc/pekobot/config.toml > /dev/null <<EOF
[agent]
default_provider = "minimax"
default_model = "minimax-text-01"

[memory]
type = "sqlite"
path = "/var/lib/pekobot/memory.db"

[tools]
registry = "pekohub"
registry_url = "https://tools.coneko.ai"
auto_install = true

[daemon]
enabled = true
poll_interval = 15

[logging]
level = "info"
format = "json"
EOF

sudo chown pekobot:pekobot /etc/pekobot/config.toml
```

### 6. Set API Keys

```bash
# Add to /etc/pekobot/environment
sudo tee /etc/pekobot/environment > /dev/null <<EOF
OPENAI_API_KEY=your-key-here
# Add other providers as needed
# ANTHROPIC_API_KEY=...
# KIMI_API_KEY=...
EOF

sudo chmod 600 /etc/pekobot/environment
sudo chown pekobot:pekobot /etc/pekobot/environment
```

### 7. Install Systemd Service

```bash
# Create service file
sudo tee /etc/systemd/system/pekobot.service > /dev/null <<EOF
[Unit]
Description=Pekobot Agent Runtime
Documentation=https://docs.coneko.ai/pekobot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=pekobot
Group=pekobot
EnvironmentFile=/etc/pekobot/environment
ExecStart=/usr/local/bin/pekobot daemon start --config /etc/pekobot/config.toml
ExecStop=/usr/local/bin/pekobot daemon stop
ExecReload=/usr/local/bin/pekobot daemon restart
Restart=always
RestartSec=10
StartLimitInterval=60s
StartLimitBurst=3

# Security
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/pekobot /var/log/pekobot
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true

# Resource limits
LimitNOFILE=65535
MemoryMax=512M

[Install]
WantedBy=multi-user.target
EOF

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable pekobot
sudo systemctl start pekobot
```

### 8. Verify Installation

```bash
# Check status
sudo systemctl status pekobot

# View logs
sudo journalctl -u pekobot -f

# Test CLI
pekobot --version
pekobot agent list
pekobot team list
```

## Update Pekobot

### Automatic Update

```bash
# Check for updates
pekobot update --check

# Update to latest (if available)
pekobot update
```

### Manual Update

```bash
# Get latest version
VERSION=$(curl -fsSL https://api.github.com/repos/coneko/pekobot/releases/latest | grep '"tag_name":' | cut -d'"' -f4 | sed 's/^v//')
PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m | sed 's/x86_64/x86_64/;s/aarch64/aarch64/')

# Download new version
curl -fsSL -o pekobot.tar.gz \
  "https://github.com/coneko/pekobot/releases/download/v${VERSION}/pekobot-${PLATFORM}.tar.gz"

# Stop service
sudo systemctl stop pekobot

# Replace binary
tar -xzf pekobot.tar.gz
sudo mv pekobot /usr/local/bin/pekobot
sudo chmod +x /usr/local/bin/pekobot

# Start service
sudo systemctl start pekobot

# Verify
pekobot --version
```

## Firewall Configuration

If using gateway features:

```bash
# Allow gateway port (default: 18789)
sudo ufw allow 18789/tcp

# Or restrict to specific IP
sudo ufw allow from 192.168.1.0/24 to any port 18789
```

## Reverse Proxy (Nginx)

```nginx
server {
    listen 443 ssl http2;
    server_name pekobot.yourdomain.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://localhost:18789;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

## Monitoring

### Health Check

```bash
# Create health check script
sudo tee /usr/local/bin/pekobot-health.sh > /dev/null <<'EOF'
#!/bin/bash
if ! pekobot daemon status | grep -q "running"; then
    echo "Pekobot is not running"
    exit 1
fi
exit 0
EOF

sudo chmod +x /usr/local/bin/pekobot-health.sh
```

### Log Rotation

```bash
sudo tee /etc/logrotate.d/pekobot > /dev/null <<EOF
/var/log/pekobot/*.log {
    daily
    missingok
    rotate 14
    compress
    delaycompress
    notifempty
    create 0640 pekobot pekobot
    sharedscripts
    postrotate
        systemctl reload pekobot
    endscript
}
EOF
```

## Troubleshooting

### Service Won't Start

```bash
# Check logs
sudo journalctl -u pekobot -n 50

# Check config
pekobot config validate ./pekobot.toml

# Test with debug logging
sudo systemctl stop pekobot
sudo -u pekobot pekobot daemon start --verbose
```

### Permission Issues

```bash
# Fix ownership
sudo chown -R pekobot:pekobot /var/lib/pekobot
sudo chown -R pekobot:pekobot /var/log/pekobot
sudo chmod 600 /etc/pekobot/environment
```

### High Memory Usage

```bash
# Check memory usage
sudo systemctl show pekobot --property=MemoryCurrent

# Adjust limits in service file
sudo systemctl edit pekobot
# Add: [Service]
#      MemoryMax=256M
```

## Uninstall

```bash
# Stop and disable service
sudo systemctl stop pekobot
sudo systemctl disable pekobot

# Remove files
sudo rm -f /etc/systemd/system/pekobot.service
sudo rm -f /usr/local/bin/pekobot
sudo rm -rf /etc/pekobot
sudo rm -rf /var/lib/pekobot
sudo rm -rf /var/log/pekobot

# Remove user (optional)
sudo userdel pekobot

# Reload systemd
sudo systemctl daemon-reload
```

## Docker Deployment (Alternative)

See [Docker deployment guide](../install/docker.md) for containerized deployment.
