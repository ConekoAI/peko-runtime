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
| `src/commands/daemon.rs` | **Resolved** | All process lifecycle primitives extracted to `DaemonProcessService`. File reduced from ~620 to ~267 lines. |
| `src/commands/auth.rs` | **Resolved** | `CredentialsStore`, `Credential`, and all file I/O extracted to `src/common/credentials_store.rs`. `CredentialsService` created in `src/common/services/credentials_service.rs`. `auth.rs` now delegates entirely to the service layer. Reduced from ~275 to ~175 lines. |

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

### `src/commands/daemon.rs` — Resolved ✅

**Status:** Fixed in Phase 2.

All inline process lifecycle primitives extracted to `DaemonProcessService`:
- `is_process_running` → `common::process::kill::is_process_running`
- `kill_by_pid` / `kill_all_by_name` / `wait_for_exit` → `common::process::kill`
- `wait_for_healthy` → `common::process::health::wait_for_healthy`
- `spawn_daemon` / `stop_daemon` / `is_daemon_running` / `get_daemon_status` → `common::services::DaemonProcessService`

File reduced from ~620 lines to ~267 lines (target: <300).

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

### Phase 1 — `auth.rs` (Quick Win, ~1–2 hours) ✅ **COMPLETE**
1. ✅ Create `src/common/credentials_store.rs` containing `Credential`, `CredentialsStore`, `load_credentials`, `save_credentials`.
2. ✅ Create `src/common/services/credentials_service.rs` with `CredentialsService::new(paths)`, `.load()`, `.save()`, `.set(provider, key)`, `.remove(provider)`, `.list()`, `.get()`, `.get_api_key()`, `.test_provider()`, `.credentials_path()`.
3. ✅ Update `auth.rs` to use `CredentialsService`. Removed all inline data model and I/O code. `handle_auth` is now synchronous (no async needed).
4. ✅ Target met: `auth.rs` reduced from ~275 to ~175 lines.
5. ✅ Added `Clone` + `Debug` to `GlobalPaths` so services can own it.
6. ✅ Unit tests: 8 for `CredentialsStore` + 8 for `CredentialsService` = 16 new tests, all passing.

### Phase 2 — `daemon.rs` (Medium, ~4–6 hours) ✅ **COMPLETE**
1. ✅ Extended `common::process::kill` with `is_process_running`, `kill_by_pid`, `kill_all_by_name`, `wait_for_exit`.
2. ✅ Extended `common::process::health` with `wait_for_healthy` for one-shot readiness polling.
3. ✅ Created `src/common/services/daemon_process_service.rs` with:
   - `spawn_daemon(interval_secs) -> Result<Child>`
   - `stop_daemon(force) -> Result<()>`
   - `is_daemon_running() -> Result<bool>`
   - `wait_for_daemon_ready(timeout) -> Result<bool>`
   - `get_daemon_status() -> Result<DaemonStatus>`
   - PID file read/write helpers.
4. ✅ Refactored `daemon.rs` to delegate to `DaemonProcessService`.
5. ✅ Target met: `daemon.rs` reduced from ~620 to ~267 lines.
6. ✅ Unit tests: 4 for `DaemonProcessService` + 4 for process primitives = 8 new tests, all passing.

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
- [x] `daemon.rs` uses `common::process` primitives for spawn/kill/health checks.
- [x] `auth.rs` does not contain `CredentialsStore` or file I/O logic.
- [ ] All four files have <400 lines of non-test code.
- [ ] Unit tests for the extracted services exist.

## Related Issues

- #019 — God Files & Mixed Concerns
- #014 — Extension Architecture Scattering
- #010 — Competing Session Abstractions
