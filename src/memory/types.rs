//! Memory types

/// Memory entry
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub metadata: Option<serde_json::Value>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub access_count: i64,
    pub last_accessed_at: Option<chrono::DateTime<chrono::Utc>>,
}
