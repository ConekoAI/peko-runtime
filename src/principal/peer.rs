//! `Peer` and `PeerRegistry` — per-peer quota attribution (F20).
//!
//! F19 wired per-principal quota metering via `QuotaScope` (task-local)
//! + `MeteredProvider` (auto-charging wrapper). F20 adds a second
//! dimension: per-peer metering. A `peer_id` identifies the channel
//! that triggered an LLM call (pekohub user sub, API key id, "local",
//! or an arbitrary peer-shaped string).
//!
//! ## Why a registry (not just on-demand)
//!
//! `PeerRegistry` is eager: at daemon startup we scan
//! `<config_dir>/peers/` and load every peer's meter. New peers
//! discovered mid-run materialize their directory + state file via
//! [`PeerRegistry::get_or_create`]. Mirrors [`PrincipalManager`](super::manager::PrincipalManager).
//!
//! ## Stacking
//!
//! `PeerRegistry::get_or_create(peer_id)` returns `Arc<Peer>` whose
//! `quota_meter` is one of the meters in the active `QuotaScope`
//! stack at the call site. Per-peer meters are stacked alongside
//! per-principal meters via nested `QuotaScope::with` calls; see
//! [`StackedMeteredProvider`](crate::providers::StackedMeteredProvider)
//! for the wrapper that charges every meter in the stack.
//!
//! ## Storage layout
//!
//! ```text
//! <config_dir>/peers/<peer_id>/peer.toml          # QuotaConfig
//! <config_dir>/peers/<peer_id>/quota_state.json   # QuotaState counters
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::quota::{QuotaConfig, QuotaMeter, QuotaState};

/// Errors that `PeerRegistry` can return. Mirrors the shape of
/// [`PrincipalManagerError`](super::manager::PrincipalManagerError)
/// for the slim subset of operations that can fail.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("invalid peer_id: {0}")]
    InvalidPeerId(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    Config(String),

    #[error("quota meter init failed: {0}")]
    QuotaInit(String),
}

/// A registered peer. Holds a `quota_meter` keyed by `peer_id`.
///
/// `Peer` is intentionally minimal — much leaner than `Principal`.
/// Peers have no workspace, no router, no agent prompts; just an ID
/// and a quota meter. Future F-series work can extend `Peer` with
/// per-peer preferences (display name, allowed principals, etc.)
/// without changing the meter-attribution path.
pub struct Peer {
    /// The peer's stable identifier. Used as the directory name
    /// (`<config_dir>/peers/<peer_id>/`).
    pub peer_id: String,

    /// Quota meter for this peer. Unlimited by default (every
    /// `QuotaConfig` field `None`). Set per-peer via
    /// [`PeerRegistry::set_config`].
    pub quota_meter: Arc<QuotaMeter>,
}

/// On-disk configuration for a peer. Deserialized from `peer.toml`.
///
/// F20 only stores the quota config. Future F-series work can add
/// per-peer preferences (display name, allowed principals) without
/// changing the storage shape — every field has `#[serde(default)]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerConfig {
    /// Quota config for this peer. `None` (or empty) means unlimited.
    #[serde(default)]
    pub quota: Option<QuotaConfig>,
}

impl PeerConfig {
    /// Resolve the effective `QuotaConfig` for this peer. Empty /
    /// missing config → `QuotaConfig::default()` (unlimited).
    #[must_use]
    pub fn effective_quota(&self) -> QuotaConfig {
        self.quota.clone().unwrap_or_default()
    }
}

/// In-memory registry of `Peer`s keyed by `peer_id`. Mirrors
/// [`PrincipalManager`](super::manager::PrincipalManager) for the
/// slim subset of operations peers need.
pub struct PeerRegistry {
    /// `peer_id` → `Arc<Peer>`. The inner `Arc<QuotaMeter>` is what
    /// call sites stack via `QuotaScope::with`.
    peers: RwLock<HashMap<String, Arc<Peer>>>,

    /// `<config_dir>/peers/`. Used by [`Self::get_or_create`] to
    /// materialize new peer directories and by [`Self::load`] to
    /// discover existing peers at startup.
    root_dir: PathBuf,
}

impl PeerRegistry {
    /// Scan `root_dir` and load every peer's meter from disk.
    /// Peers whose `peer.toml` is missing or malformed fall back to
    /// `QuotaConfig::default()` (unlimited). Peers without a state
    /// file start with a fresh `QuotaState`. Missing root is OK — we
    /// create it.
    pub async fn load_or_init(
        root_dir: PathBuf,
        now: DateTime<Utc>,
    ) -> Result<Arc<Self>, PeerError> {
        tokio::fs::create_dir_all(&root_dir).await?;

        let registry = Arc::new(Self {
            peers: RwLock::new(HashMap::new()),
            root_dir: root_dir.clone(),
        });

        let mut entries = tokio::fs::read_dir(&root_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(peer_id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Err(e) = validate_peer_id(peer_id) {
                warn!(
                    peer_id = %peer_id,
                    error = %e,
                    "skipping peer directory with invalid name"
                );
                continue;
            }

            match load_one_peer(&path, peer_id, now).await {
                Ok(peer) => {
                    debug!(peer_id = %peer_id, "loaded peer");
                    registry
                        .peers
                        .write()
                        .await
                        .insert(peer_id.to_string(), peer);
                }
                Err(e) => {
                    warn!(
                        peer_id = %peer_id,
                        error = %e,
                        "failed to load peer; skipping"
                    );
                }
            }
        }

        Ok(registry)
    }

    /// Look up an already-loaded peer. Returns `None` if the peer
    /// has never been seen this session.
    pub async fn get(&self, peer_id: &str) -> Option<Arc<Peer>> {
        self.peers.read().await.get(peer_id).cloned()
    }

    /// Look up an already-loaded peer OR create a new one with an
    /// unlimited `QuotaConfig`. The new peer's directory and state
    /// file are materialized on first use so the daemon can persist
    /// across restarts.
    ///
    /// This is the call site's normal entry point — `Agent` calls
    /// this once at run time to resolve the peer meter for the
    /// active `ToolContext`.
    pub async fn get_or_create(
        self: &Arc<Self>,
        peer_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Arc<Peer>, PeerError> {
        validate_peer_id(peer_id)?;

        if let Some(existing) = self.get(peer_id).await {
            return Ok(existing);
        }

        // Not loaded. Materialize the directory + meter + state file.
        let dir = self.root_dir.join(peer_id);
        tokio::fs::create_dir_all(&dir).await?;

        let quota_state_path = dir.join("quota_state.json");
        let quota_meter = QuotaMeter::load_or_init(
            QuotaConfig::default(), // unlimited until operator sets one
            Some(quota_state_path),
            now,
        )
        .await
        .map_err(|e| PeerError::QuotaInit(e.to_string()))?;
        let quota_meter = Arc::new(quota_meter);

        // Write a default peer.toml so the directory is a valid
        // peer root for the next startup's load_or_init scan.
        let peer_toml = dir.join("peer.toml");
        if !peer_toml.exists() {
            let cfg = PeerConfig::default();
            let toml_str =
                toml::to_string_pretty(&cfg).map_err(|e| PeerError::Config(e.to_string()))?;
            tokio::fs::write(&peer_toml, toml_str).await?;
        }

        let peer = Arc::new(Peer {
            peer_id: peer_id.to_string(),
            quota_meter,
        });

        // Insert under the write lock; if a concurrent caller raced
        // us and already inserted, prefer the existing one.
        let mut guard = self.peers.write().await;
        if let Some(existing) = guard.get(peer_id) {
            return Ok(existing.clone());
        }
        guard.insert(peer_id.to_string(), peer.clone());
        Ok(peer)
    }

    /// List every loaded peer. Used by `peko peer list` (deferred)
    /// and by the future observability dashboards.
    pub async fn list_all(&self) -> Vec<Arc<Peer>> {
        self.peers.read().await.values().cloned().collect()
    }

    /// Replace the quota config for `peer_id`, persisting the new
    /// config to `peer.toml`. Existing meter counters are preserved.
    /// Creates the peer directory + state file if the peer was
    /// previously unknown — same semantics as `get_or_create` for
    /// the disk side, but does not insert an in-memory peer on the
    /// happy path; the operator can then call `get` or
    /// `get_or_create` to materialize it.
    pub async fn set_config(
        self: &Arc<Self>,
        peer_id: &str,
        cfg: QuotaConfig,
    ) -> Result<(), PeerError> {
        validate_peer_id(peer_id)?;

        let dir = self.root_dir.join(peer_id);
        tokio::fs::create_dir_all(&dir).await?;
        let peer_toml = dir.join("peer.toml");

        let peer_cfg = PeerConfig { quota: Some(cfg.clone()) };
        let toml_str =
            toml::to_string_pretty(&peer_cfg).map_err(|e| PeerError::Config(e.to_string()))?;
        tokio::fs::write(&peer_toml, toml_str).await?;

        // Update the live meter too if the peer is loaded.
        if let Some(peer) = self.get(peer_id).await {
            peer.quota_meter.set_config(cfg);
        }
        Ok(())
    }

    /// Force-reset the quota window for `peer_id`. Counters go to
    /// zero, window rolls forward to `now`. Persists the new state.
    pub async fn reset(
        self: &Arc<Self>,
        peer_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), PeerError> {
        validate_peer_id(peer_id)?;

        if let Some(peer) = self.get(peer_id).await {
            peer.quota_meter.reset(now).await;
            Ok(())
        } else {
            // Unknown peer: nothing to reset, but the CLI can still
            // surface a useful error. We choose to surface
            // NotFound-like behavior by returning InvalidPeerId in
            // a future iteration; for now `Err` on missing is
            // implicit via `get_or_create` not being called here.
            Err(PeerError::Config(format!(
                "peer '{peer_id}' is not loaded; cannot reset"
            )))
        }
    }

    /// Snapshot the current `QuotaState` for `peer_id`. Returns
    /// `None` if the peer isn't loaded.
    pub async fn snapshot(&self, peer_id: &str) -> Option<QuotaState> {
        self.get(peer_id).await.map(|p| p.quota_meter.snapshot())
    }
}

/// Validate a `peer_id` for use as a directory name. Rejects path
/// traversal, control chars, empty, and overlong inputs.
fn validate_peer_id(peer_id: &str) -> Result<(), PeerError> {
    if peer_id.is_empty() {
        return Err(PeerError::InvalidPeerId("empty".into()));
    }
    if peer_id.len() > 255 {
        return Err(PeerError::InvalidPeerId(format!(
            "longer than 255 chars (got {})",
            peer_id.len()
        )));
    }
    if peer_id == "." || peer_id == ".." {
        return Err(PeerError::InvalidPeerId(peer_id.into()));
    }
    for ch in peer_id.chars() {
        if ch == '/'
            || ch == '\\'
            || ch == '\0'
            || ch.is_control()
            || ch == ':'  // Windows-reserved
            || ch == '*'
            || ch == '?'
            || ch == '"'
            || ch == '<'
            || ch == '>'
            || ch == '|'
        {
            return Err(PeerError::InvalidPeerId(format!(
                "contains forbidden character: {ch:?}"
            )));
        }
    }
    Ok(())
}

/// Load one peer's meter from `dir` (a directory at
/// `<root>/<peer_id>/`). Reads `peer.toml` for `QuotaConfig` and
/// `quota_state.json` for counters; falls back to defaults on
/// missing files.
async fn load_one_peer(
    dir: &Path,
    peer_id: &str,
    now: DateTime<Utc>,
) -> Result<Arc<Peer>, PeerError> {
    let peer_toml = dir.join("peer.toml");
    let quota_state_path = dir.join("quota_state.json");

    let quota_config = if peer_toml.exists() {
        let toml_str = tokio::fs::read_to_string(&peer_toml).await?;
        let cfg: PeerConfig =
            toml::from_str(&toml_str).map_err(|e| PeerError::Config(e.to_string()))?;
        cfg.effective_quota()
    } else {
        QuotaConfig::default()
    };

    let quota_meter = QuotaMeter::load_or_init(
        quota_config,
        Some(quota_state_path),
        now,
    )
    .await
    .map_err(|e| PeerError::QuotaInit(e.to_string()))?;

    Ok(Arc::new(Peer {
        peer_id: peer_id.to_string(),
        quota_meter: Arc::new(quota_meter),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_peer_id_accepts_normal_names() {
        assert!(validate_peer_id("pekohub-user-bob").is_ok());
        assert!(validate_peer_id("local").is_ok());
        assert!(validate_peer_id("api-key-123").is_ok());
        assert!(validate_peer_id("user@domain.com").is_ok());
    }

    #[test]
    fn validate_peer_id_rejects_traversal() {
        assert!(validate_peer_id("..").is_err());
        assert!(validate_peer_id(".").is_err());
        assert!(validate_peer_id("../etc/passwd").is_err());
        assert!(validate_peer_id("foo/bar").is_err());
        assert!(validate_peer_id("foo\\bar").is_err());
    }

    #[test]
    fn validate_peer_id_rejects_control_chars() {
        assert!(validate_peer_id("foo\0bar").is_err());
        assert!(validate_peer_id("foo\nbar").is_err());
        assert!(validate_peer_id("foo\tbar").is_err());
    }

    #[test]
    fn validate_peer_id_rejects_empty_and_too_long() {
        assert!(validate_peer_id("").is_err());
        let long = "a".repeat(256);
        assert!(validate_peer_id(&long).is_err());
    }

    #[test]
    fn validate_peer_id_rejects_windows_reserved() {
        assert!(validate_peer_id("foo:bar").is_err());
        assert!(validate_peer_id("foo*bar").is_err());
        assert!(validate_peer_id("foo?bar").is_err());
        assert!(validate_peer_id("foo|bar").is_err());
    }

    #[tokio::test]
    async fn load_or_init_creates_empty_dir_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root.clone(), Utc::now())
            .await
            .unwrap();
        assert!(root.exists(), "load_or_init must create the root dir");
        assert!(reg.list_all().await.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_makes_unlimited_meter_on_first_use() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root.clone(), Utc::now())
            .await
            .unwrap();

        let peer = reg
            .get_or_create("pekohub-user-bob", Utc::now())
            .await
            .unwrap();
        assert_eq!(peer.peer_id, "pekohub-user-bob");
        // Default meter is unlimited.
        assert!(!peer.quota_meter.config().has_any_limit());

        // Subsequent get returns the same peer.
        let peer2 = reg.get("pekohub-user-bob").await.unwrap();
        assert!(Arc::ptr_eq(&peer, &peer2));

        // Disk artifacts exist.
        assert!(root.join("pekohub-user-bob/peer.toml").exists());
        // The quota_state.json file is written on first `charge`, not
        // on meter creation — F20 mirrors `PrincipalManager`'s
        // behavior here. The directory exists; the state file will
        // appear after the first metered LLM call.
    }

    #[tokio::test]
    async fn get_or_create_rejects_invalid_peer_id() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root, Utc::now()).await.unwrap();
        assert!(reg.get_or_create("../escape", Utc::now()).await.is_err());
        assert!(reg.get_or_create("", Utc::now()).await.is_err());
        assert!(reg.get_or_create("foo/bar", Utc::now()).await.is_err());
    }

    #[tokio::test]
    async fn set_config_persists_to_peer_toml_and_updates_meter() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root.clone(), Utc::now())
            .await
            .unwrap();

        // Create the peer first so the in-memory meter exists.
        let _ = reg.get_or_create("alice", Utc::now()).await.unwrap();
        // Apply a config.
        reg.set_config(
            "alice",
            QuotaConfig {
                input_tokens: Some(1000),
                output_tokens: None,
                request_count: None,
                cycle: crate::quota::QuotaCycle::Daily,
            },
        )
        .await
        .unwrap();

        // Reload from disk and verify the config survived.
        let reg2 = PeerRegistry::load_or_init(root, Utc::now()).await.unwrap();
        let alice = reg2.get("alice").await.unwrap();
        assert_eq!(alice.quota_meter.config().input_tokens, Some(1000));
    }

    #[tokio::test]
    async fn reset_clears_counters_for_loaded_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root, Utc::now()).await.unwrap();
        let peer = reg.get_or_create("bob", Utc::now()).await.unwrap();
        peer.quota_meter.set_config(QuotaConfig {
            input_tokens: Some(1000),
            output_tokens: None,
            request_count: None,
            cycle: crate::quota::QuotaCycle::Daily,
        });
        peer.quota_meter
            .charge(&crate::common::types::message::TokenUsage {
                input: 50,
                output: 0,
                total: 50,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(peer.quota_meter.snapshot().input_tokens, 50);

        reg.reset("bob", Utc::now()).await.unwrap();
        assert_eq!(peer.quota_meter.snapshot().input_tokens, 0);
    }

    #[tokio::test]
    async fn reset_errors_on_unknown_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        let reg = PeerRegistry::load_or_init(root, Utc::now()).await.unwrap();
        assert!(reg.reset("never-seen", Utc::now()).await.is_err());
    }

    #[tokio::test]
    async fn load_picks_up_peer_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("peers");
        // Pre-create a peer directory with a config.
        let peer_dir = root.join("carol");
        tokio::fs::create_dir_all(&peer_dir).await.unwrap();
        tokio::fs::write(
            peer_dir.join("peer.toml"),
            "[quota]\ninput_tokens = 500\n",
        )
        .await
        .unwrap();

        let reg = PeerRegistry::load_or_init(root, Utc::now()).await.unwrap();
        let carol = reg.get("carol").await.unwrap();
        assert_eq!(carol.quota_meter.config().input_tokens, Some(500));
    }
}