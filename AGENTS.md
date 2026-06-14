# Pekobot — Agent Guide

> **Project:** Pekobot  
> **Version:** 0.1.0 (source of truth: `Cargo.toml`)  
> **Language:** Rust (Edition 2021)  
> **License:** MIT

---

## Project Overview

Pekobot is a Rust-based multi-agent runtime with a unified extension architecture. It provides:

- **Agent runtime** with turn-based agentic loops (LLM → tool execution → respond)
- **HTTP API daemon** (default `localhost:11435`) with SSE streaming and WebSocket support
- **Session management** via durable JSONL files with atomic writes
- **Built-in tools** (filesystem, process, apply_patch, agent_spawn, cron, etc.)
- **Team runtime** with A2A (agent-to-agent) messaging over an event bus
- **Extension system** with 22 hook points for tools, skills, MCP servers, channels, and gateways
- **Packaging** — `.agent` build/export/import, `.team` export/import, `.ext` export, registry push/pull with content-addressable storage

---

## Build Instructions

```bash
# Standard debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run a specific test
cargo test <test_name>

# Run with logging
RUST_LOG=debug cargo run -- <args>
```

---

## Code Style

The project uses `clippy` and `rustfmt` with a relaxed configuration (see `clippy.toml`):

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy

# Run clippy with all features
cargo clippy --all-features
```

Key style notes from `clippy.toml`:
- `cognitive-complexity-threshold = 30`
- `allow-unwrap-in-tests = true`
- `avoid-breaking-exported-api = false`

---

## Architecture Overview

```
src/
├── agent/          # Agent management (stateless manager, config, lifecycle)
├── commands/       # CLI command implementations
├── common/         # Shared services (AgentService, AgentConfigService, etc.)
├── compaction/     # Session compaction / context summarization
├── cron/           # Cron job scheduling and persistence
├── daemon/         # HTTP daemon (Axum-based), health, info endpoints
│   └── background_runtime/  # Generic process supervision (manager, supervisor, adapter traits)
├── engine/         # Core agentic loop execution engine
├── extension/      # Extension Framework (singular) — generic, zero external deps
│   ├── adapters/   # ExtensionTypeAdapter trait, ManifestFormat, BuiltInAdapters, parsing utilities
│   ├── async_exec/ # Async task execution framework (migrated from tools::framework)
│   ├── core/       # ExtensionCore — 22 hook-point registry
│   ├── integration/# Extension integration layer
│   ├── manager/    # ExtensionManager — lifecycle (install, enable, disable)
│   ├── protocols/  # Shared protocol utilities (process transport, validation, schema filter)
│   ├── services/   # Extension services (tool execution, reserved params)
│   ├── transport/  # Extension transport layer
│   └── types/      # Extension type definitions (ExtensionManifest, HookResult, etc.)
├── extensions/     # Extension Type Implementations (plural) — MCP, Gateway, Skill, Builtin, General, Universal
│   ├── builtin/    # Built-in tool adapter
│   ├── gateway/    # Gateway adapter, runtime, protocol
│   ├── general/    # General extension adapter
│   ├── mcp/        # MCP adapter, runtime, protocol (absorbed src/mcp/)
│   ├── migration/  # Legacy extension migration utilities
│   ├── skill/      # Skill adapter
│   └── universal/  # Universal tool adapter and protocol
├── identity/       # DID identity system, ed25519 keys
├── image/          # Agent image manifest, build, registry client
├── ipc/            # Inter-process communication
├── observability/  # Metrics, logging, tracing
├── portable/       # OCI-inspired image packaging (.agent packages)
├── prompt/         # Prompt assembly (markdown files, skills injection)
├── providers/      # LLM provider integrations (OpenAI, Anthropic, Ollama, Kimi, etc.)
├── registry/       # Image registry push/pull with auth
├── runtime/        # Runtime configuration and state
├── session/        # Session JSONL management, branching, indexing
├── team/           # Team composition, shared services, event bus
├── tools/          # Built-in tools and tool factory
│   ├── builtin/    # Built-in tool implementations
│   ├── core/       # Tool trait definitions (Tool, ToolContext, ToolResult)
│   └── registry/   # Tool factory and creation helpers
├── types/          # Core type definitions shared across modules
└── main.rs         # CLI entry point (clap-based)
```

---

## Key Modules and Their Purposes

| Module | Purpose |
|--------|---------|
| `agent` | Agent instance lifecycle, stateless manager, registration |
| `daemon` | Axum HTTP server, REST API, WebSocket, SSE streaming |
| `engine` | Turn-based agentic loop: input → LLM → tools → response |
| `extension` | Generic extension framework (ADR-017) — hook points, registries, types, managers, and shared services. Zero dependencies on extension type implementations. |
| `extensions` | Extension type implementations (MCP, Gateway, Skill, Builtin, General, Universal). Each type lives in its own directory. |
| `portable` | Unified packaging layer — `.agent` packages (build from directory, export, import), `.team` packages (export/import with checksums), `.ext` packages (extension export), local `AgentRegistry` with content-addressable layer storage. Merged from former `src/image/`. |
| `session` | JSONL persistence, atomic writes, branching, `.index.json` sidecars |
| `team` | Multi-agent teams, shared bus, shared MCPs, file workspace |
| `tools` | Built-in tool implementations (filesystem, process, cron, spawn, etc.). Tool traits and factory only — frameworks moved to `extensions`. |
| `providers` | LLM client abstractions (chat completions, streaming, tool calling) |
| `portable` | Unified packaging layer — `.agent` build/export/import, `.team` export/import, `.ext` export, local `AgentRegistry` with content-addressable layer storage |
| `registry` | HTTP client for push/pull to remote registries with bearer/basic auth |
| `cron` | Persistent cron jobs (`cron.json`), missed-job recovery on restart |

---

## Testing Approach

- **Unit tests** are co-located in `#[cfg(test)]` modules within source files.
- **Integration tests** live in `tests/`; the legacy `e2e_tests/` PowerShell scripts are being dismantled per `docs/integration/TESTING.md` §7.
- **Benchmarks** live in `benches/`.
- Tests cover critical paths: extension lifecycle, agent lifecycle, provider operations, session operations, tool operations.
- The project targets 80%+ test coverage.

```bash
# Run all tests
cargo test

# Run tests with all features
cargo test --all-features
```

---

## How to Add Features

1. **Identify the domain** — Is this an agent feature? A tool? A provider? An extension?
2. **Add code in the appropriate `src/<module>/`** — Follow existing module structure.
3. **Update tests** — Add unit tests in `#[cfg(test)]` and integration tests if needed.
4. **Update documentation** — If the change affects public APIs, update `API_SURFACE.md`, `API_CONTRACT.md`, `DATA_MODEL.md`, or `CAPABILITY_INTERFACE.md` as appropriate.
5. **Run the full test suite** — `cargo test` and `cargo clippy` must pass.
6. **Update `CHANGELOG.md`** — Add an entry under the current version (0.1.0).

---

## Important Notes

- **Version:** The canonical project version is **0.1.0** as declared in `Cargo.toml`. Several documentation files previously referenced `2.0` or `v2.0` — these have been aligned to `0.1.0` because `Cargo.toml` is the ground truth.
- **Daemon default bind:** `127.0.0.1:11435`. Binding to `0.0.0.0` requires explicit config and prints a warning.
- **Session durability:** JSONL is the source of truth; SQLite (`state.db`) is a rebuildable index.
- **Credential isolation:** API keys are never passed to tool subprocesses. The `process` tool strips `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`.
- **Module Boundaries (Issue 014 / Issue 015 / Issue 016):** After the Issue 015 type-oriented restructure and Issue 016 boundary cleanup:
  - `src/extension/` (singular) contains the **generic extension framework** — core, types, manager, async_exec, transport, services, protocols/shared, and the new adapters module. It has **zero dependencies** on `crate::extensions`, `crate::mcp`, `crate::daemon`, or `crate::tools`.
  - `src/extensions/` (plural) contains **extension type implementations** — each type (mcp, gateway, skill, builtin, general, universal) lives in its own directory with its adapter, runtime, and protocol code.
  - `src/extensions/mcp/protocol/` absorbed the former `src/mcp/` module (client, transport, types, config, discovery, manager). `src/mcp/` no longer exists.
  - `src/extensions/mcp/runtime/` contains MCP runtime adapters, tool proxies, and starters (migrated from `src/extensions/runtime/` and `src/mcp/`).
  - `src/extensions/gateway/runtime/` contains Gateway runtime adapters, router, and starters (migrated from `src/extensions/runtime/` and `src/daemon/background_runtime/`).
  - `src/daemon/background_runtime/` contains only generic process supervision code (traits, manager, supervisor, starter registry).
  - `src/tools/framework/` has been removed.
  - `src/extension/core/` has zero dependencies on `crate::extensions`, `crate::mcp`, `crate::daemon`, or `crate::tools`.
  - **Execution primitives** (`ToolContext`, `ToolError`, `AbortSignal`, `ToolResult`, `ToolWithContext`, `ToolContextAdapter`) live in `extension::types::tool_exec` so the generic framework can use them without depending on `crate::tools`. `tools::core` re-exports them for convenience.
  - **Dependency direction:** `extension` → (no tools deps). `tools::core` → imports execution primitives from `extension::types`. `tools::builtin` may import `async_exec` from `extension` (acceptable: tools depends on extension). The bidirectional loop is broken.

---

## Registry Commands

The CLI supports registry push/pull with a configurable default registry.

### Setting a default registry

```bash
# Set pekohub.org as the default
peko registry set-default pekohub.org

# Use a local registry for development
peko registry set-default localhost:3000

# Check current default
peko registry get-default
```

### Push and pull with bare references

When a default registry is configured, you can use bare references:

```bash
# Push (resolves to pekohub.org/peko/agents/my-agent:v1.0)
peko agent push my-agent:v1.0

# Pull (resolves to pekohub.org/peko/agents/my-agent:v1.0)
peko agent pull my-agent:v1.0

# Override for a single command
peko agent push my-agent:v1.0 --registry localhost:3000
```

### Authentication

```bash
# Log in to the default registry
peko login --api-key ph_xxxxxxxx

# Log in to a specific registry
peko login --registry localhost:3000 --api-key ph_xxxxxxxx

# Log out
peko logout
```

Full references (`host/path:tag`) continue to work as before:

```bash
peko agent push my-agent:v1.0 custom.registry.com/peko/agents/my-agent:v1.0
```

---

## Related Documentation

- `README.md` — Human-facing quick start and feature overview
- `API_CONTRACT.md` — HTTP API contract
- `API_SURFACE.md` — Public Rust API surface
- `DATA_MODEL.md` — On-disk and in-memory data formats
- `CAPABILITY_INTERFACE.md` — Built-in tools, sandbox model, cron lifecycle
- `REQUIREMENTS_SPEC.md` — Testable requirements by domain
- `CHANGELOG.md` — Version history
- `docs/architecture/` — Architecture overviews and ADRs
