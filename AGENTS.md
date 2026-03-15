# AGENTS.md - AI Agent Guide for Pekobot

> Quick reference for AI agents working on the Pekobot codebase

---

## Project Overview

**Pekobot** is a filesystem-first runtime for AI agents. Agents are directories with minimal required configuration.

**Key Principle:**
- **Required**: `config.toml` and `sessions/`
- **Optional**: Everything else (loaded if present)

---

## Agent Directory Structure

### Minimal Agent (Just 2 Required Things)

```
my-agent/
├── config.toml              # REQUIRED
└── sessions/                # REQUIRED (system-managed)
```

### Typical Agent

```
my-agent/
├── config.toml              # REQUIRED
├── AGENT.md                 # Optional: Behavior description
├── BOOTSTRAP.md             # Optional: System prompt
├── IDENTITY.md              # Optional: Identity/personality
├── SOUL.md                  # Optional: Core values
├── projects/                # Optional: Knowledge/workspaces
├── memories/                # Optional: Long-term memory
├── tools/                   # Optional: Custom tools
└── sessions/                # REQUIRED (gitignore this)
```

---

## Required Files

### config.toml

```toml
[agent]
name = "my-agent"
version = "1.0.0"

[provider]
provider_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
```

---

## Optional Markdown Files

All markdown files are **optional** - runtime loads them if present:

| File | Purpose |
|------|---------|
| `AGENT.md` | Agent behavior, responsibilities |
| `BOOTSTRAP.md` | Initial system prompt |
| `IDENTITY.md` | Identity, name, personality |
| `SOUL.md` | Core values, traits |
| `TOOLS.md` | Tool usage guidelines |
| `SKILLS.md` | Available skills |

**No `bootstrap/` folder** - markdown files go in the agent root.

---

## Common Tasks

### Create Minimal Agent

```bash
mkdir my-agent
cd my-agent

# Create config.toml
cat > config.toml << 'TOML'
[agent]
name = "my-agent"
version = "1.0.0"

[provider]
model = "claude-3-5-sonnet"
TOML

# Run immediately
pekobot run ./
```

### Add Personality

```bash
# Optional: Add identity
cat > IDENTITY.md << 'MD'
# Identity

Name: Archie
Role: Technical Assistant
Personality: Curious, thorough
MD
```

### Add Knowledge

```bash
# Optional: Add projects
mkdir projects
cat > projects/README.md << 'MD'
# Project Documentation

This agent has access to project knowledge.
MD
```

### Add Custom Tool

```bash
# Optional: Add tool
mkdir tools
cat > tools/my_search.py << 'PY'
#!/usr/bin/env python3
import sys
print(f"Searching for: {sys.argv[1]}")
PY
chmod +x tools/my_search.py
```

### Package for Distribution

```bash
# Only when sharing
pekobot package build ./ -t my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0
```

---

## Filesystem Loading (Code)

```rust
use pekobot::infrastructure::filesystem::FilesystemLoader;

// Load agent directory
let agent = FilesystemLoader::load("./my-agent/").await?;

// config.toml is required
// Everything else is optional
```

---

## Key Implementation Files

| Component | Path |
|-----------|------|
| Filesystem Loader | `src/infrastructure/filesystem/loader.rs` |
| Config Parser | `src/config/` |
| Agent Runtime | `src/agent/agentic_loop_v4.rs` |
| Package Builder | `src/infrastructure/package/builder.rs` |

---

## CLI Quick Reference

```bash
# Run from filesystem
pekobot run ./my-agent/
pekobot run ./my-agent/ --watch

# Package commands
pekobot package build ./my-agent/ -t my-agent:v1.0
pekobot package inspect my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0

# Team commands
pekobot team deploy -f team.yaml
```

---

## Rules

| Rule | Reason |
|------|--------|
| Only `config.toml` + `sessions/` required | Minimal barrier to entry |
| Markdown files in root (no `bootstrap/`) | Flat, simple structure |
| Load files dynamically | Flexible, extensible |
| Run from filesystem | No build step for dev |
| Package only for distribution | Optimization, not requirement |

---

*Required: config.toml. Optional: Everything else.*
