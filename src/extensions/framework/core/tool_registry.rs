//! Tool Registry
//!
//! This module implements the registry for tools and tool policy.
//!
//! ## Key shape
//!
//! Entries are keyed by `(String, PrincipalId)`. Built-in, universal, and MCP
//! tools are registered once at core init under
//! [`PrincipalId::system`](crate::subject::PrincipalId::system) — the
//! "system" sentinel that is visible to every principal. Per-principal tools
//! (e.g. a principal's `Skill` or `AgentCatalog`) are registered under the
//! principal's own `PrincipalId` and shadow any same-named system entry.
//!
//! Read paths (`is_tool_enabled`, `get_tool_hook_id`, `list_tool_names`,
//! `tool_count`, `resolve_canonical_ids`) use a two-probe fallback:
//! `(name, principal_id)` first, then `(name, PrincipalId::system())`.
//!
//! Built on [`crate::common::registry::SharedRegistry`] to avoid hand-rolling
//! `Arc<RwLock<HashMap<K, V>>>` patterns.

use crate::common::registry::SharedRegistry;
use crate::extensions::framework::types::{
    ActiveExtensionSet, Capabilities, Capability, ExtensionId, HookId,
};
use crate::subject::PrincipalId;
use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{debug, instrument, warn};

/// Registry for tools and tool policy
///
/// This component manages tool registrations and enforces the capability-based
/// enablement policy. The tool index is backed by a [`SharedRegistry`] for
/// thread-safe access.
#[derive(Debug)]
pub struct ToolRegistry {
    /// Tool index: maps `(tool_name, principal_id)` to the `HookId` of the
    /// execution handler. Built-ins and universal tools are registered under
    /// [`PrincipalId::system`]; per-principal tools under their own id.
    pub(crate) tool_index: SharedRegistry<(String, PrincipalId), HookId>,

    /// Maps `(tool_name, principal_id)` to the owning extension ID for
    /// capability checking. Same key shape as `tool_index` — the fallback
    /// applies identically so a built-in (owned by `builtin:tool:Read`)
    /// is treated as global regardless of which principal is querying.
    tool_owners: RwLock<HashMap<(String, PrincipalId), ExtensionId>>,
}

impl ToolRegistry {
    /// Create a new Tool Registry
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_index: SharedRegistry::new(),
            tool_owners: RwLock::new(HashMap::new()),
        }
    }

    /// Look up the owning extension for `tool_name` from `principal_id`'s
    /// perspective: probes `(name, principal_id)` then `(name, system())`
    /// and returns whichever is present. None means the tool is unknown.
    async fn lookup_owner(&self, tool_name: &str, principal_id: &PrincipalId) -> Option<ExtensionId> {
        let owners = self.tool_owners.read().await;
        if let Some(ext_id) = owners.get(&(tool_name.to_string(), principal_id.clone())) {
            return Some(ext_id.clone());
        }
        if !std::ptr::eq(
            std::ptr::from_ref(principal_id),
            std::ptr::from_ref(PrincipalId::system()),
        ) {
            if let Some(ext_id) = owners.get(&(tool_name.to_string(), PrincipalId::system().clone())) {
                return Some(ext_id.clone());
            }
        }
        None
    }

    /// Check if a tool is enabled under the given capability set.
    ///
    /// The capability `tool:{tool_name}` must be granted. Wildcards such as
    /// `tool:*` are expanded by the capability set.
    ///
    /// When `active_extensions` is provided, the tool's owning extension must
    /// be present in the active set. Built-in tools are owned by
    /// `builtin:tool:{tool_name}` pseudo-extensions; extension-provided tools
    /// are owned by their canonical extension ID. A tool with no recorded owner
    /// is treated as a built-in/core tool and is gated only by its capability.
    pub async fn is_tool_enabled(
        &self,
        tool_name: &str,
        capabilities: &Capabilities,
        active_extensions: Option<&ActiveExtensionSet>,
        principal_id: &PrincipalId,
    ) -> bool {
        if !capabilities.is_granted(&Capability::new(format!("tool:{tool_name}"))) {
            return false;
        }
        if let Some(active) = active_extensions {
            if let Some(owner) = self.lookup_owner(tool_name, principal_id).await {
                // Built-in tools are owned by pseudo-extensions such as
                // `builtin:tool:Bash`.  They are always present and are gated
                // solely by the matching capability grant.
                let is_builtin = owner.0.starts_with("builtin:tool:");
                if !is_builtin && !active.is_active(&owner.0) {
                    return false;
                }
            }
        }
        true
    }

    /// Register a tool in the index
    ///
    /// The tool is keyed by `(tool_name, principal_id)`. Pass
    /// [`PrincipalId::system`](crate::subject::PrincipalId::system) as
    /// `principal_id` to register a globally-visible tool (built-ins,
    /// universal, MCP). Per-principal tools override same-named system
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
        self.tool_owners.write().await.insert(key, extension_id);
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
        self.tool_owners.write().await.remove(&key);
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
    pub async fn list_tool_names(&self, principal_id: &PrincipalId) -> Vec<String> {
        self.visible_names(principal_id).await.into_iter().collect()
    }

    /// Internal helper: the set of tool names `principal_id` can see.
    /// Acquires the read lock once and dedupes by tool name.
    async fn visible_names(&self, principal_id: &PrincipalId) -> std::collections::HashSet<String> {
        self.tool_index
            .read(|map| {
                let mut seen: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
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

    /// Resolve a list of bare tool names to their canonical
    /// `extension_id` form (from `principal_id`'s perspective).
    ///
    /// For each input name, if the registry knows the tool's owning
    /// `extension_id` (e.g. `"Read"` → `"builtin:tool:Read"`), the
    /// canonical form is returned. For unknown names — typically
    /// extension-provided skills or MCPs whose canonical ID is
    /// already the bare name — the bare name is returned unchanged.
    pub async fn resolve_canonical_ids(
        &self,
        names: &[String],
        principal_id: &PrincipalId,
    ) -> Vec<String> {
        let owners = self.tool_owners.read().await;
        names
            .iter()
            .map(|name| {
                let per_principal = owners.get(&(name.clone(), principal_id.clone()));
                let ext_id = per_principal.or_else(|| {
                    if std::ptr::eq(
                        std::ptr::from_ref(principal_id),
                        std::ptr::from_ref(PrincipalId::system()),
                    ) {
                        None
                    } else {
                        owners.get(&(name.clone(), PrincipalId::system().clone()))
                    }
                });
                ext_id.map(|id| id.0.clone()).unwrap_or_else(|| name.clone())
            })
            .collect()
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

    #[tokio::test]
    async fn test_unknown_tool_with_per_call_allowlist_uses_capability() {
        let registry = ToolRegistry::new();
        // No registration, so no owner is recorded.
        let caps = Capabilities::with_grants(["tool:custom_skill"]);
        assert!(
            registry
                .is_tool_enabled("custom_skill", &caps, None, system())
                .await
        );
    }

    #[tokio::test]
    async fn test_empty_allowlist_denies_all() {
        let registry = ToolRegistry::new();
        registry
            .register_tool(
                "Read",
                HookId::new(),
                ExtensionId::new("builtin:tool:Read"),
                system(),
            )
            .await
            .unwrap();

        // An empty capability set must fail-closed.
        let caps = Capabilities::new();
        assert!(
            !registry.is_tool_enabled("Read", &caps, None, system()).await,
            "empty capability set should deny every tool"
        );
    }

    #[tokio::test]
    async fn test_wildcard_capability_matches() {
        let registry = ToolRegistry::new();
        registry
            .register_tool(
                "Read",
                HookId::new(),
                ExtensionId::new("builtin:tool:Read"),
                system(),
            )
            .await
            .unwrap();

        let caps = Capabilities::with_grants(["tool:*"]);
        assert!(
            registry
                .is_tool_enabled("Read", &caps, None, system())
                .await
        );
    }

    #[tokio::test]
    async fn test_inactive_owning_extension_denies_tool() {
        let registry = ToolRegistry::new();
        registry
            .register_tool(
                "Read",
                HookId::new(),
                ExtensionId::new("universal:read"),
                system(),
            )
            .await
            .unwrap();

        let caps = Capabilities::with_grants(["tool:Read"]);
        let active = ActiveExtensionSet::empty();
        assert!(
            !registry
                .is_tool_enabled("Read", &caps, Some(&active), system())
                .await,
            "tool whose owning extension is inactive should be denied"
        );

        let active = ActiveExtensionSet::with_ids(["universal:read"]);
        assert!(
            registry
                .is_tool_enabled("Read", &caps, Some(&active), system())
                .await,
            "tool whose owning extension is active should be permitted"
        );
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

        let cap = Capabilities::with_grants(["tool:Bash"]);
        assert!(
            registry.is_tool_enabled("Bash", &cap, None, &p1).await,
            "principal without an override should see the system tool"
        );
        assert!(
            registry.get_tool_hook_id("Bash", &p1).await.is_some(),
            "principal without an override should resolve the system hook"
        );
    }
}
