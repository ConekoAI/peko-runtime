# Issue 020: Commands Layer Directly Manipulates Internals (High Severity)

**Status:** Open — Partially Resolved  
**Labels:** `refactoring`, `architecture`, `high-severity`, `commands`, `service-layer`

## Summary

Multiple command files bypass the established service layer and directly touch module internals — filesystem structures, in-memory registries, and low-level persistence APIs. This violates the architectural boundary between CLI presentation and business logic, making the codebase fragile and difficult to test.

Positive examples exist (`team.rs`, `agent/handlers.rs`) where all business logic is delegated to `TeamService`/`AgentService` and handlers only perform CLI rendering. These should be the model.

---

## Progress Overview

| File | Status | Notes |
|------|--------|-------|
| `src/commands/session.rs` | **Resolved** | All direct `MetadataController`/`SessionStorage`/`Session::open_by_id` usage removed. Now delegates to `SessionService`. |
| `src/commands/ext.rs` | **Partially Resolved** | Direct `enable_hook`/`disable_hook` calls extracted into `Services::enable_builtin_hooks`/`disable_builtin_hooks`, but `global_core()` is still accessed directly in the command file (lines 138, 289) and inside the service methods themselves. |
| `src/commands/daemon.rs` | **Unchanged** | Still reimplements all process lifecycle primitives inline. |
| `src/commands/auth.rs` | **Unchanged** | `CredentialsStore` and file I/O still live inline in the command handler. |

---

## Affected Files & Violations

### `src/commands/ext.rs` — Direct `global_core()` Access Remains

**Lines:** 138, 289–290  
**Status:** Partially fixed — hook manipulation moved to `Services`, but global core access persists.

```rust
// Line 138 — still directly accesses global core
let core = crate::extension::core::global_core().expect("Global ExtensionCore not initialized");

// Lines 289–290 — still directly accesses global core
let builtins = if let Some(core) = crate::extension::core::global_core() {
    core.list_builtin_extensions().await
} else {
    Vec::new()
};
```

The previous inline `enable_hook`/`disable_hook` loops (original issue lines 756–768, 844–857) were extracted into `extension::services::Services::enable_builtin_hooks()` and `disable_builtin_hooks()`. However, those service methods **still internally access `global_core()` directly** — the violation was moved down one layer, not eliminated.

**Why this is wrong:**
- `ExtensionCore` is an internal registry. Command handlers should not reach into global state.
- `Services::enable_builtin_hooks` / `disable_builtin_hooks` are static methods that internally reach for global state — they cannot be unit-tested without initializing the global core.
- The service layer should receive its dependencies via constructor injection, not reach for globals.

**Recommended fix:**
- Refactor `Services` to accept an `Arc<ExtensionCore>` at construction time (or add a dedicated `ExtensionCoreService` wrapper).
- Replace `Services::enable_builtin_hooks(capability)` with a method on an injected service instance: `extension_service.enable_builtin_hooks(capability).await`.
- Remove all `global_core()` calls from `ext.rs`.

---

### `src/commands/session.rs` — Resolved ✅

**Status:** Fixed in recent commits.

All direct imports and usage of `MetadataController`, `SessionStorage`, and `Session::open_by_id` have been removed from the command handler. The file now delegates entirely to `SessionService`:

- `SessionService::list_sessions()` / `list_sessions_synced()`
- `SessionService::get_session()` / `get_session_synced()`
- `SessionService::delete_session()`
- `SessionService::open_session()`
- `SessionService::get_history()`
- `SessionService::branch_session()`

`SessionService` itself still uses `MetadataController` and `SyncSessionStorage` internally, which is acceptable — the service layer is the correct place to encapsulate those implementation details.

**Remaining concern:** File is ~450 lines (slightly over the 400-line target). Consider extracting presentation helpers into `session::presentation` if they haven't been already.

---

### `src/commands/daemon.rs` — Reimplements PID Management, Process Spawn/Kill, Readiness Polling

**Lines:** 220–246, 333–496, 499–538, 544–587  
**Status:** Unchanged since issue opened.

```rust
// Line 220-246: Inline process-running check
fn is_process_running(pid: u32) -> bool {
    #[cfg(windows)] { /* PowerShell Get-Process */ }
    #[cfg(unix)] { unsafe { libc::kill(pid as libc::pid_t, 0) == 0 } }
}

// Line 333-496: Inline graceful shutdown + PID kill + fallback kill
async fn stop_daemon(force: bool) -> anyhow::Result<()> { ... }

// Line 499-538: Inline daemon spawn
async fn spawn_daemon(paths: &GlobalPaths, interval: u64) -> anyhow::Result<()> { ... }

// Line 544-587: Inline readiness polling
async fn wait_for_daemon_ready() -> bool { ... }
```

**Why this is wrong:**
- `src/common/process/` already contains `spawn.rs`, `kill.rs`, and `health.rs` with `spawn_process`, `graceful_shutdown`, `force_kill_child`, and `HealthCheckLoop`.
- The daemon command handler reimplements all of these concerns with platform-specific code (Windows PowerShell, taskkill, Unix `kill`/`pkill`).
- `daemon::background_runtime` also contains process supervision code. There are now **three** places with process lifecycle logic.

**Recommended fix:**
- Refactor `daemon.rs` to use `common::process::{spawn_process, graceful_shutdown, HealthCheckLoop}`.
- If the common primitives are insufficient (e.g., they don't cover PID-file-based kill or IPC graceful shutdown), **extend them** rather than inlining.
- Consider creating a `DaemonProcessService` that encapsulates spawn/stop/status/status-check logic and manages the PID file.

---

### `src/commands/auth.rs` — Full CredentialsStore with File I/O and Serialization

**Lines:** 42–125  
**Status:** Unchanged since issue opened.

```rust
// Lines 42-87
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct CredentialsStore {
    pub version: u32,
    pub credentials: HashMap<String, Credential>,
}

impl CredentialsStore { /* get, set, remove, providers */ }

// Lines 90-125
fn load_credentials(paths: &GlobalPaths) -> Result<CredentialsStore> { ... }
fn save_credentials(paths: &GlobalPaths, store: &CredentialsStore) -> Result<()> { ... }
```

**Why this is wrong:**
- `CredentialsStore` is a data model + persistence layer. It should live in `src/common/` or `src/identity/`, not in a command handler.
- File I/O (`std::fs::read_to_string`, `std::fs::write`, permission setting) is a low-level concern.
- The command handler should call `CredentialsService::load()`, `CredentialsService::save()`, etc.

**Recommended fix:**
- Move `CredentialsStore`, `Credential`, `load_credentials`, `save_credentials` to `src/common/credentials_store.rs`.
- Create a `CredentialsService` that wraps load/save/validate operations.
- `auth.rs` should only handle CLI argument parsing and rendering.

---

## Positive Examples (Model to Follow)

- **`src/commands/team.rs`** — delegates all business logic to `TeamService`. The handler only maps CLI args to service calls and formats output.
- **`src/commands/agent/handlers.rs`** — delegates to `AgentService`. No direct filesystem access, no direct config parsing.
- **`src/commands/session.rs`** — now delegates to `SessionService` after recent fixes.

---

## Migration Plan (Updated)

### Phase 1 — `auth.rs` (Quick Win, ~1–2 hours)
1. Create `src/common/credentials_store.rs` containing `Credential`, `CredentialsStore`, `load_credentials`, `save_credentials`.
2. Create `src/common/services/credentials_service.rs` with `CredentialsService::new(paths)`, `.load()`, `.save()`, `.set(provider, key)`, `.remove(provider)`, `.list()`.
3. Update `auth.rs` to use `CredentialsService`. Remove all inline data model and I/O code.
4. Target: `auth.rs` <150 lines.

### Phase 2 — `daemon.rs` (Medium, ~4–6 hours)
1. Audit `common::process` primitives against daemon needs:
   - Does `spawn_process` support `Stdio::null()`, `kill_on_drop(false)`, env injection? If not, extend `ProcessSpawnConfig`.
   - Does `graceful_shutdown` support PID-file-based termination (not just `Child` handle)? If not, add `graceful_shutdown_by_pid(pid, force)` to `common::process::kill`.
   - Does `HealthCheckLoop` support one-shot readiness polling? If not, add `wait_for_healthy(check_fn, timeout, interval)`.
2. Create `src/common/services/daemon_process_service.rs` encapsulating:
   - `spawn_daemon(paths, interval) -> Result<Child>`
   - `stop_daemon(force) -> Result<()>`
   - `is_daemon_running() -> bool`
   - `wait_for_daemon_ready(timeout) -> bool`
   - PID file read/write helpers.
3. Refactor `daemon.rs` to delegate to `DaemonProcessService`.
4. Target: `daemon.rs` <300 lines.

### Phase 3 — `ext.rs` (Medium, ~3–4 hours)
1. Refactor `Services` (or create `ExtensionCoreService`) to accept `Arc<ExtensionCore>` via constructor instead of calling `global_core()` internally.
2. Update `create_manager_with_adapters()` to receive the core from the caller (e.g., from an already-injected service).
3. Replace `global_core()` calls in `ext.rs` with service method calls.
4. Target: `ext.rs` <500 lines (may require splitting subcommand handlers into `src/commands/ext/` submodules).

### Phase 4 — Cleanup & Linting
1. Add an architectural lint (or code-review checklist) that flags any command file importing from:
   - `session::metadata_controller`
   - `session::jsonl`
   - `session::sync`
   - `extension::core::global_core`
   - `std::fs` / `tokio::fs` (with exceptions for path resolution)
2. Verify all four files meet line-count targets.
3. Add unit tests for extracted services (`CredentialsService`, `DaemonProcessService`, `ExtensionCoreService`).

---

## Acceptance Criteria (Updated)

- [ ] `ext.rs` does not import `extension::core::global_core` or call `enable_hook`/`disable_hook` directly (including transitively through static `Services` methods).
- [x] `session.rs` does not import `metadata_controller` or `jsonl::SessionStorage`.
- [ ] `daemon.rs` uses `common::process` primitives for spawn/kill/health checks.
- [ ] `auth.rs` does not contain `CredentialsStore` or file I/O logic.
- [ ] All four files have <400 lines of non-test code.
- [ ] Unit tests for the extracted services exist.

## Related Issues

- #019 — God Files & Mixed Concerns
- #014 — Extension Architecture Scattering
- #010 — Competing Session Abstractions
