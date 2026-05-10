# Issue 022: MCP Python Processes Linger After `ext stop`

**Status:** Resolved  
**Labels:** `bug`, `windows`, `mcp`, `extensions`, `process-management`  
**Related:** Issue 021 (`ext uninstall` Leaves Locked Directories on Windows), E2E test workaround in `e2e_tests/packaging/team_full_lifecycle.ps1`

---

## Problem

`peko ext stop <mcp-extension>` signals the MCP runtime to shut down, but the underlying Python child process (the actual MCP server) is not reliably terminated. The process lingers in the background, holding open file handles to the extension directory.

This causes a cascading failure:

1. `ext stop standard-echo` → returns success, but Python process is still running
2. `ext uninstall standard-echo` → fails to remove the directory because the Python process holds a file lock (Windows os error 32)
3. `ext pull` or `ext install` of the same extension → fails because the old directory still exists and is locked

### Symptom

After running `ext stop` on an MCP extension:

- The extension's Python process (`python.exe` running the MCP server script) is still visible in Task Manager / `tasklist`
- `ext uninstall` fails with: `Failed to remove extension at "...": 另一个程序正在使用此文件，进程无法访问。 (os error 32)`
- The extension directory under `%APPDATA%/pekobot/extensions/<id>` cannot be deleted until the Python process is manually killed

### Where the failure occurs

The MCP runtime adapter starts a Python process via `std::process::Command` (or similar process spawning). When `ext stop` is called, the shutdown signal is sent to the runtime adapter, but:

- The adapter may only close stdin/stdout pipes or send a JSON-RPC `exit` notification
- The Python process may not actually exit because:
  - It has no handler for the shutdown signal
  - It's blocked on I/O and never checks for the signal
  - It's a child-of-child process (e.g., Python spawns another Python via `uvicorn` or `stdio` transport)
  - The Windows job object or process group is not configured to terminate children together

### Why it happens

On **Windows**, process trees are not automatically terminated when the parent exits. Unlike Unix where `SIGHUP` or process group signals can cascade, Windows requires explicit use of:

- **Job Objects** (`CreateJobObject` / `AssignProcessToJobObject` with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) to ensure child processes die with the parent
- **Explicit child enumeration and termination** via `EnumProcesses` or `NtQueryInformationProcess`
- **Ctrl+Break signal** sent to the process group

The current implementation likely relies on the MCP adapter to "do the right thing" when its transport closes, but Python's default behavior on Windows is to ignore pipe closure and keep running.

### E2E test impact

The `team_full_lifecycle.ps1` E2E test currently works around this with a `Stop-McpProcesses` helper that brute-force kills any `python.exe` process whose command line matches the extension name:

```powershell
function Stop-McpProcesses {
    Get-CimInstance Win32_Process -Filter "Name='python.exe'" | Where-Object {
        $_.CommandLine -match "standard-echo|mcp.*echo"
    } | ForEach-Object {
        Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
}
```

This is fragile because:
- It may kill unrelated Python processes
- It relies on string matching the command line
- It is not a general solution for all MCP extensions

---

## Root Cause Analysis

### Process spawning

The MCP extension runtime spawns a Python process. The exact location depends on the adapter implementation, but typically:

- `src/extensions/mcp/runtime/` — MCP runtime adapters and starters
- `src/daemon/background_runtime/` — generic process supervision (traits, manager, supervisor)

The generic process supervision layer (`background_runtime`) provides traits for process lifecycle but may not enforce Windows job object semantics.

### Shutdown path

When `ext stop` is called:

1. CLI sends stop request to daemon
2. Daemon calls `ExtensionManager::stop_runtime()` or equivalent
3. Runtime adapter receives shutdown signal
4. Adapter signals the transport to close
5. **Python process should exit, but doesn't**

The gap is between step 4 and 5. The transport closure is not a reliable termination mechanism on Windows for Python processes that:
- Use `asyncio` event loops
- Have blocking I/O threads
- Spawn their own child processes

---

## Fix Applied

### Approach: Windows Job Objects + Improved Force Kill

The fix combines **Option A** (Windows Job Objects) with **Option C** (Timeout + Force Kill), exactly as recommended.

#### Changes Made

1. **`Cargo.toml`** — Added `Win32_System_JobObjects` feature to the existing `windows-sys` dependency.

2. **`src/common/process/job_object.rs`** (new) — Windows job object wrapper:
   - Creates a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
   - Provides `assign_process()` to add a child process immediately after spawn
   - `Drop` closes the handle, triggering OS-level tree kill
   - Marked `Send + Sync` so it can live inside `ManagedRuntime`

3. **`src/common/process/mod.rs`** — Exports `JobObject`.

4. **`src/common/process/config.rs`** — Added `use_job_object: bool` flag (default `true`).

5. **`src/common/process/spawn.rs`** — On Windows, creates a job object and assigns the spawned child to it before returning.

6. **`src/common/process/kill.rs`** —
   - `graceful_shutdown()` now accepts an optional `JobObject`; the job handle is dropped after the force-kill attempt, ensuring any surviving descendants are terminated.
   - `force_kill_child()` on Windows now tries `taskkill /T /F /PID` first (kills the whole tree) before falling back to `child.kill()`.

7. **`src/daemon/background_runtime/supervisor.rs`** —
   - `RuntimeKind::Process` stores the `JobObject` on Windows.
   - `stop_runtime()` passes the job to `graceful_shutdown()`.

8. **`e2e_tests/packaging/team_full_lifecycle.ps1`** — Removed the `Stop-McpProcesses` workaround.

### Why This Works

- **Job objects** provide an OS-level guarantee: when the last handle is closed, Windows kills **all** processes in the job, including grandchildren spawned by the Python MCP server.
- **`taskkill /T`** is a fallback that kills the entire process tree explicitly.
- On **Unix**, behavior is unchanged — process group signals already handle this.

---

## Acceptance Criteria

- [x] `ext stop` on an MCP extension reliably terminates the underlying Python process on Windows
- [x] `ext uninstall` immediately after `ext stop` succeeds without file lock errors
- [x] Child processes spawned by the MCP server (e.g., `uvicorn`, nested Python) are also terminated
- [x] E2E test workaround (`Stop-McpProcesses`) can be removed
- [x] No regressions on Linux/macOS

---

## Related Code

- `src/extensions/mcp/runtime/` — MCP runtime adapters, tool proxies, starters
- `src/daemon/background_runtime/` — generic process supervision (manager, supervisor, adapter traits)
- `src/extension/manager/` — extension lifecycle (install, enable, start, stop, uninstall)
- `e2e_tests/packaging/team_full_lifecycle.ps1` — `Stop-McpProcesses` workaround

---

## Related Issues

- Issue 021: `ext uninstall` Leaves Locked Directories on Windows — the symptom that this issue causes
