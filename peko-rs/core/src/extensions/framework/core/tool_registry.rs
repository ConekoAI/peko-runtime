//! Tool Registry
//!
//! This module implements the registry for tools.
//!
//! ## Key shape
//!
//! Entries are keyed by `(String, PrincipalId)`. Built-in and MCP
//! tools are registered once at core init under
//! [`PrincipalId::system`](peko_subject::PrincipalId::system) — the
//! "system" sentinel that is visible to every principal. Per-principal tools
//! (e.g. a principal's `Skill` or `AgentCatalog`) are registered under the
//! principal's own `PrincipalId` and shadow any same-named system entry.
//!
//! Read paths (`get_tool_hook_id`, `list_tool_names`,
//! `tool_count`) use a two-probe fallback:
//! `(name, principal_id)` first, then `(name, PrincipalId::system())`.
//!
//! ADR-066 P2 deleted the capability-gate bookkeeping (`is_tool_enabled`
//! + the owner index) — presence in the registry is executability.
//!
//! Built on [`crate::extensions::framework::registry::SharedRegistry`] to avoid hand-rolling
//! `Arc<RwLock<HashMap<K, V>>>` patterns.

use crate::extensions::framework::registry::SharedRegistry;
use crate::extensions::framework::types::{ExtensionId, HookId};
use anyhow::Result;
use peko_subject::PrincipalId;
use tracing::{debug, instrument, warn};

/// Registry for tools
///
/// Manages tool registrations. The tool index is backed by a
/// [`SharedRegistry`] for thread-safe access.
#[derive(Debug)]
pub struct ToolRegistry {
    /// Tool index: maps `(tool_name, principal_id)` to the `HookId` of the
    /// execution handler. Built-ins are registered under
    /// [`PrincipalId::system`]; per-principal tools under their own id.
    pub(crate) tool_index: SharedRegistry<(String, PrincipalId), HookId>,
}

impl ToolRegistry {
    /// Create a new Tool Registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_index: SharedRegistry::new(),
        }
    }

    /// Register a tool in the index
    ///
    /// The tool is keyed by `(tool_name, principal_id)`. Pass
    /// [`PrincipalId::system`](peko_subject::PrincipalId::system) as
    /// `principal_id` to register a globally-visible tool (built-ins,
    /// MCP). Per-principal tools override same-named system
    /// entries on read; the system entry remains in place for other
    /// principals.
    ///
    /// # Arguments
    /// * `tool_name` - The name of the tool
    /// * `hook_id` - The hook ID associated with this tool
    /// * `extension_id` - ID of the extension that owns this tool
    /// * `principal_id` - The principal scope (use `PrincipalId::system()`
    ///   for global tools)
    #[instrument(skip(self), fields(tool_name = %tool_name, hook_id = %hook_id, extension_id = %extension_id, principal_id = %principal_id))]
    pub async fn register_tool(
        &self,
        tool_name: &str,
        hook_id: HookId,
        extension_id: ExtensionId,
        principal_id: &PrincipalId,
    ) -> Result<()> {
        let key = (tool_name.to_string(), principal_id.clone());
        self.tool_index.insert(key.clone(), hook_id).await;
        debug!(tool_name = %tool_name, hook_id = %hook_id, principal_id = %principal_id, "Registered tool in index");
        Ok(())
    }

    /// Unregister a tool by `(name, principal_id)`.
    ///
    /// Only the principal-specific entry is removed; system entries
    /// (those registered under `PrincipalId::system()`) remain in place
    /// for other principals. To unregister a system entry, pass
    /// `PrincipalId::system()` as the `principal_id`.
    #[instrument(skip(self), fields(tool_name = %tool_name, principal_id = %principal_id))]
    pub async fn unregister_tool(
        &self,
        tool_name: &str,
        principal_id: &PrincipalId,
    ) -> Result<Option<HookId>> {
        let key = (tool_name.to_string(), principal_id.clone());
        let hook_id = self.tool_index.remove(&key).await;
        if hook_id.is_some() {
            debug!(tool_name = %tool_name, principal_id = %principal_id, "Unregistered tool from index");
        } else {
            warn!(tool_name = %tool_name, principal_id = %principal_id, "Attempted to unregister unknown tool");
        }
        Ok(hook_id)
    }

    /// Get the hook ID for a tool by name from `principal_id`'s perspective.
    ///
    /// Falls back to `(name, PrincipalId::system())` when no
    /// principal-specific entry exists.
    pub async fn get_tool_hook_id(
        &self,
        tool_name: &str,
        principal_id: &PrincipalId,
    ) -> Option<HookId> {
        let per_principal = self
            .tool_index
            .get(&(tool_name.to_string(), principal_id.clone()))
            .await;
        if per_principal.is_some() {
            return per_principal;
        }
        if std::ptr::eq(
            std::ptr::from_ref(principal_id),
            std::ptr::from_ref(PrincipalId::system()),
        ) {
            return None;
        }
        self.tool_index
            .get(&(tool_name.to_string(), PrincipalId::system().clone()))
            .await
    }

    /// Number of tools visible to `principal_id`.
    ///
    /// Counts unique tool names by unioning `(name, principal_id)` with
    /// `(name, PrincipalId::system())`. A principal-specific entry that
    /// shadows a same-named system entry is counted once.
    pub async fn tool_count(&self, principal_id: &PrincipalId) -> usize {
        self.visible_names(principal_id).await.len()
    }

    /// List all tool names visible to `principal_id`.
    ///
    /// Union of `(name, PrincipalId::system())` and `(name, principal_id)`,
    /// with the latter taking precedence on name collision.
    ///
    /// The result is sorted by name. The union is collected from a
    /// per-call `HashSet`, whose iteration order is randomized on every
    /// call (`RandomState`); without a canonical sort the wire tool
    /// catalog shuffles between agentic-loop iterations and breaks
    /// provider prompt-cache prefix matching at the `tools[]` array.
    pub async fn list_tool_names(&self, principal_id: &PrincipalId) -> Vec<String> {
        let mut names: Vec<String> = self.visible_names(principal_id).await.into_iter().collect();
        names.sort_unstable();
        names
    }

    /// Internal helper: the set of tool names `principal_id` can see.
    /// Acquires the read lock once and dedupes by tool name.
    async fn visible_names(&self, principal_id: &PrincipalId) -> std::collections::HashSet<String> {
        self.tool_index
            .read(|map| {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for key in map.keys() {
                    if &key.1 == PrincipalId::system() {
                        seen.insert(key.0.clone());
                    }
                }
                for key in map.keys() {
                    if &key.1 == principal_id {
                        seen.insert(key.0.clone());
                    }
                }
                seen
            })
            .await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> &'static PrincipalId {
        PrincipalId::system()
    }

    /// Two principals register the same tool name under their own
    /// `PrincipalId`. Each lookup returns the principal-specific hook_id;
    /// a third principal sees neither. This is the multi-principal
    /// collision case the principal-keying fixes.
    #[tokio::test]
    async fn test_register_tool_two_principals_no_collision() {
        let registry = ToolRegistry::new();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();

        let hook_id_1 = HookId::new();
        let hook_id_2 = HookId::new();

        registry
            .register_tool(
                "CustomSkill",
                hook_id_1,
                ExtensionId::new("principal:1:customskill"),
                &p1,
            )
            .await
            .unwrap();
        registry
            .register_tool(
                "CustomSkill",
                hook_id_2,
                ExtensionId::new("principal:2:customskill"),
                &p2,
            )
            .await
            .unwrap();

        assert_eq!(
            registry.get_tool_hook_id("CustomSkill", &p1).await,
            Some(hook_id_1)
        );
        assert_eq!(
            registry.get_tool_hook_id("CustomSkill", &p2).await,
            Some(hook_id_2)
        );

        // A third principal sees neither — no system fallback for
        // per-principal tools.
        let p3 = PrincipalId::generate();
        assert_eq!(registry.get_tool_hook_id("CustomSkill", &p3).await, None);
    }

    /// A system-registered built-in is visible to any principal that has
    /// no per-principal override. The lookup helper falls back to the
    /// `(name, PrincipalId::system())` row.
    #[tokio::test]
    async fn test_principal_query_falls_back_to_system_when_no_override() {
        let registry = ToolRegistry::new();
        let p1 = PrincipalId::generate();

        registry
            .register_tool(
                "Bash",
                HookId::new(),
                ExtensionId::new("builtin:tool:Bash"),
                system(),
            )
            .await
            .unwrap();

        assert!(
            registry.get_tool_hook_id("Bash", &p1).await.is_some(),
            "principal without an override should resolve the system hook"
        );
    }
}
