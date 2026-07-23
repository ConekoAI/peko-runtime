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
- **Built-in tools** (Read, Write, Edit, Bash, Agent, CronCreate/CronDelete/CronList, AsyncSpawn/AsyncOutput/AsyncStop/AsyncStatus/AsyncList, TaskCreate/TaskGet/TaskList/TaskUpdate, session, etc.)
- **Extension system** with 22 hook points for tools, skills, MCP servers, channels, and gateways
- **Packaging** — `.principal` build/export/import, `.ext` export, registry push/pull with content-addressable storage
  (the `.agent` and `.team` archive formats were retired when the
  standalone agent CRUD surface was rescoped into the principal-as-
  single-actor model)

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

Peko is a Cargo workspace with the strict separation the migration aimed for.
The root `peko` package remains the compatibility facade and CLI; extracted
contracts and binaries live under `crates/`. Final workspace members:

- `peko-events` — neutral agentic event contract (`AgenticEvent`, `LifecyclePhase`,
  `ToolId`/`ToolCallId`/`RunId`) shared by the engine and legacy provider streaming.
- `peko-message` — neutral message contract (`ContentBlock`, `LlmMessage`,
  `MessageRole`, `TokenUsage`, `AgentMessage`, `MessageConverter`, `MessageContext`,
  `SteeringProvider`, `ContextTransformer`) shared by providers, sessions, quota,
  extensions, and the agentic loop.
- `peko-subject` — canonical actor type and identifier newtypes (ADR-041):
  `Subject`, `SubjectKind`, `PrincipalId`, `PrincipalDID`, `SubjectParseError`,
  `subject_from_string_with_default_user`. Pure value/type layer with no inbound
  edge from principal, agents, engine, daemon, providers, or extensions.
- `peko-tools-core` — `Tool`, `ToolContext`, `AbortSignal`, `ToolResult`,
  `ToolError`, `ToolInterruptNotice`, `ContextSource`, `ToolExposure` (F34).
  No dependency on extension host or built-in tools.
- `peko-provider-api` — provider contract types (`ChatOptions`, `ChatResponse`,
  `StreamEvent`, `ContentDelta`, `StopReason`, `BlockType`, `ToolDefinition`,
  `ContentBlockId`, `ThinkingEffort`, `ThinkingFormat`, `ThinkingKeep`,
  `ToolChoice`, `ServiceTier`, `ProviderCompat`, `DeferredToolsMode`,
  `CacheRetention`). Depends on `peko-message` + `peko-tools-core` only.
- `peko-extension-api` — stable framework contracts (`ExtensionId`, `HookId`,
  `HookInput`/`HookOutput`/`HookResult`, `ExtensionManifest`, `Capability`,
  `Capabilities`, `ActiveExtensionSet`, `ToolMetadata`, `ToolSource`,
  `ToolRuntimeContext`, `AsyncReceipt`, `AsyncTaskStatus`, `MessageEnvelope`,
  `SessionSnapshot`, `PromptBuildState`, `ToolRegistryAccess`,
  `ReservedParamsConfig`, `ParamSource`, `ConfigFormat`).
- `peko-extension-host` — concrete framework implementation (registry, hook
  dispatch, capability gate, async executor, transport, manager/store,
  scaffold, skill catalog, integration, framework services, `SimpleRegistry`/
  `SharedRegistry`). Depends only on `peko-extension-api` + `peko-message` +
  `peko-tools-core` + `peko-provider-api` + `peko-subject`.
- `peko-engine` — agentic loop core. Owns `chunker`, `event_processor`, `state`,
  `stream_buffer`, `stream_orchestrator`, `tool_stream`, `parallel_gate`,
  `events` re-export, `error` (`AgenticError` taxonomy),
  `stream_types` (Phase 9b.1), `async_completion` (Phase 9b.N.1),
  `funnel` (Phase 9b.N.2 — F37 `execute_tool_via_core*` canonical chokepoint),
  `tool_executor` + `session_view` (Phase 9b.N.3),
  `compaction` + `compaction_orchestrator` (Phase 9b.N.4),
  `agent_view` + `async_inbox` + `iteration_state` (Phase 9b.N.5a — trait ports the
  `agentic_loop.rs` lift in 9b.N.5b will consume).
  Phase 9b.N.1 added `peko-extension-api` + `peko-extension-host` as
  direct deps for `AsyncTaskStatus` + `CompletionEvent`.
  Phase 9b.N.2 introduced the `ToolFunnel` trait port (in
  `peko-extension-host`) so the funnel can route through a trait
  rather than holding a direct borrow of root `ExtensionCore`; the
  trait is transient scaffolding and disappears when Phase 8
  bulk-moves `ExtensionCore` into `peko-extension-host`.
  Phase 9b.N.3 widened `ToolFunnel` to expose the full engine-facing
  surface of `ExtensionCore` (`is_parallelizable`, `pre_tool_use`,
  `post_tool_use`, `execute_tool_via_hook`) so `tool_executor.rs`
  could lift into `peko-engine`. The hook methods hide
  `HookPoint`/`HookInput` construction (still root-only) inside the
  impl in `src/engine/extension_core_funnel_compat.rs`. The
  `SessionView` trait (in `peko-engine`) plus the `SessionCore`
  marker use a blanket impl over `Arc<RwLock<T>>` so root's
  `impl SessionCore for crate::session::Session` makes
  `Arc<RwLock<Session>>` satisfy `SessionView` without a
  local-foreign orphan collision (Arc is not a fundamental type for
  the orphan rule).
  Phase 9b.N.4 lifted `compaction_orchestrator.rs` into `peko-engine`
  via three trait ports. `ToolFunnel` gained three hook-firing
  methods for the compaction + session-state hooks
  (`invoke_session_compaction_pre_hook`,
  `invoke_session_compaction_post_hook`,
  `invoke_session_state_change_hook`) returning the trimmed
  `HookDecision` (3 variants) so the trait stays free of
  root-only `HookPoint`/`HookInput`. `SessionView` was extended
  with `record_compaction`, `load_previous_compaction_summary`,
  and `update_context_cache` for the orchestrator's session
  writes. A new `CompactorBackend` trait
  (`peko-engine::compaction::backend`) abstracts
  `BackgroundCompactor` so the orchestrator holds
  `Box<dyn CompactorBackend>`. `CompactionEntry::details` was
  widened from `Option<CompactionDetails>` to
  `Option<serde_json::Value>` for trait-port compatibility, with
  serde round-trip at the boundaries. `peko-extension-api` gained
  `CompactionPreparationPayload`, `CompactionResultPayload`, and
  `HookDecision` (lifted from root's `hook_io`).
  Phase 9b.N.5a introduced the trait ports the 9b.N.5b `agentic_loop.rs`
  lift will consume. `AgentView` (12 methods) abstracts `Agent`'s
  engine-facing surface so the loop never holds an `Arc<Agent>`
  directly; `has_llm_resolver()` collapses `Agent::llm_resolver()`
  to a bool because the loop only does `Some(_) / None` matching.
  `AsyncInboxLike` + `AsyncInboxItem` abstracts the
  `SharedSessionInbox` drain — the two relevant `InboxItem`
  variants map to `peko_extension_host::CompletionEvent` and
  `peko_extension_host::SteeringMessage` (root's
  `completion_queue` types are field-by-field identical but
  distinct types pending the Phase 11 protocol extraction that
  unblocks the bulk move). `CapabilityDiffTracker` lifts into
  `peko-engine::iteration_state` (small loop-local state).
  `ToolFunnel` gained `invoke_stop_hook` +
  `invoke_after_agent_hook` mirroring the pre/post tool-use
  pair so the lifted loop never imports `HookPoint` /
  `HookInput` directly. The actual `src/engine/agentic_loop.rs`
  lift is Phase 9b.N.5b.
- `peko-quota` — per-principal token quota (F18/F19). `QuotaMeter`,
  `QuotaScope`, `QuotaState`, `QuotaConfig`, `QuotaError`.
- `peko-tools-builtin` — concrete built-in tool implementations (filesystem,
  async control, cron, session, messaging, skill, task). Port traits live
  beside the tools so daemon state stays out of this crate.
- `peko-protocol` — IPC + tunnel wire-shape contracts (Phase 11a).
  `AuthCredential`, `PrincipalSendControlMode`, `AuthHeader`,
  `MAX_PACKET_SIZE`, `HEARTBEAT_INTERVAL_SECS`, `CLI_TIMEOUT_SECS`.
  Depends only on `serde` + `serde_json`. Bulk `RequestPacket`/
  `ResponsePacket`/`TunnelMessage` stay in root pending future cleanup.
- `peko-daemon` — long-running background daemon binary (Phase 12). Depends
  only on root `peko` lib for the daemon entry surface (`Daemon`,
  `DaemonConfig`, `LaunchMode`, `PathResolver`). The CLI's `daemon start`
  background-spawn path resolves this artifact next to its own executable
  and prefers it over re-exec'ing the CLI binary (Phase 11c).

```text
crates/
├── engine/                 # Agentic loop core (peko-engine)
├── events/                 # Neutral agentic event contract (peko-events)
├── extension-api/          # Framework API contracts (peko-extension-api)
├── extension-host/         # Framework host impl (peko-extension-host)
├── message/                # Neutral message contract (peko-message)
├── peko-daemon/            # Long-running daemon binary (peko-daemon)
├── provider-api/           # Provider contract types (peko-provider-api)
├── protocol/               # IPC + tunnel wire-shape contracts (peko-protocol)
├── quota/                  # Per-principal token quota (peko-quota)
├── subject/                # Canonical actor type (peko-subject, ADR-041)
├── tools-builtin/          # Concrete built-in tool implementations
└── tools-core/             # Tool execution API (peko-tools-core, F34 ToolExposure)
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
│   ├── packaging/          # OCI-inspired .principal/.ext archive handling
│   │                       # (.agent/.team archives were retired with
│   │                       #  the principal-as-single-actor migration)
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
| `common` | Shared services (`StatelessAgentService`, `ConfigAuthority`, `Vault`, `ExtensionConfigService`, etc.) and core types |
| `daemon` | Axum HTTP server, REST API, WebSocket, SSE streaming, `AppState` composition root |
| `engine` | Turn-based agentic loop: input → LLM → tools → response |
| `extensions::framework` | Generic extension framework (ADR-017) — hook points, registries, types, managers, and shared services. Zero dependencies on concrete extension type implementations. |
| `extensions` (sibling submodules) | Extension type implementations (MCP, Gateway, Skill, Builtin, General, Universal). Each type lives in its own directory. |
| `identity` | DID identity, keychain, key storage, resolver, runtime identity |
| `registry` | Local packaging/export/import (`PrincipalPackager`/`PrincipalUnpackager`, `.principal`/`.ext` archives) and remote registry client |
| `session` | JSONL persistence, atomic writes, branching, `.index.json` sidecars, compaction |
| `tools` | Built-in tool implementations and tool trait surface |
| `providers` | LLM client abstractions (chat completions, streaming, tool calling) |
| `tunnel` | Pekohub tunnel protocol, A2A dispatcher, runtime discovery |
| `cron` | Persistent cron jobs (`cron.json`), missed-job recovery on restart |

---

## Testing Approach

- **Unit tests** are co-located in `#[cfg(test)]` modules within source files.
- **Integration tests** live in `tests/`; the legacy PowerShell `e2e_tests/` tree was renamed to `e2e_tests_archive/` and now serves as a fixture source for the new Rust integration tests.
- **New CLI integration tests** (Phase B migration, then
  retargeted for the principal-as-single-actor model in the parity
  branch):
  - `tests/cli_send.rs` — `peko send` (targets a Principal; mock LLM)
  - `tests/cli_basics.rs` — Offline `peko principal`/`peko config`
  - `tests/cli_extensions.rs`, `tests/cli_extensions_l3.rs` — Extension system
  - `tests/cli_cron.rs` — `peko cron` create/list/delete (mock LLM)
  - `tests/cli_subagent.rs` — `peko subagent` + `agent_spawn` (mock LLM)
  - `tests/cli_tools.rs` — Built-in tools (Bash, Read, Write, …) (mock LLM)
  - `tests/cli_providers.rs` — Real-LLM tier (minimax, kimi)
  - `tests/cli_agent_signature.rs` — Principal packager signature
    verification (auto-discovered, run on demand via Make target)
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
| `lint-workspace` | `crates/**`, root `Cargo.toml`/`Cargo.lock`, or `scripts/check_workspace_deps.py` changed (Phase 12b) | < 5 s | `python3 scripts/check_workspace_deps.py` |
| `unit-linux` | `src/**` or `tests/**` changed | ~3 min | `cargo test --lib` |
| `unit-windows` | Windows-specific paths or `[windows]` keyword / schedule / manual | ~5 min | `cargo test --lib` |
| `integration` | `tests/**`, `docker/**`, `Dockerfile*`, or workflow changed; or schedule / manual | ~10-15 min | `make docker-up` + `make test-integration` |
| `integration-llm` | `src/**` or `tests/**` changed AND `[llm]` keyword / schedule / manual | ~5 min extra | `make test-integration-llm` |

### Cleanup phases (post-migration)

The 13-member workspace migration is complete, but the migration
was deliberately conservative: every historical `peko::subject`,
`peko::quota`, `peko::tools::core`, `peko::common::types::message`
import path is preserved through `pub use peko_foo::*` shims in
`src/`. Seven trait-port compat impls (`agent_view_compat.rs`,
`async_completion_compat.rs`, `async_inbox_compat.rs`,
`background_compactor_factory_compat.rs`,
`compaction_backend_compat.rs`, `extension_core_funnel_compat.rs`,
`provider_view_compat.rs`, `session_view_compat.rs`) live in
`src/engine/`. The 3,871-line `agentic_loop_compat.rs` keeps
engine integration tests in root because they reference root-only
fixtures (`Agent`, `ExtensionCore`, `SessionManager`, `Provider`,
`MockAdapter`, `BuiltinToolAdapter`, `LlmResolver`).

Big root-only domains remain: `src/providers/` (~12k lines),
`src/extensions/` (~37k lines), `src/session/` (~17k lines),
`src/daemon/` (~6k lines), `src/agents/` (~8k lines),
`src/tunnel/` (~11k lines), `src/ipc/` (~16k lines),
`src/principal/` (~6k lines), `src/registry/` (~7k lines),
`src/auth/` (~2k lines), `src/identity/` (~3k lines),
`src/chat_log/` (~700 lines). Deferred tool extractions:
`BashTool`, `ToolSearchTool`, `AgentCatalog`, `ToolRuntime`.

The cleanup goal is **codex-rs-like cleanliness**: no top-level
facade, no compat shims, per-crate test fixtures, strict CI-enforced
dep-graph, 26+ workspace members at the right granularity for the
domain size.

#### Phase index

| Phase | Goal | Headline historical-path breakage |
|---|---|---|
| 0 | Cosmetic + tooling baseline (no path breakage) | none |
| 1 | Root facade boundary + canonical path inventory | none yet |
| 2 | Consolidate duplicate `CompletionEvent` (engine vs extension-host) | `peko::extensions::framework::async_exec::executor::completion_queue::CompletionEvent` |
| 3 | Extract `peko-identity` | `peko::identity::*` |
| 4 | Extract `peko-auth` | `peko::auth::*` |
| 5 | Extract `peko-chat-log` | `peko::chat_log::*` |
| 6 | Extract `peko-providers` | `peko::providers::*` |
| 7 | Extract `peko-session` (incl. `InboxRegistry`) | `peko::session::*` |
| 8 | Bulk-move extension host implementation into `peko-extension-host` | `peko::extensions::framework::*` |
| 9 | Extract `peko-agents` | `peko::agents::*` |
| 10 | Move remaining built-in tools (non-deferred) into `peko-tools-builtin` | `peko::tools::builtin::*` (excl. bash, tool_search, agent_catalog) |
| 11 | Extract `peko-registry` | `peko::registry::*` |
| 12 | Extract `peko-tunnel` + `peko-ipc` | `peko::tunnel::*`, `peko::ipc::*` |
| 13 | Extract daemon implementation | `peko::daemon::*` |
| 14 | Extract remaining runtime domains | `peko::observability::*`, `peko::cron::*`, `peko::principal::*` |
| 15 | **Delete pure re-export shims** | `peko::subject::*`, `peko::quota::*`, `peko::tools::core::*`, `peko::common::types::message::*` |
| 16 | Delete trait-port compat impls | `peko::engine::{agent,async_completion,async_inbox,background_compactor_factory,compaction_backend,extension_core_funnel,provider,session}_view_compat::*` |
| 17 | Build `peko-engine-test-support` + move engine tests | (no root path breakage; tests relocate) |
| 18 | Move deferred built-in tools (`BashTool`, `ToolSearchTool`, `AgentCatalog`) + `tool_runtime.rs` | `peko::tools::builtin::bash`, `peko::tools::builtin::tool_search`, `peko::tools::builtin::agent_catalog`, `peko::engine::tool_runtime` |

#### Final crate layout (26 workspace members)

- `crates/engine` — agentic loop core.
- `crates/engine-test-support` — dev-deps-only fixtures for engine tests.
- `crates/events` — neutral agentic event contract.
- `crates/extension-api` — extension framework contracts.
- `crates/extension-host` — extension framework implementation.
- `crates/message` — neutral message contract.
- `crates/providers` — concrete provider implementations.
- `crates/provider-api` — provider contracts.
- `crates/protocol` — IPC + tunnel wire contracts.
- `crates/quota` — token quota.
- `crates/subject` — canonical actor type.
- `crates/tools-builtin` — concrete built-in tool implementations (incl. bash, tool_search, agent_catalog).
- `crates/tools-core` — tool API.
- `crates/agents` — agent lifecycle + subagent execution.
- `crates/session` — session persistence + compaction.
- `crates/auth` — authentication/authorization.
- `crates/identity` — DID identity + key storage.
- `crates/chat-log` — append-only chat-log storage.
- `crates/registry` — packaging + registry client + trust store.
- `crates/tunnel` — tunnel protocol + A2A dispatcher.
- `crates/ipc` — IPC server + handlers.
- `crates/observability` — instrumentation.
- `crates/cron` — persistent cron scheduling.
- `crates/principal` — principal orchestration.
- `crates/peko-daemon` (binary + lib) — daemon implementation + entry point.
- root `peko` (lib + bin) — CLI entry + thin composition only.

#### Cleanup invariant

**Every historical `peko::...` import path is intentionally broken.**
No new `pub use peko_*::*` shims. The 4 pure-shim modules that exist
today (`src/subject.rs`, `src/quota/mod.rs`, `src/tools/core/mod.rs`,
`src/common/types/message.rs`) carry deletion banners pointing to
Phase 15. Every other historical path breaks across Phases 3–14,
16, and 18.

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
- **Credential isolation:** API keys are stored in the OS keychain, not in environment variables. The `Bash` tool inherits the runtime environment and does not scrub env vars; keep secrets out of `env` in agent configs.
- **Module Boundaries (Issue 014 / Issue 015 / Issue 016 / Issue 020):**
  - `src/extensions/framework/` contains the **generic extension framework** — core, types, manager, async_exec, transport, services, protocols/shared, and adapters. It has **zero dependencies** on concrete extension type implementations under `src/extensions/<type>/`.
  - `src/extensions/<type>/` (builtin, gateway, general, mcp, skill, universal) contains **extension type implementations**. Each type lives in its own directory and should not import from sibling extension types.
  - `src/extensions/framework/core/` has zero dependencies on `crate::extensions::<type>`, `crate::daemon`, or `crate::tools`.
  - **Execution primitives** (`ToolContext`, `ToolError`, `AbortSignal`, `ToolResult`, `ToolWithContext`, `ToolContextAdapter`, `ToolProgressEvent`) and the `ContextSource` trait live in `tools::core/exec.rs` and `tools::core/context_source.rs` (moved from `extensions::framework::types/` and `extensions::framework::protocols/shared/`). The blanket impl `impl<T: Tool> ToolWithContext for T` is in place now that the cycle is broken.
  - **Dependency direction:**
    - `extensions::framework` depends on `tools::core` (one-way, for `Tool`, `ToolContext`, `ContextSource`, and other execution primitives). It does **not** depend on `tools::builtin` or any concrete extension type.
    - `tools::core` does **not** depend on `extensions::framework`. The previous bidirectional loop is broken.
    - `tunnel` depends on `tools::core` (for the `Tool` trait) and **does not** depend on `agents` in production code.
    - `agents` depends on `tunnel` (for the `AgentMessageService` trait used by `PrincipalSendTool`) and does **not** depend on `tunnel::principal_send_tool`'s concrete types.
    - `extensions::framework` does **not** depend on `agents`, `tunnel`, `daemon`, or `principal` (enforced by `check_module_boundaries.sh` Rules 5 and 6).
  - Cycles 4 (`tools::core ↔ extension::types`) and 5 (`tunnel ↔ agents`) from `PLAN.md` §2.5 are now actually broken (not reshuffled).
  - `src/commands/` should delegate to services and not import low-level persistence/packaging modules directly (e.g. `crate::registry::packaging::`, `crate::common::services::config_authority::`, `crate::identity::storage::`, `crate::session::jsonl::`, `crate::session::metadata_controller::`). `scripts/check_module_boundaries.sh` enforces this as an advisory rule while existing violations are being resolved.

- **Workspace dependency rules (Phase 12b):** the path-grep `check_module_boundaries.sh` covers in-`src/` rules. For crate-level edges — `peko-provider-api` MUST NOT depend on `peko-engine`, `peko-protocol` is `serde`+`serde_json` only, the leaf crates (`peko-message` / `peko-subject` / `peko-tools-core` / `peko-events`) MUST NOT depend on any other `peko-*`, etc. — `scripts/check_workspace_deps.py` reads every `crates/*/Cargo.toml` and asserts a 71-entry forbidden-edge table derived from the workspace-migration plan. Run locally with `python3 scripts/check_workspace_deps.py` (add `--print-graph` to see the actual edges). The script fires automatically in the `lint-workspace` CI job whenever `crates/**`, root `Cargo.toml`, `Cargo.lock`, or the script itself change. New forbidden edges surface here before a PR can land; adding a rule is one line in `FORBIDDEN_EDGES` with a doc comment explaining the rationale.

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
# Push (resolves to pekohub.org/peko/principals/my-principal:v1.0)
peko principal push my-principal:v1.0

# Pull (resolves to pekohub.org/peko/principals/my-principal:v1.0)
peko principal pull my-principal:v1.0

# Override for a single command
peko principal push my-principal:v1.0 --registry localhost:3000
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
peko principal push my-principal:v1.0 custom.registry.com/peko/principals/my-principal:v1.0
```

---

## Related Documentation

- `README.md` — Human-facing quick start and feature overview
- `API_SURFACE.md` — Public Rust API surface
- `DATA_MODEL.md` — On-disk and in-memory data formats
- `CHANGELOG.md` — Version history
- `docs/README.md` — Documentation index
- `docs/architecture/adr/` — Architecture Decision Records (ADR-001 through ADR-039)
