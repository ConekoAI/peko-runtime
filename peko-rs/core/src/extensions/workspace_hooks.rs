//! Workspace-resident hook scanner (ADR-047 §5 Phase 4).
//!
//! Hooks live under `<workspace>/hooks/<id>/hook.toml`. Each hook
//! declares a `binds` list with one or more of the four F31x observe-only
//! hook points (`PreToolUse` / `PostToolUse` / `Stop` / `AfterAgent`),
//! plus — since ADR-052 D6 — `PromptSection` binds whose command stdout
//! becomes a named section of the per-iteration `<runtime-context>`
//! tail message. Each bind names an external `command` to spawn when
//! the hook fires. Output defaults to JSON (matches Claude Code's hook
//! protocol) and falls back to plain text on the remaining hook points.
//!
//! The scanner is the single canonical path for hook discovery. The
//! legacy general-extension `hooks:` YAML block continues to work as a
//! compatibility path for now; deleting it is a separate PR.
//!
//! ## Manifest format
//!
//! ```toml
//! # ~/.peko/principal/alice/hooks/notify-on-write/hook.toml
//! binds = [
//!   { point = "PostToolUse", tool_name = "Write" },
//!   # ADR-052 D6: stdout becomes the `## weather` runtime-context section.
//!   { point = "PromptSection", section = "weather" },
//! ]
//!
//! command = "/usr/local/bin/peko-notify"
//! args = ["--hook", "write"]
//! timeout_secs = 10
//! output = "text"   # "json" (default) | "text"
//! env = { PEKO_PRINCIPAL = "alice" }
//! ```
//!
//! `tool_name` is an optional exact selector for `PreToolUse` / `PostToolUse`.
//! Omit it to observe all tools; wildcard selectors are rejected. `section` is
//! required for `PromptSection` (nonempty, no control characters). The six
//! points are PreToolUse, PostToolUse, Stop, AfterAgent, PromptSection, and
//! SessionContextBuild. Bindings run in registration order (directory names
//! sorted, then manifest bind order); legacy `priority` fields are ignored.
//! Prompt-section commands append their text to the named runtime-context
//! section, including built-in sections.
//!
//! ## Failure isolation
//!
//! Malformed manifests are logged at `warn!` and skipped; the scanner
//! continues with the next hook. A single broken `hook.toml` cannot
//! prevent other hooks from loading — the same posture as the MCP
//! scanner.

use crate::extensions::command_handler::{
    CommandHookConfig, CommandHookHandler, CommandOutputFormat,
};
use crate::extensions::workspace_dispatcher::{WorkspaceHookDispatcher, WorkspaceHookPoint};
use anyhow::{anyhow, Context, Result};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// One workspace binding. Tool selectors are optional exact names;
/// `PromptSection` requires a section name. Registration order controls dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindSpec {
    pub point: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// ADR-052 D6: prompt-section name for `PromptSection` binds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

/// The shape of a `<workspace>/hooks/<id>/hook.toml` manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookManifest {
    pub binds: Vec<BindSpec>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub output: Option<String>,
}

/// Scan `<workspace>/hooks/<id>/hook.toml` and register each hook with
/// the shared `WorkspaceHookDispatcher`. Returns the number of successfully-
/// registered hook bindings.
///
/// The scanner is additive over any hooks already registered by other
/// principals. Per-hook
/// failures are logged and skipped; the function returns the count of
/// hooks that successfully registered, never a hard error for
/// individual hook misconfigurations.
///
/// Mirrors [`crate::extensions::mcp::workspace::load_workspace_mcp_servers`]
/// in shape: a single canonical workspace scanner per tool surface,
/// called once per principal boot.
pub async fn load_workspace_hooks(
    hooks_dir: &Path,
    core: &WorkspaceHookDispatcher,
    principal_id: &PrincipalId,
) -> Result<usize> {
    if !hooks_dir.exists() {
        debug!(
            "Hooks workspace dir does not exist, skipping: {}",
            hooks_dir.display()
        );
        return Ok(0);
    }

    let mut entries = match tokio::fs::read_dir(hooks_dir).await {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Failed to read hooks workspace dir {}: {e}",
                hooks_dir.display()
            );
            return Ok(0);
        }
    };

    let mut dir_entries = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        dir_entries.push(entry);
    }

    dir_entries.sort_by_key(|entry| entry.file_name());
    let mut loaded = 0usize;
    for entry in dir_entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(hook_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };

        let manifest_path = path.join("hook.toml");
        if !manifest_path.exists() {
            debug!(
                "Hook {} has no hook.toml manifest; skipping",
                path.display()
            );
            continue;
        }

        match register_one_hook(&manifest_path, &hook_id, core, principal_id).await {
            Ok(n) => loaded += n,
            Err(e) => warn!(
                "Hook manifest {} failed to register: {e:#}",
                manifest_path.display()
            ),
        }
    }

    if loaded > 0 {
        info!(
            "registered {loaded} workspace hook binding(s) from {}",
            hooks_dir.display()
        );
    }

    Ok(loaded)
}

async fn register_one_hook(
    manifest_path: &Path,
    hook_id: &str,
    core: &WorkspaceHookDispatcher,
    principal_id: &PrincipalId,
) -> Result<usize> {
    let raw = tokio::fs::read_to_string(manifest_path)
        .await
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: HookManifest =
        toml::from_str(&raw).with_context(|| format!("parse {}", manifest_path.display()))?;

    if manifest.binds.is_empty() {
        return Err(anyhow!(
            "hook {} has empty `binds` list — at least one of \
             PreToolUse / PostToolUse / Stop / AfterAgent / PromptSection / SessionContextBuild is required",
            manifest_path.display()
        ));
    }

    // Each manifest yields one `CommandHookHandler` per bind. The
    // handler's `extension_dir` is the hook's own directory so relative
    // `command` paths resolve next to the manifest.
    let hook_dir = manifest_path
        .parent()
        .ok_or_else(|| anyhow!("manifest path has no parent: {}", manifest_path.display()))?
        .to_path_buf();

    let output_format = match manifest.output.as_deref() {
        Some("text") => CommandOutputFormat::Text,
        Some("json") => CommandOutputFormat::Json,
        Some(other) => {
            warn!("Hook {hook_id} has unknown output format `{other}`; defaulting to JSON");
            CommandOutputFormat::Json
        }
        None => CommandOutputFormat::Json,
    };

    let config = CommandHookConfig {
        command: manifest.command.clone(),
        args: manifest.args.clone(),
        env: manifest.env.clone(),
        timeout_secs: manifest
            .timeout_secs
            .unwrap_or(crate::extensions::command_handler::DEFAULT_COMMAND_TIMEOUT_SECS),
        output_format,
    };

    // Validate every bind before registering any part of this manifest.
    let points = manifest
        .binds
        .iter()
        .map(|bind| bind_to_point(bind, hook_id))
        .collect::<Result<Vec<_>>>()?;
    let mut registered = 0usize;
    for point in points {
        let handler = Arc::new(CommandHookHandler::new(
            config.clone(),
            hook_dir.clone(),
            point.clone(),
        ));
        core.register_hook(point, handler, principal_id).await?;
        registered += 1;
    }

    Ok(registered)
}

/// Resolve a workspace binding. Omit `tool_name` to observe all tools;
/// present selectors must be exact names. Priorities no longer affect order.
fn bind_to_point(bind: &BindSpec, hook_id: &str) -> Result<WorkspaceHookPoint> {
    match bind.point.as_str() {
        "PreToolUse" | "PostToolUse" => {
            if let Some(name) = &bind.tool_name {
                if name.trim().is_empty() || name.contains(['*', '?']) {
                    return Err(anyhow!("hook {hook_id}: tool_name must be an exact name; omit it to observe all tools"));
                }
            }
            if bind.point == "PreToolUse" {
                Ok(WorkspaceHookPoint::PreToolUse { tool_name: bind.tool_name.clone() })
            } else {
                Ok(WorkspaceHookPoint::PostToolUse { tool_name: bind.tool_name.clone() })
            }
        }
        "Stop" => Ok(WorkspaceHookPoint::Stop),
        "AfterAgent" => Ok(WorkspaceHookPoint::AfterAgent),
        "SessionContextBuild" => Ok(WorkspaceHookPoint::SessionContextBuild),
        "PromptSection" => {
            let section = bind.section.as_deref().ok_or_else(|| anyhow!("PromptSection bind requires section (hook {hook_id})"))?;
            if section.trim().is_empty() || section.chars().any(char::is_control) {
                return Err(anyhow!("PromptSection section must be nonempty and contain no control characters (hook {hook_id})"));
            }
            Ok(WorkspaceHookPoint::PromptSection { section: section.into() })
        }
        other => Err(anyhow!("hook {hook_id}: unsupported bind point {other}; allowed: PreToolUse | PostToolUse | Stop | AfterAgent | PromptSection | SessionContextBuild")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::runtime::ToolingRuntime;
    use peko_extension_api::{PromptSectionRequest, ToolCallSpec, ToolFunnel};

    fn manifest(binds: &str) -> HookManifest {
        toml::from_str(&format!("binds = [{binds}]\ncommand = \"/bin/echo\"\n")).unwrap()
    }

    #[test]
    fn selectors_are_exact_or_absent_and_retired_points_are_rejected() {
        for point in [
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "AfterAgent",
            "SessionContextBuild",
        ] {
            assert!(bind_to_point(
                &manifest(&format!("{{ point = \"{point}\" }}")).binds[0],
                "test"
            )
            .is_ok());
        }
        for selector in ["*", "mcp:*", "Read?"] {
            let m = manifest(&format!(
                "{{ point = \"PreToolUse\", tool_name = \"{selector}\" }}"
            ));
            assert!(bind_to_point(&m.binds[0], "test").is_err());
        }
        assert!(bind_to_point(&manifest("{ point = \"AgentInit\" }").binds[0], "test").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn commands_render_tail_sections_and_observe_owner_context_in_registration_order() {
        let temp = tempfile::TempDir::new().unwrap();
        let hooks = temp.path().join("hooks");
        let output = temp.path().join("seen.txt");
        for (id, text, priority) in [("b", "second", 1000), ("a", "first", -1000)] {
            let dir = hooks.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("hook.toml"), format!(r#"
binds = [{{ point = "PromptSection", section = "weather", priority = {priority} }}, {{ point = "PreToolUse" }}, {{ point = "PostToolUse", tool_name = "missing" }}, {{ point = "Stop" }}, {{ point = "AfterAgent" }}, {{ point = "SessionContextBuild" }}, {{ point = "PromptSection", section = "session_context" }}]
command = "/bin/sh"
args = ["-c", "printf '%s:%s:%s:%s\\n' '{text}' \"$PEKO_PRINCIPAL_ID\" \"$PEKO_WORKSPACE\" \"$PEKO_HOOK_POINT\" >> '{output}'; printf '{text}'"]
output = "text"
"#, output=output.display())).unwrap();
        }
        let runtime = ToolingRuntime::standalone();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        assert_eq!(
            load_workspace_hooks(&hooks, runtime.hooks(), &p1)
                .await
                .unwrap(),
            14
        );
        let request = PromptSectionRequest {
            principal_id: p1.to_string(),
            workspace: temp.path().to_string_lossy().into_owned(),
            session_id: "session".into(),
        };
        let sections = runtime.render_prompt_sections(&request).await;
        assert_eq!(sections.get("weather"), Some("first\nsecond"));
        assert_eq!(
            sections.get("session_context"),
            Some("first\nfirst\nsecond\nsecond")
        );
        let other = runtime
            .render_prompt_sections(&PromptSectionRequest {
                principal_id: p2.to_string(),
                ..request.clone()
            })
            .await;
        assert_eq!(other.get("weather"), None);
        let mut call = ToolCallSpec::new("missing", serde_json::json!({}));
        call.principal_id = Some(p1.to_string());
        call.workspace = Some(request.workspace.clone());
        assert!(!runtime.execute(call).await.unwrap().2);
        use peko_extension_api::EngineHooks;
        runtime
            .fire_stop_hook(serde_json::json!({"principal_id":p1,"workspace":request.workspace}))
            .await;
        runtime
            .fire_after_agent_hook(
                serde_json::json!({"principal_id":p1,"workspace":request.workspace}),
            )
            .await;
        let lines = std::fs::read_to_string(&output).unwrap();
        assert_eq!(lines.lines().count(), 14);
        assert!(lines
            .lines()
            .all(|line| line.contains(&p1.to_string()) && line.contains(&request.workspace)));
        for point in [
            "tool.pre.missing",
            "tool.post.missing",
            "agent.stop",
            "agent.after",
        ] {
            let matching: Vec<_> = lines.lines().filter(|line| line.ends_with(point)).collect();
            assert!(matching[0].starts_with("first:"));
            assert!(matching[1].starts_with("second:"));
        }
    }

    #[tokio::test]
    async fn invalid_manifest_has_no_partial_registration_and_other_hooks_still_load() {
        let temp = tempfile::TempDir::new().unwrap();
        for (id, binds) in [
            ("bad", "{point = \"Stop\"}, {point = \"retired\"}"),
            ("good", "{point = \"Stop\"}"),
        ] {
            let dir = temp.path().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("hook.toml"),
                format!("binds = [{binds}]\ncommand = \"/bin/echo\"\n"),
            )
            .unwrap();
        }
        let dispatcher = WorkspaceHookDispatcher::new();
        assert_eq!(
            load_workspace_hooks(temp.path(), &dispatcher, &PrincipalId::generate())
                .await
                .unwrap(),
            1
        );
        assert_eq!(dispatcher.hook_count().await, 1);
    }
}
