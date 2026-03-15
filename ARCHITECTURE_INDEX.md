# Architecture Document Index

> Navigation guide for Pekobot architecture documentation

---

## Document Overview

### Required Reading Order

1. **REQUIREMENTS_SPEC.md** - What we're building
2. **UNIFIED_ARCHITECTURE_SPEC.md** - How we're building it
3. **AGENTS.md** - Quick reference for development

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
REQUIREMENTS_SPEC.md
        │
        ▼ (informs)
UNIFIED_ARCHITECTURE_SPEC.md v3.1 (Current)
        │
        ├── Aligned with current implementation:
        │   ├── config.toml (not agent.toml)
        │   ├── Markdown files in root (not bootstrap/)
        │   ├── Optional content loading
        │   └── Minimal requirements
        │
        └── Previous documents (context/historical):
            ├── GRAND_ARCHITECTURE.md [DEPRECATED]
            ├── TECHNICAL_EXECUTIVE_SPEC.md [DEPRECATED]
            └── ARCHITECTURE_ALIGNMENT_REPORT.md [DEPRECATED]
```

---

## Document Reference

### Primary Documents

| Document | Version | Purpose | Status |
|----------|---------|---------|--------|
| **REQUIREMENTS_SPEC.md** | 1.0 | Business → Technical requirements | Current |
| **UNIFIED_ARCHITECTURE_SPEC.md** | 3.1 | Technical architecture | Current |
| **AGENTS.md** | 3.1 | Developer quick reference | Current |

### Navigation

| Document | Version | Purpose | Status |
|----------|---------|---------|--------|
| **ARCHITECTURE_INDEX.md** | 3.1 | This document | Current |

### Historical Context (Deprecated)

| Document | Purpose | Status |
|----------|---------|--------|
| **GRAND_ARCHITECTURE.md** | Original vision | Deprecated |
| **TECHNICAL_EXECUTIVE_SPEC.md** | Original tech spec | Deprecated |
| **ARCHITECTURE_ALIGNMENT_REPORT.md** | Implementation analysis | Deprecated |
| **EXECUTIVE_SUMMARY.md** | Business summary | Superseded by REQUIREMENTS_SPEC.md |

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

## Reading Paths

### For Understanding the Project

1. **REQUIREMENTS_SPEC.md** - Understand what we're building
2. **UNIFIED_ARCHITECTURE_SPEC.md** - Understand technical approach
3. **AGENTS.md** - Get started with development

### For Development Work

1. **AGENTS.md** - Quick reference
2. **UNIFIED_ARCHITECTURE_SPEC.md** - Architecture details
3. **REQUIREMENTS_SPEC.md** - Success criteria and constraints

### For Architecture Decisions

1. **REQUIREMENTS_SPEC.md** - Constraints and requirements
2. **UNIFIED_ARCHITECTURE_SPEC.md** - Authoritative guidance

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
| REQUIREMENTS_SPEC.md | 1.0 | 2026-03-15 |
| UNIFIED_ARCHITECTURE_SPEC.md | 3.1 | 2026-03-15 |
| AGENTS.md | 3.1 | 2026-03-15 |
| ARCHITECTURE_INDEX.md | 3.1 | 2026-03-15 |
| GRAND_ARCHITECTURE.md | 2.0 | 2026-03-13 | [DEPRECATED] |
| TECHNICAL_EXECUTIVE_SPEC.md | 1.0 | 2026-03-13 | [DEPRECATED] |
| ARCHITECTURE_ALIGNMENT_REPORT.md | 1.0 | 2026-03-12 | [DEPRECATED] |

---

*Required: config.toml. Optional: Everything else.*
