//! `Workflow` builtin (ADR-061 D1/D6/D7, phase 2b) — runs an
//! agent-authored Python file from `<workspace>/workflows/` as an OS
//! subprocess with the caller's identity injected into its environment.
//!
//! A workflow is *procedural memory*: loops, conditionals, and polling
//! the principal wrote once and can re-run (manually, or via
//! `CronCreate` → `SpawnTool` → `Workflow`) instead of re-deriving the
//! procedure in-context every turn. Everything the workflow does that
//! matters re-enters the daemon as an attributed `ExecuteTool` call —
//! the runner itself performs no privileged effects.
//!
//! ## Spawn-time env injection (D6)
//!
//! The child gets a **minimal** environment (never the daemon's full
//! env — provider keys and other secrets must not leak into workflows):
//! `PATH`/`HOME` (+ platform bits) for interpreter usability, plus the
//! PEKO identity set:
//!
//! | Var | Value |
//! |---|---|
//! | `PEKO_DAEMON_SOCK` | daemon's unix socket (`ipc::default_socket_path()`) |
//! | `PEKO_WORKSPACE` | the calling principal's workspace path |
//! | `PEKO_PRINCIPAL_ID` | the calling principal's stable id |
//! | `PEKO_SESSION_KEY` | the calling session's attribution key (below) |
//! | `PEKO_RUN_TOKEN` | freshly minted run token (TTL = timeout + 60s) |
//! | `PEKO_WORKFLOW_DEPTH` | nesting depth of the spawned process |
//!
//! `PEKO_SESSION_KEY` round-trips through
//! [`peko_session::key::parse_session_key`] with `parts.agent` = the
//! calling principal's name — that is the whole contract, since the
//! `ExecuteTool` handler resolves attribution from exactly that
//! segment. On the agentic-loop path `ToolContext.session_id` carries
//! the session UUID, so the key is constructed as
//! `agent:{principal}:workflow:{session_uuid}`; on a nested
//! (workflow→`ExecuteTool`) path the handler threads the parent's
//! session key through, and it is reused verbatim.
//!
//! ## Guardrails (D8)
//!
//! - **Recursion**: `PEKO_WORKFLOW_DEPTH` at/above
//!   [`MAX_WORKFLOW_DEPTH`] refuses the spawn. The depth is
//!   server-derived: the `ExecuteTool` handler stamps it from the
//!   validated run token (`_workflow_depth`, stripped from the wire
//!   otherwise); the process env is the fallback for direct calls.
//! - **Output discipline**: stdout/stderr are captured with a bounded
//!   TAIL ([`OUTPUT_CAP_BYTES`] each) + a truncation marker — nothing
//!   unbounded reaches the caller's context.
//! - **Timeout/abort**: default 300s, hard cap 3600s; on timeout or
//!   `ToolContext` abort the child is killed (plain `kill`, no process
//!   group, for the spike).
//!
//! ## `workflows/` prompt catalog
//!
//! [`WorkspaceWorkflowsPromptHandler`] renders the per-turn catalog of
//! `*.py` files (name + first docstring line) into the tail
//! `<runtime-context>` message — presence = visibility, mirroring the
//! agents/skills handlers (ADR-050).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use peko_session::key::{parse_session_key, sanitize_key_component};
use peko_tools_core::{Tool, ToolContext, ToolError};

use crate::extensions::framework::core::{HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{HookOutput, HookResult, ToolRuntimeContext};
use crate::ipc::run_tokens::RunTokenRegistry;
use crate::principal::manager::PrincipalManager;
use crate::principal::Principal;

/// Synthetic tool name surfaced to the LLM. Single source of truth so
/// registration sites (daemon) and tests don't drift. Also read by the
/// `ExecuteTool` handler, which stamps `_workflow_depth` into calls
/// carrying a validated run token.
pub const WORKFLOW_TOOL_NAME: &str = "Workflow";

/// Maximum workflow nesting depth (ADR-061 D8). A workflow running at
/// this depth may not spawn another workflow — agent → wf(1) → wf(2)
/// is the deepest legal chain.
pub const MAX_WORKFLOW_DEPTH: u32 = 2;

/// Default run timeout when `timeout_ms` is absent (5 minutes).
const DEFAULT_TIMEOUT_MS: u64 = 300_000;
/// Hard ceiling on `timeout_ms` (1 hour).
const MAX_TIMEOUT_MS: u64 = 3_600_000;
/// Per-stream capture cap; the TAIL is kept when a stream overflows.
const OUTPUT_CAP_BYTES: usize = 16 * 1024;
/// Marker prepended to a truncated stream (the kept portion is the
/// tail, so the marker sits at the top).
const TRUNCATION_MARKER: &str = "[… truncated by peko: showing the last 16 KiB …]\n";
/// Extra lifetime minted onto a run token past the run timeout.
const RUN_TOKEN_TTL_MARGIN: Duration = Duration::from_mins(1);

/// `Workflow` runner tool.
///
/// Holds a `Weak<PrincipalManager>` (caller resolution, same pattern
/// as `ModelCallTool`) and the daemon-shared `RunTokenRegistry` it
/// mints spawn tokens from.
pub struct WorkflowTool {
    principals: Weak<PrincipalManager>,
    run_tokens: Arc<RunTokenRegistry>,
}

impl WorkflowTool {
    /// Construct with the daemon's principal manager + run-token
    /// registry.
    #[must_use]
    pub fn new(principals: Weak<PrincipalManager>, run_tokens: Arc<RunTokenRegistry>) -> Self {
        Self {
            principals,
            run_tokens,
        }
    }

    /// Resolve the calling principal server-side from
    /// `ctx.principal_name`. Fail-closed: the runner never spawns an
    /// unattributed process.
    async fn resolve_caller(&self, ctx: &ToolContext) -> anyhow::Result<Arc<Principal>> {
        let name = ctx
            .principal_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "Workflow requires a calling-principal context \
                     (ToolContext.principal_name is unset); refusing to spawn an \
                     unattributed process"
                )
            })?;
        let manager = self.principals.upgrade().ok_or_else(|| {
            anyhow!(
                "Workflow is not wired to a PrincipalManager on this runtime; \
                 the tool is only available in daemon mode"
            )
        })?;
        manager.get_by_name(name).await.ok_or_else(|| {
            anyhow!("Workflow: unknown principal '{name}'; refusing an unattributed spawn")
        })
    }
}

impl std::fmt::Debug for WorkflowTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &'static str {
        WORKFLOW_TOOL_NAME
    }

    fn description(&self) -> String {
        "Run an agent-authored Python workflow from this principal's \
         `workflows/` directory as a subprocess. `path` is relative to \
         `workflows/` (e.g. \"triage.py\"); `args` are passed to the script; \
         `timeout_ms` defaults to 300000 (capped at 3600000). The workflow \
         process runs with PEKO_* identity env injected and can call back \
         into the runtime (ModelCall, Glob, …) via the peko_workflow SDK; \
         every callback is attributed to this principal and passes the \
         capability gate. Returns exit code plus bounded stdout/stderr \
         tails. Use when: a saved procedure (loop, poll, batch) should run \
         as code instead of a turn-by-turn agent loop. Don't use when: the \
         task is one tool call — call the tool directly."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workflow file relative to the principal's `workflows/` directory (e.g. \"triage.py\"). Must be a `.py` file inside that directory."
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional argv passed to the script."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Run timeout in milliseconds (default 300000, max 3600000). On timeout the process is killed."
                },
                "_workflow_depth": {
                    "type": "integer",
                    "description": "Server-injected nesting depth (ExecuteTool run-token path). Callers must not set this; the daemon strips/overwrites it."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn parallelizable(&self) -> bool {
        // Subprocess spawn with per-run env/cwd; exclusive like Bash.
        false
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        // The funnel always routes through `execute_with_context`;
        // without a ToolContext there is no principal to attribute the
        // spawn to, so the bare entry point refuses.
        Err(ToolError::Other(
            "Workflow requires a ToolContext (principal attribution); \
             invoke it through the extension funnel, not Tool::execute"
                .to_string(),
        )
        .into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // ── 1. Parse arguments ───────────────────────────────────────
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("Workflow: `path` is required"))?;
        let args: Vec<String> = params
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|&t| t > 0)
            .map_or(DEFAULT_TIMEOUT_MS, |t| t.min(MAX_TIMEOUT_MS));

        // ── 2. Recursion guard (D8) — server-derived depth first ─────
        // `_workflow_depth` arrives via the ExecuteTool handler (stamped
        // from the validated run token); the env var is the fallback for
        // direct (non-IPC) invocations. Both default to 0.
        let caller_depth = params
            .get("_workflow_depth")
            .and_then(Value::as_u64)
            .and_then(|d| u32::try_from(d).ok())
            .or_else(|| {
                std::env::var("PEKO_WORKFLOW_DEPTH")
                    .ok()
                    .and_then(|v| v.parse::<u32>().ok())
            })
            .unwrap_or(0);
        if caller_depth >= MAX_WORKFLOW_DEPTH {
            bail!(
                "Workflow refused: nesting depth {caller_depth} reaches the \
                 maximum of {MAX_WORKFLOW_DEPTH} — a workflow may not spawn \
                 itself (ADR-061 D8)"
            );
        }

        // ── 3. Caller attribution (server-side) ──────────────────────
        let principal = self.resolve_caller(ctx).await?;
        let principal_name = principal.name().await;
        let workspace = principal.workspace_path.clone();

        // ── 4. Path guard: canonicalize inside <workspace>/workflows ──
        let script = resolve_workflow_path(&workspace, path)?;

        // ── 5. Interpreter ───────────────────────────────────────────
        let python = which::which("python3").map_err(|_| {
            anyhow!(
                "Workflow: `python3` not found on PATH — install Python 3 (or put a \
                 `python3` shim on PATH) to run workflows"
            )
        })?;

        // ── 6. Identity env (D6) + run token ─────────────────────────
        let session_key = workflow_session_key(&principal_name, ctx.session_id.as_deref());
        let run_token = self.run_tokens.mint(
            &principal_name,
            &session_key,
            caller_depth + 1,
            Duration::from_millis(timeout_ms) + RUN_TOKEN_TTL_MARGIN,
        );
        let env = build_workflow_env(
            &workspace,
            &principal,
            &session_key,
            &run_token,
            caller_depth + 1,
        );

        // ── 7. Spawn with bounded capture ────────────────────────────
        let started = Instant::now();
        let mut command = tokio::process::Command::new(python);
        command
            .arg(&script)
            .args(&args)
            .current_dir(&workspace)
            .env_clear()
            .envs(&env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| anyhow!("Workflow: failed to spawn python3 for '{path}': {e}"))?;

        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let stdout_task = tokio::spawn(async move {
            match stdout_pipe.as_mut() {
                Some(pipe) => read_tail(pipe, OUTPUT_CAP_BYTES).await,
                None => (Vec::new(), false),
            }
        });
        let stderr_task = tokio::spawn(async move {
            match stderr_pipe.as_mut() {
                Some(pipe) => read_tail(pipe, OUTPUT_CAP_BYTES).await,
                None => (Vec::new(), false),
            }
        });

        enum Outcome {
            Exited(std::process::ExitStatus),
            TimedOut,
            Aborted,
        }
        let outcome = {
            let mut abort_rx = ctx.abort_signal();
            tokio::select! {
                status = child.wait() => {
                    match status {
                        Ok(status) => Outcome::Exited(status),
                        Err(e) => {
                            return Err(anyhow!("Workflow: waiting for '{path}' failed: {e}"));
                        }
                    }
                }
                () = tokio::time::sleep(Duration::from_millis(timeout_ms)) => Outcome::TimedOut,
                _ = abort_rx.changed() => Outcome::Aborted,
            }
        };

        if matches!(outcome, Outcome::TimedOut | Outcome::Aborted) {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        let (stdout_tail, stdout_truncated) = stdout_task.await.unwrap_or_default();
        let (stderr_tail, stderr_truncated) = stderr_task.await.unwrap_or_default();
        let stdout = render_stream(&stdout_tail, stdout_truncated);
        let stderr = render_stream(&stderr_tail, stderr_truncated);
        let duration_ms = started.elapsed().as_millis() as u64;

        match outcome {
            Outcome::Aborted => {
                // The token dies with the run it was minted for.
                Err(ToolError::Aborted.into())
            }
            Outcome::TimedOut => Ok(json!({
                "workflow": path,
                "success": false,
                "timed_out": true,
                "exit_code": Value::Null,
                "duration_ms": duration_ms,
                "stdout": stdout,
                "stderr": stderr,
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
            })),
            Outcome::Exited(status) => Ok(json!({
                "workflow": path,
                "success": status.success(),
                "timed_out": false,
                "exit_code": status.code(),
                "duration_ms": duration_ms,
                "stdout": stdout,
                "stderr": stderr,
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
            })),
        }
    }
}

/// Resolve `rel` to a canonical path inside `<workspace>/workflows/`,
/// refusing absolute paths, `..` traversal, symlink escapes, missing
/// files, and non-`.py` targets.
fn resolve_workflow_path(workspace: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    if Path::new(rel).is_absolute() {
        bail!("Workflow: `path` must be relative to the workspace workflows/ directory, got absolute '{rel}'");
    }
    let root = workspace.join("workflows");
    let canonical_root = root.canonicalize().map_err(|_| {
        anyhow!(
            "Workflow: no workflows/ directory in the principal workspace ({})",
            root.display()
        )
    })?;
    let candidate = root.join(rel);
    // canonicalize resolves symlinks, so a symlink pointing outside the
    // root fails the starts_with check below.
    let canonical = candidate.canonicalize().map_err(|_| {
        anyhow!(
            "Workflow: '{rel}' not found under {}",
            canonical_root.display()
        )
    })?;
    if !canonical.starts_with(&canonical_root) {
        bail!("Workflow: '{rel}' escapes the workflows/ directory — refused");
    }
    if !canonical.is_file() {
        bail!("Workflow: '{rel}' is not a file");
    }
    if canonical.extension().and_then(|e| e.to_str()) != Some("py") {
        bail!("Workflow: '{rel}' is not a Python (.py) file");
    }
    Ok(canonical)
}

/// Build the session key injected as `PEKO_SESSION_KEY` (D6). The key
/// must round-trip through `parse_session_key` with `agent` = the
/// calling principal's name — that segment is what `ExecuteTool`'s
/// attribution resolves from.
///
/// - Nested path (workflow → `ExecuteTool` → `Workflow`): the handler
///   threads the parent's session key into `ToolContext.session_id`;
///   it already names this principal, so reuse it verbatim.
/// - Agent-loop path: `ctx.session_id` carries the session UUID; wrap
///   it as `agent:{principal}:workflow:{uuid}`.
/// - No session context: `agent:{principal}:workflow:direct`.
fn workflow_session_key(principal_name: &str, session_id: Option<&str>) -> String {
    match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(key) if parse_session_key(key).agent == principal_name => key.to_string(),
        Some(uuid) => format!(
            "agent:{}:workflow:{}",
            sanitize_key_component(principal_name),
            sanitize_key_component(uuid)
        ),
        None => format!(
            "agent:{}:workflow:direct",
            sanitize_key_component(principal_name)
        ),
    }
}

/// The minimal child environment (D6): platform usability vars only,
/// plus the PEKO identity set. The daemon's full env is never
/// inherited — secrets must not leak into workflow processes.
fn build_workflow_env(
    workspace: &Path,
    principal: &Principal,
    session_key: &str,
    run_token: &str,
    depth: u32,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    // Presence-filtered platform passthrough: PATH (interpreter lookup
    // for anything the script itself shells out to), HOME (python's
    // user dirs), locale + temp dirs, Windows process basics.
    for key in [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "TMPDIR",
        "TEMP",
        "TMP",
        "SystemRoot",
        "COMSPEC",
        "USERPROFILE",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert(
        crate::ipc::DAEMON_SOCK_ENV.to_string(),
        crate::ipc::default_socket_path()
            .to_string_lossy()
            .to_string(),
    );
    env.insert(
        "PEKO_WORKSPACE".to_string(),
        workspace.to_string_lossy().to_string(),
    );
    env.insert("PEKO_PRINCIPAL_ID".to_string(), principal.id.0.clone());
    env.insert("PEKO_SESSION_KEY".to_string(), session_key.to_string());
    env.insert("PEKO_RUN_TOKEN".to_string(), run_token.to_string());
    env.insert("PEKO_WORKFLOW_DEPTH".to_string(), depth.to_string());
    env
}

/// Read a child stream to EOF, keeping only the last `cap` bytes.
/// Returns `(tail, truncated)`.
async fn read_tail(reader: &mut (impl AsyncReadExt + Unpin), cap: usize) -> (Vec<u8>, bool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > cap {
                    let excess = buf.len() - cap;
                    buf.drain(..excess);
                    truncated = true;
                }
            }
        }
    }
    (buf, truncated)
}

/// Render a captured stream for the result JSON: lossy UTF-8, with the
/// truncation marker prepended when the tail was cut.
fn render_stream(tail: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(tail);
    if truncated {
        format!("{TRUNCATION_MARKER}{text}")
    } else {
        text.into_owned()
    }
}

// ────────────────────────────────────────────────────────────────────
// `workflows/` prompt catalog (D1, phase 2b)
// ────────────────────────────────────────────────────────────────────

/// Default priority for the workflows-catalog prompt section. Sits just
/// under the agents/skills catalogs (both 90).
pub const WORKFLOW_CATALOG_HOOK_PRIORITY: i32 = 88;

/// Hard cap on the rendered workflows catalog (same budget as the
/// skills catalog); on overflow whole lines are dropped from the end
/// and a directory-listing pointer is appended.
const WORKFLOWS_CATALOG_MAX_BYTES: usize = 8 * 1024;

/// Cache key: per-file `(relative_path, mtime, len)` stats for every
/// `*.py` in the workflows dir, sorted by path for determinism.
type DirFingerprint = Vec<(String, SystemTime, u64)>;

/// Workspace-scanning handler for the `workflows` prompt section.
///
/// Registered once per core (see `principal/context.rs`). At invoke
/// time it resolves the workspace from the hook context's
/// `ToolRuntimeContext`, scans `<workspace>/workflows/*.py`, and
/// renders one line per workflow: `- {name}: {first docstring line}
/// (workflows/{name}.py)`. Presence = visibility (ADR-050); no
/// capability filter. A missing dir / no workspace yields
/// [`HookResult::PassThrough`] so the section is stripped.
#[derive(Debug, Default)]
pub struct WorkspaceWorkflowsPromptHandler {
    cache: Mutex<Option<(DirFingerprint, String)>>,
}

impl WorkspaceWorkflowsPromptHandler {
    /// Create a new workspace-scanning workflows handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn render_catalog(&self, workspace: &str) -> Option<String> {
        let workflows_dir = Path::new(workspace).join("workflows");
        let key = workflows_dir_fingerprint(&workflows_dir)?;

        {
            let cache = self.cache.lock().expect("workflows catalog cache poisoned");
            if let Some((cached_key, text)) = &*cache {
                if *cached_key == key {
                    return (!text.is_empty()).then(|| text.clone());
                }
            }
        }

        let text = scan_workflows_dir(&workflows_dir, workspace);

        let mut cache = self.cache.lock().expect("workflows catalog cache poisoned");
        *cache = Some((key, text.clone()));

        (!text.is_empty()).then_some(text)
    }
}

/// Fingerprint every `*.py` directly under `workflows_dir` as
/// `(relative_path, mtime, len)` entries, sorted by path. `None` when
/// the dir doesn't exist (section retracts).
fn workflows_dir_fingerprint(workflows_dir: &Path) -> Option<DirFingerprint> {
    let mut stats = Vec::new();
    for entry in std::fs::read_dir(workflows_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        let rel = path
            .strip_prefix(workflows_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        stats.push((rel, mtime, meta.len()));
    }
    stats.sort();
    Some(stats)
}

#[async_trait]
impl HookHandler for WorkspaceWorkflowsPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let workspace = ctx
            .get_state::<ToolRuntimeContext>("tool_context")
            .and_then(|rtc| rtc.workspace.clone());

        let Some(workspace) = workspace.filter(|w| !w.is_empty()) else {
            return HookResult::PassThrough;
        };

        match self.render_catalog(&workspace) {
            Some(text) => HookResult::Continue(HookOutput::Text(text)),
            None => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "workflows".to_string(),
            priority: WORKFLOW_CATALOG_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        WORKFLOW_CATALOG_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        "WorkspaceWorkflowsPromptHandler".to_string()
    }
}

/// Scan `<workspace>/workflows/*.py` (top level only) and render the
/// catalog, capped at [`WORKFLOWS_CATALOG_MAX_BYTES`]. Unreadable files
/// are skipped; files without a docstring render name-only.
fn scan_workflows_dir(workflows_dir: &Path, workspace: &str) -> String {
    let entries = match std::fs::read_dir(workflows_dir) {
        Ok(entries) => entries,
        Err(_) => return String::new(),
    };

    let mut lines = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let description = std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| first_docstring_line(&content));
        let line = match description {
            Some(desc) => format!("- {stem}: {desc} (workflows/{stem}.py)"),
            None => format!("- {stem} (workflows/{stem}.py)"),
        };
        lines.push(line);
    }
    // Deterministic ordering — directory iteration order is
    // platform-dependent.
    lines.sort();

    let notice =
        format!("(more workflows in {workspace}/workflows/ — list the directory to see all)");
    let mut out = String::new();
    let mut iter = lines.iter().peekable();
    while let Some(line) = iter.next() {
        // Reserve room for the truncation notice whenever more lines
        // remain, so the total stays under the cap even on overflow.
        let reserve = if iter.peek().is_some() {
            notice.len() + 1
        } else {
            0
        };
        if !out.is_empty() && out.len() + line.len() + 1 + reserve > WORKFLOWS_CATALOG_MAX_BYTES {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    if iter.peek().is_some() {
        out.push('\n');
        out.push_str(&notice);
    }
    out
}

/// Extract the first text line of a module docstring, if the file opens
/// with one. Shebangs, comments, and blank lines are skipped first;
/// both `"""` and `'''` delimiters are recognized; one-line
/// (`"""text"""`) and multi-line forms both yield their first non-empty
/// line. A file whose first statement is not a docstring returns `None`.
fn first_docstring_line(content: &str) -> Option<String> {
    let mut in_doc = false;
    let mut quote = "";
    for line in content.lines() {
        let trimmed = line.trim();
        if !in_doc {
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut matched = false;
            for q in ["\"\"\"", "'''"] {
                if let Some(rest) = trimmed.strip_prefix(q) {
                    in_doc = true;
                    quote = q;
                    matched = true;
                    if let Some(one_line) = rest.strip_suffix(q) {
                        // `"""text"""` form (or the empty `""""""`).
                        let text = one_line.trim();
                        return (!text.is_empty()).then(|| text.to_string());
                    }
                    // Multi-line form: first-line content wins when present.
                    let text = rest.trim();
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                    break;
                }
            }
            if !matched {
                return None;
            }
        } else {
            let text = trimmed.trim_end_matches(quote).trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
            if trimmed.ends_with(quote) {
                // Docstring closed without any content line.
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::extensions::framework::core::ExtensionServices;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use peko_auth::Subject;
    use peko_extension_api::Capabilities;
    use tempfile::TempDir;

    fn principal_config(name: &str) -> crate::principal::PrincipalConfig {
        crate::principal::PrincipalConfig {
            name: name.to_string(),
            id: None,
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Capabilities::starter_bundle(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![],
            preferred_model_id: None,
            quota: None,
            children: Default::default(),
        }
    }

    struct Fixture {
        _temp: TempDir,
        manager: Arc<PrincipalManager>,
        tool: WorkflowTool,
        workspace: PathBuf,
    }

    /// PrincipalManager with one principal, plus the Workflow tool wired
    /// to it (mirror of the model_call fixture). The principal's
    /// workspace gets an empty `workflows/` directory.
    async fn fixture(name: &str) -> Fixture {
        let temp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let manager = Arc::new(PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        manager
            .create(principal_config(name))
            .await
            .expect("create principal");
        let workspace = manager
            .get_by_name(name)
            .await
            .expect("principal")
            .workspace_path
            .clone();
        std::fs::create_dir_all(workspace.join("workflows")).expect("workflows dir");

        let tool = WorkflowTool::new(Arc::downgrade(&manager), Arc::new(RunTokenRegistry::new()));
        Fixture {
            _temp: temp,
            manager,
            tool,
            workspace,
        }
    }

    fn ctx_for(principal_name: &str, session_id: Option<&str>) -> ToolContext {
        let ctx =
            ToolContext::default_for_tool(WORKFLOW_TOOL_NAME).with_principal_name(principal_name);
        match session_id {
            Some(id) => ctx.with_session_id(id),
            None => ctx,
        }
    }

    fn write_workflow(workspace: &Path, name: &str, body: &str) {
        std::fs::write(workspace.join("workflows").join(name), body).expect("write workflow");
    }

    fn python3_available() -> bool {
        which::which("python3").is_ok()
    }

    // ── Path guard ───────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn path_guard_refuses_parent_traversal() {
        let fx = fixture("guard").await;
        // An escaping target that exists on disk (so canonicalize
        // succeeds and only the prefix check refuses it).
        let outside = fx.workspace.join("outside.py");
        std::fs::write(&outside, "print('no')").expect("write outside");
        let err = fx
            .tool
            .execute_with_context(json!({"path": "../outside.py"}), &ctx_for("guard", None))
            .await
            .expect_err("traversal must be refused");
        assert!(
            format!("{err:#}").contains("escapes"),
            "error should name the escape: {err:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn path_guard_refuses_absolute_path() {
        let fx = fixture("guard").await;
        let err = fx
            .tool
            .execute_with_context(json!({"path": "/etc/passwd.py"}), &ctx_for("guard", None))
            .await
            .expect_err("absolute path must be refused");
        assert!(format!("{err:#}").contains("relative"), "got: {err:#}");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn path_guard_refuses_symlink_escape() {
        let fx = fixture("guard").await;
        let outside = fx._temp.path().join("secret.py");
        std::fs::write(&outside, "print('no')").expect("write outside");
        std::os::unix::fs::symlink(&outside, fx.workspace.join("workflows").join("link.py"))
            .expect("symlink");
        let err = fx
            .tool
            .execute_with_context(json!({"path": "link.py"}), &ctx_for("guard", None))
            .await
            .expect_err("symlink escape must be refused");
        assert!(format!("{err:#}").contains("escapes"), "got: {err:#}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn path_guard_refuses_non_python_file() {
        let fx = fixture("guard").await;
        write_workflow(&fx.workspace, "run.sh", "#!/bin/sh\n");
        let err = fx
            .tool
            .execute_with_context(json!({"path": "run.sh"}), &ctx_for("guard", None))
            .await
            .expect_err("non-.py must be refused");
        assert!(format!("{err:#}").contains(".py"), "got: {err:#}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn path_guard_missing_file_is_clear_error() {
        let fx = fixture("guard").await;
        let err = fx
            .tool
            .execute_with_context(json!({"path": "nope.py"}), &ctx_for("guard", None))
            .await
            .expect_err("missing file must error");
        assert!(format!("{err:#}").contains("not found"), "got: {err:#}");
    }

    // ── Depth guard ──────────────────────────────────────────────────

    /// A caller at MAX depth is refused before the filesystem or the
    /// interpreter are touched (works with a nonexistent path).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn depth_guard_refuses_at_max() {
        let fx = fixture("deep").await;
        let err = fx
            .tool
            .execute_with_context(
                json!({"path": "anything.py", "_workflow_depth": MAX_WORKFLOW_DEPTH}),
                &ctx_for("deep", None),
            )
            .await
            .expect_err("max depth must refuse");
        assert!(format!("{err:#}").contains("nesting depth"), "got: {err:#}");
    }

    /// The env var is the fallback depth channel for direct (non-IPC)
    /// invocations.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn depth_guard_reads_env_fallback() {
        let fx = fixture("depenv").await;
        std::env::set_var("PEKO_WORKFLOW_DEPTH", "2");
        let result = fx
            .tool
            .execute_with_context(json!({"path": "anything.py"}), &ctx_for("depenv", None))
            .await;
        std::env::remove_var("PEKO_WORKFLOW_DEPTH");
        let err = result.expect_err("env depth 2 must refuse");
        assert!(format!("{err:#}").contains("nesting depth"), "got: {err:#}");
    }

    // ── Session-key construction ─────────────────────────────────────

    #[test]
    fn session_key_round_trips_through_parse() {
        // Agent-loop path: UUID wrapped into the workflow namespace.
        let key = workflow_session_key("caller", Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(
            key,
            "agent:caller:workflow:550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(parse_session_key(&key).agent, "caller");

        // Nested path: a parent workflow's key naming this principal is
        // reused verbatim.
        let parent = "agent:caller:workflow:abc";
        assert_eq!(workflow_session_key("caller", Some(parent)), parent);

        // A foreign key (another principal) is never trusted: it is
        // sanitized into the identifier segment.
        let foreign = workflow_session_key("caller", Some("agent:other:workflow:x"));
        assert_eq!(parse_session_key(&foreign).agent, "caller");

        // No session context: direct marker.
        assert_eq!(
            workflow_session_key("caller", None),
            "agent:caller:workflow:direct"
        );
    }

    // ── Principal attribution ────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn unknown_principal_fails_closed() {
        let fx = fixture("known").await;
        write_workflow(&fx.workspace, "ok.py", "print('hi')\n");
        let err = fx
            .tool
            .execute_with_context(json!({"path": "ok.py"}), &ctx_for("ghost", None))
            .await
            .expect_err("unknown principal must error");
        assert!(format!("{err:#}").contains("ghost"), "got: {err:#}");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn missing_principal_context_fails_closed() {
        let fx = fixture("known").await;
        write_workflow(&fx.workspace, "ok.py", "print('hi')\n");
        let ctx = ToolContext::default_for_tool(WORKFLOW_TOOL_NAME);
        let err = fx
            .tool
            .execute_with_context(json!({"path": "ok.py"}), &ctx)
            .await
            .expect_err("missing principal context must error");
        assert!(format!("{err:#}").contains("principal"), "got: {err:#}");
    }

    // ── Interpreter resolution ───────────────────────────────────────

    /// With PATH shadowed to an empty dir, interpreter resolution fails
    /// with a message naming `python3`. Serial + restore because PATH is
    /// process-global.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn missing_interpreter_is_clear_error() {
        let fx = fixture("nointerp").await;
        write_workflow(&fx.workspace, "ok.py", "print('hi')\n");
        let saved = std::env::var("PATH").ok();
        let empty = fx._temp.path().join("empty-path");
        std::fs::create_dir(&empty).expect("empty dir");
        std::env::set_var("PATH", &empty);
        let result = fx
            .tool
            .execute_with_context(json!({"path": "ok.py"}), &ctx_for("nointerp", None))
            .await;
        match saved {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        let err = result.expect_err("missing interpreter must error");
        assert!(format!("{err:#}").contains("python3"), "got: {err:#}");
    }

    // ── Subprocess behavior (python3-gated) ──────────────────────────

    /// A script runs to completion: identity env is injected, args pass
    /// through, and the daemon's own secrets do NOT leak (env_clear).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn runs_script_and_injects_identity_env() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let fx = fixture("runner").await;
        write_workflow(
            &fx.workspace,
            "env_dump.py",
            r#"import json, os, sys
keys = ["PEKO_DAEMON_SOCK", "PEKO_WORKSPACE", "PEKO_PRINCIPAL_ID",
        "PEKO_SESSION_KEY", "PEKO_RUN_TOKEN", "PEKO_WORKFLOW_DEPTH",
        "PEKO_WORKFLOW_TEST_SENTINEL"]
print(json.dumps({k: os.environ.get(k) for k in keys}))
print("argv:" + ",".join(sys.argv[1:]))
"#,
        );
        // Sentinel proves the daemon env is not blanket-inherited.
        std::env::set_var("PEKO_WORKFLOW_TEST_SENTINEL", "should-not-leak");

        let out = fx
            .tool
            .execute_with_context(
                json!({
                    "path": "env_dump.py",
                    "args": ["one", "two"],
                }),
                &ctx_for("runner", Some("550e8400-e29b-41d4-a716-446655440000")),
            )
            .await
            .expect("run");
        std::env::remove_var("PEKO_WORKFLOW_TEST_SENTINEL");

        assert_eq!(out["success"], true, "got: {out}");
        assert_eq!(out["exit_code"], 0);
        assert!(out["stdout"].as_str().unwrap().contains("argv:one,two"));

        let env_json = out["stdout"]
            .as_str()
            .unwrap()
            .lines()
            .next()
            .expect("first stdout line");
        let env: serde_json::Value = serde_json::from_str(env_json).expect("env json");

        let ws = fx.workspace.to_string_lossy().to_string();
        assert_eq!(env["PEKO_WORKSPACE"].as_str().unwrap(), ws);
        let principal = fx.manager.get_by_name("runner").await.expect("principal");
        assert_eq!(env["PEKO_PRINCIPAL_ID"].as_str().unwrap(), principal.id.0);
        assert_eq!(
            env["PEKO_SESSION_KEY"].as_str().unwrap(),
            "agent:runner:workflow:550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(env["PEKO_WORKFLOW_DEPTH"].as_str().unwrap(), "1");
        let token = env["PEKO_RUN_TOKEN"].as_str().unwrap();
        assert_eq!(token.len(), 43, "32-byte url-safe token");
        assert!(
            env["PEKO_DAEMON_SOCK"]
                .as_str()
                .unwrap()
                .ends_with("daemon.sock"),
            "got: {}",
            env["PEKO_DAEMON_SOCK"].as_str().unwrap()
        );
        assert!(
            env["PEKO_WORKFLOW_TEST_SENTINEL"].is_null(),
            "daemon env must not leak into the workflow"
        );
    }

    /// Non-zero exit is reported as data (`success: false`,
    /// `exit_code`), not as a tool error.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn nonzero_exit_is_reported_not_errored() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let fx = fixture("runner").await;
        write_workflow(
            &fx.workspace,
            "fail.py",
            "import sys\nprint('oops', file=sys.stderr)\nsys.exit(3)\n",
        );
        let out = fx
            .tool
            .execute_with_context(json!({"path": "fail.py"}), &ctx_for("runner", None))
            .await
            .expect("run");
        assert_eq!(out["success"], false);
        assert_eq!(out["exit_code"], 3);
        assert_eq!(out["timed_out"], false);
        assert!(out["stderr"].as_str().unwrap().contains("oops"));
    }

    /// Output streams are capped: the tail is kept, the marker is
    /// prepended, and the flag is set.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn output_is_capped_to_tail() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let fx = fixture("runner").await;
        write_workflow(
            &fx.workspace,
            "flood.py",
            "print('a' * (1024 * 1024))\nprint('TAIL-MARKER')\n",
        );
        let out = fx
            .tool
            .execute_with_context(json!({"path": "flood.py"}), &ctx_for("runner", None))
            .await
            .expect("run");
        assert_eq!(out["stdout_truncated"], true, "got: {out}");
        let stdout = out["stdout"].as_str().unwrap();
        assert!(stdout.starts_with(TRUNCATION_MARKER), "marker missing");
        assert!(stdout.ends_with("TAIL-MARKER\n"), "tail missing");
        assert!(
            stdout.len() <= TRUNCATION_MARKER.len() + OUTPUT_CAP_BYTES + 16,
            "len: {}",
            stdout.len()
        );
    }

    /// A runaway script is killed at the timeout; partial output and
    /// `timed_out` come back as data.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn timeout_kills_runaway_script() {
        if !python3_available() {
            eprintln!("skipping: python3 not on PATH");
            return;
        }
        let fx = fixture("runner").await;
        write_workflow(
            &fx.workspace,
            "hang.py",
            "import time\nprint('start', flush=True)\ntime.sleep(30)\n",
        );
        let started = std::time::Instant::now();
        let out = fx
            .tool
            .execute_with_context(
                json!({"path": "hang.py", "timeout_ms": 300}),
                &ctx_for("runner", None),
            )
            .await
            .expect("run");
        let elapsed = started.elapsed();
        assert_eq!(out["timed_out"], true, "got: {out}");
        assert_eq!(out["success"], false);
        assert_eq!(out["exit_code"], serde_json::Value::Null);
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "kill should be prompt, took {elapsed:?}"
        );
    }

    // ── Prompt catalog ───────────────────────────────────────────────

    fn workflows_hook_ctx(workspace: Option<&str>) -> HookContext {
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "workflows".to_string(),
                priority: WORKFLOW_CATALOG_HOOK_PRIORITY,
            },
            crate::extensions::framework::types::HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );
        if let Some(ws) = workspace {
            ctx.set_state(
                "tool_context",
                ToolRuntimeContext::new()
                    .with_workspace(ws)
                    .with_principal_id("test-principal"),
            );
        }
        ctx
    }

    fn handle_text(result: HookResult) -> Option<String> {
        match result {
            HookResult::Continue(HookOutput::Text(text)) => Some(text),
            _ => None,
        }
    }

    #[tokio::test]
    async fn workflows_catalog_renders_name_and_docstring() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("workflows");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("triage.py"),
            "#!/usr/bin/env python3\n\"\"\"Triage inbound signals.\"\"\"\nimport os\n",
        )
        .unwrap();
        // Multi-line docstring: first content line wins.
        std::fs::write(
            dir.join("monitor.py"),
            "\"\"\"\nMonitor channels and wake on anomalies.\n\nDetails here.\n\"\"\"\n",
        )
        .unwrap();
        // No docstring → name-only line.
        std::fs::write(dir.join("plain.py"), "print('x')\n").unwrap();
        // Non-Python files are skipped.
        std::fs::write(dir.join("notes.txt"), "not a workflow").unwrap();

        let handler = WorkspaceWorkflowsPromptHandler::new();
        let text = handle_text(
            handler
                .handle(workflows_hook_ctx(Some(&temp.path().to_string_lossy())))
                .await,
        )
        .expect("expected catalog text");

        assert!(
            text.contains("- triage: Triage inbound signals. (workflows/triage.py)"),
            "got: {text}"
        );
        assert!(
            text.contains(
                "- monitor: Monitor channels and wake on anomalies. (workflows/monitor.py)"
            ),
            "got: {text}"
        );
        assert!(text.contains("- plain (workflows/plain.py)"), "got: {text}");
        assert!(!text.contains("notes"), "got: {text}");
        // Sorted by name.
        let monitor_pos = text.find("monitor").unwrap();
        let plain_pos = text.find("plain").unwrap();
        let triage_pos = text.find("triage").unwrap();
        assert!(
            monitor_pos < plain_pos && plain_pos < triage_pos,
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn workflows_catalog_passes_through_without_workspace() {
        let handler = WorkspaceWorkflowsPromptHandler::new();
        let result = handler.handle(workflows_hook_ctx(None)).await;
        assert!(matches!(result, HookResult::PassThrough));
    }

    #[tokio::test]
    async fn workflows_catalog_passes_through_on_missing_dir() {
        let temp = TempDir::new().unwrap();
        let handler = WorkspaceWorkflowsPromptHandler::new();
        let result = handler
            .handle(workflows_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;
        assert!(matches!(result, HookResult::PassThrough));
    }

    #[tokio::test]
    async fn workflows_catalog_rescans_on_change() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("workflows");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "\"\"\"First.\"\"\"\n").unwrap();

        let handler = WorkspaceWorkflowsPromptHandler::new();
        let ws = temp.path().to_string_lossy().to_string();

        let first =
            handle_text(handler.handle(workflows_hook_ctx(Some(&ws))).await).expect("catalog");
        assert!(
            first.contains("- a: First. (workflows/a.py)"),
            "got: {first}"
        );
        assert!(!first.contains("b.py"), "got: {first}");

        std::fs::write(dir.join("b.py"), "\"\"\"Second.\"\"\"\n").unwrap();
        let second =
            handle_text(handler.handle(workflows_hook_ctx(Some(&ws))).await).expect("catalog");
        assert!(
            second.contains("b.py"),
            "rescan must see the new file: {second}"
        );
    }

    #[test]
    fn workflows_catalog_truncates_at_byte_cap() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("workflows");
        std::fs::create_dir(&dir).unwrap();
        // ~200 workflows × ~90 bytes each — well over the 8 KB cap.
        for i in 0..200 {
            std::fs::write(
                dir.join(format!("wf-{i:03}.py")),
                "\"\"\"A workflow description long enough to fill the catalog quickly.\"\"\"\n",
            )
            .unwrap();
        }
        let ws = temp.path().to_string_lossy().to_string();
        let out = scan_workflows_dir(&dir, &ws);
        assert!(
            out.len() <= WORKFLOWS_CATALOG_MAX_BYTES,
            "len: {}",
            out.len()
        );
        assert!(out.contains("(more workflows in "), "got: {out}");
        assert!(out.contains("list the directory to see all"), "got: {out}");
        assert!(out.contains("- wf-000:"), "first entry survives: {out}");
    }

    #[test]
    fn first_docstring_line_variants() {
        assert_eq!(
            first_docstring_line("\"\"\"One line.\"\"\"\n"),
            Some("One line.".to_string())
        );
        assert_eq!(
            first_docstring_line("#!/usr/bin/env python3\n# comment\n\n\"\"\"Real.\"\"\"\n"),
            Some("Real.".to_string())
        );
        assert_eq!(
            first_docstring_line("\"\"\"\nFirst content line.\nMore.\n\"\"\"\n"),
            Some("First content line.".to_string())
        );
        assert_eq!(
            first_docstring_line("'''single quotes'''\n"),
            Some("single quotes".to_string())
        );
        // First statement is not a docstring.
        assert_eq!(first_docstring_line("import os\n"), None);
        // Empty docstring.
        assert_eq!(first_docstring_line("\"\"\"\"\"\"\n"), None);
        assert_eq!(first_docstring_line("\"\"\"\n\n\"\"\"\n"), None);
        assert_eq!(first_docstring_line(""), None);
    }
}
