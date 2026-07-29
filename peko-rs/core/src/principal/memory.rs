use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::Mutex;

/// Principal-owned session memory abstraction.
///
/// The Principal owns a session memory namespace. Concrete
/// implementations may back it with any store (filesystem JSONL today),
/// but the surface is intentionally narrow: record, find, list, and
/// resolve the directory. Persisted artifacts other than sessions
/// (preferences, todos, files) live outside this trait — the LLM
/// writes those to the workspace via `Write` and reads them via
/// `Read`, while session continuity flows through `RootRouter` and
/// `PrincipalManager`.
#[async_trait]
pub trait PrincipalMemory: Send + Sync {
    /// Record or update a session artifact in the principal's memory index.
    async fn record_session(&self, artifact: SessionArtifact) -> Result<(), MemoryError>;

    /// Find the most recent session artifact for a peer.
    async fn find_latest_session_for_peer(
        &self,
        peer: &peko_auth::Subject,
    ) -> Result<Option<SessionArtifact>, MemoryError>;

    /// List all sessions, most recent first.
    async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, MemoryError>;

    /// Get the path to the principal's session directory.
    fn sessions_dir(&self) -> PathBuf;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArtifact {
    pub session_id: String,
    pub peer: peko_auth::Subject,
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

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Persistent memory index for a Principal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MemoryIndex {
    #[serde(default)]
    sessions: Vec<SessionArtifact>,
}

/// Default filesystem-backed memory implementation.
///
/// **Phase A.** The constructor parameter is now the **Local tier root**
/// (i.e. `{data_dir}/principals/{name}/local`) rather than the principal's
/// workspace root. Sessions live at `<local_root>/sessions/` and the
/// memory index at `<local_root>/memory_index.json`. Previously the
/// runtime writer double-joined `memory/` here and the IPC
/// `PathResolver::principal_sessions_dir` read from a different path
/// — session exports were silently empty. The new layout has the runtime
/// writer and the resolver agree on the same `<local_root>/sessions` path.
///
/// This is intentionally simple for the first slice; vector recall and
/// consolidation are deferred.
pub struct DefaultPrincipalMemory {
    /// Local tier root: `{data_dir}/principals/{name}/local`.
    local_root: PathBuf,
    /// Serializes the `load_index → mutate → save_index` sequence in
    /// `record_session`. Without this, concurrent receives on the same
    /// principal race to overwrite each other's index appends — last
    /// writer wins and the index silently drops session records. This is
    /// the production fix for the flake observed in CI on
    /// `concurrent_receives_are_isolated` (1 of 10 sessions lost under
    /// heavy contention; see [[test-concurrent-receives-root-race]]).
    ///
    /// Per-principal scope is correct because `DefaultPrincipalMemory`
    /// is owned by a single Principal — peers landing on different
    /// principals don't share this lock.
    index_lock: Mutex<()>,
}

impl DefaultPrincipalMemory {
    /// Construct a memory store rooted at `local_root`.
    ///
    /// `local_root` MUST be the Local tier directory returned by
    /// `PathResolver::principal_layout(name).local.root`. The runtime
    /// writer and the IPC resolver must agree on this path; passing a
    /// different path re-introduces the pre-Phase A silent-session-loss
    /// bug.
    pub fn new(local_root: PathBuf) -> Self {
        Self {
            local_root,
            index_lock: Mutex::new(()),
        }
    }

    /// Path to `memory_index.json` directly under the Local root.
    ///
    /// Previously `<workspace>/memory/memory_index.json`; now
    /// `<local_root>/memory_index.json`.
    fn index_path(&self) -> PathBuf {
        self.local_root.join("memory_index.json")
    }

    async fn load_index(&self) -> Result<MemoryIndex, MemoryError> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(MemoryIndex::default());
        }
        let contents = tokio::fs::read_to_string(&path).await?;
        serde_json::from_str(&contents).map_err(|e| MemoryError::Serialization(e.to_string()))
    }

    async fn save_index(&self, index: &MemoryIndex) -> Result<(), MemoryError> {
        let path = self.index_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let contents = serde_json::to_string_pretty(index)
            .map_err(|e| MemoryError::Serialization(e.to_string()))?;

        // Write atomically: a plain `tokio::fs::write` truncates the index
        // and writes in place, so a crash mid-write leaves a partially
        // written / corrupt `memory_index.json`. Instead write to a sibling
        // temp file, flush it to disk, then `rename(2)` over the index —
        // rename is atomic on the same filesystem, so a reader either sees
        // the old index or the new one, never a torn write. Writers are
        // serialized per-principal by `index_lock`, so a fixed temp name is
        // safe within the process; a leftover temp from a crashed run is
        // simply overwritten on the next save.
        use tokio::io::AsyncWriteExt;
        let tmp_path = path.with_extension("json.tmp");
        let mut tmp = tokio::fs::File::create(&tmp_path).await?;
        tmp.write_all(contents.as_bytes()).await?;
        tmp.sync_all().await?;
        drop(tmp);
        tokio::fs::rename(&tmp_path, &path).await?;
        Ok(())
    }
}

#[async_trait]
impl PrincipalMemory for DefaultPrincipalMemory {
    async fn record_session(&self, artifact: SessionArtifact) -> Result<(), MemoryError> {
        // Hold the index lock for the full read-modify-write so concurrent
        // recorders don't lose updates (see [[test-concurrent-receives-root-race]]
        // for the symptom this prevents: 9/10 sessions in the index when 10
        // peers race, because each `load_index` reads the pre-append state).
        let _guard = self.index_lock.lock().await;
        let mut index = self.load_index().await?;
        // Remove existing record for this session_id, then append updated one.
        index
            .sessions
            .retain(|s| s.session_id != artifact.session_id);
        index.sessions.push(artifact);
        // Keep most recent first.
        index
            .sessions
            .sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        self.save_index(&index).await
    }

    async fn find_latest_session_for_peer(
        &self,
        peer: &peko_auth::Subject,
    ) -> Result<Option<SessionArtifact>, MemoryError> {
        // Acquire the lock so we don't observe an in-flight rewrite. The
        // alternative — letting the read proceed without coordination —
        // risks a `tokio::fs::read_to_string` of a partially-written file
        // and a `serde_json` parse error surfaced as `MemoryError`.
        let _guard = self.index_lock.lock().await;
        let index = self.load_index().await?;
        let peer_key = peer.to_string();
        Ok(index
            .sessions
            .into_iter()
            .filter(|s| s.peer_key() == peer_key)
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at)))
    }

    async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, MemoryError> {
        let _guard = self.index_lock.lock().await;
        let index = self.load_index().await?;
        Ok(index.sessions)
    }

    fn sessions_dir(&self) -> PathBuf {
        // Phase A: sessions live directly under the Local root.
        self.local_root.join("sessions")
    }
}
