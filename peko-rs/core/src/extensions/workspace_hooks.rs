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
//!   { point = "PromptSection", section = "weather", priority = 50 },
//! ]
//!
//! command = "/usr/local/bin/peko-notify"
//! args = ["--hook", "write"]
//! timeout_secs = 10
//! output = "text"   # "json" (default) | "text"
//! env = { PEKO_PRINCIPAL = "alice" }
//! ```
//!
//! `tool_name` is required for `PreToolUse` / `PostToolUse` and ignored
//! for `Stop` / `AfterAgent`. Wildcard patterns (`"mcp:*"`) match the
//! `HookRegistry::get_hooks_for_point` grammar. `section` is required
//! for `PromptSection` (non-empty, no control characters); `priority`
//! is optional (default 100) and orders handlers within a section.
//! A `PromptSection` bind MAY deliberately reuse a built-in name
//! (`identity` / `agents` / `skills`) to augment the built-in catalog —
//! the registry aggregates all handlers for the section.
//!
//! ## Failure isolation
//!
//! Malformed manifests are logged at `warn!` and skipped; the scanner
//! continues with the next hook. A single broken `hook.toml` cannot
//! prevent other hooks from loading — the same posture as the MCP and
//! universal-tool scanners.

use crate::extensions::command_handler::{
    CommandHookConfig, CommandHookHandler, CommandOutputFormat,
};
use crate::extensions::framework::core::hook_points::{HookPoint, HookPointBuilder};
use crate::extensions::framework::core::ExtensionCore;
use crate::extensions::framework::types::ExtensionId;
use anyhow::{anyhow, Context, Result};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// One entry in a hook's `binds` list.
///
/// `point` selects one of the four F31x observe-only hook points or —
/// since ADR-052 D6 — `PromptSection`; the remaining fields are
/// point-specific. `tool_name` is required for `PreToolUse` /
/// `PostToolUse` and ignored for `Stop` / `AfterAgent`. `section` is
/// required for `PromptSection`; `priority` optionally overrides the
/// default section priority (100).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindSpec {
    pub point: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// ADR-052 D6: prompt-section name for `PromptSection` binds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// ADR-052 D6: handler ordering within the section (higher first;
    /// default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
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
/// the global `ExtensionCore`. Returns the number of successfully-
/// registered hook bindings.
///
/// The scanner is additive over any hooks already registered by other
/// paths (built-in agent prompts, general extensions). Per-hook
/// failures are logged and skipped; the function returns the count of
/// hooks that successfully registered, never a hard error for
/// individual hook misconfigurations.
///
/// Mirrors [`crate::extensions::mcp::workspace::load_workspace_mcp_servers`]
/// and [`crate::extensions::universal::workspace::load_workspace_universal_tools`]
/// in shape: a single canonical workspace scanner per tool surface,
/// called once per principal boot.
pub async fn load_workspace_hooks(
    hooks_dir: &Path,
    core: &ExtensionCore,
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
    core: &ExtensionCore,
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
             PreToolUse / PostToolUse / Stop / AfterAgent / PromptSection is required",
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

    // Each workspace hook gets a per-principal ExtensionId. The hook
    // registry uses this to filter handlers when `active_extensions`
    // is passed by the dispatcher (the principal only sees hooks
    // registered under its own scope or the system scope).
    let extension_id = ExtensionId::new(format!("principal:{}/hook:{}", principal_id, hook_id));

    let mut registered = 0usize;
    for bind in &manifest.binds {
        let point = bind_to_point(bind, hook_id)?;
        let handler = Arc::new(CommandHookHandler::new(
            config.clone(),
            hook_dir.clone(),
            point.clone(),
        ));
        core.register_hook(point, handler, &extension_id)
            .await
            .with_context(|| format!("register bind {:?} for hook {hook_id}", bind.point))?;
        registered += 1;
    }

    Ok(registered)
}

/// Resolve a `BindSpec` into a concrete `HookPoint`. Wildcards are
/// passed through — the `HookRegistry::get_hooks_for_point` grammar
/// (`tool.execute.*`, `mcp:*`, etc.) handles pattern matching.
fn bind_to_point(bind: &BindSpec, hook_id: &str) -> Result<HookPoint> {
    match bind.point.as_str() {
        "PreToolUse" => {
            let tool = bind
                .tool_name
                .as_deref()
                .ok_or_else(|| anyhow!("PreToolUse bind requires `tool_name` (hook {hook_id})"))?;
            Ok(HookPointBuilder::pre_tool_use(tool))
        }
        "PostToolUse" => {
            let tool = bind
                .tool_name
                .as_deref()
                .ok_or_else(|| anyhow!("PostToolUse bind requires `tool_name` (hook {hook_id})"))?;
            Ok(HookPointBuilder::post_tool_use(tool))
        }
        "Stop" => Ok(HookPointBuilder::stop()),
        "AfterAgent" => Ok(HookPointBuilder::after_agent()),
        // ADR-052 D6: a `PromptSection` bind turns the command's stdout
        // into a named `<runtime-context>` tail section. No other
        // validation — a hook MAY deliberately bind a built-in name
        // ("identity" / "agents" / "skills") to augment the built-in
        // catalog; registry aggregation concatenates all handlers for
        // the section.
        "PromptSection" => {
            let section = bind
                .section
                .as_deref()
                .ok_or_else(|| anyhow!("PromptSection bind requires `section` (hook {hook_id})"))?;
            if section.trim().is_empty() {
                return Err(anyhow!(
                    "PromptSection bind `section` must not be empty or whitespace-only \
                     (hook {hook_id})"
                ));
            }
            if section.chars().any(char::is_control) {
                return Err(anyhow!(
                    "PromptSection bind `section` must not contain control characters \
                     or newlines (hook {hook_id})"
                ));
            }
            Ok(HookPoint::PromptSystemSection {
                section: section.to_string(),
                priority: bind.priority.unwrap_or(100),
            })
        }
        other => Err(anyhow!(
            "hook {hook_id} bind point `{other}` is not supported; \
             allowed: PreToolUse | PostToolUse | Stop | AfterAgent | PromptSection"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_pre_tool_use_manifest() {
        let raw = r#"
binds = [{ point = "PreToolUse", tool_name = "Bash" }]
command = "/bin/echo"
"#;
        let m: HookManifest = toml::from_str(raw).unwrap();
        assert_eq!(m.binds.len(), 1);
        assert_eq!(m.binds[0].point, "PreToolUse");
        assert_eq!(m.binds[0].tool_name.as_deref(), Some("Bash"));
        assert_eq!(m.command, "/bin/echo");
        assert!(m.args.is_empty());
        assert!(m.env.is_empty());
        assert_eq!(m.timeout_secs, None);
    }

    #[test]
    fn parses_full_manifest_with_env_timeout_and_pattern() {
        let raw = r#"
binds = [
    { point = "PreToolUse", tool_name = "mcp:*" },
    { point = "PostToolUse", tool_name = "Write" },
    { point = "Stop" },
    { point = "AfterAgent" },
]
command = "./notify.sh"
args = ["--hook", "write"]
env = { PEKO_PRINCIPAL = "alice", PEKO_DRY_RUN = "1" }
timeout_secs = 5
output = "text"
"#;
        let m: HookManifest = toml::from_str(raw).unwrap();
        assert_eq!(m.binds.len(), 4);
        assert_eq!(m.args, vec!["--hook", "write"]);
        assert_eq!(
            m.env.get("PEKO_PRINCIPAL").map(String::as_str),
            Some("alice")
        );
        assert_eq!(m.timeout_secs, Some(5));
        assert_eq!(m.output.as_deref(), Some("text"));
    }

    #[test]
    fn bind_to_point_pre_tool_use_requires_tool_name() {
        let bind = BindSpec {
            point: "PreToolUse".into(),
            tool_name: None,
            section: None,
            priority: None,
        };
        let err = bind_to_point(&bind, "h").unwrap_err();
        assert!(err.to_string().contains("requires `tool_name`"));
    }

    #[test]
    fn bind_to_point_rejects_unsupported_kind() {
        let bind = BindSpec {
            point: "SessionStart".into(),
            tool_name: None,
            section: None,
            priority: None,
        };
        let err = bind_to_point(&bind, "h").unwrap_err();
        assert!(err.to_string().contains("SessionStart"));
        assert!(err.to_string().contains("PreToolUse"));
    }

    #[test]
    fn bind_to_point_maps_all_four_supported_kinds() {
        let pre = bind_to_point(
            &BindSpec {
                point: "PreToolUse".into(),
                tool_name: Some("Bash".into()),
                section: None,
                priority: None,
            },
            "h",
        )
        .unwrap();
        let post = bind_to_point(
            &BindSpec {
                point: "PostToolUse".into(),
                tool_name: Some("Bash".into()),
                section: None,
                priority: None,
            },
            "h",
        )
        .unwrap();
        let stop = bind_to_point(
            &BindSpec {
                point: "Stop".into(),
                tool_name: None,
                section: None,
                priority: None,
            },
            "h",
        )
        .unwrap();
        let after = bind_to_point(
            &BindSpec {
                point: "AfterAgent".into(),
                tool_name: None,
                section: None,
                priority: None,
            },
            "h",
        )
        .unwrap();
        assert_eq!(pre.name(), "tool.pre.Bash");
        assert_eq!(post.name(), "tool.post.Bash");
        assert_eq!(stop.name(), "loop.stop");
        assert_eq!(after.name(), "agent.after");
    }

    #[test]
    fn parses_prompt_section_bind() {
        let raw = r#"
binds = [
    { point = "PromptSection", section = "weather", priority = 50 },
    { point = "PromptSection", section = "status-board" },
]
command = "/bin/echo"
"#;
        let m: HookManifest = toml::from_str(raw).unwrap();
        assert_eq!(m.binds.len(), 2);
        assert_eq!(m.binds[0].point, "PromptSection");
        assert_eq!(m.binds[0].section.as_deref(), Some("weather"));
        assert_eq!(m.binds[0].priority, Some(50));
        assert_eq!(m.binds[1].section.as_deref(), Some("status-board"));
        assert_eq!(m.binds[1].priority, None);
        assert_eq!(m.binds[0].tool_name, None);
    }

    #[test]
    fn bind_to_point_prompt_section_default_and_custom_priority() {
        let default = bind_to_point(
            &BindSpec {
                point: "PromptSection".into(),
                tool_name: None,
                section: Some("weather".into()),
                priority: None,
            },
            "h",
        )
        .unwrap();
        assert_eq!(default.name(), "prompt.system_section.weather");
        assert_eq!(default.priority(), Some(100));

        let custom = bind_to_point(
            &BindSpec {
                point: "PromptSection".into(),
                tool_name: None,
                section: Some("weather".into()),
                priority: Some(50),
            },
            "h",
        )
        .unwrap();
        assert_eq!(custom.priority(), Some(50));
    }

    #[test]
    fn bind_to_point_prompt_section_requires_section() {
        let bind = BindSpec {
            point: "PromptSection".into(),
            tool_name: None,
            section: None,
            priority: None,
        };
        let err = bind_to_point(&bind, "h").unwrap_err();
        assert!(err.to_string().contains("requires `section`"));
    }

    #[test]
    fn bind_to_point_prompt_section_rejects_blank_and_control_names() {
        for bad in ["", "   ", "with\nnewline", "with\ttab"] {
            let bind = BindSpec {
                point: "PromptSection".into(),
                tool_name: None,
                section: Some(bad.into()),
                priority: None,
            };
            let err = bind_to_point(&bind, "h").unwrap_err();
            assert!(
                err.to_string().contains("PromptSection bind `section`"),
                "section {bad:?} should be rejected, got: {err}"
            );
        }
    }

    /// A `PromptSection` bind MAY reuse a built-in section name to
    /// augment the built-in catalog — `bind_to_point` does not reject
    /// `identity` / `agents` / `skills`.
    #[test]
    fn bind_to_point_prompt_section_allows_builtin_names() {
        let bind = BindSpec {
            point: "PromptSection".into(),
            tool_name: None,
            section: Some("agents".into()),
            priority: None,
        };
        let point = bind_to_point(&bind, "h").unwrap();
        assert_eq!(point.name(), "prompt.system_section.agents");
    }

    #[test]
    fn empty_binds_list_is_a_parse_error_not_a_panic() {
        let raw = r#"
binds = []
command = "/bin/true"
"#;
        // Parsing succeeds — semantic validation happens later in
        // `register_one_hook`. The unit-test surfaces the parse path
        // here so the empty-binds contract is explicit.
        let m: HookManifest = toml::from_str(raw).unwrap();
        assert!(m.binds.is_empty());
    }

    /// End-to-end: write a workspace `hooks/<id>/hook.toml` whose
    /// command prints a marker, run the scanner, fire a
    /// `PreToolUse("Bash")` hook, and assert the captured stdout makes
    // it into the `HookResult`. Exercises parse → register → fire.
    #[tokio::test]
    async fn end_to_end_register_and_fire() {
        use crate::extensions::framework::types::{HookInput, HookOutput, HookResult};
        use peko_subject::PrincipalId;

        let tmp = tempfile::tempdir().unwrap();

        // Write a portable echo script into the temp dir so the test
        // works on both Unix (where `/bin/echo` is fine) and Windows
        // (where `/bin/echo` does not exist). The hook.toml points
        // `command` at this script; the scanner's per-hook CWD is the
        // script's directory, so a relative path resolves.
        let (script_name, script_body) = if cfg!(windows) {
            ("echo-hook.bat", "@echo off\r\necho hook-fired\r\n")
        } else {
            ("echo-hook.sh", "#!/bin/sh\necho hook-fired\n")
        };
        let script_path = tmp.path().join(script_name);
        std::fs::write(&script_path, script_body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script_path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script_path, perm).unwrap();
        }

        let hook_dir = tmp.path().join("hooks").join("notify-bash");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let manifest = format!(
            r#"
binds = [{{ point = "PreToolUse", tool_name = "Bash" }}]
command = "{}"
args = []
output = "text"
"#,
            script_path.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(hook_dir.join("hook.toml"), manifest).unwrap();

        let core = ExtensionCore::new();
        let pid = PrincipalId::generate();
        let loaded = load_workspace_hooks(&tmp.path().join("hooks"), &core, &pid)
            .await
            .unwrap();
        assert_eq!(loaded, 1, "exactly one binding registered");

        // Build a minimal HookContext — the handler doesn't read
        // anything besides the dispatch path. We pass an empty
        // HookInput::Unit since the test script only echoes.
        let handler_id = core.hook_count().await;
        assert!(handler_id >= 1);

        // Fire PreToolUse("Bash") and observe the captured stdout.
        let input = HookInput::Unit;
        let result = core
            .invoke_hook(
                crate::extensions::framework::core::hook_points::HookPointBuilder::pre_tool_use(
                    "Bash",
                ),
                input,
            )
            .await;
        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(
                    text.contains("hook-fired"),
                    "expected hook command stdout in output, got: {text:?}"
                );
            }
            other => panic!("expected HookResult::Continue(Text), got {other:?}"),
        }
    }

    /// ADR-052 D6 end-to-end: write a workspace `hooks/<id>/hook.toml`
    /// with a `PromptSection` bind, run the scanner, then fire the
    /// section through the same `ToolFunnel` port the renderer uses
    /// (`invoke_prompt_section_hook`) and assert the command's stdout
    /// comes back. Also pins the `registered_prompt_sections` scoping:
    /// the owning principal sees the section, another principal doesn't.
    #[tokio::test]
    async fn end_to_end_prompt_section_bind_fires_via_funnel_port() {
        use peko_extension_api::ToolFunnel;
        use peko_subject::PrincipalId;

        let tmp = tempfile::tempdir().unwrap();

        let (script_name, script_body) = if cfg!(windows) {
            ("echo-section.bat", "@echo off\r\necho clear-skies\r\n")
        } else {
            ("echo-section.sh", "#!/bin/sh\necho clear-skies\n")
        };
        let script_path = tmp.path().join(script_name);
        std::fs::write(&script_path, script_body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&script_path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&script_path, perm).unwrap();
        }

        let hook_dir = tmp.path().join("hooks").join("weather");
        std::fs::create_dir_all(&hook_dir).unwrap();
        let manifest = format!(
            r#"
binds = [{{ point = "PromptSection", section = "weather", priority = 50 }}]
command = "{}"
args = []
output = "text"
"#,
            script_path.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(hook_dir.join("hook.toml"), manifest).unwrap();

        let core = ExtensionCore::new();
        let pid = PrincipalId::generate();
        let loaded = load_workspace_hooks(&tmp.path().join("hooks"), &core, &pid)
            .await
            .unwrap();
        assert_eq!(loaded, 1, "exactly one binding registered");

        // The funnel port enumerates the section for the owning
        // principal only.
        let pid_str = pid.to_string();
        let mine = core
            .registered_prompt_sections(Some(pid_str.as_str()), None)
            .await;
        assert_eq!(mine, vec!["weather".to_string()], "got: {mine:?}");
        let other = PrincipalId::generate().to_string();
        let theirs = core
            .registered_prompt_sections(Some(other.as_str()), None)
            .await;
        assert!(
            !theirs.contains(&"weather".to_string()),
            "other principal must not see the hook, got: {theirs:?}"
        );

        // Fire the section through the same port the renderer uses.
        let text = core
            .invoke_prompt_section_hook("weather", 100, Some(pid_str.as_str()), None, None, None)
            .await;
        match text {
            Some(text) => assert!(
                text.contains("clear-skies"),
                "expected hook command stdout in section text, got: {text:?}"
            ),
            None => panic!("expected Some(section text), got None"),
        }
    }

    /// Scanner is additive over existing handlers: a malformed
    /// manifest must not block a well-formed one in the same directory.
    #[tokio::test]
    async fn malformed_manifest_does_not_block_well_formed_neighbor() {
        use peko_subject::PrincipalId;

        let tmp = tempfile::tempdir().unwrap();

        // Bad: missing `command`.
        let bad = tmp.path().join("hooks").join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(
            bad.join("hook.toml"),
            r#"
binds = [{ point = "Stop" }]
"#,
        )
        .unwrap();

        // Good: well-formed, but the command path won't resolve, so
        // the scanner logs and returns 0 — still counts as a clean
        // load (the handler's runtime failure is separate).
        let good = tmp.path().join("hooks").join("good");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(
            good.join("hook.toml"),
            r#"
binds = [{ point = "Stop" }]
command = "/nonexistent/binary"
"#,
        )
        .unwrap();

        let core = ExtensionCore::new();
        let pid = PrincipalId::generate();
        let loaded = load_workspace_hooks(&tmp.path().join("hooks"), &core, &pid)
            .await
            .unwrap();
        // The bad manifest fails parsing (no `command`) and is
        // skipped; the good one registers (the runtime is the
        // separate failure surface).
        assert_eq!(
            loaded, 1,
            "good hook should still register despite bad neighbor"
        );
    }

    /// Scanner returns 0 when the hooks directory doesn't exist
    /// rather than erroring.
    #[tokio::test]
    async fn missing_hooks_dir_is_zero_not_error() {
        use peko_subject::PrincipalId;

        let tmp = tempfile::tempdir().unwrap();
        let core = ExtensionCore::new();
        let pid = PrincipalId::generate();
        let loaded = load_workspace_hooks(&tmp.path().join("nope"), &core, &pid)
            .await
            .unwrap();
        assert_eq!(loaded, 0);
    }

    /// Hooks directory without any `hook.toml` is a zero-count
    /// (entries are directories; a directory with no manifest is
    /// skipped, not failed).
    #[tokio::test]
    async fn hooks_dir_without_manifests_is_zero() {
        use peko_subject::PrincipalId;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("hooks")).unwrap();
        let core = ExtensionCore::new();
        let pid = PrincipalId::generate();
        let loaded = load_workspace_hooks(&tmp.path().join("hooks"), &core, &pid)
            .await
            .unwrap();
        assert_eq!(loaded, 0);
    }
}
