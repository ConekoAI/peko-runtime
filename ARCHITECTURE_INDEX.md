# Architecture Document Index

> Navigation guide for Pekobot architecture documentation

---

## Current Architecture: v3.1 (Filesystem-First)

**Status:** Minimal requirements, maximum flexibility

### Required vs Optional

| Component | Status |
|-----------|--------|
| `config.toml` | **REQUIRED** |
| `sessions/` | **REQUIRED** (system-managed) |
| `AGENT.md`, `BOOTSTRAP.md`, etc. | Optional (loaded if present) |
| `projects/`, `memories/`, `tools/` | Optional (loaded if present) |

**No `bootstrap/` folder** - markdown files live in agent root.

---

## Document Hierarchy

```
UNIFIED_ARCHITECTURE_SPEC.md v3.1 (Current)
├── Aligned with current implementation:
│   ├── config.toml (not agent.toml)
│   ├── Markdown files in root (not bootstrap/)
│   ├── Optional content loading
│   └── Minimal requirements
│
└── Previous documents (context):
    ├── GRAND_ARCHITECTURE.md (vision)
    ├── TECHNICAL_EXECUTIVE_SPEC.md (components)
    └── AGENT_CONTAINER_SPEC.md (packaging format)
```

---

## Document Reference

### Primary Document

| Document | Version | Status |
|----------|---------|--------|
| **UNIFIED_ARCHITECTURE_SPEC.md** | v3.1 | **Current** |

### Supporting Documents

| Document | Purpose | Status |
|----------|---------|--------|
| **AGENTS.md** | Development guide | v3.1 Updated |
| **ARCHITECTURE_INDEX.md** | This document | v3.1 Updated |
| **GRAND_ARCHITECTURE.md** | Vision | Valid context |
| **TECHNICAL_EXECUTIVE_SPEC.md** | Components | Valid context |
| **AGENT_CONTAINER_SPEC.md** | Package format | Valid for distribution |

---

## Quick Start

```bash
# Create minimal agent
mkdir my-agent
cd my-agent

# Required: config.toml
cat > config.toml << 'TOML'
[agent]
name = "my-agent"
version = "1.0.0"

[provider]
model = "claude-3-5-sonnet"
TOML

# Run immediately
pekobot run ./

# Optional: Add markdown files
cat > AGENT.md << 'MD'
# Agent Description

You are a helpful assistant.
MD

# Optional: Add folders
mkdir projects
echo "# Project docs" > projects/README.md
```

---

## Reading Path

1. **UNIFIED_ARCHITECTURE_SPEC.md** v3.1 - Current architecture
2. **AGENTS.md** - Development guide
3. **GRAND_ARCHITECTURE.md** - Vision and context

---

## Key Changes

| Aspect | Previous | Current v3.1 |
|--------|----------|--------------|
| Config file | `agent.toml` | `config.toml` |
| Prompt location | `bootstrap/*.md` | Root `*.md` |
| Requirements | Layered package | `config.toml` + `sessions/` |
| Markdown files | Mandatory | Optional |
| Content folders | Mandatory | Optional |

---

## Version History

| Document | Version | Date |
|----------|---------|------|
| UNIFIED_ARCHITECTURE_SPEC.md | 3.1 | 2026-03-15 |
| AGENTS.md | 3.1 | 2026-03-15 |
| ARCHITECTURE_INDEX.md | 3.1 | 2026-03-15 |

---

*Required: config.toml. Optional: Everything else.*
