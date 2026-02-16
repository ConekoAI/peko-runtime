//! Memory persistence types

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// Memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique entry ID
    pub id: String,
    /// Agent DID that owns this memory
    pub agent_did: String,
    /// Memory scope
    pub scope: MemoryScope,
    /// Memory type/category
    pub memory_type: String,
    /// Content (JSON)
    pub content: serde_json::Value,
    /// Vector embedding (optional, for semantic search)
    pub embedding: Option<Vec<f32>>,
    /// Created timestamp
    pub created_at: DateTime<Utc>,
    /// Updated timestamp
    pub updated_at: DateTime<Utc>,
    /// Expiration timestamp (optional)
    pub expires_at: Option<DateTime<Utc>>,
    /// Importance score (0.0 - 1.0)
    pub importance: f32,
    /// Associated conversation/thread ID
    pub thread_id: Option<String>,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Source (what created this memory)
    pub source: String,
}

/// Memory scope - who can access this memory
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum MemoryScope {
    /// Only accessible by the specific agent
    #[serde(rename = "agent")]
    Agent,
    /// Accessible by all agents in the same tenant
    #[serde(rename = "tenant")]
    Tenant,
    /// Accessible by all agents in this Pekobot instance
    #[serde(rename = "local")]
    Local,
    /// Accessible across Coneko network
    #[serde(rename = "network")]
    Network,
    /// System-level memory (configuration, etc.)
    #[serde(rename = "system")]
    System,
}

impl std::fmt::Display for MemoryScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryScope::Agent => write!(f, "agent"),
            MemoryScope::Tenant => write!(f, "tenant"),
            MemoryScope::Local => write!(f, "local"),
            MemoryScope::Network => write!(f, "network"),
            MemoryScope::System => write!(f, "system"),
        }
    }
}

/// Memory query
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryQuery {
    /// Agent DID to filter by
    pub agent_did: Option<String>,
    /// Memory scope to filter by
    pub scope: Option<MemoryScope>,
    /// Memory types to include
    pub memory_types: Option<Vec<String>>,
    /// Text search query
    pub text_query: Option<String>,
    /// Semantic search query (will be embedded)
    pub semantic_query: Option<String>,
    /// Tags to filter by (all must match)
    pub tags: Option<Vec<String>>,
    /// Thread ID to filter by
    pub thread_id: Option<String>,
    /// Minimum importance score
    pub min_importance: Option<f32>,
    /// Maximum age (seconds)
    pub max_age_seconds: Option<i64>,
    /// Limit results
    pub limit: Option<usize>,
    /// Offset for pagination
    pub offset: Option<usize>,
    /// Order by: created_at, updated_at, importance
    pub order_by: Option<String>,
    /// Order direction: asc, desc
    pub order_direction: Option<String>,
}

/// Memory configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Enable semantic search (requires embedding model)
    pub enable_semantic_search: bool,
    /// Embedding model to use
    pub embedding_model: Option<String>,
    /// Maximum memory entries per agent
    pub max_entries_per_agent: Option<usize>,
    /// Default TTL for memories (seconds)
    pub default_ttl_seconds: Option<i64>,
    /// Auto-cleanup expired memories
    pub auto_cleanup: bool,
    /// Cleanup interval (seconds)
    pub cleanup_interval_seconds: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enable_semantic_search: false,
            embedding_model: None,
            max_entries_per_agent: Some(10000),
            default_ttl_seconds: None,
            auto_cleanup: true,
            cleanup_interval_seconds: 3600,
        }
    }
}

/// Memory search result with relevance score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    /// The memory entry
    pub entry: MemoryEntry,
    /// Relevance score (0.0 - 1.0)
    pub relevance: f32,
    /// Match type: exact, text, semantic
    pub match_type: String,
}

/// Memory statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Total entries
    pub total_entries: usize,
    /// Entries by scope
    pub entries_by_scope: std::collections::HashMap<String, usize>,
    /// Entries by type
    pub entries_by_type: std::collections::HashMap<String, usize>,
    /// Average importance
    pub avg_importance: f32,
    /// Oldest entry timestamp
    pub oldest_entry: Option<DateTime<Utc>>,
    /// Newest entry timestamp
    pub newest_entry: Option<DateTime<Utc>>,
}

impl MemoryEntry {
    /// Create a new memory entry
    pub fn new(
        agent_did: &str,
        scope: MemoryScope,
        memory_type: &str,
        content: serde_json::Value,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            agent_did: agent_did.to_string(),
            scope,
            memory_type: memory_type.to_string(),
            content,
            embedding: None,
            created_at: now,
            updated_at: now,
            expires_at: None,
            importance: 0.5,
            thread_id: None,
            tags: vec![],
            source: "agent".to_string(),
        }
    }

    /// Set expiration
    pub fn with_expiration(mut self, seconds: i64) -> Self {
        self.expires_at = Some(Utc::now() + chrono::Duration::seconds(seconds));
        self
    }

    /// Set importance
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Set tags
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set thread ID
    pub fn with_thread(mut self, thread_id: &str) -> Self {
        self.thread_id = Some(thread_id.to_string());
        self
    }

    /// Check if entry is expired
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => exp < Utc::now(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_entry_creation() {
        let entry = MemoryEntry::new(
            "did:pekobot:local:test",
            MemoryScope::Agent,
            "conversation",
            serde_json::json!({"key": "value"}),
        );

        assert_eq!(entry.agent_did, "did:pekobot:local:test");
        assert_eq!(entry.scope, MemoryScope::Agent);
        assert_eq!(entry.memory_type, "conversation");
        assert!(!entry.is_expired());
    }

    #[test]
    fn test_memory_entry_builder() {
        let entry = MemoryEntry::new(
            "did:pekobot:local:test",
            MemoryScope::Agent,
            "fact",
            serde_json::json!({"data": 123}),
        )
        .with_importance(0.9)
        .with_tags(vec!["important".to_string()]);

        assert_eq!(entry.importance, 0.9);
        assert_eq!(entry.tags, vec!["important"]);
    }

    #[test]
    fn test_memory_expiration() {
        let entry = MemoryEntry::new(
            "did:pekobot:local:test",
            MemoryScope::Agent,
            "temp",
            serde_json::json!({}),
        )
        .with_expiration(-1); // Already expired

        assert!(entry.is_expired());
    }

    #[test]
    fn test_memory_scope_display() {
        assert_eq!(MemoryScope::Agent.to_string(), "agent");
        assert_eq!(MemoryScope::Tenant.to_string(), "tenant");
        assert_eq!(MemoryScope::Network.to_string(), "network");
    }

    #[test]
    fn test_memory_query_default() {
        let query = MemoryQuery::default();
        assert!(query.agent_did.is_none());
        assert!(query.text_query.is_none());
        assert!(query.limit.is_none());
    }
}
