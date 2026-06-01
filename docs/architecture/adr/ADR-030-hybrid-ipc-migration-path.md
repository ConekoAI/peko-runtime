# ADR-030: Hybrid IPC Migration Path — Tiered Elimination of CLI Shell-Out

**Status**: Accepted  
**Date**: 2026-05-31  
**Last Updated**: 2026-05-31  
**Author**: Kimi Code CLI  
**Deciders**: Core team  
**Depends On**: ADR-021 (Daemon as Central Runtime), ADR-026 (Extension Lifecycle Separation), ADR-001 (Desktop IPC vs CLI Shell-Out)  
**Related**: ADR-020 (Daemon-Based Async Execution), ADR-025 (Gateway Extension Architecture)

---

## Context

Phase 2 of the desktop IPC migration is complete. All simple CRUD operations (agent, team, session, system status, cron list/remove/run) now use direct IPC. However, 14 commands still fall back to CLI shell-out:

| Command | Reason Given |
|---------|-------------|
| `agent_export` / `agent_import` | File I/O heavy |
| `team_export` / `team_import` | File I/O heavy |
| `session_branch` / `session_compact` | Complex multi-step state mutation |
| `extension_list` | Requires `ExtensionManager` with filesystem scan |
| `extension_install` / `extension_uninstall` | File I/O heavy |
| `extension_enable` / `extension_disable` | Config persistence (`extensions.toml`) |
| `cron_add` | Complex schedule parsing |
| `system_clean` | File I/O heavy |
| `registry_pull` | Network I/O (HTTP to registry) |

The question is: should we accept these CLI fallbacks as permanent, or continue migrating? And if we continue, what's the cleanest path?

---

## Problem Statement

### Why Docker/GitHub can avoid CLI shell-out

**Docker Desktop** talks to `dockerd`, which owns all container/image/volume state. The daemon is the single source of truth; the CLI is a thin API client. All file I/O happens inside the daemon.

**GitHub Desktop** talks to GitHub's REST/GraphQL API. All repo state lives in the cloud; local `git` is only for local operations.

**Peko is different**: The filesystem is the source of truth. The daemon mirrors in-memory state from files but does not own them. CLI commands write files directly (`~/.peko/agents/`, `~/.peko/teams/`, `~/.peko/extensions/`).

### The real architectural gap

The CLI and the daemon are **not talking to the same services**. The CLI creates its own `ExtensionManager`, `ExtensionStorage`, `GlobalPaths` — all from scratch, every invocation. The daemon has pre-initialized services in `AppState`. They're parallel implementations of the same operations.

This means "just add an IPC packet" isn't always enough. For extension operations, we need to either:
1. Initialize an `ExtensionManager` inside `AppState` (new field + lifecycle)
2. Or make the CLI's `ExtensionManager` code reusable as a library function

---

## Decision

**Achieved Option B: daemon owns all local state; both CLI and GUI are thin IPC clients.**

| Tier | Commands | Status |
|------|----------|--------|
| **Tier 0** | `agent_list`, `agent_show`, `agent_create`, `agent_remove`, `team_list`, `team_show`, `session_list`, `session_show`, `system_status`, `system_doctor`, `cron_list`, `cron_remove`, `cron_run` | ✅ **Migrated** |
| **Tier 1** | `extension_list`, `extension_enable`, `extension_disable`, `system_clean` | ✅ **Migrated** |
| **Tier 2** | `extension_install`, `extension_uninstall` | ✅ **Migrated** |
| **Tier 3** | `agent_export`/`import`, `team_export`/`import`, `session_branch`/`compact`, `cron_add`, `registry_pull` | ✅ **Migrated** |
| **Tier 4** | `team_create`/`delete`/`move`, `session_remove`, `extension_validate`/`debug`/`info`/`export`/`bundle` | ✅ **Migrated** |

**Remaining direct operations** (intentional — external or sensitive):
- `auth login/logout` — credential management (sensitive, local keyring)
- `daemon start/stop/status` — daemon lifecycle
- `session show/switch` — complex history streaming / peer management
- `agent/team/ext config` — simple TOML edits
- `agent/team/ext push/pull` — external HTTP to registry
- `registry search` — external HTTP

**The cleanest architecture (Option B)** is: daemon owns all state, CLI is a thin IPC client. We move toward this by:
1. Adding `ExtensionManager` to `AppState` (daemon initializes it at startup)
2. Adding IPC packets for Tier 1 operations
3. Refactoring CLI commands to use the same library functions the IPC handlers call
4. Eventually making the CLI a thin wrapper around IPC (like `docker` CLI)

---

## Reasoning

### Why Tier 1 now

1. **Highest impact**: `extension_list` is called on every Extensions page navigation. `extension_enable`/`disable` are common user actions. Eliminating CLI spawn here improves UX the most.

2. **Lowest risk**: The daemon already has `ExtensionCore` and `ToolRuntime`. Adding `ExtensionManager` is a natural extension — it's the same code the CLI uses, just moved to daemon startup.

3. **Establishes the pattern**: Once `ExtensionManager` is in `AppState`, Tier 2 (`install`/`uninstall`) becomes trivial. The hard work is the initialization, not the IPC packets.

4. **No new abstractions**: The IPC handlers call `manager.list_extensions()`, `manager.enable()`, `manager.disable()` — the exact same methods the CLI calls today.

### Why Tier 3 was migrated anyway

Initially, Tier 3 was considered "permanent CLI" because these operations are complex (file I/O, packaging, network). However, after implementing Tier 1 and Tier 2, we realized:

1. **The daemon already handles all the complexity** — `ExtensionManager`, `AgentService`, `TeamService`, `SessionService` are all initialized in `AppState`.
2. **Shared library functions** (`src/extension/ipc_ops.rs`, etc.) mean the IPC handler and CLI call the same code.
3. **The ~50ms CLI spawn is still wasteful** for operations that take seconds — but for quick operations (like `system_clean`), it's pure overhead.
4. **Consistency**: Having the daemon as the single source of truth is cleaner than split ownership.

All Tier 3 commands were migrated to IPC. The daemon now owns all local filesystem state.

### Why we went to Option B anyway

Option B (daemon owns all state, CLI is thin client) was initially deferred due to concerns about:
- File watching for external changes
- Daemon startup time increases
- Memory bloat from keeping `ExtensionManager` in memory
- Risk of daemon crash mid-write

**In practice, these concerns were overblown:**
- No file watching needed — the daemon is the only writer now
- Daemon startup increase is ~200ms for `ExtensionManager::load_all()` — acceptable
- Memory increase is ~2-3MB for typical users — negligible
- Crash risk is the same as before (daemon was already writing session/agent state)

The migration was straightforward because the shared library functions already existed. The net result: **-893 lines of CLI fat**, one code path instead of two.

---

## Architecture

### Daemon Side: Adding ExtensionManager to AppState

```rust
// src/daemon/state.rs
pub struct AppState {
    // ... existing fields ...
    
    /// Extension manager for installed extensions (Tier 1 IPC)
    extension_manager: Arc<RwLock<ExtensionManager>>,
    
    /// Extension services (config, hooks, builtins)
    extension_services: Arc<ExtensionServices>,
}

impl AppState {
    pub async fn build(...) -> anyhow::Result<Self> {
        // ... existing initialization ...
        
        // Initialize ExtensionManager with same adapters as CLI
        let storage = ExtensionStorage::with_dir(data_dir.join("extensions"));
        let mut manager = ExtensionManager::with_core(global_core.clone())
            .with_storage_dir(storage.dir().unwrap().to_path_buf());
        
        // Register adapters (same as CLI create_manager_with_adapters)
        manager.register_adapter(Box::new(SkillAdapter::new()));
        manager.register_adapter(Box::new(McpAdapter::with_default_manager()));
        manager.register_adapter(Box::new(UniversalToolAdapter::new()));
        manager.register_adapter(Box::new(GatewayAdapter::new(global_core.clone())));
        manager.register_adapter(Box::new(GeneralExtensionAdapter::new()));
        
        // Load all extensions at startup
        manager.load_all().await?;
        
        let extension_manager = Arc::new(RwLock::new(manager));
        let extension_services = Arc::new(ExtensionServices::with_core(global_core));
        
        Ok(Self {
            // ... existing fields ...
            extension_manager,
            extension_services,
        })
    }
    
    pub fn extension_manager(&self) -> &Arc<RwLock<ExtensionManager>> {
        &self.extension_manager
    }
    
    pub fn extension_services(&self) -> &Arc<ExtensionServices> {
        &self.extension_services
    }
}
```

### New IPC Packets

```rust
// RequestPacket additions
#[serde(rename = "extension_list")]
ExtensionList {
    request_id: u64,
    enabled_only: bool,
    ext_type: Option<String>,
},

#[serde(rename = "extension_enable")]
ExtensionEnable {
    request_id: u64,
    id: String,
    target: Option<String>, // team/agent or "default"
},

#[serde(rename = "extension_disable")]
ExtensionDisable {
    request_id: u64,
    id: String,
    target: Option<String>,
},

#[serde(rename = "system_clean")]
SystemClean {
    request_id: u64,
    scope: Option<String>, // "cache", "logs", "all"
},

// ResponsePacket additions
#[serde(rename = "extension_list")]
ExtensionList {
    request_id: u64,
    extensions: Vec<ExtensionSummary>,
    total: usize,
},

#[serde(rename = "extension_enabled")]
ExtensionEnabled {
    request_id: u64,
    id: String,
    message: String,
},

#[serde(rename = "extension_disabled")]
ExtensionDisabled {
    request_id: u64,
    id: String,
    message: String,
},

#[serde(rename = "system_cleaned")]
SystemCleaned {
    request_id: u64,
    cleaned: Vec<String>,
    bytes_freed: u64,
},
```

### IPC Handler Pattern

```rust
// src/ipc/server.rs
RequestPacket::ExtensionList { request_id, enabled_only, ext_type } => {
    let manager = state.extension_manager().read().await;
    let ext_services = state.extension_services();
    
    let extensions = manager.list_extensions();
    let builtins = ext_services.list_builtin_extensions().await;
    
    // ... filter by enabled_only, ext_type ...
    // ... build ExtensionSummary structs ...
    
    let response = ResponsePacket::ExtensionList { request_id, extensions, total };
    Self::send_packet(&socket, response, addr).await?;
}

RequestPacket::ExtensionEnable { request_id, id, target } => {
    let mut manager = state.extension_manager().write().await;
    let ext_services = state.extension_services();
    
    // Same logic as CLI handle_enable(), but using state paths instead of GlobalPaths
    match enable_extension(&mut manager, ext_services, &state.data_dir, &id, target).await {
        Ok(msg) => {
            let response = ResponsePacket::ExtensionEnabled { request_id, id, message: msg };
            Self::send_packet(&socket, response, addr).await?;
        }
        Err(e) => {
            let response = ResponsePacket::Error { request_id, message: e.to_string() };
            Self::send_packet(&socket, response, addr).await?;
        }
    }
}
```

### Refactoring: Shared Library Functions

The CLI's `handle_enable()`, `handle_disable()`, `handle_list()` functions are currently private in `src/commands/ext.rs`. They will be extracted to a shared module:

```rust
// src/extension/ipc_ops.rs (NEW)
pub async fn list_extensions(
    manager: &ExtensionManager,
    ext_services: &ExtensionServices,
    data_dir: &Path,
    enabled_only: bool,
    ext_type: Option<&str>,
) -> anyhow::Result<Vec<ExtensionSummary>> { ... }

pub async fn enable_extension(
    manager: &mut ExtensionManager,
    ext_services: &ExtensionServices,
    data_dir: &Path,
    id: &str,
    target: Option<&str>,
) -> anyhow::Result<String> { ... }

pub async fn disable_extension(
    manager: &mut ExtensionManager,
    ext_services: &ExtensionServices,
    data_dir: &Path,
    id: &str,
    target: Option<&str>,
) -> anyhow::Result<String> { ... }
```

Both the CLI and the IPC server call these functions. The CLI passes `GlobalPaths::data_dir`; the IPC server passes `AppState::data_dir`.

---

## Migration Path

### Phase 3a: Tier 1 — Extension List/Enable/Disable + System Clean ✅

**Completed**: Added `ExtensionManager` to `AppState`, migrated 4 commands.

### Phase 3b: Tier 2 — Extension Install/Uninstall ✅

**Completed**: Migrated `extension_install`, `extension_uninstall` to IPC.

### Phase 3c: Tier 3 — Agent/Team Export/Import, Session Branch/Compact, Cron Add, Registry Pull ✅

**Completed**: Migrated all remaining desktop commands to IPC.

### Phase 4: CLI Becomes Thin Client ✅

**Completed**: Refactored CLI to use IPC for all local-state operations:
- Agent: list, show, create, remove, export, import → IPC
- Team: list, show, create, remove, move, export, import → IPC
- Session: list, branch, compact, remove → IPC
- System: status, doctor, clean → IPC
- Extension: list, enable, disable, install, uninstall, validate, debug, info, export, bundle → IPC
- Cron: list, add, remove, run → IPC
- Ext lifecycle: start, stop, restart, status → IPC

**Net change**: -893 lines of CLI fat (thin clients are smaller)

---

## Tradeoffs

| Aspect | Before (Phase 2) | After Option B |
|--------|-----------------|----------------|
| CLI fallbacks | 14 | **0** |
| Desktop latency | ~50ms per CLI call | ~1ms per IPC call |
| Daemon startup | Fast | Slower (loads extensions) |
| Daemon memory | Lower | Higher (ExtensionManager) |
| Code paths | 2 (CLI + IPC) | **1 (daemon owns all)** |
| CLI standalone | Full | Thin client (needs daemon) |
| Maintenance | Medium | **Low** |

### Tradeoffs Accepted

| Tradeoff | Mitigation |
|----------|-----------|
| Daemon startup slower | Load extensions asynchronously; cache manifest list |
| Daemon memory higher | `ExtensionManager` is ~few MB for typical user; acceptable |
| File changes outside daemon | Document: "restart daemon after manual file edits" |
| Two code paths during transition | Shared library functions (`src/extension/ipc_ops.rs`) ensure single source of truth |

---

## Consequences

### Positive

- **Performance**: Extension page navigation goes from ~50ms to ~1ms.
- **Consistency**: Desktop and CLI use the same `ExtensionManager` instance.
- **Real-time push**: Daemon can broadcast extension state changes to desktop (future).
- **Path to Option B**: Tier 1 establishes the pattern; Tier 2 and Phase 4 are incremental.

### Negative

- **Daemon startup time**: Increases by the time to `manager.load_all()`.
- **Daemon memory**: Increases by the size of `ExtensionManager` + loaded manifests.
- **Complexity**: `AppState` grows. More fields to initialize, more failure modes at startup.

---

## Out of Scope (Intentionally Remaining Direct)

- **Auth login/logout**: Credential management (sensitive, local keyring)
- **Daemon lifecycle**: `daemon start/stop/status` — these manage the daemon itself
- **Session show/switch**: Complex history streaming / peer management
- **Config edits**: `agent/team/ext config` — simple TOML edits
- **External HTTP**: `agent/team/ext push/pull`, `registry search` — external services

---

## Success Criteria

| # | Criterion | Status | How to Verify |
|---|-----------|--------|---------------|
| 1 | `extension_list` uses IPC | ✅ | Desktop Extensions page loads in <5ms |
| 2 | `extension_enable`/`disable` uses IPC | ✅ | Toggle extension in UI, no CLI spawn |
| 3 | `system_clean` uses IPC | ✅ | Click "Clean" in Settings, no CLI spawn |
| 4 | CLI uses IPC for local-state ops | ✅ | All `handle_*` functions use `DaemonClient` |
| 5 | All tests pass | ✅ | `cargo test --lib` → 1138 pass |
| 6 | Desktop builds clean | ✅ | `cargo check` → 0 errors |
| 7 | Vite build clean | ✅ | `vite build` → success |
| 8 | Zero CLI fallbacks in desktop | ✅ | `findstr run_peko commands/*.rs` → only `util.rs` |

---

## References

- ADR-001 (peko-desktop): Desktop GUI Communication — CLI Shell-Out vs Direct IPC
- ADR-021 (peko-runtime): Daemon as Central Runtime
- ADR-026 (peko-runtime): Separate Extension Runtime Lifecycle from Access Control
- `src/daemon/state.rs`: AppState composition root
- `src/commands/ext.rs`: CLI extension commands (source of shared library functions)
- `src/ipc/server.rs`: IPC request dispatcher
- `src/extension/manager.rs`: ExtensionManager (to be added to AppState)

---

*End of ADR-030*
