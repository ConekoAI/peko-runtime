# Issue 021: `ext uninstall` Leaves Locked Directories on Windows

**Status:** Open  
**Labels:** `bug`, `windows`, `extensions`, `packaging`  
**Related:** E2E test workaround in `e2e_tests/packaging/team_full_lifecycle.ps1`

---

## Problem

On Windows, `peko ext uninstall <id>` may fail to fully remove the extension directory from `%APPDATA%/pekobot/extensions/<id>` because the daemon (or a child process spawned by the extension, e.g. an MCP server) still holds an open handle to a file inside that directory.

Windows prevents deletion of files and directories while any handle is open. Unix systems allow unlinking open files (the inode persists until the last handle closes), so this bug is **Windows-only**.

### Symptom

After running `ext uninstall`, the extension directory may:
- Still exist as an empty or partially-deleted folder
- Cause a subsequent `ext install` of the same extension to fail with a sharing violation
- Leave orphan `*_old_*` directories behind (from the install-side workaround in `copy_to_storage`)

### Where the failure occurs

`src/extension/manager/storage.rs::remove_from_storage()` (line 84):

```rust
pub fn remove_from_storage(&self, extension_id: &ExtensionId) -> Result<()> {
    let target_dir = storage_dir.join(&extension_id.0);
    if target_dir.exists() {
        std::fs::remove_dir_all(&target_dir)  // Fails if locked
            .with_context(|| format!("Failed to remove extension at {target_dir:?}"))?;
    }
    Ok(())
}
```

The caller (`ExtensionManager::uninstall`) logs this as a warning and continues:

```rust
if let Err(e) = self.storage.remove_from_storage(id) {
    warn!("Failed to remove extension from storage: {}", e);
}
```

So the uninstall *appears* to succeed (the extension is removed from the in-memory registry and hooks are unregistered) but the filesystem artifact remains locked.

### Why it happens

`ExtensionManager::uninstall()` (`src/extension/manager/mod.rs:436`) performs these steps:

1. Remove from in-memory `extensions` map
2. Unregister hooks from `ExtensionCore`
3. **Shutdown stateful adapters** (MCP servers, gateways, etc.) — `adapter.shutdown(state).await`
4. **Remove from filesystem** — `storage.remove_from_storage(id)`

Step 3 is async and signals shutdown, but the actual process termination or handle release may not complete before step 4 runs. On Windows, `remove_dir_all` then gets `ERROR_SHARING_VIOLATION` (os error 32) or `ERROR_DIR_NOT_EMPTY`.

### Existing workaround (install side)

`copy_to_storage()` already has a Windows-specific fallback:

```rust
if let Err(e) = std::fs::remove_dir_all(&target_dir) {
    let temp_name = format!("{}_old_{}", extension_id.0, ...);
    let temp_dir = storage_dir.join(&temp_name);
    if std::fs::rename(&target_dir, &temp_dir).is_ok() {
        let _ = std::fs::remove_dir_all(&temp_dir); // Best-effort
    }
}
```

`remove_from_storage` has **no equivalent logic**.

### Test impact

The `team_full_lifecycle.ps1` E2E test currently works around this by skipping `ext uninstall` before re-installing pulled `.ext` packages, relying on `ext install`'s overwrite behavior. The final cleanup `ext uninstall` at test teardown is marked "best effort".

---

## Proposed Fix

Apply the same rename-then-best-effort-delete pattern from `copy_to_storage` to `remove_from_storage`:

1. Try `remove_dir_all` directly first (fast path)
2. On failure, atomically rename the locked directory to a temp name (e.g. `<id>_uninstalled_<timestamp>`)
3. Return `Ok(())` — the extension is logically uninstalled
4. Best-effort delete the renamed directory (may succeed later when handles are released)

This matches the existing install-side strategy and is a minimal, safe change.

---

## Acceptance Criteria

- [ ] `remove_from_storage` handles locked directories gracefully on Windows
- [ ] `ext uninstall` followed by `ext install` of the same extension works reliably on Windows
- [ ] E2E test workaround in `team_full_lifecycle.ps1` can be removed
- [ ] No regressions on Linux/macOS

---

## Related Code

- `src/extension/manager/storage.rs` — `remove_from_storage()`
- `src/extension/manager/mod.rs` — `ExtensionManager::uninstall()`
- `e2e_tests/packaging/team_full_lifecycle.ps1` — commented-out uninstall block
