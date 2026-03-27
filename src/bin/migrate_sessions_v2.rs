//! Standalone utility to migrate session index from v1 to v2 format
//!
//! This is a one-time migration tool that converts the old session index format
//! (registry.json + sidecar files) to the new two-file format (sessions.json + peers.json).
//!
//! Usage:
//!   cargo run --bin migrate_sessions_v2 -- <sessions_dir>
//!
//! Example:
//!   cargo run --bin migrate_sessions_v2 -- ~/.pekobot/agents/myagent/sessions

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tokio::fs;
use tracing::info;

/// Migration report
#[derive(Debug, Default)]
pub struct MigrationReport {
    pub sessions_migrated: usize,
    pub peers_migrated: usize,
    pub duplicates_removed: usize,
    pub sidecars_removed: usize,
}

/// Old registry entry structure
#[derive(Deserialize)]
struct OldRegistry {
    peers: HashMap<String, OldPeerEntry>,
}

#[derive(Deserialize)]
struct OldPeerEntry {
    active_session_id: String,
    sessions: HashMap<String, OldSessionInfo>,
}

#[derive(Deserialize)]
struct OldSessionInfo {
    session_id: String,
    transcript_file: String,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
    parent_id: Option<String>,
}

/// Old index entry structure
#[derive(Deserialize)]
struct OldIndexEntry {
    session_id: String,
    agent_name: String,
    session_key: Option<String>,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
    total_tokens: Option<usize>,
    transcript_file: String,
    title: Option<String>,
    parent_session_id: Option<String>,
    ended: bool,
    trigger: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    channel: Option<String>,
    recipient: Option<String>,
    cwd: Option<String>,
}

/// New session entry structure
#[derive(Serialize, Clone)]
struct SessionEntry {
    session_id: String,
    agent_name: String,
    created_at: u64,
    updated_at: u64,
    message_count: usize,
    turn_count: u32,
    input_tokens: usize,
    output_tokens: usize,
    total_tokens: usize,
    transcript_file: String,
    title: Option<String>,
    parent_session_id: Option<String>,
    ended: bool,
    trigger: String,
    provider: Option<String>,
    model: Option<String>,
    channel: Option<String>,
    recipient: Option<String>,
    cwd: Option<String>,
    peer_type: Option<String>,
    peer_id: Option<String>,
}

/// Peer info structure
#[derive(Serialize, Default)]
struct PeerIndex {
    peers: HashMap<String, PeerInfo>,
}

#[derive(Serialize)]
struct PeerInfo {
    active_session_id: String,
    session_ids: Vec<String>,
}

/// Migrate from old format to new two-file format
async fn migrate_to_v2(sessions_dir: &Path) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();

    // Check if already migrated
    if sessions_dir.join("sessions.json").exists()
        && sessions_dir.join("peers.json").exists()
        && !sessions_dir.join("registry.json").exists()
    {
        info!("Session index already at v2");
        return Ok(report);
    }

    info!("Migrating session index to v2 format...");

    // 1. Load old registry.json if exists
    let mut peer_mappings: HashMap<String, (String, Vec<String>)> = HashMap::new();

    let registry_path = sessions_dir.join("registry.json");
    if registry_path.exists() {
        let content = fs::read_to_string(&registry_path).await?;
        let old_registry: OldRegistry = serde_json::from_str(&content)?;

        for (peer_key, peer_entry) in old_registry.peers {
            let session_ids: Vec<String> = peer_entry.sessions.keys().cloned().collect();
            peer_mappings.insert(peer_key, (peer_entry.active_session_id, session_ids));
        }

        report.peers_migrated = peer_mappings.len();
    }

    // 2. Load old sessions.json (may have duplicates)
    let mut new_sessions: HashMap<String, SessionEntry> = HashMap::new();

    let old_sessions_path = sessions_dir.join("sessions.json");
    if old_sessions_path.exists() {
        let content = fs::read_to_string(&old_sessions_path).await?;
        let old_entries: HashMap<String, OldIndexEntry> = serde_json::from_str(&content)?;

        // Deduplicate by session_id (keep most recent)
        let mut by_session: HashMap<String, Vec<OldIndexEntry>> = HashMap::new();
        for (_, entry) in old_entries {
            by_session
                .entry(entry.session_id.clone())
                .or_default()
                .push(entry);
        }

        for (session_id, mut entries) in by_session {
            if entries.len() > 1 {
                report.duplicates_removed += entries.len() - 1;
            }

            // Keep most recent
            entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
            let best = &entries[0];

            let entry = SessionEntry {
                session_id: session_id.clone(),
                agent_name: best.agent_name.clone(),
                created_at: best.created_at,
                updated_at: best.updated_at,
                message_count: best.message_count,
                turn_count: 0, // Will be calculated from events
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: best.total_tokens.unwrap_or(0),
                transcript_file: best.transcript_file.clone(),
                title: best.title.clone(),
                parent_session_id: best.parent_session_id.clone(),
                ended: best.ended,
                trigger: best.trigger.clone().unwrap_or_else(|| "user".to_string()),
                provider: best.provider.clone(),
                model: best.model.clone(),
                channel: best.channel.clone(),
                recipient: best.recipient.clone(),
                cwd: best.cwd.clone(),
                peer_type: None, // Migration: peer info not available in old format
                peer_id: None,
            };

            new_sessions.insert(session_id, entry);
        }

        report.sessions_migrated = new_sessions.len();
    }

    // 3. Build new peers.json
    let mut new_peers = PeerIndex::default();
    for (peer_key, (active_id, session_ids)) in peer_mappings {
        // Filter to only existing sessions
        let valid_ids: Vec<String> = session_ids
            .into_iter()
            .filter(|id| new_sessions.contains_key(id))
            .collect();

        if !valid_ids.is_empty() {
            let active = if valid_ids.contains(&active_id) {
                active_id
            } else {
                valid_ids[0].clone()
            };

            new_peers.peers.insert(
                peer_key,
                PeerInfo {
                    active_session_id: active,
                    session_ids: valid_ids,
                },
            );
        }
    }

    // 4. Write new files
    if !new_sessions.is_empty() {
        let sessions_json = serde_json::to_string_pretty(&new_sessions)?;
        fs::write(sessions_dir.join("sessions.json"), sessions_json).await?;
    }

    if !new_peers.peers.is_empty() {
        let peers_json = serde_json::to_string_pretty(&new_peers)?;
        fs::write(sessions_dir.join("peers.json"), peers_json).await?;
    }

    // 5. Delete old files
    if registry_path.exists() {
        fs::remove_file(&registry_path).await?;
    }

    // Remove old sidecar files
    if sessions_dir.exists() {
        let mut entries = fs::read_dir(sessions_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".index.json") {
                fs::remove_file(entry.path()).await?;
                report.sidecars_removed += 1;
            }
        }
    }

    info!(
        "Migration complete: {} sessions, {} peers, {} duplicates removed, {} sidecars removed",
        report.sessions_migrated,
        report.peers_migrated,
        report.duplicates_removed,
        report.sidecars_removed
    );

    Ok(report)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Get sessions directory from command line
    let sessions_dir = std::env::args()
        .nth(1)
        .expect("Usage: cargo run --bin migrate_sessions_v2 -- <sessions_dir>");

    let path = std::path::Path::new(&sessions_dir);
    if !path.exists() {
        anyhow::bail!("Sessions directory does not exist: {}", sessions_dir);
    }

    if !path.is_dir() {
        anyhow::bail!("Path is not a directory: {}", sessions_dir);
    }

    // Run migration
    let report = migrate_to_v2(path).await?;

    println!("\nMigration Report:");
    println!("  Sessions migrated: {}", report.sessions_migrated);
    println!("  Peers migrated: {}", report.peers_migrated);
    println!("  Duplicates removed: {}", report.duplicates_removed);
    println!("  Sidecars removed: {}", report.sidecars_removed);

    Ok(())
}
