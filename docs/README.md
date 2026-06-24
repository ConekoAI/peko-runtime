# Peko Documentation

**Last Updated:** 2026-06-23
**Version:** 0.1.0

Complete documentation for the Peko multi-agent runtime.

---

## Quick Navigation

### 🚀 Getting Started

- **[Getting Started Guide](getting-started/GETTING_STARTED.md)** — Build and run your first agent
- **[Tutorial: Building Your First Agent](getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md)** — Step-by-step walkthrough

### 📚 User Guides

- **[User's Guide](user-guide/USERS_GUIDE.md)** — Concepts, sessions, teams, extensions
- **[CLI Reference](user-guide/CLI_REFERENCE.md)** — Every `peko` command and flag

### 🏗️ Architecture

- **[Extension System](architecture/EXTENSION_SYSTEM.md)** — Unified extension architecture
- **[Architecture Decision Records](architecture/adr/)** — ADR-001 through ADR-039
- **[Public API Surface](../API_SURFACE.md)** — Rust public API contracts
- **[Data Model](../DATA_MODEL.md)** — On-disk and in-memory data formats
- **[Changelog](../CHANGELOG.md)** — Version history
- **[Agent Guide](../AGENTS.md)** — Build instructions, code style, CI tiers

### 🔌 MCP

- **[MCP Overview](mcp/MCP.md)** — Model Context Protocol integration
- **[MCP Quick Start](mcp/QUICK_START.md)** — Install and enable an MCP server
- **[MCP Migration Guide](mcp/MIGRATION_GUIDE.md)** — Moving tools to MCP
- **[Reserved Parameters Guide](mcp/mcp_reserved_params_guide.md)** — Runtime context injection
- **[Reserved Parameters Proposal](mcp/mcp_reserved_params_proposal.md)** — Design proposal
- **[Universal vs MCP Comparison](mcp/universal_vs_mcp_comparison.md)** — Protocol tradeoffs

---

## Repository Layout (docs only)

```
docs/
├── README.md                        # This file
├── getting-started/
│   ├── GETTING_STARTED.md
│   └── TUTORIAL_BUILDING_FIRST_AGENT.md
├── user-guide/
│   ├── USERS_GUIDE.md
│   └── CLI_REFERENCE.md
├── architecture/
│   ├── EXTENSION_SYSTEM.md
│   └── adr/                         # ADR-001 through ADR-039
└── mcp/
    ├── MCP.md
    ├── QUICK_START.md
    ├── MIGRATION_GUIDE.md
    ├── mcp_reserved_params_guide.md
    ├── mcp_reserved_params_proposal.md
    └── universal_vs_mcp_comparison.md
```

For top-level project docs, see [`../README.md`](../README.md).

---

## ADR Index

| Range | Topic |
|-------|-------|
| [ADR-001](../) — ADR-015 | Early architecture (JSONL sessions, did, providers, tools) |
| [ADR-016](architecture/adr/ADR-016.md) | Unified session resolution + async completion *(Proposed — never accepted)* |
| [ADR-017](architecture/adr/ADR-017.md) | Unified Extension Architecture |
| [ADR-018a](architecture/adr/ADR-018a-tool-execution-unification.md) / [018b](architecture/adr/ADR-018b-unified-tool-registry.md) / [018c](architecture/adr/ADR-018c-tool-naming-cleanup.md) | Tool execution unification |
| [ADR-019](architecture/adr/ADR-019-dynamic-tool-and-prompt-updates.md) | Dynamic tool / prompt updates |
| [ADR-020](architecture/adr/ADR-020-daemon-based-async-execution.md) | Daemon-based async execution |
| [ADR-021](architecture/adr/ADR-021-daemon-as-central-runtime.md) | Daemon as central runtime |
| [ADR-022](architecture/adr/ADR-022-session-compaction.md) | Session compaction |
| [ADR-023](architecture/adr/ADR-023-minimal-a2a-messaging.md) | Minimal A2A messaging |
| [ADR-024](architecture/adr/ADR-024-unified-extension-manifest.md) | Unified extension manifest |
| [ADR-025](architecture/adr/ADR-025-gateway-extension.md) | Gateway extension |
| [ADR-026](architecture/adr/ADR-026-extension-lifecycle-separation.md) | Extension lifecycle separation |
| [ADR-027](architecture/adr/ADR-027-unified-packaging.md) | Unified packaging (`.agent`/`.team`/`.ext`) |
| [ADR-028](architecture/adr/ADR-028-top-level-config-cli.md) | Top-level config CLI |
| [ADR-029](architecture/adr/ADR-029-cli-registry-defaults.md) | CLI registry defaults |
| [ADR-030](architecture/adr/ADR-030-hybrid-ipc-migration-path.md) | Hybrid IPC migration path |
| [ADR-031](architecture/adr/ADR-031-agent-team-membership.md) | Agent/team membership |
| [ADR-032](architecture/adr/ADR-032-runtime-identity-and-multi-host-awareness.md) | Runtime identity & multi-host awareness |
| [ADR-033](architecture/adr/ADR-033-ownership-and-permission-model.md) | Ownership & permission model |
| [ADR-034](architecture/adr/ADR-034-runtime-authentication-and-authorization.md) | Runtime auth/authz |
| [ADR-035](architecture/adr/ADR-035-runtime-pekohub-tunnel-protocol.md) | Runtime ↔ Pekohub tunnel protocol |
| [ADR-036](architecture/adr/ADR-036-extension-developer-experience.md) | `peko ext init` and semantic validation |
| [ADR-037](architecture/adr/ADR-037-agent-extension-bundling-and-layer-rationalization.md) | Agent-extension bundling |
| [ADR-038](architecture/adr/ADR-038-named-pipes-on-windows.md) | Windows named-pipe transport |
| [ADR-039](architecture/adr/ADR-039-principal-model.md) | Principal model (User/Agent/Team/Public) |

---

## Contributing to Documentation

- Keep user-facing docs aligned with the actual `peko` CLI — run `peko <cmd> --help` and copy-paste real output.
- ADR numbers are permanent; once an ADR is merged its file name and number do not change. If a later ADR supersedes it, link forward in the new ADR rather than renaming.
- When you delete or rename code, search the docs for the old name in the same commit and update them.