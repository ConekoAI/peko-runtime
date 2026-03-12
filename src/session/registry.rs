//! Session Registry - Peer to Session mapping with switching/branching support
//!
//! This module provides the infrastructure for:
//! - Mapping peer keys to multiple session files (UUID-based naming)
//! - Session switching (change active session for a peer)
//! - Session branching (fork current session)
//! - Session creation (/new)
//!
//! Architecture:
//! ```
//! Peer Key (agent:myagent:peer:user:alice)
//!     ↓
//! SessionRegistryEntry {
//!     active_session_id: "uuid-1",
//!     sessions: {
//!         "uuid-1": SessionInfo { file: "uuid-1.jsonl", created_at: ..., parent: None },
//!         "uuid-2": SessionInfo { file: "uuid-2.jsonl", created_at: ..., parent: "uuid-1" },
//!     }
//! }
//! ```

use crate::session::jsonl::SessionStorage;
use crate::session::lock::FileLock;
use crate::session::Peer;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Information about a single session file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Unique session ID (UUID, matches filename)
    pub session_id: String,
    /// Path to transcript file (relative to sessions dir)
    pub transcript_file: String,
    /// Creation timestamp
    pub created_at: u64,
    /// Last update timestamp
    pub updated_at: u64,
    /// Parent session ID (for branched sessions)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Session label (optional user-defined name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Message count (cached)
    pub message_count: usize,
    /// Whether this session is archived
    #[serde(default)]
    pub archived: bool,
}

impl SessionInfo {
    /// Create new session info
    pub fn new(session_id: String, transcript_file: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            session_id,
            transcript_file,
            created_at: now,
            updated_at: now,
            parent_id: None,
            label: None,
            message_count: 0,
            archived: false,
        }
    }

    /// Create a branched session
    pub fn branched(session_id: String, transcript_file: String, parent_id: String) -> Self {
        let mut info = Self::new(session_id, transcript_file);
        info.parent_id = Some(parent_id);
        info
    }

    /// Update timestamp
    pub fn touch(&mut self) {
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }
}

/// Registry entry for a peer - tracks all sessions for that peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRegistryEntry {
    /// Currently active session ID
    pub active_session_id: String,
    /// All sessions for this peer (session_id -> info)
    pub sessions: HashMap<String, SessionInfo>,
}

impl PeerRegistryEntry {
    /// Create new entry with initial session
    pub fn new(active_session_id: String, session_info: SessionInfo) -> Self {
        let mut sessions = HashMap::new();
        sessions.insert(active_session_id.clone(), session_info);

        Self {
            active_session_id,
            sessions,
        }
    }

    /// Add a new session and make it active
    pub fn add_session(&mut self, session_info: SessionInfo) {
        self.active_session_id = session_info.session_id.clone();
        self.sessions
            .insert(session_info.session_id.clone(), session_info);
    }

    /// Switch to a different session
    pub fn switch_to(&mut self, session_id: &str) -> Result<()> {
        if !self.sessions.contains_key(session_id) {
            return Err(anyhow::anyhow!("Session {} not found", session_id));
        }
        self.active_session_id = session_id.to_string();
        Ok(())
    }

    /// Get active session info
    pub fn active(&self) -> Option<&SessionInfo> {
        self.sessions.get(&self.active_session_id)
    }

    /// Get active session info (mutable)
    pub fn active_mut(&mut self) -> Option<&mut SessionInfo> {
        self.sessions.get_mut(&self.active_session_id)
    }

    /// Get session by ID
    pub fn get(&self, session_id: &str) -> Option<&SessionInfo> {
        self.sessions.get(session_id)
    }

    /// List all non-archived sessions
    pub fn list_active(&self) -> Vec<&SessionInfo> {
        self.sessions.values().filter(|s| !s.archived).collect()
    }

    /// Archive a session (soft delete)
    pub fn archive(&mut self, session_id: &str) -> Result<()> {
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.archived = true;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Session {} not found", session_id))
        }
    }
}

/// Session registry - maps peer keys to their sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRegistry {
    /// Map of peer key to registry entry
    pub peers: HashMap<String, PeerRegistryEntry>,
    /// Registry format version
    pub version: u32,
}

impl SessionRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
            version: 1,
        }
    }

    /// Get or create entry for a peer
    pub fn entry(&mut self, peer_key: &str) -> &mut PeerRegistryEntry {
        self.peers.entry(peer_key.to_string()).or_insert_with(|| {
            // Create initial session
            let session_id = generate_session_id();
            let transcript_file = format!("{}.jsonl", session_id);
            let session_info = SessionInfo::new(session_id.clone(), transcript_file);
            PeerRegistryEntry::new(session_id, session_info)
        })
    }

    /// Get entry for a peer (returns None if not exists)
    pub fn get(&self, peer_key: &str) -> Option<&PeerRegistryEntry> {
        self.peers.get(peer_key)
    }

    /// Get mutable entry for a peer
    pub fn get_mut(&mut self, peer_key: &str) -> Option<&mut PeerRegistryEntry> {
        self.peers.get_mut(peer_key)
    }

    /// Get active session ID for a peer
    pub fn active_session_id(&self, peer_key: &str) -> Option<String> {
        self.peers
            .get(peer_key)
            .map(|e| e.active_session_id.clone())
    }

    /// Get active session transcript file for a peer
    pub fn active_transcript_file(&self, peer_key: &str) -> Option<String> {
        self.peers
            .get(peer_key)
            .and_then(|e| e.active())
            .map(|s| s.transcript_file.clone())
    }

    /// Switch active session for a peer
    pub fn switch_session(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        if let Some(entry) = self.peers.get_mut(peer_key) {
            entry.switch_to(session_id)?;
            info!("Switched {} to session {}", peer_key, session_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Peer {} not found in registry", peer_key))
        }
    }

    /// Create new session for a peer (/new)
    pub fn create_new_session(&mut self, peer_key: &str) -> String {
        let session_id = generate_session_id();
        let transcript_file = format!("{}.jsonl", session_id);
        let session_info = SessionInfo::new(session_id.clone(), transcript_file);

        if let Some(entry) = self.peers.get_mut(peer_key) {
            entry.add_session(session_info);
            info!("Created new session {} for {}", session_id, peer_key);
        } else {
            // Create new entry with this session
            let entry = PeerRegistryEntry::new(session_id.clone(), session_info);
            self.peers.insert(peer_key.to_string(), entry);
            info!("Created peer {} with session {}", peer_key, session_id);
        }

        session_id
    }

    /// Branch current session (/branch)
    pub fn branch_session(&mut self, peer_key: &str, parent_id: &str) -> Result<String> {
        let session_id = generate_session_id();
        let transcript_file = format!("{}.jsonl", session_id);
        let session_info =
            SessionInfo::branched(session_id.clone(), transcript_file, parent_id.to_string());

        if let Some(entry) = self.peers.get_mut(peer_key) {
            entry.add_session(session_info);
            info!(
                "Branched session {} from {} for {}",
                session_id, parent_id, peer_key
            );
            Ok(session_id)
        } else {
            Err(anyhow::anyhow!("Peer {} not found", peer_key))
        }
    }

    /// List all sessions for a peer
    pub fn list_sessions(&self, peer_key: &str) -> Vec<&SessionInfo> {
        self.peers
            .get(peer_key)
            .map(|e| e.list_active())
            .unwrap_or_default()
    }

    /// Get all peers
    pub fn list_peers(&self) -> Vec<&String> {
        self.peers.keys().collect()
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a new session ID (UUID-like)
fn generate_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Session registry manager - handles persistence
#[derive(Debug, Clone)]
pub struct SessionRegistryManager {
    /// Path to registry file
    path: PathBuf,
    /// Sessions directory
    sessions_dir: PathBuf,
}

impl SessionRegistryManager {
    /// Create manager for an agent
    pub async fn for_agent(agent_name: &str) -> Result<Self> {
        let sessions_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("agents")
            .join(agent_name)
            .join("sessions");

        fs::create_dir_all(&sessions_dir).await?;

        let path = sessions_dir.join("registry.json");

        Ok(Self { path, sessions_dir })
    }

    /// Open at specific path
    pub fn open(sessions_dir: impl AsRef<Path>) -> Self {
        let sessions_dir = sessions_dir.as_ref().to_path_buf();
        let path = sessions_dir.join("registry.json");

        Self { path, sessions_dir }
    }

    /// Load registry from disk
    pub async fn load(&self) -> Result<SessionRegistry> {
        if !self.path.exists() {
            return Ok(SessionRegistry::new());
        }

        let content = fs::read_to_string(&self.path)
            .await
            .with_context(|| format!("Failed to read registry: {}", self.path.display()))?;

        if content.trim().is_empty() {
            return Ok(SessionRegistry::new());
        }

        let registry: SessionRegistry = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse registry: {}", self.path.display()))?;

        Ok(registry)
    }

    /// Save registry to disk
    pub async fn save(&self, registry: &SessionRegistry) -> Result<()> {
        // Acquire lock
        let _lock = FileLock::acquire(&self.path, 5000).await?;

        // Serialize
        let json = serde_json::to_string_pretty(registry)?;

        // Write atomically
        let temp_path = self.path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all(json.as_bytes()).await?;
            file.flush().await?;
        }

        fs::rename(&temp_path, &self.path).await?;

        Ok(())
    }

    /// Get active session ID for a peer
    pub async fn get_active_session_id(&self, peer_key: &str) -> Result<Option<String>> {
        let registry = self.load().await?;
        Ok(registry.active_session_id(peer_key))
    }

    /// Switch to a different session
    pub async fn switch_session(&self, peer_key: &str, session_id: &str) -> Result<()> {
        let mut registry = self.load().await?;
        registry.switch_session(peer_key, session_id)?;
        self.save(&registry).await?;
        Ok(())
    }

    /// Create new session (/new)
    ///
    /// Note: This only updates the registry. The actual session file
    /// should be created by BaseSession::create_with_key.
    pub async fn create_new(&self, peer_key: &str) -> Result<String> {
        let mut registry = self.load().await?;
        let session_id = registry.create_new_session(peer_key);
        self.save(&registry).await?;

        info!(
            "Registry: Created new session {} for {}",
            session_id, peer_key
        );
        Ok(session_id)
    }

    /// Branch current session (/branch)
    pub async fn branch(&self, peer_key: &str, label: Option<String>) -> Result<String> {
        let mut registry = self.load().await?;

        // Get current session ID
        let parent_id = registry
            .active_session_id(peer_key)
            .ok_or_else(|| anyhow::anyhow!("No active session to branch from"))?;

        // Create branched session
        let session_id = registry.branch_session(peer_key, &parent_id)?;

        // Add label if provided
        if let Some(label) = label {
            if let Some(entry) = registry.get_mut(peer_key) {
                if let Some(session) = entry.sessions.get_mut(&session_id) {
                    session.label = Some(label);
                }
            }
        }

        self.save(&registry).await?;

        // Copy parent transcript to new file
        let parent_file = registry
            .get(peer_key)
            .and_then(|e| e.get(&parent_id))
            .map(|s| s.transcript_file.clone())
            .ok_or_else(|| anyhow::anyhow!("Parent session not found"))?;

        let new_file = format!("{}.jsonl", session_id);
        let parent_path = self.sessions_dir.join(&parent_file);
        let new_path = self.sessions_dir.join(&new_file);

        fs::copy(&parent_path, &new_path).await?;

        info!(
            "Branched session {} from {} for {}",
            session_id, parent_id, peer_key
        );
        Ok(session_id)
    }

    /// List all sessions for a peer
    pub async fn list_sessions(&self, peer_key: &str) -> Result<Vec<SessionInfo>> {
        let registry = self.load().await?;
        Ok(registry
            .list_sessions(peer_key)
            .into_iter()
            .cloned()
            .collect())
    }

    /// Get sessions directory
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_registry_new() {
        let mut registry = SessionRegistry::new();
        let peer_key = "agent:test:peer:user:alice";

        // Get entry (auto-creates)
        let entry = registry.entry(peer_key);
        assert_eq!(entry.sessions.len(), 1);

        let active_id = entry.active_session_id.clone();
        assert!(!active_id.is_empty());
    }

    #[test]
    fn test_create_new_session() {
        let mut registry = SessionRegistry::new();
        let peer_key = "agent:test:peer:user:alice";

        // Initialize peer with entry()
        let _ = registry.entry(peer_key);

        // Initial session
        let session1 = registry.active_session_id(peer_key).unwrap();

        // Create new session
        let session2 = registry.create_new_session(peer_key);

        assert_ne!(session1, session2);
        assert_eq!(registry.get(peer_key).unwrap().sessions.len(), 2);
        assert_eq!(registry.active_session_id(peer_key).unwrap(), session2);
    }

    #[test]
    fn test_switch_session() {
        let mut registry = SessionRegistry::new();
        let peer_key = "agent:test:peer:user:alice";

        // Initialize peer
        let _ = registry.entry(peer_key);

        let session1 = registry.active_session_id(peer_key).unwrap();
        let session2 = registry.create_new_session(peer_key);

        // Switch back to session1
        registry.switch_session(peer_key, &session1).unwrap();
        assert_eq!(registry.active_session_id(peer_key).unwrap(), session1);

        // Switch to session2
        registry.switch_session(peer_key, &session2).unwrap();
        assert_eq!(registry.active_session_id(peer_key).unwrap(), session2);
    }

    #[test]
    fn test_branch_session() {
        let mut registry = SessionRegistry::new();
        let peer_key = "agent:test:peer:user:alice";

        // Initialize peer
        let _ = registry.entry(peer_key);

        let parent = registry.active_session_id(peer_key).unwrap();
        let child = registry.branch_session(peer_key, &parent).unwrap();

        assert_ne!(parent, child);

        let entry = registry.get(peer_key).unwrap();
        let child_info = entry.get(&child).unwrap();
        assert_eq!(child_info.parent_id, Some(parent));
    }
}
