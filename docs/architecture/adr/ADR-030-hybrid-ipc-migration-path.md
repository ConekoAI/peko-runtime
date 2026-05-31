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

**Adopt a tiered migration strategy (Option C) with Option B as the long-term target.**

| Tier | Commands | Status | Rationale |
|------|----------|--------|-----------|
| **Tier 0** | `agent_list`, `agent_show`, `agent_create`, `agent_remove`, `team_list`, `team_show`, `session_list`, `session_show`, `system_status`, `system_doctor`, `cron_list`, `cron_remove`, `cron_run` | ✅ **Migrated** | Simple CRUD; daemon services already own this state |
| **Tier 1** | `extension_list`, `extension_enable`, `extension_disable`, `system_clean` | 🔄 **Migrate now** | Daemon already has `ExtensionCore`; `ExtensionManager` can be added to `AppState`. These are the most frequently-called remaining fallbacks. |
| **Tier 2** | `extension_install`, `extension_uninstall` | ⏸️ **Defer** | File I/O heavy but feasible once `ExtensionManager` is in `AppState`. Evaluate after Tier 1. |
| **Tier 3** | `agent_export`/`import`, `team_export`/`import`, `session_branch`/`compact`, `cron_add`, `registry_pull` | 🛑 **Permanent CLI** | Complex packaging, archive creation, network I/O, or multi-step state mutations. These are acceptable as CLI-only operations. |

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

### Why Tier 3 stays CLI permanently

| Command | Why It Stays CLI |
|---------|-----------------|
| `agent_export` / `import` | Creates `.agent` archives. Complex packaging logic with encryption. |
| `team_export` / `import` | Creates `.team` archives. Complex packaging + dependency resolution + extension auto-pull. |
| `session_branch` / `compact` | Complex multi-step state mutations across session files. |
| `cron_add` | Complex schedule parsing (`ScheduleKind::Every { every_ms }`, cron expressions, etc.). |
| `registry_pull` | Network I/O to external registry. Independent of local daemon state. |

These are not "simple" operations that benefit from IPC speed. They're long-running, complex workflows where the ~50ms CLI spawn is irrelevant compared to the seconds of actual work.

### Why not go straight to Option B

Option B (daemon owns all state, CLI is thin client) requires:
- File watching for external changes (CLI or user edits files directly)
- Daemon startup time increases (scans `~/.peko/extensions/` on every start)
- Memory bloat (daemon keeps `ExtensionManager` in memory permanently)
- Risk of daemon crash mid-write corrupting files

These are solvable but not trivial. Tier 1 gives us 80% of the benefit with 20% of the effort.

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

### Phase 3a: Tier 1 — Extension List/Enable/Disable + System Clean

**Goal**: Eliminate the most frequently-called CLI fallbacks.

1. Add `ExtensionManager` + `ExtensionServices` to `AppState`
2. Extract shared library functions from `src/commands/ext.rs` to `src/extension/ipc_ops.rs`
3. Add `ExtensionList`, `ExtensionEnable`, `ExtensionDisable`, `SystemClean` IPC packets
4. Add IPC handlers in `server.rs`
5. Add `IpcClient` methods in desktop
6. Migrate desktop `extension_list`, `extension_enable`, `extension_disable`, `system_clean` to IPC
7. Test and verify

**Estimated time**: 3–4 days.

### Phase 3b: Tier 2 — Extension Install/Uninstall (Optional)

**Goal**: Evaluate whether to migrate after Tier 1 proves stable.

1. Add `ExtensionInstall { path }`, `ExtensionUninstall { id }` IPC packets
2. IPC handlers call `manager.install()` / `manager.uninstall()`
3. Desktop migrates `extension_install`, `extension_uninstall`

**Decision gate**: Only proceed if Tier 1 is stable for 2+ weeks with no issues.

### Phase 4: CLI Becomes Thin Client (Option B Target)

**Goal**: The CLI is a thin wrapper around IPC, like `docker` CLI.

1. Refactor all CLI commands to use `DaemonClientService` (IPC) instead of direct file I/O
2. For Tier 3 commands (export/import/branch/compact), the CLI still does file I/O but uses shared library functions
3. Remove `GlobalPaths` from CLI commands where possible
4. Document: "The daemon owns runtime state; the CLI owns packaging and complex workflows"

**Estimated time**: 2–3 weeks (deferred until after Tier 1+2).

---

## Tradeoffs

| Aspect | Before (Phase 2) | After Tier 1 | After Option B |
|--------|-----------------|--------------|----------------|
| CLI fallbacks | 14 | 10 | 5 (Tier 3 only) |
| Desktop latency | ~50ms per CLI call | ~1ms per IPC call | ~1ms per IPC call |
| Daemon startup | Fast | Slower (loads extensions) | Slower (loads everything) |
| Daemon memory | Lower | Higher (ExtensionManager) | Higher (full state) |
| Code paths | 2 (CLI + IPC) | 2 (shrinking) | 1 (daemon owns all) |
| CLI standalone | Full | Full | Thin client (needs daemon) |
| Maintenance | Medium | Medium | Low |

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

## Out of Scope

- **File watching**: Not needed for Tier 1. If user installs an extension outside the desktop, they restart the daemon. Future ADR if needed.
- **Extension install/uninstall via IPC**: Tier 2, decision gate after Tier 1 stability.
- **Agent/team export/import via IPC**: Tier 3, permanent CLI. These are packaging operations, not runtime state.
- **Session branch/compact via IPC**: Tier 3, permanent CLI. Complex state mutations.
- **Registry pull via IPC**: Tier 3, permanent CLI. Network I/O to external service.

---

## Success Criteria

| # | Criterion | How to Verify |
|---|-----------|---------------|
| 1 | `extension_list` uses IPC | Desktop Extensions page loads in <5ms (was ~50ms) |
| 2 | `extension_enable`/`disable` uses IPC | Toggle extension in UI, no CLI spawn in logs |
| 3 | `system_clean` uses IPC | Click "Clean" in Settings, no CLI spawn |
| 4 | CLI still works | `peko ext list`, `peko ext enable`, `peko ext disable` still function |
| 5 | All tests pass | `cargo test --lib` in peko-runtime → 1070+ pass |
| 6 | Desktop builds clean | `cargo check` in peko-desktop → 0 errors |

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
