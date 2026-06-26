use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Principal-owned memory abstraction.
///
/// The Principal owns its memory namespace. Concrete implementations may
/// store sessions as JSONL, memories in SQLite/vectors, files on disk, etc.
#[async_trait]
pub trait PrincipalMemory: Send + Sync {
    /// Record or update a session artifact in the principal's memory index.
    async fn record_session(&self,
        artifact: SessionArtifact,
    ) -> Result<(), MemoryError>;

    /// Find the most recent session artifact for a peer.
    async fn find_latest_session_for_peer(
        &self,
        peer: &crate::auth::Subject,
    ) -> Result<Option<SessionArtifact>, MemoryError>;

    /// List all sessions, most recent first.
    async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, MemoryError>;

    /// Store a generic artifact in the principal's memory.
    async fn store(&self, artifact: Artifact) -> Result<(), MemoryError>;

    /// Recall relevant artifacts.
    async fn recall(&self, query: &str, k: usize) -> Result<Vec<Artifact>, MemoryError>;

    /// Compact / consolidate memory.
    async fn compact(&self) -> Result<CompactSummary, MemoryError>;

    /// Get the path to the principal's session directory.
    fn sessions_dir(&self) -> PathBuf;

    /// Get the supervisor agent's dedicated session path.
    fn supervisor_session_path(&self) -> PathBuf;
}

#[derive(Debug, Clone)]
pub enum Artifact {
    Session(SessionArtifact),
    Memory(MemoryArtifact),
    Todo(TodoArtifact),
    File(FileArtifact),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArtifact {
    pub session_id: String,
    pub peer: crate::auth::Subject,
    #[serde(default)]
    pub title: Option<String>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub summary: Option<String>,
}

impl SessionArtifact {
    fn peer_key(&self) -> String {
        self.peer.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct MemoryArtifact {
    pub id: String,
    pub content: String,
    pub kind: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct TodoArtifact {
    pub id: String,
    pub title: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct FileArtifact {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct CompactSummary {
    pub sessions_compacted: usize,
    pub memories_archived: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("recall failed: {0}")]
    RecallFailed(String),
}

/// Persistent memory index for a Principal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemoryIndex {
    #[serde(default)]
    sessions: Vec<SessionArtifact>,
}

/// Default filesystem-backed memory implementation.
///
/// Sessions are stored as JSONL under `<workspace>/memory/sessions/`.
/// A `memory_index.json` file tracks session metadata for fast recall.
/// This is intentionally simple for the first slice; vector recall and
/// consolidation are deferred.
pub struct DefaultPrincipalMemory {
    workspace_path: PathBuf,
}

impl DefaultPrincipalMemory {
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }

    fn memory_dir(&self) -> PathBuf {
        self.workspace_path.join("memory")
    }

    fn index_path(&self) -> PathBuf {
        self.memory_dir().join("memory_index.json")
    }

    async fn load_index(&self) -> Result<MemoryIndex, MemoryError> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(MemoryIndex::default());
        }
        let contents = tokio::fs::read_to_string(&path).await?;
        serde_json::from_str(&contents)
            .map_err(|e| MemoryError::Serialization(e.to_string()))
    }

    async fn save_index(&self, index: &MemoryIndex) -> Result<(), MemoryError> {
        let path = self.index_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let contents = serde_json::to_string_pretty(index)
            .map_err(|e| MemoryError::Serialization(e.to_string()))?;
        tokio::fs::write(&path, contents).await?;
        Ok(())
    }
}

#[async_trait]
impl PrincipalMemory for DefaultPrincipalMemory {
    async fn record_session(
        &self,
        artifact: SessionArtifact,
    ) -> Result<(), MemoryError> {
        let mut index = self.load_index().await?;
        // Remove existing record for this session_id, then append updated one.
        index.sessions.retain(|s| s.session_id != artifact.session_id);
        index.sessions.push(artifact);
        // Keep most recent first.
        index
            .sessions
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        self.save_index(&index).await
    }

    async fn find_latest_session_for_peer(
        &self,
        peer: &crate::auth::Subject,
    ) -> Result<Option<SessionArtifact>, MemoryError> {
        let index = self.load_index().await?;
        let peer_key = peer.to_string();
        Ok(index
            .sessions
            .into_iter()
            .filter(|s| s.peer_key() == peer_key)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at)))
    }

    async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, MemoryError> {
        let index = self.load_index().await?;
        Ok(index.sessions)
    }

    async fn store(&self, _artifact: Artifact) -> Result<(), MemoryError> {
        // TODO(ADR-041): persist non-session artifacts.
        Ok(())
    }

    async fn recall(&self, _query: &str, _k: usize) -> Result<Vec<Artifact>, MemoryError> {
        // TODO(ADR-041): implement vector/keyword recall.
        Ok(Vec::new())
    }

    async fn compact(&self) -> Result<CompactSummary, MemoryError> {
        // TODO(ADR-041): implement memory consolidation.
        Ok(CompactSummary::default())
    }

    fn sessions_dir(&self) -> PathBuf {
        self.memory_dir().join("sessions")
    }

    fn supervisor_session_path(&self) -> PathBuf {
        self.sessions_dir().join("supervisor.jsonl")
    }
}
