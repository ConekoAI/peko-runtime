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
- **Registry** for image packaging, push/pull with content-addressable storage

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
├── engine/         # Core agentic loop execution engine
├── extensions/     # Unified Extension Architecture (ADR-017)
│   ├── adapters/   # Type-specific adapters (skill, mcp, tool, channel, gateway)
│   ├── core/       # ExtensionCore — 22 hook-point registry
│   ├── integration/# Extension integration layer
│   ├── manager/    # ExtensionManager — lifecycle (install, enable, disable)
│   ├── services/   # Extension services
│   ├── transport/  # Extension transport layer
│   └── types/      # Extension type definitions
├── identity/       # DID identity system, ed25519 keys
├── image/          # Agent image manifest, build, registry client
├── ipc/            # Inter-process communication
├── mcp/            # Model Context Protocol client integration
├── observability/  # Metrics, logging, tracing
├── portable/       # OCI-inspired image packaging (.agent packages)
├── prompt/         # Prompt assembly (markdown files, skills injection)
├── providers/      # LLM provider integrations (OpenAI, Anthropic, Ollama, Kimi, etc.)
├── registry/       # Image registry push/pull with auth
├── runtime/        # Runtime configuration and state
├── session/        # Session JSONL management, branching, indexing
├── team/           # Team composition, shared services, event bus
├── tools/          # Built-in tools and tool factory
│   └── framework/  # Tool framework internals
│       └── shared/ # Shared tool utilities (schema_filter, etc.)
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
| `extensions` | Unified hook-based system for all capabilities (ADR-017) |
| `session` | JSONL persistence, atomic writes, branching, `.index.json` sidecars |
| `team` | Multi-agent teams, shared bus, shared MCPs, file workspace |
| `tools` | Built-in tool implementations (filesystem, process, cron, spawn, etc.) |
| `providers` | LLM client abstractions (chat completions, streaming, tool calling) |
| `image` | Image building, SHA-256 digests, content-addressable storage |
| `registry` | Push/pull to remote registries with bearer/basic auth |
| `cron` | Persistent cron jobs (`cron.json`), missed-job recovery on restart |

---

## Testing Approach

- **Unit tests** are co-located in `#[cfg(test)]` modules within source files.
- **Integration tests** live in `tests/` and `e2e_tests/`.
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
