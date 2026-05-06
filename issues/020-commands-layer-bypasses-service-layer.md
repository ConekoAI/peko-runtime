# Issue 020: Commands Layer Directly Manipulates Internals (High Severity)

**Status:** Open  
**Labels:** `refactoring`, `architecture`, `high-severity`, `commands`, `service-layer`

## Summary

Multiple command files bypass the established service layer and directly touch module internals — filesystem structures, in-memory registries, and low-level persistence APIs. This violates the architectural boundary between CLI presentation and business logic, making the codebase fragile and difficult to test.

Positive examples exist (`team.rs`, `agent/handlers.rs`) where all business logic is delegated to `TeamService`/`AgentService` and handlers only perform CLI rendering. These should be the model.

## Affected Files & Violations

### `src/commands/ext.rs` — Direct ExtensionCore Hook Manipulation

**Lines:** 372–377, 756–768, 844–857  
**Violation:** Calls `global_core().list_builtin_extensions()`, `core.enable_hook()`, `core.disable_hook()` directly from a command handler.

```rust
// Line 372-377
let builtins = if let Some(core) = crate::extension::core::global_core() {
    core.list_builtin_extensions().await
} else {
    Vec::new()
};

// Line 756-768 (inside handle_enable_builtin)
if let Some(core) = crate::extension::core::global_core() {
    let builtins = core.list_builtin_extensions().await;
    for b in &builtins {
        if b.name.eq_ignore_ascii_case(capability) {
            let ext_id = ExtensionId::new(&b.id);
            let hooks = core.get_hooks_for_extension(&ext_id).await;
            for hook in hooks {
                let _ = core.enable_hook(&hook.id).await;
            }
        }
    }
}
```

**Why this is wrong:**
- `ExtensionCore` is an internal registry. Command handlers should not reach into global state.
- `ExtensionService` (or `AgentConfigService`) should encapsulate enable/disable semantics.
- Makes it impossible to unit-test the command handler without initializing the global core.

**Recommended fix:**
- Add `ExtensionService::list_builtins()`, `ExtensionService::enable_builtin(capability, target)`, `ExtensionService::disable_builtin(capability, target)`.
- Command handler delegates to the service.

---

### `src/commands/session.rs` — Direct MetadataController / SessionStorage / Session::open_by_id

**Lines:** 17, 345, 453, 745, 782, 860, 969  
**Violation:** Directly imports and uses `MetadataController`, `SessionStorage`, and `Session::open_by_id` instead of routing through `SessionService`.

```rust
// Line 17
use crate::session::metadata_controller::MetadataController;

// Line 345
let mut controller = MetadataController::new(sessions_dir);
let metadata_list = controller.list_metadata(true).await?;

// Line 453
let mut controller = MetadataController::new(&loc.sessions_dir);
let Some(metadata) = controller.get_metadata(session_id, true).await? else { ... };

// Line 745
let mut controller = crate::session::MetadataController::new(&loc.sessions_dir);

// Line 782
let deleted = controller.delete_session(session_id).await?;

// Line 860
let session = crate::session::Session::open_by_id(
    agent, session_id, &loc.sessions_dir, Some(&peer)
).await?;

// Line 969
let storage = crate::session::jsonl::SessionStorage::new(loc.sessions_dir.clone());
```

**Why this is wrong:**
- `SessionService` already exists (`src/common/services/session_service.rs`) and provides `get_history`, but the command handler bypasses it for all other operations.
- `MetadataController` is an internal implementation detail of the session subsystem. CLI should not know it exists.
- `Session::open_by_id` is a low-level constructor. The service layer should manage session lifecycle.

**Recommended fix:**
- Extend `SessionService` with: `list_sessions`, `get_metadata`, `delete_session`, `open_session`, `count_compactions`.
- Replace all direct `MetadataController`/`SessionStorage`/`Session::open_by_id` usage in `session.rs` with `SessionService` calls.

---

### `src/commands/daemon.rs` — Reimplements PID Management, Process Spawn/Kill, Readiness Polling

**Lines:** 219–246, 332–481, 498–538, 540–587  
**Violation:** Reimplements process supervision primitives inline despite `src/common/process/` existing.

```rust
// Line 219-246: Inline process-running check
fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)] {
        let output = Command::new("powershell")
            .args([...])
            .output();
        ...
    }
    #[cfg(unix)] {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
}

// Line 332-481: Inline graceful shutdown + PID kill + fallback kill
async fn stop_daemon(force: bool) -> anyhow::Result<()> {
    // 1. Try IPC graceful shutdown
    // 2. PID-based taskkill/kill
    // 3. Wait loop
    // 4. Final verification + fallback process kill
}

// Line 498-538: Inline daemon spawn
async fn spawn_daemon(paths: &GlobalPaths, interval: u64) -> anyhow::Result<()> {
    let mut cmd = Command::new(&exe_path);
    cmd.arg("daemon").arg("start").arg("--foreground")...;
    let mut child = cmd.spawn()?;
    let daemon_ready = wait_for_daemon_ready().await;
    ...
}

// Line 540-587: Inline readiness polling
async fn wait_for_daemon_ready() -> bool {
    for i in 0..40 {
        tokio::time::sleep(...).await;
        match ConnectionManager::try_connect().await { ... }
    }
}
```

**Why this is wrong:**
- `src/common/process/` already contains `spawn.rs`, `kill.rs`, and `health.rs` with `spawn_process`, `graceful_shutdown`, `force_kill_child`, and `HealthCheckLoop`.
- The daemon command handler reimplements all of these concerns with platform-specific code (Windows PowerShell, taskkill, Unix `kill`/`pkill`).
- `daemon::background_runtime` also contains process supervision code. There are now **three** places with process lifecycle logic.

**Recommended fix:**
- Refactor `daemon.rs` to use `common::process::{spawn_process, graceful_shutdown, HealthCheckLoop}`.
- If the common primitives are insufficient, extend them rather than inlining.
- Consider creating a `DaemonProcessService` that encapsulates spawn/stop/status/status-check logic.

---

### `src/commands/auth.rs` — Full CredentialsStore with File I/O and Serialization

**Lines:** 42–103  
**Violation:** Contains a complete `CredentialsStore` implementation with JSON serialization, file I/O, and permission management inline in a command handler file.

```rust
// Lines 42-87
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct CredentialsStore {
    pub version: u32,
    pub credentials: HashMap<String, Credential>,
}

impl CredentialsStore {
    pub fn get(&self, provider: &str) -> Option<&Credential> { ... }
    pub fn set(&mut self, provider: &str, api_key: String) { ... }
    pub fn remove(&mut self, provider: &str) -> bool { ... }
    pub fn providers(&self) -> Vec<String> { ... }
}

// Lines 89-103
fn load_credentials(paths: &GlobalPaths) -> Result<CredentialsStore> { ... }
fn save_credentials(paths: &GlobalPaths, store: &CredentialsStore) -> Result<()> { ... }
```

**Why this is wrong:**
- `CredentialsStore` is a data model + persistence layer. It should live in `src/common/` or `src/identity/` (if auth-related), not in a command handler.
- File I/O (`std::fs::read_to_string`, `std::fs::write`, permission setting) is a low-level concern.
- The command handler should call `CredentialsService::load()`, `CredentialsService::save()`, etc.

**Recommended fix:**
- Move `CredentialsStore`, `Credential`, `load_credentials`, `save_credentials` to `src/common/credentials.rs` or `src/identity/credentials_store.rs`.
- Create a `CredentialsService` that wraps load/save/validate operations.
- `auth.rs` should only handle CLI argument parsing and rendering.

## Positive Examples (Model to Follow)

- **`src/commands/team.rs`** — delegates all business logic to `TeamService`. The handler only maps CLI args to service calls and formats output.
- **`src/commands/agent/handlers.rs`** — delegates to `AgentService`. No direct filesystem access, no direct config parsing.

## Recommended Actions

1. **Audit all `src/commands/*.rs`** for direct imports of `metadata_controller`, `jsonl`, `SessionStorage`, `ExtensionCore`, `global_core`, `MetadataController`, or inline file I/O.
2. **Create missing service methods** where the service layer is incomplete (e.g., `SessionService::delete_session`, `ExtensionService::enable_builtin`).
3. **Move data models + persistence** out of command handlers (`CredentialsStore`, `ExtensionConfig`).
4. **Reuse `common::process`** primitives in `daemon.rs` instead of reimplementing.
5. **Add an architectural lint** (or code-review checklist) that flags any command file importing from `session::metadata_controller`, `session::jsonl`, or `extension::core::global_core`.

## Acceptance Criteria

- [ ] `ext.rs` does not import `extension::core::global_core` or call `enable_hook`/`disable_hook` directly.
- [ ] `session.rs` does not import `metadata_controller` or `jsonl::SessionStorage`.
- [ ] `daemon.rs` uses `common::process` primitives for spawn/kill/health checks.
- [ ] `auth.rs` does not contain `CredentialsStore` or file I/O logic.
- [ ] All four files have <400 lines of non-test code.
- [ ] Unit tests for the extracted services exist.

## Related Issues

- #019 — God Files & Mixed Concerns
- #014 — Extension Architecture Scattering
- #010 — Competing Session Abstractions
