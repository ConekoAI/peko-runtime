# ADR-065: Per-File Workspace Locks for Cross-Agent Write Serialization

**Status:** Accepted (2026-09-29). Implemented on branch
`feat/workspace-file-lock`.
**Date:** 2026-09-29
**Related:** F33 parallel tool dispatch gate
(`peko-rs/engine/src/parallel_gate.rs`), `peko-rs/fs-persistence`
(`FileLock`), AGENTS.md §3.2 (`tools/builtin/fs/`).
**Driver:** multiple agents (within one principal or across principals)
sharing a runtime process can concurrently `Write`/`Edit` the same
workspace file. The F33 gate serializes tools only *within* one agent's
runtime; across agents the tools performed unsynchronized
read-modify-writes, silently losing updates.

---

## 1. Context

### 1.1 The race

`WriteTool` and `EditTool` implement plain filesystem mutations:

- `Write` (overwrite/append): `fs::write` / open-append, full content.
- `Edit`: read file → string replace → `fs::write` full content.

Both declare `parallelizable() == false` (F33), which the engine's
`ParallelGate` honors with a write-lock on a **per-runtime**
`tokio::sync::RwLock` — so *within* one agent, two fs-mutating calls
never execute concurrently. The gate does not and cannot coordinate
across agents: two subagents in one principal, or agents of two
principals on the same daemon, race freely on the same path. The
realistic failure is **lost update / last-writer-wins** (each writer
produces a full-file image), not a torn file — but a lost update is
exactly the silent corruption agents cannot recover from, because the
winner's tool result reported success.

### 1.2 Options considered

1. **Single writer per runtime** — one process-wide mutex around all
   fs-mutating tool executions. Rejected:
   - Global head-of-line blocking: a bulk write stalls agents editing
     unrelated files, defeating the parallelism subagents exist for.
   - It does not close the hole: `Bash` mutations (`sed -i`, scripts)
     bypass any Write/Edit-level gate, so the cost is paid while the
     guarantee silently stops at the shell boundary.
   - Conflicts with the F33 design intent: serialize *only when
     needed*.
2. **Per-file advisory lock (chosen)** — contention only on the same
   canonical path; composes with F33 (gate = intra-agent ordering,
   file lock = cross-agent); the `Bash` escape hatch remains but the
   cost is proportional to actual same-file contention, and coverage
   can be extended incrementally.

### 1.3 Pre-existing infrastructure

`peko-fs-persistence::FileLock` already provides atomic `O_EXCL`
acquisition, PID-liveness + 30s-age stale recovery, and crash-safe
`Drop` release; the runtime's own JSONL stores depend on it. Two
defects in that crate had to be addressed before reuse (§2.3).

## 2. Decision

### 2.1 `WorkspaceFileLock` (fs-persistence)

New module `peko-rs/fs-persistence/src/workspace_lock.rs`:

- **Lock key:** SHA-256 of the canonical target path; lock files live
  in a runtime-owned directory (`<data_dir>/locks/<sha256>.lock`).
  This keeps `.lock` litter out of user workspaces, makes relative and
  absolute spellings of the same file contend on one lock, and avoids
  the `with_extension("lock")` collision (`foo.txt` vs `foo.md` →
  `foo.lock`; a file literally named `x.lock` colliding with its own
  lock). For not-yet-created targets, the nearest existing ancestor is
  canonicalized and the suffix re-appended, so two agents creating the
  same new file still contend.
- **Acquisition:** reuses `FileLock` via the new
  `FileLock::acquire_at(lock_path, timeout)` entry point (explicit
  lock-file path, since the hashed name cannot be derived from the
  data path by extension substitution).
- **Fail-fast, not block:** default timeout 5s
  (`DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS`). On expiry the error message
  is `file busy: another writer holds the lock for <path> …` — a
  structured, retryable condition. The calling model should re-read
  the file and retry, not assume destructive failure. This is the
  contract documented for models via the tool error text.
- Stale-lock recovery is inherited from `FileLock` (dead PID or
  >30s age ⇒ removable). Known edge: a single write held open longer
  than 30s could have its lock stolen; accepted as astronomically
  unlikely for workspace-sized files (the JSONL stores already accept
  the same trade).

### 2.2 Tool wiring

- `WriteTool` and `EditTool` gain `with_lock_dir(Option<PathBuf>)`.
  When set, the tool acquires the lock (before the read, for `Edit`)
  and holds it to the end of the mutation via `Drop`. When `None`
  (legacy/test construction), behavior is unchanged.
- Production sites — `engine/tool_runtime.rs::register_builtins` and
  `tools/registry/factory.rs` — pass
  `<default_data_dir()>/locks`.
- The F33 `parallelizable() == false` flags are unchanged: the gate
  still serializes fs-mutating tools within one agent; the file lock
  adds the missing cross-agent guarantee.

### 2.3 fs-persistence cleanup

- **`LockManager` removed.** It was dead code (no constructor call
  sites anywhere in the workspace) and its refcount branch was broken:
  when the lock was already held in-process it incremented the
  refcount and then *still* called `FileLock::acquire`, which fails
  against the holder's own lock file. In-process serialization belongs
  to the `ParallelGate` layer, not to a refcounted file-lock map.
- `FileLock::lock_path` (`with_extension("lock")`) is **unchanged**
  for its existing session-store callers — changing the derivation
  would strand lock files across a daemon upgrade. The collision
  hazard is avoided by not using derived paths for workspace locks.

### 2.4 Known limits (documented, not solved)

- `Bash` mutations bypass workspace locks. The guarantee is
  "Write/Edit-to-Write/Edit serialization"; agents mutating files
  through shell commands were and remain outside any lock scheme.
- Locks are advisory and per-runtime-directory: a process running
  with a different `PEKO_DATA_DIR` (or an external editor) does not
  contend.

## 3. Consequences

- Two agents editing the same file serialize; the loser either waits
  ≤5s and proceeds against fresh content, or gets a `file busy` error
  it can retry. Lost updates on Write/Edit paths are eliminated for
  daemon-managed agents.
- Unrelated files never contend; no head-of-line blocking.
- New dependency edges: `peko-fs-persistence` gains `sha2` and
  `dirs`; `peko` (core) gains `peko-fs-persistence` (previously only
  transitive via session/channel/plan).
- Lock files under `<data_dir>/locks/` are ephemeral; safe to delete
  while the daemon is stopped, self-healing (stale detection) while
  it runs.
