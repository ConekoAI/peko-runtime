# Peko — Agent Guide

> **Project:** Peko  
> **Version:** 0.1.0 (source of truth: `Cargo.toml`)  
> **Language:** Rust (Edition 2021)  
> **License:** MIT

---

## Project Overview

Peko is a Rust-based multi-agent runtime with a unified extension architecture. It provides:

- **Agent harness** with turn-based agentic loops (LLM → tool execution → respond)
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
├── agents/                 # Agent management (stateless manager, config, lifecycle, prompts)
├── auth/                   # Authentication and authorization (principal, ownership, JWT, API keys)
├── commands/               # CLI command implementations
├── common/                 # Shared services and core types (AgentService, config authority, vault, KV, types)
├── cron/                   # Cron job scheduling and persistence
├── daemon/                 # HTTP daemon (Axum-based), health, info endpoints, AppState composition root
│   └── background_runtime/ # Generic process supervision (manager, supervisor, adapter traits)
├── engine/                 # Core agentic loop execution engine
├── extensions/             # Extension framework + type implementations
│   ├── framework/          # Generic extension framework (ADR-017) — core, types, manager, async_exec, transport, services, protocols/shared
│   ├── builtin/            # Built-in tool adapter
│   ├── gateway/            # Gateway adapter, runtime, protocol
│   ├── general/            # General extension adapter
│   ├── mcp/                # MCP adapter, runtime, protocol
│   ├── skill/              # Skill adapter
│   └── universal/          # Universal tool adapter and protocol
├── identity/               # DID identity system, ed25519 keys, key storage, runtime identity
├── ipc/                    # Inter-process communication
├── observability/          # Metrics, logging, tracing, audit
├── providers/              # LLM provider integrations (v3: catalog + resolver)
│   ├── adapters/           # OpenAI / Anthropic / openai-compatible ApiAdapters
│   ├── catalog.rs          # ProviderCatalog — runtime-owned, persisted to `~/.peko/providers.toml`
│   ├── templates.rs        # Built-in preset templates with curated model lists
│   ├── resolver.rs         # LlmResolver — precedence: override > session > agent > default > first
│   └── core.rs             # Unified Provider type
├── registry/               # Local packaging/export/import and remote registry push/pull
│   ├── packaging/          # OCI-inspired .agent/.team/.ext archive handling
│   └── client.rs           # HTTP registry client
├── session/                # Session JSONL management, branching, indexing, compaction
├── tools/                  # Built-in tools and tool factory
│   ├── builtin/            # Built-in tool implementations
│   ├── core/               # Tool trait definitions
│   └── registry/           # Tool factory and creation helpers
├── tunnel/                 # Tunnel / network layer — Pekohub A2A protocol, dispatcher, known runtimes
├── main.rs                 # CLI entry point (clap-based)
└── lib.rs                  # Library surface (public domains + re-exports)
```

---

## Key Modules and Their Purposes

| Module | Purpose |
|--------|---------|
| `agents` | Agent instance lifecycle, stateless manager, registration, prompts |
| `auth` | Principal ownership, permission grants, API keys, JWT, rate limiting |
| `commands` | Clap argument parsing and command handlers (still transitioning to thin service delegation) |
| `common` | Shared services (`AgentService`, `SessionService`, `ConfigAuthority`, `Vault`, etc.) and core types |
| `daemon` | Axum HTTP server, REST API, WebSocket, SSE streaming, `AppState` composition root |
| `engine` | Turn-based agentic loop: input → LLM → tools → response |
| `extensions::framework` | Generic extension framework (ADR-017) — hook points, registries, types, managers, and shared services. Zero dependencies on concrete extension type implementations. |
| `extensions` (sibling submodules) | Extension type implementations (MCP, Gateway, Skill, Builtin, General, Universal). Each type lives in its own directory. |
| `identity` | DID identity, keychain, key storage, resolver, runtime identity |
| `registry` | Local packaging/export/import (`AgentRegistry`, `.agent`/`.team`/`.ext` archives) and remote registry client |
| `session` | JSONL persistence, atomic writes, branching, `.index.json` sidecars, compaction |
| `tools` | Built-in tool implementations and tool trait surface |
| `providers` | LLM client abstractions (chat completions, streaming, tool calling) |
| `tunnel` | Pekohub tunnel protocol, A2A dispatcher, runtime discovery |
| `cron` | Persistent cron jobs (`cron.json`), missed-job recovery on restart |

---

## Testing Approach

- **Unit tests** are co-located in `#[cfg(test)]` modules within source files.
- **Integration tests** live in `tests/`; the legacy PowerShell `e2e_tests/` tree was renamed to `e2e_tests_archive/` and now serves as a fixture source for the new Rust integration tests.
- **New CLI integration tests** (Phase B migration):
  - `tests/cli_send.rs` — `peko send` with mock LLM
  - `tests/cli_session.rs` — `peko session` with mock LLM
  - `tests/cli_basics.rs` — Offline agent/team/config commands
  - `tests/cli_extensions.rs`, `tests/cli_extensions_l3.rs`, `tests/cli_compaction.rs` — Extension system and session compaction
- **Scenario tests** live in `tests/scenarios/` (registered explicitly in `Cargo.toml`):
  - `s1_local_agent_with_extensions` through `s6_principal_grant_revoke_roundtrip`
  - `tunnel_security` — Tunnel protocol security checks
- **Fixtures** for scenario tests live in `e2e_tests_archive/` (legacy PowerShell e2e tree, kept as a fixture source).
- **Benchmarks** live in `benches/`.
- Tests cover critical paths: extension lifecycle, agent lifecycle, provider operations, session operations, tool operations.

### CI tiers (see `.github/workflows/integration.yml`)

The workflow runs a path-aware, six-tier pipeline. Doc-only PRs (only
`*.md`, `PLAN.md`, `CHANGES.md`, `docs/**`) do **not** trigger CI at all
(workflow-level `paths` filter). For PRs that do trigger:

| Tier | Trigger | Wall-clock (warm) | Make target |
|---|---|---|---|
| `smoke` | `src/**` or `tests/**` changed | < 6 min | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib` |
| `lint` | `src/**` changed | < 1 min | `bash scripts/check_module_boundaries.sh` |
| `unit-linux` | `src/**` or `tests/**` changed | ~3 min | `cargo test --lib` |
| `unit-windows` | Windows-specific paths or `[windows]` keyword / schedule / manual | ~5 min | `cargo test --lib` |
| `integration` | `tests/**`, `docker/**`, `Dockerfile*`, or workflow changed; or schedule / manual | ~10-15 min | `make docker-up` + `make test-integration` |
| `integration-llm` | `src/**` or `tests/**` changed AND `[llm]` keyword / schedule / manual | ~5 min extra | `make test-integration-llm` |

### Local quick feedback loop

```bash
# Fastest: fmt + clippy + lib tests only (no Docker)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --lib

# Mock-LLM tier (needs docker compose stack)
make docker-up
make test-integration
make docker-down

# Real-LLM tier (needs MINIMAX_API_KEY + KIMI_API_KEY)
make test-integration-llm

# Run all tests
cargo test
cargo test --all-features
```

---

## How to Add Features

1. **Identify the domain** — Is this an agent feature? A tool? A provider? An extension?
2. **Add code in the appropriate `src/<module>/`** — Follow existing module structure.
3. **Update tests** — Add unit tests in `#[cfg(test)]` and integration tests if needed.
4. **Update documentation** — If the change affects public APIs, update `API_SURFACE.md` and `DATA_MODEL.md` as appropriate.
5. **Run the full test suite** — `cargo test` and `cargo clippy` must pass.
6. **Update `CHANGELOG.md`** — Add an entry under the current version (0.1.0).

---

## Important Notes

- **Version:** The canonical project version is **0.1.0** as declared in `Cargo.toml`. Several documentation files previously referenced `2.0` or `v2.0` — these have been aligned to `0.1.0` because `Cargo.toml` is the ground truth.
- **Daemon default bind:** `127.0.0.1:11435`. Binding to `0.0.0.0` requires explicit config and prints a warning.
- **Session durability:** JSONL is the source of truth; SQLite (`state.db`) is a rebuildable index.
- **Credential isolation:** API keys are never passed to tool subprocesses. The `process` tool strips `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`.
- **Module Boundaries (Issue 014 / Issue 015 / Issue 016 / Issue 020):**
  - `src/extensions/framework/` contains the **generic extension framework** — core, types, manager, async_exec, transport, services, protocols/shared, and adapters. It has **zero dependencies** on concrete extension type implementations under `src/extensions/<type>/`.
  - `src/extensions/<type>/` (builtin, gateway, general, mcp, skill, universal) contains **extension type implementations**. Each type lives in its own directory and should not import from sibling extension types.
  - `src/extensions/framework/core/` has zero dependencies on `crate::extensions::<type>`, `crate::daemon`, or `crate::tools`.
  - **Execution primitives** (`ToolContext`, `ToolError`, `AbortSignal`, `ToolResult`, `ToolWithContext`, `ToolContextAdapter`, `ToolProgressEvent`) and the `ContextSource` trait live in `tools::core/exec.rs` and `tools::core/context_source.rs` (moved from `extensions::framework::types/` and `extensions::framework::protocols/shared/`). The blanket impl `impl<T: Tool> ToolWithContext for T` is in place now that the cycle is broken.
  - **Dependency direction:**
    - `extensions::framework` depends on `tools::core` (one-way, for `Tool`, `ToolContext`, `ContextSource`, and other execution primitives). It does **not** depend on `tools::builtin` or any concrete extension type.
    - `tools::core` does **not** depend on `extensions::framework`. The previous bidirectional loop is broken.
    - `tunnel` depends on `tools::core` (for the `Tool` trait) and **does not** depend on `agents` in production code.
    - `agents` depends on `tunnel` (for the `AgentMessageService` trait used by `A2aSendTool`) and does **not** depend on `tunnel::a2a_send_tool`'s concrete types.
    - `extensions::framework` does **not** depend on `agents`, `tunnel`, or `daemon` (enforced by `check_module_boundaries.sh` Rule 5).
  - Cycles 4 (`tools::core ↔ extension::types`) and 5 (`tunnel ↔ agents`) from `PLAN.md` §2.5 are now actually broken (not reshuffled).
  - `src/commands/` should delegate to services and not import low-level persistence/packaging modules directly (e.g. `crate::registry::packaging::`, `crate::common::services::config_authority::`, `crate::identity::storage::`, `crate::session::jsonl::`, `crate::session::metadata_controller::`). `scripts/check_module_boundaries.sh` enforces this as an advisory rule while existing violations are being resolved.

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
- `API_SURFACE.md` — Public Rust API surface
- `DATA_MODEL.md` — On-disk and in-memory data formats
- `CHANGELOG.md` — Version history
- `docs/README.md` — Documentation index
- `docs/architecture/adr/` — Architecture Decision Records (ADR-001 through ADR-039)
