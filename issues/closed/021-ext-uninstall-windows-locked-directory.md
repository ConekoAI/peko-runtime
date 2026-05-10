# Issue 021: `ext uninstall` Leaves Locked Directories on Windows

**Status:** Resolved  
**Labels:** `bug`, `windows`, `extensions`, `packaging`  
**Related:** Issue 022 (MCP Python Processes Linger After `ext stop`), E2E test `e2e_tests/packaging/team_full_lifecycle.ps1`

---

## Problem

On Windows, `peko ext uninstall <id>` may fail to fully remove the extension directory from `%APPDATA%/pekobot/extensions/<id>` because the daemon (or a child process spawned by the extension, e.g. an MCP server) still holds an open handle to a file inside that directory.

Windows prevents deletion of files and directories while any handle is open. Unix systems allow unlinking open files (the inode persists until the last handle closes), so this bug is **Windows-only**.

### Symptom

After running `ext uninstall`, the extension directory may:
- Still exist as an empty or partially-deleted folder
- Cause a subsequent `ext install` of the same extension to fail with a sharing violation
- Leave orphan `*_old_*` or `*_uninstalled_*` directories behind

### Where the failure occurs

`src/extension/manager/storage.rs::remove_from_storage()` and `copy_to_storage()`:

Both functions try `std::fs::remove_dir_all()` first, then fall back to `std::fs::rename()` on Windows. However, if a recently-terminated child process still has transient file handles (common with Python MCP servers), **both** operations can fail with `ERROR_SHARING_VIOLATION` (os error 32) because Windows may not release handles immediately upon process termination.

The caller (`ExtensionManager::uninstall`) logs this as a warning and continues:

```rust
if let Err(e) = self.storage.remove_from_storage(id) {
    warn!("Failed to remove extension from storage: {}", e);
}
```

So the uninstall *appears* to succeed (the extension is removed from the in-memory registry and hooks are unregistered) but the filesystem artifact remains locked.

### Why it happens

`ExtensionManager::uninstall()` performs these steps:

1. Remove from in-memory `extensions` map
2. Unregister hooks from `ExtensionCore`
3. **Shutdown stateful adapters** (MCP servers, gateways, etc.) — `adapter.shutdown(state).await`
4. **Remove from filesystem** — `storage.remove_from_storage(id)`

Step 3 signals shutdown, but the actual process termination or handle release may not complete before step 4 runs. On Windows, `remove_dir_all` then gets `ERROR_SHARING_VIOLATION` (os error 32) or `ERROR_DIR_NOT_EMPTY`.

Even the rename fallback could fail if the lock is held strongly enough (e.g., the process's working directory is set to the extension folder).

---

## Fix Applied

### 1. Retry logic with delays in storage operations

Added `remove_dir_all_with_retry()` and `rename_with_retry()` helpers in `src/extension/manager/storage.rs`:

- **10 retries** with **200ms delays** between attempts
- Applied to both `remove_from_storage()` and `copy_to_storage()`
- This gives the OS time to clean up handles from recently-terminated processes

```rust
fn remove_dir_all_with_retry(path: &Path) -> std::io::Result<()> {
    let mut last_err = None;
    for attempt in 0..WINDOWS_REMOVE_RETRIES {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                thread::sleep(Duration::from_millis(WINDOWS_REMOVE_RETRY_DELAY_MS));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| ...))
}
```

### 2. Unified `remove_locked_dir()` helper

Extracted a shared `remove_locked_dir()` function used by `remove_from_storage()`:

1. Try `remove_dir_all` with retries (fast path)
2. On failure, atomically rename to a temp name with retries
3. Best-effort delete the renamed directory
4. Return `Ok(())` — the extension is logically removed

### 3. Retry logic in `ExtensionUnpackager::install()`

Also added retry logic in `src/extension/manager/packaging.rs` for the temp-directory cleanup during `.ext` package extraction.

### 4. Process termination delays (coordinated with Issue 022)

In `src/common/process/kill.rs`:
- `force_kill_child()` now sleeps **300ms** after successful `taskkill /T /F` on Windows, giving the OS time to finish terminating the tree before returning
- `graceful_shutdown()` now waits for `child.wait()` after force-kill and sleeps **300ms** before dropping the job object, ensuring handles are released before callers proceed

---

## Acceptance Criteria

- [x] `remove_from_storage` handles locked directories gracefully on Windows
- [x] `ext uninstall` followed by `ext install` of the same extension works reliably on Windows
- [x] E2E test workaround in `team_full_lifecycle.ps1` can be removed
- [x] No regressions on Linux/macOS

---

## Related Code

- `src/extension/manager/storage.rs` — `remove_from_storage()`, `copy_to_storage()`, `remove_locked_dir()`
- `src/extension/manager/packaging.rs` — `ExtensionUnpackager::install()`
- `src/common/process/kill.rs` — `graceful_shutdown()`, `force_kill_child()`
- `src/extension/manager/mod.rs` — `ExtensionManager::uninstall()`
- `e2e_tests/packaging/team_full_lifecycle.ps1`
