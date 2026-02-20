# Pekobot Tool Registry Configuration

Pekobot supports multiple tool registry backends for maximum flexibility. Choose the setup that works best for your needs.

## Quick Start

### Option 1: Use Public Pekohub (Easiest)
```toml
# ~/.config/pekobot/config.toml
[tools]
# Tools are downloaded from https://tools.coneko.ai
# No configuration needed — this is the default!
```

### Option 2: Self-Hosted Pekohub (Private/Enterprise)
```toml
[tools.registry]
type = "pekohub"
url = "https://tools.mycompany.com"
# api_key = "optional-for-private-registries"

[tools]
core = ["http", "filesystem", "cron", "memory"]
on_demand = ["social_media", "calendar", "email"]
```

### Option 3: Offline Mode (Air-Gapped)
```toml
[tools.registry]
type = "local"
path = "/opt/pekobot/tools"

[tools]
# All tools must be pre-installed in /opt/pekobot/tools/
core = ["http", "filesystem", "cron", "memory", "calendar", "email"]
```

### Option 4: Build from Source (Developer)
```toml
[tools.registry]
type = "source"
source_path = "~/pekohub/tools"
build_cache = "~/.cache/pekobot/tool-builds"

[tools]
# Tools are built from source on first use
on_demand = ["social_media", "calendar", "email"]
```

### Option 5: Multiple Registries (Fallback)
```toml
[tools.registry]
primary = { type = "pekohub", url = "https://tools.coneko.ai" }
fallbacks = [
    { type = "pekohub", url = "https://tools.mycompany.com" },
    { type = "local", path = "~/.local/share/pekobot/tools" },
]
```

---

## Registry Types

### `pekohub` — HTTP API Registry

Connects to a Pekohub instance (ours, yours, or anyone's).

```toml
[tools.registry]
type = "pekohub"
url = "https://tools.coneko.ai"
# Optional: API key for private registries
# api_key = "${secret:PEKOHUB_API_KEY}"
```

**Features:**
- ✅ Anonymous downloads (public tools)
- ✅ Authenticated access (private tools)
- ✅ Automatic platform detection
- ✅ Signature verification
- ✅ Local caching

**Self-Host Your Own:**
```bash
# Deploy your own Pekohub
git clone https://github.com/coneko/pekohub
cd pekohub
wrangler deploy
# Your tools available at https://your-domain.com
```

---

### `local` — Filesystem Registry

Tools stored locally on disk. No network required.

```toml
[tools.registry]
type = "local"
path = "/path/to/tools"
```

**Directory Structure:**
```
/path/to/tools/
├── social_media/
│   ├── 1.0.0/
│   │   ├── social_media-linux-x64
│   │   └── social_media-macos-arm64
│   └── latest/ -> 1.0.0/
├── calendar/
│   └── 2.1.0/
│       └── calendar-linux-x64
```

**Use Cases:**
- Air-gapped environments
- Pre-approved tools only
- Custom internal tools

---

### `source` — Build from Source

Tools are compiled from source on first use.

```toml
[tools.registry]
type = "source"
source_path = "~/src/pekohub-tools"
build_cache = "~/.cache/pekobot/tool-builds"
```

**Requirements:**
- Rust toolchain installed
- Tool source code available

**Example:**
```bash
# Clone tool sources
git clone https://github.com/coneko/pekohub-tools ~/src/pekohub-tools

# First use triggers build
pekobot agent use-tool my_agent social_media
# → Building social_media from source...
# → Compiling...
# → Tool ready!
```

**Pros:**
- Full transparency (read the source)
- Custom modifications
- No binary trust issues

**Cons:**
- Requires Rust toolchain
- Slower first use (compile time)
- Higher resource usage

---

### `embedded` — Agent Package

Tools bundled with a portable agent package.

```toml
# This is automatic when importing .agent files
[agent]
package = "my_agent.agent"
# Tools extracted from package on import
```

**No configuration needed** — handled automatically during `pekobot import`.

---

## Advanced: Multi-Registry with Fallback

Try multiple registries in order:

```toml
[tools.registry]
# Primary: Your company's private registry
primary = { type = "pekohub", url = "https://tools.mycompany.com", api_key = "${secret:COMPANY_TOKEN}" }

# Fallback 1: Public Pekohub (for common tools)
[[tools.registry.fallbacks]]
type = "pekohub"
url = "https://tools.coneko.ai"

# Fallback 2: Local filesystem (last resort)
[[tools.registry.fallbacks]]
type = "local"
path = "~/.local/share/pekobot/tools"

# Fallback 3: Build from source
tools.registry.allow_source_builds = true
```

**Search Order:**
1. Local cache (fastest)
2. Primary registry
3. Fallback registries (in order)
4. Source build (if enabled)

---

## Environment-Specific Configs

### Development
```toml
# ~/.config/pekobot/config.dev.toml
[tools.registry]
type = "source"
source_path = "~/dev/pekohub/tools"
allow_source_builds = true
```

### Production (Air-Gapped)
```toml
# /etc/pekobot/config.toml
[tools.registry]
type = "local"
path = "/opt/pekobot/tools"

[tools.registry.fallbacks]
# Empty — no external access
```

### CI/CD
```toml
# .pekobot.toml (in repo)
[tools.registry]
primary = { type = "pekohub", url = "https://tools.mycompany.com" }
allow_source_builds = false  # Faster builds
```

---

## Tool Installation Methods

### Method 1: Download from Registry (Default)
```bash
# Automatically downloaded on first use
pekobot agent use-tool my_agent social_media
```

### Method 2: Manual Install to Local Registry
```bash
# Download binary manually
wget https://example.com/tools/social_media-linux-x64
chmod +x social_media-linux-x64

# Install to local registry
mkdir -p ~/.local/share/pekobot/tools/social_media/1.0.0
cp social_media-linux-x64 ~/.local/share/pekobot/tools/social_media/1.0.0/

# Update config to use local registry
pekobot config set tools.registry.type local
pekobot config set tools.registry.path ~/.local/share/pekobot/tools
```

### Method 3: Build from Source
```bash
# Clone source
git clone https://github.com/coneko/pekohub-tools
cd pekohub-tools/social_media

# Build
cargo build --release

# Install to local registry
mkdir -p ~/.local/share/pekobot/tools/social_media/1.0.0
cp target/release/social_media ~/.local/share/pekobot/tools/social_media/1.0.0/social_media-linux-x64
```

---

## Security Considerations

### Anonymous Downloads (Public Registries)
- ✅ Anyone can download public tools
- ✅ No API keys needed
- ⚠️ Verify tool signatures!

```toml
[tools.security]
verify_signatures = true
trusted_publishers = ["coneko", "mycompany"]
```

### Private Registries
```toml
[tools.registry]
type = "pekohub"
url = "https://tools.mycompany.com"
api_key = "${secret:PEKOHUB_API_KEY}"  # Stored in Secret Manager
```

### Local-Only (Maximum Security)
```toml
[tools.registry]
type = "local"
path = "/opt/pekobot/tools"

[tools]
# Only pre-approved, audited tools
allowed_tools = ["calendar", "email"]
```

---

## Troubleshooting

### "Tool not found in any registry"
```bash
# Check registry connectivity
pekobot registry ping

# List available tools
pekobot registry list

# Try specific registry
pekobot registry list --registry https://tools.coneko.ai
```

### "Build failed" (Source mode)
```bash
# Check Rust installation
rustc --version
cargo --version

# Check tool source exists
ls ~/.local/share/pekobot/tool-sources/social_media
```

### Cache issues
```bash
# Clear tool cache
rm -rf ~/.cache/pekobot/tools

# Force re-download
pekobot tool install social_media --force
```

---

## Migration Scenarios

### From Bundled to Pekohub
```bash
# 1. Update config to use Pekohub
pekobot config set tools.registry.type pekohub
pekobot config set tools.registry.url https://tools.coneko.ai

# 2. Remove bundled tools (optional)
# Edit Cargo.toml, remove tool dependencies
# Rebuild: cargo build --release
```

### From Cloud to Self-Hosted
```bash
# 1. Deploy your own Pekohub
git clone https://github.com/coneko/pekohub
cd pekohub && wrangler deploy

# 2. Copy tools from public registry
curl https://tools.coneko.ai/api/v1/tools/social_media/1.0.0/binary \
  -o social_media-linux-x64
# Upload to your instance...

# 3. Update config
pekobot config set tools.registry.url https://your-domain.com
```

### From Online to Offline
```bash
# 1. Download all tools you'll need
for tool in social_media calendar email; do
  curl -O https://tools.coneko.ai/api/v1/tools/$tool/1.0.0/binary?platform=linux-x64
done

# 2. Install to local registry
mkdir -p /opt/pekobot/tools
# Copy binaries...

# 3. Switch to offline mode
pekobot config set tools.registry.type local
pekobot config set tools.registry.path /opt/pekobot/tools
```

---

## Summary

| Use Case | Registry Type | Setup Complexity |
|----------|---------------|------------------|
| Quick start | `pekohub` (public) | None |
| Enterprise | `pekohub` (self-hosted) | Low |
| Air-gapped | `local` | Medium |
| Developer | `source` | Medium |
| Maximum flexibility | Multi-registry | High |

**No lock-in!** Switch between registry types anytime. Your agents work the same regardless of where tools come from.
