# Issue 019: God Files & Mixed Concerns (High Severity)

**Status:** Open  
**Labels:** `refactoring`, `architecture`, `high-severity`, `commands`, `engine`, `daemon`

## Summary

Multiple files in the codebase violate the Single Responsibility Principle by mixing 6–10 distinct concerns in a single module. The worst offenders are in the commands layer and the engine core loop. These files are difficult to test, expensive to change, and prone to merge conflicts.

## Affected Files

| File | Lines | Mixed Concerns |
|------|-------|----------------|
| `src/commands/ext.rs` | ~1,525 | Extension lifecycle, whitelist manipulation, config persistence, validation, daemon IPC, Tier 1/2/3 manifest detection |
| `src/commands/session.rs` | ~1,119 | Session ops, compaction algorithm, history display, path resolution, active-session resolution |
| `src/engine/agentic_loop.rs` | ~1,385 | LLM iteration, tool execution, system prompt construction, skill loading, session management, compaction, event emission, legacy fallbacks |
| `src/daemon/mod.rs` | ~841 | Cron execution engine, job delivery, session janitor, async task janitor, daemon lifecycle, event filtering, agent config loading |

## Detailed Findings

### 1. `src/commands/ext.rs` — Worst Offender

This file contains a **full ExtensionConfig persistence layer** (`ExtensionConfig` struct with `load`/`save`/`set`/`unset` — lines 1063–1144), **agent whitelist manipulation** (`add_tool_to_agent_whitelist`, `handle_enable_builtin`, `handle_disable_builtin` — lines 646–860), and **3-tier manifest validation** (`handle_validate` with Tier 1/2 detection — lines 1251–1414). All of these should live in services (`ExtensionService`, `AgentConfigService`).

**Specific violations:**
- **ExtensionConfig persistence** (lines 1063–1144): A private `ExtensionConfig` struct with file I/O, TOML serialization, and scoped key-value logic. This duplicates responsibilities that should belong to `ExtensionService` or a dedicated `ExtensionConfigService`.
- **Whitelist manipulation** (lines 646–860): `add_tool_to_agent_whitelist` directly walks the filesystem (`std::fs::read_dir`), parses agent configs with `toml::from_str`, and mutates per-agent tool lists. This is business logic that should live in `AgentConfigService`.
- **Tier 1/2/3 manifest validation** (lines 1251–1414): `handle_validate` embeds the entire ADR-024 detection hierarchy (SKILL.md → server.json → manifest.yaml) with per-type adapter calls. This should be extracted to `ExtensionValidationService` or moved into `extension::adapters`.
- **Direct ExtensionCore hook manipulation** (lines 756–768, 844–857): `handle_enable_builtin` and `handle_disable_builtin` call `global_core().enable_hook()` and `disable_hook()` directly instead of routing through a service.
- **Daemon IPC mixed in** (lines 954–1061): `handle_start`, `stop`, `restart`, `status` all contain inline daemon IPC client code. These should delegate to a `DaemonClientService`.

### 2. `src/commands/session.rs` — Copy-Paste Hotspot

The active-session resolution logic is **copy-pasted 4×** (lines 189–203, 217–231, 272–286, and implicitly in `compact_session`). Each branch duplicates the same `get_active_session_for_cli` call, the same `println!` formatting, and the same error message.

**Specific violations:**
- **Direct `MetadataController` usage** (lines 345, 453, 745, 782): The command handler directly instantiates `MetadataController` and calls `list_metadata`, `get_metadata`, `delete_session` instead of delegating to `SessionService`.
- **Direct `Session::open_by_id`** (line 860): `compact_session` opens a session directly instead of using `SessionService`.
- **Direct `SessionStorage` usage** (line 969): `compact_session` creates `SessionStorage` directly to count compaction events.
- **Compaction algorithm inline** (lines 844–1038): The entire CLI compaction flow — truncation logic, summary generation, token estimation, recording — is implemented in the command handler. This should delegate to `CompactionService`.
- **History display logic** (lines 525–678): `HistoryDisplayEntry`, `load_session_history`, `history_event_to_display`, and `print_history_event` form a ~150-line presentation layer that could be extracted to a `session::presentation` module.

### 3. `src/engine/agentic_loop.rs` — ~10 Concerns in One Loop

The core agentic loop (`run_inner`, lines 257–995) handles LLM streaming, tool execution, session persistence, compaction triggers, extension hook invocation, event emission, token tracking, and provider fallback logic.

**Specific violations:**
- **Compaction integration spans 200+ lines** (lines 298–605): Config parsing (lines 309–365), turn-boundary logic (lines 416–433), pre/post hook invocation (lines 412–491, 555–594), background compactor coordination (lines 494–553), and session recording (lines 515–531). This should be extracted to a `CompactionOrchestrator` or `ContextManager`.
- **System prompt construction** (lines 1246–1382): `build_system_prompt` and `load_and_register_skills` are ~130 lines of skill discovery, filtering, and registration logic. Should live in `prompt` or a `SkillService`.
- **Tool execution inline** (lines 881–956): Tool calls are executed directly via `runtime::execute_tool_via_core_with_context`, results parsed, and session updated — all inside the loop body. Should delegate to a `ToolExecutor`.
- **Stream synthesis** (lines 1153–1243): `synthesize_stream_from_blocking` converts blocking provider responses into synthetic stream events. This is provider-layer concern, not loop concern.
- **Debug marker file I/O** (lines 353–365): A hard-coded Windows path (`C:\Users\Megad\AppData\Roaming\pekobot\compaction_debug.marker`) is written inside the engine loop. This is e2e test infrastructure leaking into production engine code.

### 4. `src/daemon/mod.rs` — Cron + Janitor + Lifecycle + Config Loading

The daemon module combines the cron execution engine, session maintenance janitor, async task janitor, event handling, delivery/announcement, and agent config loading.

**Specific violations:**
- **Cron job execution** (lines 335–593): `check_and_run_jobs`, `execute_job`, `execute_main_job`, `execute_isolated_job`, `run_job_with_agent_service`, `handle_delivery`, `send_announcement` form a ~260-line cron subsystem. Should be extracted to `daemon::cron_engine`.
- **Session janitor inline** (lines 752–816): `run_session_maintenance` walks the entire sessions directory tree, instantiates `MetadataController` per agent, and runs maintenance. Should delegate to `SessionMaintenanceService`.
- **Agent config loading** (lines 728–749): `load_agent_config` directly reads TOML files. Should use `AgentConfigService`.
- **Event filtering** (lines 449–479): `event_matches_filter` and `json_subset` are generic JSON utilities embedded in the daemon. Should live in `common` or `types`.

## Positive Examples (Model to Follow)

- `src/commands/team.rs` — delegates all business logic to `TeamService` and only handles CLI rendering.
- `src/commands/agent/handlers.rs` — delegates to `AgentService` and only handles CLI rendering.

## Recommended Actions

1. **Extract services from `ext.rs`:**
   - Move `ExtensionConfig` persistence to `ExtensionConfigService` in `src/common/services/`.
   - Move whitelist manipulation to `AgentConfigService` (extend existing service).
   - Move manifest validation to `ExtensionValidationService` or `extension::adapters::validation`.
   - Move daemon IPC helpers to `DaemonClientService`.

2. **Extract services from `session.rs`:**
   - Deduplicate active-session resolution into a single helper or move into `SessionService`.
   - Move compaction CLI logic to `CompactionService::compact_session_cli`.
   - Move history presentation to `session::presentation`.
   - Replace all direct `MetadataController`/`SessionStorage`/`Session::open_by_id` usage with `SessionService` calls.

3. **Extract orchestrators from `agentic_loop.rs`:**
   - Extract compaction logic to `CompactionOrchestrator`.
   - Extract skill loading to `SkillService` or `SystemPromptService`.
   - Extract tool execution to `ToolExecutor`.
   - Remove debug marker file I/O (move to e2e test instrumentation).

4. **Extract subsystems from `daemon/mod.rs`:**
   - Extract cron engine to `daemon::cron_engine`.
   - Extract session janitor to `SessionMaintenanceService`.
   - Extract agent config loading to use existing `AgentConfigService`.
   - Move `json_subset` to `common::json_utils`.

## Acceptance Criteria

- [ ] No command file exceeds 400 lines of non-test code.
- [ ] No command file directly instantiates `MetadataController`, `SessionStorage`, or `ExtensionCore`.
- [ ] `agentic_loop.rs` compaction logic is <50 lines in the loop body (delegated to orchestrator).
- [ ] `daemon/mod.rs` cron logic is extracted to a dedicated module.
- [ ] All extracted code has unit tests.

## Related Issues

- #014 — Extension Architecture Scattering
- #015 — Extension Type-Oriented Restructure
- #016 — Module Boundary Violation (extension ↔ tools)
