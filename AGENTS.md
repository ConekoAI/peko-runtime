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
contracts and binaries live under `peko-rs/. Final workspace members:

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
- `peko-extension-host` — **deleted in F2** (folded into root
  `peko-rs/core/src/extensions/framework/`). The framework host impl
  now lives alongside the framework contracts it implements. The
  contract types (`ToolFunnel`, `CompletionEvent`/`InboxItem`,
  `default_*_dir` helpers) lifted into `peko-extension-api` so engine
  + provider-api can reach them without depending on root (cycle).
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
  impl in `peko-rs/core/src/engine/extension_core_funnel_compat.rs`. The
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
  `HookInput` directly. The actual `peko-rs/core/src/engine/agentic_loop.rs`
  lift is Phase 9b.N.5b.
- `peko-quota` — per-principal token quota (F18/F19). `QuotaMeter`,
  `QuotaScope`, `QuotaState`, `QuotaConfig`, `QuotaError`.
- `peko-tools-builtin` — **deleted in 0.Z-E** (2026-07-25). The cron port
  (`CronRuntime` trait + 3 cron tool impls + DTOs) lives natively in
  `peko-cron`; the `tool_search_metadata` static catalog helpers live
  natively in `peko-engine`. Bulk tool implementations have been in
  root `peko-rs/core/src/tools/builtin/` since F4. Port traits for the
  lifted tools (`TodoRuntime`, `SessionRuntime`, `SkillRuntime`,
  `SubagentRuntime`, `AsyncRuntime`) live in root alongside the tool
  impls.
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
peko-rs/
├── chat-log/               # Append-only chat-log storage (peko-chat-log, Phase 5)
├── cli/                    # CLI binary (`peko` bin, Phase 0.Z-B — extracted from `peko-rs/core/`)
│   └── src/
│       ├── commands/       # All `peko <subcommand>` handlers (lifted from root commands/)
│       └── main.rs         # Binary entry point + `clap` parser
├── cron/                   # Cron scheduler + idle + event-trigger (peko-cron, Phase 14.b)
│   ├── engine/             # Agentic loop core (peko-engine)
├── events/                 # Neutral agentic event contract (peko-events)
├── extension-api/          # Framework API contracts (peko-extension-api)
├── fs-persistence/         # File-lock + atomic append helpers (peko-fs-persistence, Phase 5)
│   ├── identity/           # DID identity + key storage (peko-identity, Phase 3)
├── message/                # Neutral message contract (peko-message)
├── observability/          # Observability hub (peko-observability, Phase 14)
├── peko-daemon/            # Long-running daemon binary (peko-daemon)
├── provider-api/           # Provider contract types (peko-provider-api)
├── protocol/               # IPC + tunnel wire-shape contracts (peko-protocol)
├── quota/                  # Per-principal token quota (peko-quota)
│   ├── session/            # Session persistence + InboxRegistry (peko-session, Phase 7)
├── subject/                # Canonical actor type (peko-subject, ADR-041)
└── tools-core/             # Tool execution API (peko-tools-core, F34 ToolExposure)
peko-rs/core/              # Root lib (Phase 0.Z-B — pure lib, no `[[bin]]`; CLI lifted to peko-rs/cli/)
├── src/
│   ├── agents/             # Agent management (stateless manager, config, lifecycle, prompts)
│   ├── auth/               # Authentication and authorization (principal, ownership, JWT, API keys)
│   ├── common/             # Shared services and core types (AgentService, config authority, vault, KV, types)
│   ├── daemon/             # HTTP daemon (Axum-based), health, info endpoints, AppState composition root
│   │   └── background_runtime/ # Generic process supervision (manager, supervisor, adapter traits)
│   ├── engine/             # Core agentic loop execution engine
│   ├── extensions/         # Extension framework + type implementations
│   │   ├── framework/      # Generic extension framework (ADR-017) — core, types, manager, async_exec, transport, services, protocols/shared, AND the F2 framework host impl (registry, hook dispatch, capability gate, async executor, transport, manager/store, scaffold, skill catalog, integration, SimpleRegistry/SharedRegistry)
│   │   ├── builtin/        # Built-in tool adapter
│   │   ├── gateway/        # Gateway adapter, runtime, protocol
│   │   ├── general/        # General extension adapter
│   │   ├── mcp/            # MCP adapter, runtime, protocol
│   │   ├── skill/          # Skill adapter
│   │   └── universal/      # Universal tool adapter and protocol
│   ├── identity/           # DID identity system, ed25519 keys, key storage, runtime identity
│   ├── ipc/                # Inter-process communication
│   ├── providers/          # LLM provider integrations (v3: catalog + resolver)
│   │   ├── adapters/       # OpenAI / Anthropic / openai-compatible ApiAdapters
│   │   ├── catalog.rs      # ProviderCatalog — runtime-owned, persisted to `~/.peko/providers.toml`
│   │   ├── templates.rs    # Built-in preset templates with curated model lists
│   │   ├── resolver.rs     # LlmResolver — precedence: override > session > agent > default > first
│   │   └── core.rs         # Unified Provider type
│   ├── registry/           # Local packaging/export/import and remote registry push/pull
│   │   ├── packaging/      # OCI-inspired .principal/.ext archive handling
│   │   │                   # (.agent/.team archives were retired with
│   │   │                   #  the principal-as-single-actor migration)
│   │   └── client.rs       # HTTP registry client
│   ├── session/            # Session JSONL management, branching, indexing, compaction
│   ├── tools/              # Built-in tools and tool factory
│   │   ├── builtin/        # Built-in tool implementations
│   │   ├── core/           # Tool trait definitions
│   │   └── registry/       # Tool factory and creation helpers
│   ├── tunnel/             # Tunnel / network layer — Pekohub A2A protocol, dispatcher, known runtimes
│   └── lib.rs              # Library surface (public domains + re-exports; no longer declares `commands`)
└── tests/                  # Integration tests + scenarios (41 files incl. docker/, common/, scenarios/)
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
- **Integration tests** live in `peko-rs/core/tests/` (Phase 0.Z-D moved
  `tests/` → `peko-rs/core/tests/`); the legacy PowerShell `e2e_tests/`
  tree was renamed to `e2e_tests_archive/` and lives at
  `peko-rs/core/e2e_tests_archive/` (a fixture source for the new
  Rust integration tests).
- **New CLI integration tests** (Phase B migration, then
  retargeted for the principal-as-single-actor model in the parity
  branch):
  - `peko-rs/core/tests/cli_send.rs` — `peko send` (targets a Principal; mock LLM)
  - `peko-rs/core/tests/cli_basics.rs` — Offline `peko principal`/`peko config`
  - `peko-rs/core/tests/cli_extensions.rs`, `peko-rs/core/tests/cli_extensions_l3.rs` — Extension system
  - `peko-rs/core/tests/cli_cron.rs` — `peko cron` create/list/delete (mock LLM)
  - `peko-rs/core/tests/cli_subagent.rs` — `peko subagent` + `agent_spawn` (mock LLM)
  - `peko-rs/core/tests/cli_tools.rs` — Built-in tools (Bash, Read, Write, …) (mock LLM)
  - `peko-rs/core/tests/cli_providers.rs` — Real-LLM tier (minimax, kimi)
  - `peko-rs/core/tests/cli_agent_signature.rs` — Principal packager signature
    verification (auto-discovered, run on demand via Make target)
- **Scenario tests** live in `peko-rs/core/tests/scenarios/` (registered explicitly in `peko-rs/core/Cargo.toml`):
  - `s1_local_agent_with_extensions` through `s6_principal_grant_revoke_roundtrip`
  - `tunnel_security` — Tunnel protocol security checks
- **Fixtures** for scenario tests live in `peko-rs/core/e2e_tests_archive/` (legacy PowerShell e2e tree, kept as a fixture source).
- **Benchmarks** live in `benches/`.
- Tests cover critical paths: extension lifecycle, agent lifecycle, provider operations, session operations, tool operations.

### CI tiers (see `.github/workflows/integration.yml`)

The workflow runs a path-aware, six-tier pipeline. Doc-only PRs (only
`*.md`, `PLAN.md`, `CHANGES.md`, `docs/**`) do **not** trigger CI at all
(workflow-level `paths` filter). For PRs that do trigger:

| Tier | Trigger | Wall-clock (warm) | Make target |
|---|---|---|---|
| `smoke` | `peko-rs/cli/**`, `peko-rs/core/src/**`, or `peko-rs/core/tests/**` changed (Phase 0.Z-B) | < 6 min | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --lib` |
| `lint` | `peko-rs/cli/**` or `peko-rs/core/src/**` changed (Phase 0.Z-B) | < 1 min | `bash scripts/check_module_boundaries.sh` |
| `lint-workspace` | `peko-rs/**`, root `Cargo.toml`/`Cargo.lock`, or `scripts/check_workspace_deps.py` changed (Phase 12b) | < 5 s | `python3 scripts/check_workspace_deps.py` |
| `unit-linux` | `peko-rs/cli/**`, `peko-rs/core/src/**`, or `peko-rs/core/tests/**` changed (Phase 0.Z-B) | ~3 min | `cargo test --lib` |
| `unit-windows` | Windows-specific paths or `[windows]` keyword / schedule / manual | ~5 min | `cargo test --lib` |
| `integration` | `peko-rs/core/tests/**`, `docker/**`, `Dockerfile*`, or workflow changed; or schedule / manual | ~10-15 min | `make docker-up` + `make test-integration` |
| `integration-llm` | `peko-rs/core/src/**` or `peko-rs/core/tests/**` changed AND `[llm]` keyword / schedule / manual | ~5 min extra | `make test-integration-llm` |

### Cleanup phases (post-migration)

The 13-member workspace migration is complete. Phase 15 retired the
4 pure re-export shims (`src/subject.rs`, `src/quota/mod.rs`,
`src/tools/core/mod.rs`, `src/common/types/message.rs`) — see
PR #298. Phase 16 retired 2 of the 7 trait-port compat impls
(`agent_view_compat.rs`, `async_inbox_compat.rs`) and removed the
dead `pub use peko_engine::agentic_loop::{...}` re-export from
`agentic_loop_compat.rs` — see PR #299. One trait-port adapter
remains (`background_compactor_factory_compat.rs`); it dies when
`BackgroundCompactor` itself lifts into `peko-engine` (deferred
per Phase 6 note). The 3,871-line `agentic_loop_compat.rs` keeps
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
| 8b.2 | Delete pure-duplicate framework shims (Phase 8b follow-up) | `peko::extensions::framework::{manager::discovery,async_exec::steer,protocols::shared::schema_filter}` (root paths preserved via `pub mod X { pub use peko_extension_host::* }` shims; see PR #294) |
| 8c.1 | Host trait ports + service lift (8b follow-up) | `peko::extensions::framework::{services::*,manager::{packaging,storage},transport::create_transport_shim}` now re-export from `peko_extension_host::{services,manager}::packaging/storage` + relocated `create_transport` to `peko::ipc::create_transport` (no root path breakage; callers migrate in 8c.2; see PR #295) |
| 8c.2 | Path sweep + framework shim deletion (8b follow-up) | `peko::extensions::framework::{manager::{packaging,storage},services::{config_service,reserved_params,tool_execution,mod},transport::{create_transport_shim,mod},protocols::shared::{process_transport,proxy_utils,validation},async_exec::executor/*}` (see PR #296) |
| 8a.L | Narrow framework/ tree further (8a leftover pure shims) | `peko::extensions::framework::{integration,protocols,scaffold,skill_catalog}` deleted; live callers in `src/commands/ext.rs`, `src/extensions/skill/{mod.rs,skill_runtime_impl.rs}`, `src/extensions/framework/store.rs` migrated to canonical `peko_extension_host::{scaffold,skill_catalog}` paths (see PR #297) |
| 9a | Extract `peko-engine` (pure-dependency subset) | `src/engine/{chunker,event_processor,events,execution,state,stream_buffer,stream_orchestrator,tool_stream,parallel_gate,error}` lifted (see PR #254) |
| 9b.1 | Move `engine::stream_types` into `peko-engine` | `ToolCallInfo` lifted to `peko_message` first (see PR #255) |
| 9b.N.1 | Lift `engine::async_completion` into `peko-engine` | introduces `AsyncCompletionLike` view trait bridging the duplicate `CompletionEvent` structs Phase 8 left behind (see PR #265) |
| 9b.N.2 | Lift `engine::tool_runtime` funnel via `ToolFunnel` trait port | trait is transient scaffolding (gone when Phase 8 bulk-moves `ExtensionCore`); unblocks tool_executor/compaction_orchestrator/agentic_loop (see PR #266) |
| 9b.N.3 | Lift `engine::tool_executor` via `SessionView` trait port | `ToolFunnel` widened to expose `is_parallelizable`/`pre_tool_use`/`post_tool_use`/`execute_tool_via_hook` (see PR #267) |
| 9b.N.4 | Lift `engine::compaction_orchestrator` via `CompactorBackend` + `SessionView` | 3 new hook-firing methods on `ToolFunnel`; `CompactionEntry::details` widened to `serde_json::Value` for trait-port compat (see PR #268) |
| 9b.N.5a | `agentic_loop` trait port shims | `AgentView` (12 methods) + `AsyncInboxLike` + `CapabilityDiffTracker` + `ToolFunnel` 3 hook methods (see PR #269) |
| 9b.N.5b.1-9e | Lift `engine::agentic_loop.rs` into `peko-engine` (split across 9 sub-PRs) | 2,285 prod + 3,182 test lines lifted; `ToolCallBlock`/`ThinkingBlock` → `peko_message`; `tool_search_metadata` re-pointed; `Arc<ExtensionCore>` → `Arc<dyn ToolFunnel>` (see PRs #270-#285) |
| 10a | Extract filesystem tools (Read/Write/Edit/Glob/Grep) + `expand_tilde` into `peko-tools-builtin` | `HOOK_TIMEOUT` lifted too (see PR #256) |
| 10b | Extract cron tools + `CronRuntime` port trait | root-side `DaemonCronAdapter` (see PR #257) |
| 10c | Extract async tools + `AsyncRuntime` port trait | per-agent `Weak<ExtensionCore>` + capabilities snapshot moved into the runtime (see PR #258) |
| 10d | Extract session/task/skill tools with their port traits (`SessionRuntime`, `TodoRuntime`, `SkillRuntime`) | root-side adapters (see PR #259) |
| 10e | Extract `AgentTool` + `SubagentRuntime` port into `peko-tools-builtin::messaging` | root owns `SubagentExecutorRuntime` adapter (see PR #260) |
| 11a | Create `peko-protocol` crate (IPC auth envelope + constants) | bulk `RequestPacket`/`ResponsePacket`/`TunnelMessage` deferred (see PR #261) |
| 11b | Separate `peko-daemon` binary artifact | surgical `pub(crate)` → `pub` widening for `Daemon`/`DaemonConfig` (see PR #262) |
| 11c | Route CLI daemon-spawn through `peko-daemon` | `DaemonProcessService::spawn_daemon_with` switches to invoke `peko-daemon` next to `current_exe()` (see PR #263) |
| 12 | Lift `peko-daemon` into its own crate | daemon internals stay `pub(crate)` from the binary (see PR #264) |
| 12b | Deterministic workspace dep-graph check | `scripts/check_workspace_deps.py` (see PR #274) |
| 12c | Root facade intent cleanup | legacy facade cruft removed (see PR #275) |
| 12 | Foreground switch launches `peko-daemon` binary | CLI `--foreground` re-execs into `peko-daemon` instead of in-process fork (see PR #276) |
| 13 | Extract remaining runtime domains | `peko::daemon::*` absorbed into `peko-daemon` (PR #264); `peko::observability::*` landed in Phase 14 (PR #300); `peko::cron::*` landed in Phase 14 (PR #301); `peko::principal::config/peer/memory/agent_prompt/factory` lifted in Phase 14.c.1 (PR #302); `peko::principal::capability_evaluator/extension_store` + `OutputFormat` + `builtin_tools` lifted in Phase 14.c.2a (PR #303); `peko::principal::manager/context/agent_runner/routers/slash` still pending |
| 14 | Extract observability (✅ merged PR #300) + cron (✅ merged PR #301) + principal (✅ 14.c.1 merged PR #302; ✅ 14.c.2a merged PR #303; 14.c.2b still pending) | `peko::observability::*`; `peko::cron::*`; `peko::principal::config/peer/memory/agent_prompt/factory` + `capability_evaluator/extension_store` + `runtime::{OutputFormat,builtin_tools}`; manager/context/routers/slash still in root |
| 15 | **Delete pure re-export shims** (✅ merged PR #298, 2026-07-24) | `peko::subject::*`, `peko::quota::*`, `peko::tools::core::*`, `peko::common::types::message::*` |
| 16 | Delete trait-port compat impls (✅ merged PR #299, 2026-07-24) | `peko::engine::{agent_view_compat,async_inbox_compat}` deleted; `background_compactor_factory_compat` retained (orphan rule — needs `BackgroundCompactor` lift deferred from Phase 6); `agentic_loop_compat` narrowed (dead re-export removed, 3,871-line test module stays) |
| 17 | Build `peko-engine-test-support` + move engine tests | (no root path breakage; tests relocate) |
| 18 | Move deferred built-in tools (`BashTool`, `ToolSearchTool`, `AgentCatalog`) + `tool_runtime.rs` | `peko::tools::builtin::bash`, `peko::tools::builtin::tool_search`, `peko::tools::builtin::agent_catalog`, `peko::engine::tool_runtime` |
| F2 | **Fold back `peko-extension-host`** (✅ merged 2026-07-24) | 62 prod files → `peko-rs/core/src/extensions/framework/`; trait-only deps (`ToolFunnel`, `CompletionEvent`, `default_*_dir`) lifted into `peko-extension-api` |
| F3 | **Fold back `peko-principal`** (✅ merged 2026-07-25) | 11 prod files → `peko-rs/core/src/principal/`; CLI bin's `crate::principal::*` had to use full `peko_core::principal::*` prefix; pre-flight `grep '^pub '` on moved files prevents re-exporting types that don't exist |
| F4 | **Fold back partial `peko-tools-builtin`** (✅ merged 2026-07-25) | 30 prod files (~9500 LOC) → `peko-rs/core/src/tools/builtin/`; sat retained as cron-port-only sat (~1200 LOC) for cycle preservation (peko-cron re-exports DTOs; engine reaches `TOOL_SEARCH_TOOL_NAME`) |
| F7 | **AGENTS.md + dep-graph updates for F2/F3/F4 foldbacks** (✅ merged 2026-07-25) | no root path breakage; doc + script updates |
| 0.Z-E | **Delete `peko-tools-builtin`** (✅ merged 2026-07-25) | cron port + DTOs + 3 cron tools → `peko-cron/src/tools/`; `tool_search_metadata` → `peko-engine/src/tool_search_metadata.rs` (already canonical); `peko_cron::tools::{CronRuntime, set_global_runtime, global_runtime}` is the new home; `DaemonCronAdapter` stays in root (cycle prevention — depends on `DaemonClient`); 12 forbidden-edge entries + 1 header docstring removed from `check_workspace_deps.py` |

#### Current crate layout (20 workspace members, 2026-07-25)

Already extracted (`peko-rs/):

- `auth` — auth + DID helpers.
- `chat-log` — append-only chat-log storage.
- `cron` — cron scheduler + idle detection + event-trigger (Phase 14.b).
- `engine` — agentic loop core (Phase 9 series).
- `events` — neutral agentic event contract.
- `extension-api` — extension framework contracts (the trait-port surface; engine + provider-api reach through it).
- `fs-persistence` — filesystem persistence helpers.
- `identity` — DID identity + key storage.
- `message` — neutral message contract.
- `observability` — observability hub (audit log + metrics + tracing; Phase 14 entry).
- `peko-daemon` — daemon binary + lib (Phase 12).
- `plan` — file-backed Plan DAG primitive for the principal harness (schema + storage + per-Principal `PlanPort` + 7 `Plan*` built-in tools + `ContextInjectionKind::Plan` resume-on-start).
- `protocol` — IPC + tunnel wire contracts (Phase 11a).
- `provider-api` — provider contracts.
- `providers` — concrete provider implementations.
- `quota` — per-principal token quota.
- `session` — session storage + `InboxRegistry` (Phase 7).
- `subject` — canonical actor type + identifier newtypes.
- ~~`tools-builtin`~~ — **deleted in 0.Z-E** (2026-07-25). Cron port + DTOs + 3 cron tools now live natively in `peko-cron`; `tool_search_metadata` lives natively in `peko-engine`.
- `tools-core` — tool API.

Root `peko` (lib) — thin composition only; the bin is `peko-rs/cli`.

**F-series foldbacks (Phases F2/F3/F4, 2026-07-24 / 2026-07-25):**

Three satellites that satisfied the "≤2 external consumers + sat-internal cross-deps"
pattern were folded back into the root crate to remove the indirection:

- **F2 (2026-07-24):** `peko-extension-host` (62 prod files) → `peko-rs/core/src/extensions/framework/`. Trait-only deps (`ToolFunnel`, `CompletionEvent`/`InboxItem`, `default_*_dir`) lifted into `peko-extension-api` so engine reaches them without depending on root (cycle preservation).
- **F3 (2026-07-25):** `peko-principal` (11 prod files) → `peko-rs/core/src/principal/`. CLI bin's `crate::principal::*` (CLI's `crate::` ≠ root crate) had to use the full `peko_core::principal::*` prefix; pre-flight `grep '^pub '` on moved files prevents re-exporting types that don't exist.
- **F4 (2026-07-25):** `peko-tools-builtin` partial foldback. Bulk tool implementations (~9500 LOC / 30 files, fs/async_control/session/skill/messaging/tasks + paths.rs) lifted into `peko-rs/core/src/tools/builtin/`. Sat retained as a cron-port-only sat (~1200 LOC) because `peko-cron` re-exports the cron DTOs and engine reaches `TOOL_SEARCH_TOOL_NAME` for the static catalog entry — full deletion would force peko-cron → peko_core and engine → root cycles.
- **0.Z-E (2026-07-25):** **Delete `peko-tools-builtin`**. Cron port (`CronRuntime` trait + 3 cron tool impls + 4 DTOs + 10 helpers + global runtime registry) lifted into `peko-cron` as `peko-rs/cron/src/tools/`. `tool_search_metadata` lifted into `peko-engine` (was already canonical there). `DaemonCronAdapter` stays in root (`peko-rs/core/src/daemon/cron_runtime.rs`) — depends on `peko_core::ipc::DaemonClient`, can't move into `peko-cron` without creating a lib→sat cycle. 6 cron import sites + 5 tool_search import sites updated. 12 forbidden-edge entries + 1 header docstring deleted from `check_workspace_deps.py`. Workspace: 21 → 20 members. Pattern C closed.

Planned for later phases (not yet extracted):

- `peko-rs/engine-test-support` — dev-deps-only fixtures for engine tests (Phase 17).
- `peko-rs/agents` — agent lifecycle + subagent execution (still in root `src/agents/`).
- `peko-rs/registry` — packaging + registry client + trust store (still in root `src/registry/`).
- `peko-rs/tunnel` — tunnel protocol + A2A dispatcher (still in root `src/tunnel/`).
- `peko-rs/ipc` — IPC server + handlers (still in root `src/ipc/`).
- `peko-rs/principal` — **deleted in F3** (2026-07-25). Principal DTOs + memory + peer + agent_prompt + config + capability_evaluator + extension_store + `runtime::{OutputFormat,builtin_tools}` + `slash::extension_row` all live in `peko-rs/core/src/principal/`. The runtime-coupled subset (manager/context/agent_runner/routers/slash dispatcher) stays in root next to the `Principal` struct definition.
- `peko-rs/extension-host` — **deleted in F2** (2026-07-24). Framework host impl (registry, hook dispatch, capability gate, async executor, transport, manager/store, scaffold, skill catalog, integration, framework services, `SimpleRegistry`/`SharedRegistry`) live in `peko-rs/core/src/extensions/framework/`. The trait-only deps (`ToolFunnel`, `CompletionEvent`/`InboxItem`, `default_*_dir`) lifted into `peko-rs/extension-api/` so engine + provider-api can reach them without depending on root (cycle).
- `peko-rs/tools-builtin` — **deleted in 0.Z-E** (2026-07-25). Cron port + DTOs + 3 cron tools moved natively into `peko-cron` as `peko-rs/cron/src/tools/`; `tool_search_metadata` moved natively into `peko-engine` (was already canonical). Bulk tool implementations live in root (`peko-rs/core/src/tools/builtin/{fs,async_control,session,skill,messaging,tasks}/`) since F4.

#### Cleanup invariant

**Every historical `peko::...` import path is intentionally broken.**
No new `pub use peko_*::*` shims. The 4 pure-shim modules
(`src/subject.rs`, `src/quota/mod.rs`, `src/tools/core/mod.rs`,
`src/common/types/message.rs`) were deleted in Phase 15 (merged
PR #298, 2026-07-24). Every other historical path breaks across
Phases 3–14, 17, and 18.

#### Root facade boundary rule (Phase 1)

Every `pub mod` in `src/lib.rs` carries an inline `[kept]` or
`[extract:phase-N]` tag. The rule:

- **Root may declare composition modules** — `pub mod commands;`
  (CLI handlers) and `pub(crate) mod common;` (binary-composition
  wiring after Phase 14 trim) are the only modules that remain
  `pub`/`pub(crate)` after the cleanup completes.
- **Root must NOT `pub use peko_*::*` from another crate.** The 4
  pure-shim modules (`src/subject.rs`, `src/quota/mod.rs`,
  `src/tools/core/mod.rs`, `src/common/types/message.rs`) were
  deleted in Phase 15 (PR #298). No new shims may be added.
- **Every domain `pub mod` becomes `[extract:phase-N]`.** When the
  extraction PR lands, the root entry is removed or narrowed to
  `pub(crate)` so the public surface of `peko` (lib) reflects only
  the CLI binary + thin composition.

### Multi-model subagents (merged PR #346, commit `ee4895a6`)

The merged feature ships in 4 layers. None of this is tunable through
the CLI today — operators edit `principal.toml` directly (or set
config keys when adding a model) for the new quota knobs.

- **Per-spawn model choice** — `AgentTool`'s `model: Option<String>`
  JSON schema field is no longer a no-op. Threading:
  `SpawnRequest.model` → `ExecutionConfig.model_override` →
  `SubagentRuntime::resolve_agent_config` →
  `SubagentExecutor::execute_subagent_task`. Pre-flight
  `SpecGate::check` runs against the override so a parent picking
  Opus for a tool-using subagent with an opus-without-vision spec
  is refused **before** any LLM traffic. The `SpecGate` refusal
  surfaces as `SpawnError::SpecGateFailed { model_id, reason }`
  (post-merge audit fix) for parity with the other typed
  `SpawnError` variants.
- **F39 production quota wiring** — every `SubagentExecutor::new`
  call site chains `.with_quota_meter(...)` and
  `.with_peer_meter(...)` from `ctx.quota_meter()` /
  `ctx.peer_meter()`. Three sites: `agents/agent.rs` (root agent),
  `principal/agent_runner.rs` (production builder), and
  `agents/subagent_executor.rs` (recursive sub-subagents inherit
  parent meters so they charge the spawning principal, not
  `unlimited()`). Comment at `subagent_executor.rs:1175-1182`
  spells out the intent.
- **`ModelConfig.note: Option<String>`** — sibling of `spec`, NOT
  inside `ModelSpec` (spec stays capability-pure). 500-char cap,
  validated by `upsert()`. CLI: `peko model add/edit --note "..."`,
  `peko model edit --note ""` clears, `peko model show` prints the
  block, `peko model list --detailed` truncates to 80 chars.
- **`model_list` builtin tool** at
  `peko-rs/core/src/tools/builtin/model_list.rs`. Mirrors
  `ToolSearchTool` exactly: `Weak<ModelCatalog>`, exposure
  `Direct`, `parallelizable() == true`, schema-driven `execute()`.
  Output shape `{count, entries: [ModelSummary, …]}` is
  byte-for-byte the same as `peko model list --json` (single
  source of truth via `model_summary_from_config`).
  - Filter args: `filter` (`vision|tools|thinking|priced|json_mode`)
    AND-combined with `contains <NEEDLE>` (case-sensitive on `id`,
    case-insensitive on `display_name` and `note`). `contains
    "cron"` finds a model tagged "very cheap, use it for cron"
    even when the id is unrelated.
  - Registration funnel: `Agent::init_builtins_async` only
    instantiates when **both** `enable_model_list: true` (default)
    AND a bound `Arc<ModelCatalog>` are present. CLI stateless
    path without a resolver skips registration silently.
- **Two-gate cost ceiling** in `peko-rs/quota/src/config.rs`:
  - `cost_per_call_max: Option<f64>` — **spawn-time pre-flight**
    in `SubagentExecutor::spawn_and_execute`. Conservative 4K-in +
    1K-out token projection × `PricingHint.input_per_million` /
    `output_per_million`. Refuses the spawn with
    `SpawnError::CostCeilingExceeded { estimated, ceiling,
    model_id }` before any LLM traffic.
  - `budget_per_cycle: Option<f64>` — **mid-stream rolling cap**
    via `QuotaMeter::try_charge_with_cost` (called from
    `StackedMeteredProvider`). Folds `cost = input/1e6 *
    input_per_million + output/1e6 * output_per_million` alongside
    token/request counters. The two gates are complementary: spawn
    pre-flight catches obvious misuse (picking Opus with a $0.001
    cap), mid-stream catches over-runs of the projection.
  - Persistence: `QuotaState.cost_usd: Option<f64>` carries the
    running cycle spend across restarts.
  - **User-facing CLI gap:** `cost_per_call_max` is the only Phase-3
    knob requiring hand-edit of `principal.toml` — every other
    quota field is settable via `peko quota set`. A
    `peko quota set-cost-ceiling <principal> <usd>` helper is
    deferred to a follow-up PR.
- **`AuditSink` trait port** at `peko-rs/engine/src/audit_sink.rs`.
  The trait carries a typed `AuditEventView` (not the heavier
  `AuditEvent`) so `peko-engine` stays decoupled from
  `peko-observability`. Orphan-rule workaround: free function
  `severity_into_obs(EngineSeverity) -> ObsSeverity` at
  `peko-rs/core/src/observability/mod.rs` (no `impl From`). The
  production impl `ObservabilityAuditSink` holds
  `Arc<peko_observability::Observability>` and dispatches via
  `tokio::task::spawn` (fire-and-forget; panics surface through
  the multi-thread runtime's background logging).
- **`seen_models.json` first-use state** at
  `peko-rs/core/src/principal/seen_models.rs`. Path:
  `<workspace_path>/seen_models.json`. Atomic-write pattern
  (`serialize → .tmp → sync_all → rename`) mirrors
  `daemon/config_drift.rs:17-21, 274-298`. Shape:
  `{version: 1, models: [...]}` (`BTreeSet<String>` for stable
  JSON). Tolerated by `PrincipalContext::init`: corrupt-file ⇒
  warn + fall open to empty.
- **`AgenticLoop` emissions** — `model.selected` fires after
  every successful LLM call. Severity: `Warning` when
  `audit_first_use_for_model(model_id)` returns `true` (first
  `(principal, model)` use), `Info` thereafter. `details:
  {first_use: bool}`. The lookup closure captures
  `Arc<Mutex<BTreeSet<String>>>` so it stays `'static + Send +
  Sync`; the engine never holds a `&PrincipalContext` reference.

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
2. **Add code in the appropriate `peko-rs/core/src/<module>/`** — Follow existing module structure.
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
  - `peko-rs/core/src/extensions/framework/` contains the **generic extension framework** — core, types, manager, async_exec, transport, services, protocols/shared, and adapters. It has **zero dependencies** on concrete extension type implementations under `peko-rs/core/src/extensions/<type>/`.
  - `peko-rs/core/src/extensions/<type>/` (builtin, gateway, general, mcp, skill, universal) contains **extension type implementations**. Each type lives in its own directory and should not import from sibling extension types.
  - `peko-rs/core/src/extensions/framework/core/` has zero dependencies on `crate::extensions::<type>`, `crate::daemon`, or `crate::tools`.
  - **Execution primitives** (`ToolContext`, `ToolError`, `AbortSignal`, `ToolResult`, `ToolWithContext`, `ToolContextAdapter`, `ToolProgressEvent`) and the `ContextSource` trait live in `tools::core/exec.rs` and `tools::core/context_source.rs` (moved from `extensions::framework::types/` and `extensions::framework::protocols/shared/`). The blanket impl `impl<T: Tool> ToolWithContext for T` is in place now that the cycle is broken.
  - **Dependency direction:**
    - `extensions::framework` depends on `tools::core` (one-way, for `Tool`, `ToolContext`, `ContextSource`, and other execution primitives). It does **not** depend on `tools::builtin` or any concrete extension type.
    - `tools::core` does **not** depend on `extensions::framework`. The previous bidirectional loop is broken.
    - `tunnel` depends on `tools::core` (for the `Tool` trait) and **does not** depend on `agents` in production code.
    - `agents` depends on `tunnel` (for the `AgentMessageService` trait used by `SendPeerTool`) and does **not** depend on `tunnel::principal_send_tool`'s concrete types.
    - `extensions::framework` does **not** depend on `agents`, `tunnel`, `daemon`, or `principal` (enforced by `check_module_boundaries.sh` Rules 5 and 6).
  - Cycles 4 (`tools::core ↔ extension::types`) and 5 (`tunnel ↔ agents`) from `PLAN.md` §2.5 are now actually broken (not reshuffled).
  - **F2 / F3 foldback consequence (2026-07-25):** the framework host impl
    (`registry`, `hook dispatch`, `capability gate`, `async executor`,
    `transport`, `manager/store`, `scaffold`, `skill catalog`,
    `integration`, `SimpleRegistry`/`SharedRegistry`) and the principal
    DTOs (`config`, `peer`, `memory`, `agent_prompt`, `extension_store`,
    `capability_evaluator`, `runtime::{OutputFormat,builtin_tools}`,
    `slash::extension_row`) all live in root now. The
    `extensions::framework` module boundary is unchanged — the rule
    "framework doesn't depend on principal" is now enforced within a
    single crate (root) instead of across the
    `peko-rs/{extension-host,principal}` sat pair.
  - **F4 foldback consequence (2026-07-25):** the bulk of built-in
    tool impls live in root `tools::builtin/{fs,async_control,session,
    skill,messaging,tasks}`. `CronRuntime` port + cron DTOs +
    `tool_search_metadata` static helpers were retained in
    `peko-tools-builtin` so `peko-cron` re-exports cycle through. New
    deps in root `Cargo.toml` lifted from the sat: `glob`, `regex`,
    `chrono-tz`.
  - **0.Z-E foldback consequence (2026-07-25):** `peko-tools-builtin`
    deleted. Cron port + DTOs + 3 cron tool impls + helpers + global
    registry all live natively in `peko-cron` as `cron::tools::*`. The
    `tool_search_metadata` static helpers live natively in
    `peko-engine`. `DaemonCronAdapter` (which implements
    `CronRuntime` for the daemon side) stays in root. It was an
    IPC-loopback adapter until the 2026-08-07 round-3 field-test fix
    pack: it now holds `Arc<daemon::cron_ops::CronOps>` (the owner-cap
    gate + schedule/history writes extracted from the IPC cron
    handler) and operates in-process — no `DaemonClient`, no socket
    round trip. It still can't move into `peko-cron`: `CronOps`
    depends on root's `PathResolver` / `PrincipalManager` /
    `RuntimeAuthority`, which would force `peko-cron → peko_core`
    (lib→sat cycle). The new root
    `Cargo.toml` deps from the cron-tools migration: `chrono-tz` was
    already lifted in F4; `uuid` + `async-trait` joined `peko-cron`'s
    direct deps.
  - **2026-08-08 `send_peer` unification:** `principal_send` was
    renamed `send_peer` and gained a user branch (fire-and-forget
    notes to a human peer's conversational session) alongside the
    principal branch (the legacy sync RPC; wire/IPC names unchanged).
    Delivery goes through the `PeerMessenger` port
    (`peko-rs/core/src/principal/messenger.rs` — trait + global
    registry mirroring the `CronRuntime` pattern, installed by the
    daemon at startup). Originating-peer resolution derives from the
    calling session id (`root:{peer}` / v2 keys / subagent-suffix
    stripping / spawn-overlay `parent_session_id` walk) — NOT from
    `ToolContext.peer_id`, which is never populated in production.
    Subagents get the tool via `SubagentExecutor`'s
    `caller_principal_did` OnceLock, propagated by
    `Agent::with_caller_principal_did`. Cron `message` jobs are the
    new `CronJobAction::Notify` (pure delivery, no agent turn);
    `Send` keeps its deferred-`peko send` turn semantics.
  - `peko-rs/cli/src/commands/` should delegate to services and not import low-level persistence/packaging modules directly (e.g. `peko_core::registry::packaging::`, `peko_core::common::services::config_authority::`, `peko_core::identity::storage::`, `peko_core::session::jsonl::`, `peko_core::session::metadata_controller::`). After Phase 0.Z-B the `commands/` module lives in the `peko-cli` binary satellite; imports from `crate::X` inside CLI files become `peko_core::X`. `scripts/check_module_boundaries.sh` enforces this as an advisory rule while existing violations are being resolved.

- **Workspace dependency rules (Phase 12b):** the path-grep `check_module_boundaries.sh` covers in-`src/` rules. For crate-level edges — `peko-provider-api` MUST NOT depend on `peko-engine`, `peko-protocol` is `serde`+`serde_json` only, the leaf crates (`peko-message` / `peko-subject` / `peko-tools-core` / `peko-events`) MUST NOT depend on any other `peko-*`, etc. — `scripts/check_workspace_deps.py` reads every `peko-rs/*/Cargo.toml` and asserts a 71-entry forbidden-edge table derived from the workspace-migration plan. Run locally with `python3 scripts/check_workspace_deps.py` (add `--print-graph` to see the actual edges). The script fires automatically in the `lint-workspace` CI job whenever `peko-rs/**`, root `Cargo.toml`, `Cargo.lock`, or the script itself change. New forbidden edges surface here before a PR can land; adding a rule is one line in `FORBIDDEN_EDGES` with a doc comment explaining the rationale.

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
