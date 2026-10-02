//! `ToolCatalog` — the runtime's tool registry (ADR-066 D2).
//!
//! A `name → (Arc<dyn Tool>, ToolMetadata)` map, keyed by
//! `(tool_name, PrincipalId)` with a system-scope fallback: built-ins
//! and MCP proxies register under
//! [`PrincipalId::system`](peko_subject::PrincipalId::system) (visible
//! to every principal); per-agent tools (Agent, ChannelSend, Async*)
//! register under the owning principal's id and shadow same-named
//! system entries on read.
//!
//! Presence = visibility = executability (ADR-066 D1): there is no
//! capability or exposure filter anywhere in the catalog.
//!
//! The catalog is shared state explicitly threaded from the daemon's
//! composition root (`daemon::state`) through `PrincipalManager` →
//! `PrincipalContext` → `Agent` → the engine's `ToolFunnel` seam —
//! there is no process-global accessor. Tests construct their own.

use std::sync::Arc;

use peko_provider_api::ToolDefinition;
use peko_subject::PrincipalId;
use peko_tools_core::Tool;

use crate::extensions::framework::registry::SharedRegistry;
use crate::extensions::framework::types::{ToolMetadata, ToolSource};

/// One registered tool: the executable instance plus its metadata.
#[derive(Clone)]
struct CatalogEntry {
    tool: Arc<dyn Tool>,
    metadata: ToolMetadata,
}

impl std::fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("name", &self.metadata.name)
            .field("source", &self.metadata.source)
            .finish()
    }
}

/// The runtime's tool registry. Cheap to clone (shares the underlying
/// map via `Arc`).
#[derive(Debug, Clone, Default)]
pub struct ToolCatalog {
    tools: SharedRegistry<(String, PrincipalId), CatalogEntry>,
}

impl ToolCatalog {
    /// Create an empty catalog.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under the given principal scope. Idempotent —
    /// re-registering the same `(name, principal_id)` overwrites.
    pub async fn register(
        &self,
        tool: Arc<dyn Tool>,
        source: ToolSource,
        principal_id: &PrincipalId,
    ) {
        let metadata = ToolMetadata::new(
            tool.name().to_string(),
            tool.description(),
            tool.parameters(),
            source,
        );
        self.tools
            .insert(
                (metadata.name.clone(), principal_id.clone()),
                CatalogEntry { tool, metadata },
            )
            .await;
    }

    /// Register a tool under the system scope (visible to every
    /// principal).
    pub async fn register_system(&self, tool: Arc<dyn Tool>, source: ToolSource) {
        self.register(tool, source, PrincipalId::system()).await;
    }

    /// Unregister `(name, principal_id)`. Returns whether an entry was
    /// removed.
    pub async fn unregister(&self, tool_name: &str, principal_id: &PrincipalId) -> bool {
        self.tools
            .remove(&(tool_name.to_string(), principal_id.clone()))
            .await
            .is_some()
    }

    /// Look up a tool from `principal_id`'s perspective: probes
    /// `(name, principal_id)` first, then `(name, PrincipalId::system())`.
    pub async fn get(
        &self,
        tool_name: &str,
        principal_id: &PrincipalId,
    ) -> Option<(Arc<dyn Tool>, ToolMetadata)> {
        let key = (tool_name.to_string(), principal_id.clone());
        if let Some(entry) = self.tools.get(&key).await {
            return Some((entry.tool, entry.metadata));
        }
        if principal_id == PrincipalId::system() {
            return None;
        }
        self.tools
            .get(&(tool_name.to_string(), PrincipalId::system().clone()))
            .await
            .map(|e| (e.tool, e.metadata))
    }

    pub async fn get_tool_metadata(
        &self,
        tool_name: &str,
        principal_id: &PrincipalId,
    ) -> Option<ToolMetadata> {
        self.get(tool_name, principal_id)
            .await
            .map(|(_, metadata)| metadata)
    }

    /// All tool names visible to `principal_id` (union of system scope
    /// and the principal's own entries), sorted by name — the wire
    /// catalog must be byte-stable across agentic-loop iterations or
    /// provider prompt caches break at the `tools[]` array.
    pub async fn list_tool_names(&self, principal_id: &PrincipalId) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .keys()
            .await
            .into_iter()
            .filter(|(_, pid)| pid == PrincipalId::system() || pid == principal_id)
            .map(|(name, _)| name)
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// All tools visible to `principal_id` as metadata.
    pub async fn list_tools(&self, principal_id: &PrincipalId) -> Vec<ToolMetadata> {
        let names = self.list_tool_names(principal_id).await;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            if let Some((_, metadata)) = self.get(&name, principal_id).await {
                out.push(metadata);
            }
        }
        out
    }

    /// The native wire catalog (`tools[]` JSON-schema array) for
    /// `principal_id`: every tool registered in its scope.
    pub async fn tool_definitions(&self, principal_id: &PrincipalId) -> Vec<ToolDefinition> {
        self.list_tools(principal_id)
            .await
            .into_iter()
            .map(|m| m.to_tool_definition())
            .collect()
    }

    /// Number of tools visible to `principal_id`.
    pub async fn tool_count(&self, principal_id: &PrincipalId) -> usize {
        self.list_tool_names(principal_id).await.len()
    }

    /// Whether the named tool is parallelizable (F33 gate probe).
    /// `true` when the tool is unknown — the dispatch will fail anyway,
    /// and admitting without serializing is the right fallback.
    pub async fn is_parallelizable(&self, tool_name: &str, principal_id: &PrincipalId) -> bool {
        match self.get(tool_name, principal_id).await {
            Some((tool, _)) => tool.parallelizable(),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::ToolContext;

    struct StubTool {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> String {
            format!("The {} tool", self.name)
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }

        async fn execute_with_context(
            &self,
            _params: serde_json::Value,
            _context: &ToolContext,
        ) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    fn stub(name: &'static str) -> Arc<dyn Tool> {
        Arc::new(StubTool { name })
    }

    #[tokio::test]
    async fn register_and_get_with_system_fallback() {
        let catalog = ToolCatalog::new();
        let p1 = PrincipalId::generate();
        catalog
            .register_system(stub("Read"), ToolSource::BuiltIn)
            .await;
        catalog
            .register(stub("Agent"), ToolSource::BuiltIn, &p1)
            .await;

        assert!(catalog.get("Read", &p1).await.is_some());
        assert!(catalog.get("Agent", &p1).await.is_some());
        assert!(catalog.get("Agent", PrincipalId::system()).await.is_none());
        assert!(catalog.get("Read", PrincipalId::system()).await.is_some());
    }

    #[tokio::test]
    async fn wire_catalog_contains_every_registered_tool_in_owner_scope() {
        let catalog = ToolCatalog::new();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        catalog
            .register_system(stub("Read"), ToolSource::BuiltIn)
            .await;
        catalog
            .register(stub("OwnerTool"), ToolSource::BuiltIn, &p1)
            .await;
        let own = catalog.tool_definitions(&p1).await;
        assert_eq!(
            own.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            vec!["OwnerTool", "Read"]
        );
        assert_eq!(
            catalog
                .tool_definitions(&p2)
                .await
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Read"]
        );
    }

    #[tokio::test]
    async fn catalog_is_byte_stable_across_calls() {
        let catalog = ToolCatalog::new();
        for name in ["Grep", "Read", "Bash", "Write", "Agent", "Edit", "Glob"] {
            catalog
                .register_system(stub(name), ToolSource::BuiltIn)
                .await;
        }
        let first = catalog.tool_definitions(PrincipalId::system()).await;
        for _ in 0..16 {
            let again = catalog.tool_definitions(PrincipalId::system()).await;
            assert_eq!(
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&again).unwrap(),
                "catalog must be byte-stable across calls"
            );
        }
    }
}
