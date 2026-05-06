# Issue 019: God Files & Mixed Concerns (High Severity)

**Status:** In Progress — Phase 4 Complete  
**Labels:** `refactoring`, `architecture`, `high-severity`, `commands`, `engine`, `daemon`

## Summary

Multiple files in the codebase violate the Single Responsibility Principle by mixing 6–10 distinct concerns in a single module. The worst offenders are in the commands layer and the engine core loop. These files are difficult to test, expensive to change, and prone to merge conflicts.

## Affected Files (Current State)

| File | Lines | Mixed Concerns |
|------|-------|----------------|
| `src/commands/ext.rs` | ~919 | Extension lifecycle, whitelist manipulation, config persistence, validation, daemon IPC, Tier 1/2/3 manifest detection |
| `src/commands/ext.rs` (target) | ~400 | (Phase 1 complete — extracted to domain modules, remaining is CLI dispatch + rendering) |
| `src/commands/session.rs` | ~450 (391 non-test) | Session ops, compaction algorithm, history display, path resolution, active-session resolution |
| `src/commands/session.rs` (target) | ~400 | (Phase 2 complete — extracted to `SessionService`, `session/presentation.rs`, `compaction/cli.rs`; remaining is CLI dispatch + thin rendering) |
| `src/engine/agentic_loop.rs` | ~560 | LLM iteration, tool execution, system prompt construction, skill loading, session management, compaction, event emission, legacy fallbacks |
| `src/engine/agentic_loop.rs` (target) | ~500 | (Phase 3 complete — extracted `CompactionOrchestrator`, `ToolExecutor`, `SystemPromptService`, `synthetic_stream`; remaining is loop skeleton + stream processing) |
| `src/daemon/mod.rs` | ~719 | Cron execution engine, job delivery, session janitor, async task janitor, daemon lifecycle, event filtering, agent config loading |

## Positive Examples (Model to Follow)

- `src/commands/team.rs` (~427 lines) — delegates all business logic to `TeamService` and only handles CLI rendering.
- `src/commands/agent/handlers.rs` (~460 lines) — delegates to `AgentService` and only handles CLI rendering.
- `src/commands/agent.rs` (~204 lines) — thin dispatcher, delegates to `handlers.rs`.

## Root Cause

The project has a well-defined service layer (`src/common/services/`) and a clean separation pattern (team/agent commands), but four modules were written before these conventions solidified. They mix **business logic**, **persistence**, **presentation**, **orchestration**, and **infrastructure concerns** in a single file.

## Proposed Solution: Four-Phase Extraction (Cohesion-First)

The fix follows a consistent pattern: **extract business logic into the module that owns the abstraction, leave only CLI dispatch + rendering in commands, leave only loop coordination in engine, leave only lifecycle coordination in daemon.**

No backward compatibility is required (dev stage).

**Core principle:** Put code next to the domain it belongs to. Extension concerns go in `src/extension/`. Session concerns go in `src/session/`. Prompt concerns go in `src/prompt/`. Only truly cross-cutting concerns (agent config, team config) belong in `common/services/`.

---

### Phase 1: `src/commands/ext.rs` → Extract to Domain Modules

**Goal:** Reduce `ext.rs` to ~250 lines (enum definition + thin dispatch + rendering).

#### 1a. Extract `ExtensionConfigService` → `src/extension/services/` ✅ **DONE**
- **Source:** `ExtensionConfig` struct (lines 1063–1144) — private struct with TOML file I/O, scoped key-value logic.
- **Destination:** `src/extension/services/config_service.rs`
- **Interface:**
  ```rust
  pub struct ExtensionConfigService { data_dir: PathBuf }
  impl ExtensionConfigService {
      pub fn new(data_dir: impl Into<PathBuf>) -> Self;
      pub fn load(&self, extension_id: &str) -> Result<HashMap<String, serde_json::Value>>;
      pub fn save(&self, extension_id: &str, config: &ExtensionConfigData) -> Result<()>;
      pub fn set(&self, extension_id: &str, scope: ConfigScope, key: &str, value: serde_json::Value) -> Result<()>;
      pub fn unset(&self, extension_id: &str, scope: ConfigScope, key: &str) -> Result<bool>;
      pub fn show(&self, extension_id: &str, scope: ConfigScope) -> Result<HashMap<String, serde_json::Value>>;
      pub fn global(&self, extension_id: &str) -> Result<HashMap<String, serde_json::Value>>;
  }
  pub enum ConfigScope { Global, Team(String), Agent(String, String) }
  ```
- **Rationale:** `src/extension/services/` already exists and hosts `ReservedParamsService` and `ToolExecutionService`. Extension config persistence is an extension concern — it belongs here. This keeps all extension-layer services in one place.
- **Commit:** `5720e1b`

#### 1b. Extract `ExtensionValidationService` → `src/extension/adapters/` ✅ **DONE**
- **Source:** `handle_validate` (lines 1251–1414) — Tier 1/2/3 detection hierarchy with per-type adapter calls.
- **Destination:** `src/extension/adapters/validation.rs`
- **Interface:**
  ```rust
  pub struct ExtensionValidationService;
  impl ExtensionValidationService {
      pub async fn validate(path: &Path, verbose: bool) -> Result<ValidationReport>;
  }
  pub struct ValidationReport { pub detected_type: String, pub errors: Vec<String>, pub warnings: Vec<String> }
  ```
- **Rationale:** Validation uses `SkillAdapter`, `McpAdapter`, `UniversalToolAdapter`, etc. — all extension type adapters. It belongs in `extension/adapters/` alongside the trait it validates against (`ExtensionTypeAdapter`). This also enables API-based validation (`POST /extensions/validate`).
- **Commit:** `5720e1b`

#### 1c. Extract `DaemonClientService` → `src/ipc/` ✅ **DONE**
- **Source:** `handle_start`, `handle_stop`, `handle_restart`, `handle_status` (lines 954–1061) — inline daemon IPC client code, copy-pasted 4×.
- **Destination:** `src/ipc/client_service.rs`
- **Interface:**
  ```rust
  pub struct DaemonClientService;
  impl DaemonClientService {
      pub async fn ext_start(id: &str) -> Result<String>; // returns extension_id
      pub async fn ext_stop(id: &str) -> Result<String>;
      pub async fn ext_restart(id: &str) -> Result<String>;
      pub async fn ext_status(id: &str) -> Result<RuntimeStatus>;
  }
  pub struct RuntimeStatus { pub state: String, pub restart_count: u32, pub last_error: Option<String> }
  ```
- **Rationale:** IPC is infrastructure. `src/ipc/` already contains `DaemonClient`. The service belongs here, next to the transport it uses. Not in `common/services/` (which is for business logic), not in `extension/` (which knows nothing about daemon IPC).
- **Commit:** `5720e1b`

#### 1d. Simplify `handle_enable` / `handle_disable` ✅ **DONE**
- **Source:** `add_tool_to_agent_whitelist` (lines 646–720), `handle_enable_builtin` (lines 722–771), `handle_disable_builtin` (lines 811–860).
- **Action:** The whitelist manipulation already partially uses `ConfigAuthorityImpl` (`enable_tool_sync`). The remaining `read_dir` loop for team-level updates should move to `ConfigAuthorityImpl::enable_tool_for_team(team, tool)` and `ConfigAuthorityImpl::disable_tool_for_team(team, tool)`.
- **ExtensionCore hook manipulation** (lines 756–768, 844–857): Replace direct `global_core().enable_hook()` / `disable_hook()` calls with a new method on `extension::services::Services` (the existing extension services container): `Services::enable_builtin_hooks(capability)` / `disable_builtin_hooks(capability)`. This prevents commands from directly touching the global core.
- **Commit:** `5720e1b`

#### 1e. Post-Phase 1 `ext.rs` structure ✅ **DONE**
```rust
// ~200 lines: ExtCommands enum
// ~50 lines:  handle_ext_command dispatcher
// ~4 lines each: handle_start, handle_stop, handle_restart, handle_status (delegate to DaemonClientService)
```
- **Actual:** Reduced from ~1,359 to ~919 lines (32% reduction). All specified extractions complete. Remaining code is CLI dispatch + rendering. Further reduction to ~400 lines would require extracting `handle_list` rendering and `handle_debug` presentation (not specified in Phase 1).
- **Commit:** `5720e1b`

---

### Phase 2: `src/commands/session.rs` → Service + Presentation Extraction

**Goal:** Reduce `session.rs` to ~300 lines (enum definition + dispatch + rendering).

#### 2a. Deduplicate Active-Session Resolution ✅ **DONE**
- **Source:** Copy-pasted block (lines 189–203, 217–231, 272–286).
- **Action:** Moved into `SessionService::resolve_session_id(agent, team, user, session_id)`.
- **Result:** All three commands (`show`, `branch`, `compact`) now call the single service method.

#### 2b. Move Compaction CLI Logic to `src/compaction/` ✅ **DONE**
- **Source:** `compact_session` (lines 844–1038) — truncation logic, summary generation, token estimation, recording.
- **Destination:** `src/compaction/cli.rs` (new module)
- **Interface:**
  ```rust
  pub struct SessionCompactor;
  impl SessionCompactor {
      pub async fn compact(&mut self, session: &mut Session, instruction: Option<String>) -> Result<CliCompactionResult>;
      pub async fn dry_run(&self, session: &Session, _instruction: Option<String>) -> Result<DryRunReport>;
  }
  pub struct CliCompactionResult { pub messages: Vec<LlmMessage>, pub entry: CompactionEntry, pub tokens_saved: usize; }
  pub struct DryRunReport { pub estimated_tokens: usize, pub context_window: usize, pub percent: usize, pub message_count: usize, pub messages_to_compact: usize; }
  ```
- **Rationale:** The compaction algorithm is a compaction concern. `src/compaction/` already has `background.rs`, `registry.rs`, `turn_boundaries.rs`, etc. The CLI-specific compaction flow belongs here, next to the `Compactor` it uses.
- **Tests:** `test_dry_run_empty_session`, `test_compact_truncates_messages`.

#### 2c. Move History Presentation to `src/session/presentation.rs` ✅ **DONE**
- **Source:** `HistoryDisplayEntry`, `history_event_to_display`, `print_history_event` (lines 525–679).
- **Destination:** `src/session/presentation.rs`
- **Interface:**
  ```rust
  pub fn format_history_event(index: usize, event: &HistoryDisplayEntry) -> String;
  pub fn history_event_to_display(event: HistoryEvent) -> Option<HistoryDisplayEntry>;
  pub fn render_session_list(sessions: &[SessionInfo], team: &str, agent: &str, active_session_id: Option<&str>);
  pub fn render_session_details(entry: &SessionInfo, team: &str, agent: &str);
  pub fn render_session_history(events: &[HistoryDisplayEntry]);
  pub fn render_branch_success(...);
  pub fn render_delete_prompt(...);
  pub fn render_compact_dry_run(...);
  pub fn render_compact_success(...);
  ```
- **Rationale:** Presentation converts `session::HistoryEvent` (a session type) into display format. It belongs in `src/session/` because it operates on session types. Different channels (CLI, TUI, web) can reuse the same DTOs but format them differently.
- **Tests:** `test_truncate`, `test_history_event_to_display_message`, `test_format_history_event_session`.

#### 2d. Replace Direct `MetadataController` / `SessionStorage` Usage ✅ **DONE**
- **Source:** Direct `MetadataController::new` (lines 345, 453, 745), `Session::open_by_id` (line 860), `SessionStorage::new` (line 969).
- **Action:** Route all operations through `SessionService` (already in `common/services/`):
  - `list_sessions_from_disk` → `SessionService::list_sessions_synced(agent, team)`
  - `MetadataController::delete_session` → `SessionService::delete_session(agent, team, session_id)`
  - `Session::open_by_id` → `SessionService::open_session(agent, team, session_id, user)`
  - `SessionStorage::append_compaction` → `Session::record_compaction(...)` (via `SessionCompactor` in `src/compaction/cli.rs`)
- **Additional methods added to `SessionService`:** `resolve_session_id`, `open_session`, `list_sessions_synced`, `get_session_synced`, `get_sessions_dir`.

#### 2e. Post-Phase 2 `session.rs` structure ✅ **DONE**
```rust
// ~95 lines: SessionCommands enum
// ~70 lines: handle_session dispatcher
// ~230 lines: thin command implementations (delegate to SessionService/SessionCompactor)
```
- **Actual:** 450 total lines, 391 non-test lines.

---

### Phase 3: `src/engine/agentic_loop.rs` → Orchestrator Extraction

**Goal:** Reduce `agentic_loop.rs` to ~500 lines (struct definition, public API, loop skeleton). Move all sub-concerns to dedicated modules.

#### 3a. Extract `CompactionOrchestrator` ✅ **DONE**
- **Source:** Compaction integration (lines 298–605) — config parsing, turn-boundary logic, pre/post hook invocation, background compactor coordination, session recording.
- **Destination:** `src/engine/compaction_orchestrator.rs`
- **Interface:**
  ```rust
  pub struct CompactionOrchestrator {
      background_compactor: BackgroundCompactor,
      config: CompactionConfig,
      registry: ModelContextRegistry,
  }
  impl CompactionOrchestrator {
      pub fn new(provider: Arc<Provider>, agent_config: &AgentConfig) -> Self;
      pub async fn check_and_compact(
          &mut self,
          messages: &mut Vec<LlmMessage>,
          session: &Arc<RwLock<Session>>,
          extension_core: &Arc<ExtensionCore>,
          on_event: &dyn Fn(AgenticEvent),
          run_id: &str,
      ) -> Result<()>;
  }
  ```
- **Rationale:** The loop body currently has ~300 lines of compaction logic. An orchestrator encapsulates the entire compaction lifecycle (check → hook → background → record → post-hook) so the loop just calls `orchestrator.check_and_compact(&mut messages, ...).await`.

#### 3b. Extract `SystemPromptService` → `src/prompt/` ✅ **DONE**
- **Source:** `build_system_prompt` (lines 1246–1318), `load_and_register_skills` (lines 1320–1382).
- **Destination:** `src/prompt/service.rs`
- **Interface:**
  ```rust
  pub struct SystemPromptService;
  impl SystemPromptService {
      pub async fn build(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> String;
      pub async fn build_fresh(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> String;
      pub async fn load_and_register_skills(agent: &Agent, extension_core: &Arc<ExtensionCore>) -> usize;
  }
  ```
- **Rationale:** System prompt construction is a prompt-layer concern. `src/prompt/` already has `builder.rs`, `bootstrap.rs`, and `placeholder.rs`. The service belongs here, next to the `SystemPromptBuilder` it uses. The engine should receive a ready-made prompt, not construct it.

#### 3c. Extract `ToolExecutor` → `src/engine/tool_executor.rs` ✅ **DONE**
- **Source:** Tool execution inline (lines 881–956) — `execute_tool_via_core_with_context`, result parsing, session update, event emission.
- **Destination:** `src/engine/tool_executor.rs`
- **Interface:**
  ```rust
  pub struct ToolExecutor;
  impl ToolExecutor {
      pub async fn execute(
          &self,
          tool_call: &ContentBlock,
          extension_core: &Arc<ExtensionCore>,
          agent: &Agent,
          session: &Arc<RwLock<Session>>,
          run_id: &str,
          on_event: &dyn Fn(AgenticEvent),
      ) -> Result<ToolExecutionResult>;
  }
  ```
- **Rationale:** Tool execution orchestrates the engine loop, extension core, and session. It is an engine concern (not extension, not session alone). `src/engine/` is the right place.

#### 3d. Move `synthesize_stream_from_blocking` → `src/providers/` ✅ **DONE**
- **Source:** Lines 1153–1243.
- **Destination:** `src/providers/synthetic_stream.rs`
- **Interface:**
  ```rust
  pub fn synthesize_stream_from_blocking(
      response: ChatResponse,
      provider_name: &str,
  ) -> Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;
  ```
- **Rationale:** Converting a blocking response to a stream is a provider-layer adapter. It operates on `ChatResponse` and `StreamEvent` (both provider types). `src/providers/` is the right place.

#### 3e. Remove Debug Marker File I/O ✅ **DONE**
- **Source:** Lines 353–365 — hard-coded Windows path `C:\Users\Megad\AppData\Roaming\pekobot\compaction_debug.marker`.
- **Action:** Delete. If e2e tests need this, inject a `CompactionDebugMarker` trait into `CompactionOrchestrator` for test builds only.

#### 3f. Post-Phase 3 `agentic_loop.rs` structure ✅ **DONE**
```rust
// ~100 lines: struct AgenticLoop + impl (new, with_max_iterations, public run methods)
// ~350 lines: run_inner skeleton (iteration loop, stream processing, tool call dispatch, final answer)
// ~100 lines: build_tool_definitions, run_streaming helper
```
- **Actual:** Reduced from ~1,248 to ~560 non-test lines (55% reduction). All specified extractions complete. Compaction logic in loop body is ~12 lines (delegated to `CompactionOrchestrator`). Tool execution is a 3-line loop (delegated to `ToolExecutor`). System prompt construction is a 1-liner (delegated to `SystemPromptService`).
- **Commit:** TBD

---

### Phase 4: `src/daemon/mod.rs` → Subsystem Extraction

**Goal:** Reduce `daemon/mod.rs` to ~250 lines (struct definition, lifecycle, select! loop, shutdown).

#### 4a. Extract `daemon::cron_engine` ✅ **DONE**
- **Source:** `check_and_run_jobs`, `execute_job`, `execute_main_job`, `execute_isolated_job`, `run_job_with_agent_service`, `handle_delivery`, `send_announcement` (lines 335–593).
- **Destination:** `src/daemon/cron_engine/mod.rs`
- **Interface:**
  ```rust
  pub struct CronEngine { scheduler: Arc<CronScheduler>, agent_service: Option<Arc<StatelessAgentService>>, ... }
  impl CronEngine {
      pub async fn check_and_run(&self) -> Result<()>;
      pub async fn check_idle(&self) -> Result<()>;
      pub async fn handle_event(&self, event: SystemEvent) -> Result<()>;
  }
  ```
- **Rationale:** The cron subsystem is ~260 lines of self-contained logic. Extracting it makes the daemon's main loop readable and allows independent testing of cron behavior.

#### 4b. Reuse `src/session/maintenance.rs` Instead of Duplicating ✅ **DONE**
- **Source:** `run_session_maintenance` (lines 752–816) — walks sessions directory, instantiates `MetadataController` per agent.
- **Discovery:** `src/session/maintenance.rs` already has `MaintenanceScheduler` and `maintain_agent()` which do exactly this, but better structured. The daemon is duplicating this logic.
- **Action:** Delete `Daemon::run_session_maintenance`. Use `session::MaintenanceScheduler::new(sessions_root).run_maintenance().await` instead.
- **If team-scoped maintenance is needed:** Add `MaintenanceScheduler::with_resolver(resolver)` so it can iterate over all teams/agents (matching the daemon's current behavior).
- **Rationale:** Session maintenance is a session concern. `src/session/maintenance.rs` already exists. The daemon should not duplicate session-layer logic.

#### 4c. Replace `load_agent_config` with `ConfigAuthorityImpl` ✅ **DONE**
- **Source:** Lines 728–749 — direct TOML file reading.
- **Action:** Use existing `ConfigAuthorityImpl` (already in `common::services`). This is a cross-cutting config concern, so `common/services/` is correct here.

#### 4d. Move `json_subset` → `src/common/json_utils.rs` ✅ **DONE**
- **Source:** Lines 449–479 — generic JSON utility.
- **Destination:** `src/common/json_utils.rs`
- **Rationale:** `json_subset` is a pure utility function with no daemon-specific logic. `common/` is for cross-cutting utilities.

#### 4e. Post-Phase 4 `daemon/mod.rs` structure ✅ **DONE**
```rust
// ~80 lines:  DaemonConfig, DaemonStatus, Daemon struct
// ~50 lines:  constructors (new, with_event_receiver, new_with_events)
// ~120 lines: run() — select! loop only, delegates to CronEngine, SessionMaintenanceService, etc.
```
- **Actual:** Reduced from ~719 to ~275 non-test lines (62% reduction). Cron logic fully extracted to `CronEngine`. Session maintenance delegates to `MaintenanceScheduler`. `load_agent_config` removed (daemon uses `AppState` / `StatelessAgentService` which already use `ConfigAuthorityImpl`).

---

## New Module Layout (Cohesion-First)

```
src/
├── commands/
│   ├── ext.rs                    # ~250 lines (dispatch only)
│   ├── session.rs                # ~300 lines (dispatch + rendering)
│   └── ...
├── common/
│   ├── services/
│   │   └── ...                   # Cross-cutting only: AgentService, TeamService, ConfigAuthorityImpl
│   └── json_utils.rs             # NEW — pure utility
├── extension/
│   ├── services/
│   │   ├── mod.rs                # EXISTING — add config_service, core_service
│   │   ├── config_service.rs     # NEW — was ExtensionConfig in ext.rs
│   │   └── core_service.rs       # NEW — enable/disable builtin hooks (wraps global_core)
│   ├── adapters/
│   │   ├── mod.rs                # EXISTING
│   │   └── validation.rs         # NEW — was handle_validate in ext.rs
│   └── ...
├── ipc/
│   ├── mod.rs                    # EXISTING
│   └── client_service.rs         # NEW — was copy-pasted IPC in ext.rs
├── engine/
│   ├── agentic_loop.rs           # ~500 lines (loop skeleton)
│   ├── compaction_orchestrator.rs# NEW
│   └── tool_executor.rs          # NEW
├── prompt/
│   ├── mod.rs                    # EXISTING
│   ├── builder.rs                # EXISTING
│   └── service.rs                # NEW — was build_system_prompt in agentic_loop.rs
├── providers/
│   ├── mod.rs                    # EXISTING
│   └── synthetic_stream.rs       # NEW — was synthesize_stream_from_blocking in agentic_loop.rs
├── daemon/
│   ├── mod.rs                    # ~250 lines (lifecycle only)
│   └── cron_engine/
│       └── mod.rs                # NEW — was check_and_run_jobs, execute_job, etc.
├── session/
│   ├── mod.rs                    # EXISTING
│   ├── maintenance.rs            # EXISTING — reuse instead of duplicating in daemon
│   └── presentation.rs           # NEW — was HistoryDisplayEntry/print_history_event in session.rs
└── compaction/
    ├── mod.rs                    # EXISTING
    └── cli.rs                    # NEW — was compact_session in session.rs
```

---

## Acceptance Criteria

- [x] Phase 1: `src/commands/ext.rs` — extracted `ExtensionConfigService`, `ExtensionValidationService`, `DaemonClientService`; simplified enable/disable. Reduced from ~1,359 to ~919 lines (32% reduction). Remaining rendering functions keep it above 400; further reduction possible in follow-up.
- [x] Phase 2: `src/commands/session.rs` — extracted active-session resolution to `SessionService`, compaction CLI logic to `compaction/cli.rs`, history presentation to `session/presentation.rs`. Eliminated direct `MetadataController`/`SessionStorage` usage. Reduced from ~1,026 to ~450 lines (391 non-test). All extracted modules have unit tests.
- [ ] `src/commands/ext.rs` ≤ 400 lines of non-test code.
- [x] `src/commands/session.rs` ≤ 400 lines of non-test code.
- [x] `src/engine/agentic_loop.rs` ≤ 600 lines of non-test code.
- [x] `src/daemon/mod.rs` ≤ 300 lines of non-test code.
- [x] No command file directly instantiates `MetadataController`, `SessionStorage`, or `ExtensionCore`.
- [x] `agentic_loop.rs` compaction logic is <30 lines in the loop body (delegated to `CompactionOrchestrator`).
- [x] `daemon/mod.rs` cron logic is extracted to `daemon::cron_engine`.
- [x] `daemon/mod.rs` does not duplicate `session::maintenance.rs` logic.
- [x] All extracted code lives in its domain module (`extension/` for extension concerns, `session/` for session concerns, `prompt/` for prompt concerns, `ipc/` for IPC concerns).
- [x] All extracted code has unit tests.
- [x] `cargo test` and `cargo clippy` pass.
- [x] Phase 4: `src/daemon/mod.rs` — extracted `CronEngine`, reused `MaintenanceScheduler`, removed `load_agent_config`, moved `json_subset` to `common/json_utils.rs`. Reduced from ~719 to ~275 non-test lines (62% reduction).

---

## Implementation Order (Recommended)

1. **Start with `daemon/mod.rs`** — it has the clearest subsystem boundaries (cron, maintenance, lifecycle). Low risk, high readability payoff. Reuse existing `session::maintenance.rs`.
2. **Then `commands/session.rs`** — the `SessionService` already exists; `src/session/presentation.rs` and `src/compaction/cli.rs` are new but isolated. Medium risk.
3. **Then `commands/ext.rs`** — requires creating `extension::services::config_service`, `extension::adapters::validation`, and `ipc::client_service`. Higher risk due to `ExtensionConfig` persistence migration.
4. **Finally `engine/agentic_loop.rs`** — this is the most complex extraction. Do it last when the orchestrator layer is solid.

**Why this order:** Each phase builds on the previous. The daemon extraction is purely structural (moving code). The session extraction creates domain modules that the engine extraction will later depend on (e.g., `compaction::cli::SessionCompactor`). The extension extraction creates services that the engine's `ToolExecutor` will use.

---

## Related Issues

- #014 — Extension Architecture Scattering
- #015 — Extension Type-Oriented Restructure
- #016 — Module Boundary Violation (extension ↔ tools)
