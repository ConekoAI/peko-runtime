//! Workspace-scanning prompt handler for the `identity` prompt section
//! (ADR-052 D4 — T0 principal identity). The section rides the tail
//! `<runtime-context>` message, not the frozen system prompt.
//!
//! Registered **once** per core (see `principal/context.rs`), next to
//! the `agents` / `skills` catalog handlers. At invoke time the handler
//! resolves the workspace from the hook context's `ToolRuntimeContext`,
//! reads `<workspace>/principal.toml`, and renders the `[identity]`
//! (display name, description) and `[intent]` (goals, values,
//! preferences) sections as a compact catalog. This config is otherwise
//! registry display metadata only — the handler is what makes it reach
//! every agent in the tree.
//!
//! Presence = visibility (ADR-050): a missing `principal.toml`, or one
//! whose identity/intent fields are all empty, yields
//! [`HookResult::PassThrough`] so the section is stripped from the
//! prompt. There is deliberately **no** capability filter.
//!
//! The render result is cached in a `Mutex` keyed on the FILE's
//! `(path, mtime, len)` — not the workspace dir's mtime — so in-place
//! edits to `principal.toml` invalidate the section on the next
//! iteration (ADR-052 D2's file-level freshness fix), and the path in
//! the key keeps co-hosted principals from sharing a cache entry. That keeps the handler
//! well within the renderer's 2-second hook timeout: one `stat` per
//! iteration, one file read per change.

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;

use crate::extensions::framework::core::{HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{HookOutput, HookResult, ToolRuntimeContext};
use crate::principal::config::{PrincipalIdentityConfig, PrincipalIntentConfig};

/// Default priority for the principal-identity prompt section.
pub const IDENTITY_HOOK_PRIORITY: i32 = 100;

/// Workspace-scanning handler for the `identity` prompt section.
///
/// See the module doc for the scanning/caching contract.
#[derive(Debug, Default)]
pub struct WorkspaceIdentityPromptHandler {
    cache: Mutex<Option<((String, SystemTime, u64), String)>>,
}

impl WorkspaceIdentityPromptHandler {
    /// Create a new workspace-scanning identity handler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the identity section for `workspace`, using the
    /// `(path, file_mtime, file_len)`-keyed cache. Returns `None` when
    /// there is nothing to render (no `principal.toml`, or all
    /// identity/intent fields empty).
    fn render_section(&self, workspace: &str) -> Option<String> {
        let toml_path = Path::new(workspace).join("principal.toml");
        let metadata = std::fs::metadata(&toml_path).ok()?;
        // The path rides the key: this handler is registered once on
        // the daemon-global core but serves every principal, and two
        // principals' `principal.toml` files may share `(mtime, len)`.
        let key = (
            toml_path.to_string_lossy().to_string(),
            metadata.modified().ok()?,
            metadata.len(),
        );

        {
            let cache = self.cache.lock().expect("identity section cache poisoned");
            if let Some((cached_key, text)) = &*cache {
                if *cached_key == key {
                    return (!text.is_empty()).then(|| text.clone());
                }
            }
        }

        let text = std::fs::read_to_string(&toml_path)
            .ok()
            .and_then(|content| render_identity_section(&content))
            .unwrap_or_default();

        let mut cache = self.cache.lock().expect("identity section cache poisoned");
        *cache = Some((key, text.clone()));

        (!text.is_empty()).then_some(text)
    }
}

#[async_trait]
impl HookHandler for WorkspaceIdentityPromptHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let workspace = ctx
            .get_state::<ToolRuntimeContext>("tool_context")
            .and_then(|rtc| rtc.workspace.clone());

        let Some(workspace) = workspace.filter(|w| !w.is_empty()) else {
            return HookResult::PassThrough;
        };

        match self.render_section(&workspace) {
            Some(text) => HookResult::Continue(HookOutput::Text(text)),
            None => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "identity".to_string(),
            priority: IDENTITY_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        IDENTITY_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        "WorkspaceIdentityPromptHandler".to_string()
    }
}

/// The `[identity]` + `[intent]` subset of `principal.toml`. Parsed
/// with the canonical config types so the on-disk shape stays defined
/// in exactly one place (`principal/config.rs`); the rest of the file
/// is ignored, so a partial or unrelated `principal.toml` still parses.
#[derive(Debug, Deserialize)]
struct IdentityIntentConfig {
    #[serde(default)]
    identity: PrincipalIdentityConfig,
    #[serde(default)]
    intent: PrincipalIntentConfig,
}

/// Render the compact identity/intent section body from a
/// `principal.toml`'s content. Returns `None` when the file doesn't
/// parse or every rendered field is empty.
fn render_identity_section(toml_content: &str) -> Option<String> {
    let config: IdentityIntentConfig = toml::from_str(toml_content).ok()?;
    let mut lines: Vec<String> = Vec::new();

    if let Some(display_name) = config.identity.display_name.as_deref() {
        let display_name = display_name.trim();
        if !display_name.is_empty() {
            lines.push(format!("name: {display_name}"));
        }
    }
    if let Some(description) = config.identity.description.as_deref() {
        let description = description.trim();
        if !description.is_empty() {
            lines.push(format!("description: {description}"));
        }
    }
    let mut push_list = |label: &str, items: &[String]| {
        let items: Vec<&str> = items
            .iter()
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .collect();
        if !items.is_empty() {
            lines.push(format!("{label}:"));
            lines.extend(items.into_iter().map(|item| format!("- {item}")));
        }
    };
    push_list("goals", &config.intent.goals);
    push_list("values", &config.intent.values);
    push_list("preferences", &config.intent.preferences);

    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionServices;
    use crate::extensions::framework::types::HookInput;
    use std::sync::Arc;
    use tempfile::TempDir;

    const PRINCIPAL_TOML: &str = r#"
name = "peko-test"

[identity]
display_name = "Peko Test"
description = "A test principal"

[intent]
goals = ["Ship the prototype", "Keep tests green"]
values = ["Candor"]
preferences = [" terse answers "]
"#;

    fn identity_hook_ctx(workspace: Option<&str>) -> HookContext {
        let mut ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "identity".to_string(),
                priority: IDENTITY_HOOK_PRIORITY,
            },
            HookInput::Unit,
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
    async fn identity_handler_renders_identity_and_intent() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("principal.toml"), PRINCIPAL_TOML).unwrap();

        let handler = WorkspaceIdentityPromptHandler::new();
        let text = handle_text(
            handler
                .handle(identity_hook_ctx(Some(&temp.path().to_string_lossy())))
                .await,
        )
        .expect("expected identity section text");

        assert!(text.contains("name: Peko Test"), "got: {text}");
        assert!(
            text.contains("description: A test principal"),
            "got: {text}"
        );
        assert!(
            text.contains("goals:\n- Ship the prototype\n- Keep tests green"),
            "got: {text}"
        );
        assert!(text.contains("values:\n- Candor"), "got: {text}");
        // Preference entries are trimmed.
        assert!(
            text.contains("preferences:\n- terse answers"),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn identity_handler_passes_through_without_workspace_or_file() {
        let handler = WorkspaceIdentityPromptHandler::new();
        let result = handler.handle(identity_hook_ctx(None)).await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough without workspace, got {result:?}"
        );

        let temp = TempDir::new().unwrap();
        let result = handler
            .handle(identity_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough without principal.toml, got {result:?}"
        );
    }

    #[tokio::test]
    async fn identity_handler_passes_through_on_empty_fields() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("principal.toml"), "name = \"bare\"\n").unwrap();

        let handler = WorkspaceIdentityPromptHandler::new();
        let result = handler
            .handle(identity_hook_ctx(Some(&temp.path().to_string_lossy())))
            .await;
        assert!(
            matches!(result, HookResult::PassThrough),
            "Expected PassThrough for empty identity/intent, got {result:?}"
        );
    }

    #[tokio::test]
    async fn identity_handler_caches_and_invalidates_on_in_place_edit() {
        let temp = TempDir::new().unwrap();
        let toml_path = temp.path().join("principal.toml");
        std::fs::write(&toml_path, PRINCIPAL_TOML).unwrap();

        let handler = WorkspaceIdentityPromptHandler::new();
        let ws = temp.path().to_string_lossy().to_string();

        let first = handle_text(handler.handle(identity_hook_ctx(Some(&ws))).await)
            .expect("expected identity text");
        assert!(first.contains("Peko Test"), "got: {first}");

        // In-place content edit: same filename, no dir entry added or
        // removed, so the workspace dir mtime is untouched — only the
        // FILE's `(mtime, len)` changes. The file-level cache key must
        // catch this (ADR-052 D2); a dir-mtime key would not.
        std::fs::write(
            &toml_path,
            PRINCIPAL_TOML.replace("Peko Test", "Peko Edited Principal"),
        )
        .unwrap();

        let second = handle_text(handler.handle(identity_hook_ctx(Some(&ws))).await)
            .expect("expected updated identity text");
        assert!(second.contains("Peko Edited Principal"), "got: {second}");
        assert!(!second.contains("name: Peko Test\n"), "got: {second}");
    }
}
