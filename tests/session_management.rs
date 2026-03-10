//! Session Management Integration Tests
//!
//! Run with: cargo test --test session_management

use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

// Import from pekobot crate
use pekobot::session::index::{IndexEntry, MaintenanceConfig, MaintenanceMode, SessionIndex};
use pekobot::session::jsonl::SessionStorage;
use pekobot::session::key::{
    cli_session_key, derive_session_key, discord_session_key, parse_session_key, ChatType,
    SessionContext, SessionScope,
};
use pekobot::session::lock::FileLock;
use pekobot::types::ContentBlock;

/// Test file locking basics
#[tokio::test]
async fn test_file_lock_acquire_release() {
    let temp = TempDir::new().unwrap();
    let session_file = temp.path().join("test.jsonl");

    // Acquire lock
    let lock = FileLock::acquire(&session_file, 1000).await.unwrap();
    assert!(session_file.with_extension("lock").exists());

    // Release lock
    lock.release().await.unwrap();
    assert!(!session_file.with_extension("lock").exists());
}

/// Test file lock timeout with stale lock removal
#[tokio::test]
async fn test_file_lock_stale_removal() {
    let temp = TempDir::new().unwrap();
    let session_file = temp.path().join("test.jsonl");
    let lock_path = session_file.with_extension("lock");

    // Create a stale lock file (non-existent PID)
    let stale_lock = r#"{"pid": 99999, "created_at": "2025-01-01T00:00:00Z"}"#;
    tokio::fs::write(&lock_path, stale_lock).await.unwrap();

    // Should be able to acquire (stale lock removed)
    let lock = FileLock::acquire(&session_file, 1000).await.unwrap();
    lock.release().await.unwrap();
}

/// Test session index creation and basic operations
#[tokio::test]
async fn test_session_index_create_and_load() {
    let temp = TempDir::new().unwrap();
    let index_path = temp.path();

    // Create directory for index
    tokio::fs::create_dir_all(&index_path).await.unwrap();

    let mut index = SessionIndex::open(index_path);

    // Create initial empty index file
    tokio::fs::write(index_path.join("sessions.json"), "{}")
        .await
        .unwrap();

    // Initially empty
    let entries = index.load().await.unwrap();
    assert!(entries.is_empty());

    // Add entry
    let entry = IndexEntry::new(
        "test_123".to_string(),
        "testagent".to_string(),
        "test_123.jsonl".to_string(),
    );
    index
        .insert(
            "agent:testagent:session:test_123".to_string(),
            entry.clone(),
        )
        .await
        .unwrap();

    // Reload and verify
    let mut index2 = SessionIndex::open(index_path);
    let loaded = index2
        .get("agent:testagent:session:test_123")
        .await
        .unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().session_id, "test_123");
}

/// Test session index maintenance (prune)
#[tokio::test]
async fn test_session_index_maintenance_prune() {
    let temp = TempDir::new().unwrap();
    tokio::fs::create_dir_all(temp.path()).await.unwrap();
    // Create initial empty index file
    tokio::fs::write(temp.path().join("sessions.json"), "{}")
        .await
        .unwrap();
    let mut index = SessionIndex::open(temp.path());

    // Add old entry
    let mut old_entry = IndexEntry::new(
        "old_123".to_string(),
        "testagent".to_string(),
        "old_123.jsonl".to_string(),
    );
    old_entry.updated_at = 0; // Very old
    index.insert("old".to_string(), old_entry).await.unwrap();

    // Add new entry
    let new_entry = IndexEntry::new(
        "new_456".to_string(),
        "testagent".to_string(),
        "new_456.jsonl".to_string(),
    );
    index.insert("new".to_string(), new_entry).await.unwrap();

    // Run maintenance with 1 day prune
    let config = MaintenanceConfig {
        mode: MaintenanceMode::Auto,
        prune_after: Duration::from_secs(86400),
        max_sessions: 100,
        rotate_bytes: 10_000_000,
    };

    let report = index.maintenance(&config).await.unwrap();
    println!("Pruned: {}, Capped: {}", report.pruned, report.capped);
    println!(
        "Entries before reload: {}",
        index.load().await.unwrap().len()
    );
    assert!(
        report.pruned >= 1,
        "Expected at least 1 pruned, got {}",
        report.pruned
    );

    // Verify old entry is gone
    let entries = index.load().await.unwrap();
    println!("Entries after reload: {}", entries.len());
    assert!(
        entries.len() <= 1,
        "Expected at most 1 entry, got {}",
        entries.len()
    );
}

/// Test session index maintenance (cap)
#[tokio::test]
async fn test_session_index_maintenance_cap() {
    let temp = TempDir::new().unwrap();
    tokio::fs::create_dir_all(temp.path()).await.unwrap();
    // Create initial empty index file
    tokio::fs::write(temp.path().join("sessions.json"), "{}")
        .await
        .unwrap();
    let mut index = SessionIndex::open(temp.path());

    // Add 5 entries
    for i in 0..5 {
        let entry = IndexEntry::new(
            format!("session_{}", i),
            "testagent".to_string(),
            format!("session_{}.jsonl", i),
        );
        index.insert(format!("key_{}", i), entry).await.unwrap();
    }

    // Run maintenance with max 3 sessions
    let config = MaintenanceConfig {
        mode: MaintenanceMode::Auto,
        prune_after: Duration::from_secs(86400 * 365), // 1 year (won't prune)
        max_sessions: 3,
        rotate_bytes: 10_000_000,
    };

    let report = index.maintenance(&config).await.unwrap();
    assert_eq!(report.capped, 2); // 5 - 3 = 2 removed

    // Verify only 3 remain
    let entries = index.load().await.unwrap();
    assert_eq!(entries.len(), 3);
}

/// Test session key derivation
#[test]
fn test_session_key_derivation() {
    // CLI default
    let ctx = SessionContext::default();
    let key = derive_session_key("myagent", SessionScope::CliDefault, &ctx);
    assert_eq!(key, "agent:myagent:cli:default");

    // Global
    let key = derive_session_key("myagent", SessionScope::Global, &ctx);
    assert_eq!(key, "agent:myagent:global");

    // Per-sender
    let ctx = SessionContext {
        channel: Some("discord".to_string()),
        sender_id: Some("123456".to_string()),
        chat_type: ChatType::Direct,
        ..Default::default()
    };
    let key = derive_session_key("myagent", SessionScope::PerSender, &ctx);
    assert_eq!(key, "agent:myagent:discord:123456");

    // Per-channel
    let ctx = SessionContext {
        channel: Some("discord".to_string()),
        channel_id: Some("987654".to_string()),
        chat_type: ChatType::Channel,
        ..Default::default()
    };
    let key = derive_session_key("myagent", SessionScope::PerChannel, &ctx);
    assert_eq!(key, "agent:myagent:discord:channel:987654");
}

/// Test session key parsing
#[test]
fn test_session_key_parsing() {
    let parts = parse_session_key("agent:myagent:discord:123456");
    assert_eq!(parts.agent, "myagent");
    assert_eq!(parts.context, "discord");
    assert_eq!(parts.identifier, "123456");

    // Complex key
    let parts = parse_session_key("agent:myagent:discord:guild:111:channel:222:thread:333");
    assert_eq!(parts.agent, "myagent");
    assert_eq!(parts.context, "discord");
    assert_eq!(parts.identifier, "guild:111:channel:222:thread:333");
}

/// Test Discord-specific session keys
#[test]
fn test_discord_session_keys() {
    // DM
    let key = discord_session_key("myagent", Some("user123"), None, None, None);
    assert_eq!(key, "agent:myagent:discord:user123");

    // Guild channel
    let key = discord_session_key("myagent", None, Some("guild456"), Some("channel789"), None);
    assert_eq!(
        key,
        "agent:myagent:discord:guild:guild456:channel:channel789"
    );

    // Thread
    let key = discord_session_key(
        "myagent",
        None,
        Some("guild456"),
        Some("channel789"),
        Some("thread101"),
    );
    assert!(key.contains("thread:thread101"));
}

/// Test CLI session key helper
#[test]
fn test_cli_session_key() {
    let key = cli_session_key("myagent");
    assert_eq!(key, "agent:myagent:cli:default");
}

/// Test session storage with file locking
#[tokio::test]
async fn test_session_storage_with_locking() {
    let temp = TempDir::new().unwrap();
    let storage = SessionStorage::new(temp.path().to_path_buf());

    // Create session
    storage
        .create_session("test_session", Some("/tmp".to_string()))
        .await
        .unwrap();

    // Append messages concurrently (simulated)
    let storage2 = SessionStorage::new(temp.path().to_path_buf());

    let fut1 = storage.append_message(
        "test_session",
        None,
        "user",
        vec![ContentBlock::Text {
            text: "Hello".to_string(),
        }],
    );

    let fut2 = storage2.append_message(
        "test_session",
        None,
        "assistant",
        vec![ContentBlock::Text {
            text: "Hi there".to_string(),
        }],
    );

    // Both should succeed with locking
    let (res1, res2) = tokio::join!(fut1, fut2);
    assert!(res1.is_ok());
    assert!(res2.is_ok());

    // Verify both messages are in session
    let entries = storage.load_session("test_session").await.unwrap();
    let messages: Vec<_> = entries
        .into_iter()
        .filter(|e| matches!(e, pekobot::session::jsonl::SessionEntry::Message { .. }))
        .collect();
    assert_eq!(messages.len(), 2);
}

/// Test session index migration from directory
#[tokio::test]
async fn test_session_index_migration() {
    let temp = TempDir::new().unwrap();
    let sessions_dir = temp.path().join("sessions");
    tokio::fs::create_dir_all(&sessions_dir).await.unwrap();

    // Create initial empty index file
    tokio::fs::write(sessions_dir.join("sessions.json"), "{}")
        .await
        .unwrap();

    // Create old-style session files (without index)
    for i in 0..3 {
        let session_file = sessions_dir.join(format!("legacy_session_{}.jsonl", i));
        tokio::fs::write(
            &session_file,
            r#"{"type":"session","version":3,"id":"legacy_session","timestamp":"2025-01-01T00:00:00Z"}
"#,
        )
        .await
        .unwrap();
    }

    // Open index (should be empty)
    let mut index = SessionIndex::open(&sessions_dir);
    let entries = index.load().await.unwrap();
    assert!(entries.is_empty());

    // Run migration
    let count = index.migrate_from_directory("testagent").await.unwrap();
    assert_eq!(count, 3);

    // Verify index now has entries
    let entries = index.load().await.unwrap();
    assert_eq!(entries.len(), 3);
}

/// Test index entry touch updates timestamp
#[test]
fn test_index_entry_touch() {
    let mut entry = IndexEntry::new(
        "test".to_string(),
        "agent".to_string(),
        "test.jsonl".to_string(),
    );

    let old_updated = entry.updated_at;

    // Wait a tiny bit
    std::thread::sleep(Duration::from_millis(10));

    entry.touch();

    assert!(entry.updated_at > old_updated);
}

/// Integration test: Full session lifecycle
#[tokio::test]
async fn test_full_session_lifecycle() {
    let temp = TempDir::new().unwrap();
    let sessions_dir = temp.path().join("sessions");
    tokio::fs::create_dir_all(&sessions_dir).await.unwrap();

    // Create initial empty index file
    tokio::fs::write(sessions_dir.join("sessions.json"), "{}")
        .await
        .unwrap();

    // 1. Create storage and index
    let storage = SessionStorage::new(sessions_dir.clone());
    let mut index = SessionIndex::open(&sessions_dir);

    // 2. Create session
    let session_id = "lifecycle_test";
    storage
        .create_session(session_id, Some("/tmp".to_string()))
        .await
        .unwrap();

    // 3. Create index entry with key
    let session_key = format!("agent:testagent:session:{}", session_id);
    let mut entry = IndexEntry::new(
        session_id.to_string(),
        "testagent".to_string(),
        format!("{}.jsonl", session_id),
    );
    entry.session_key = Some(session_key.clone());
    index.insert(session_key.clone(), entry).await.unwrap();

    // 4. Append messages
    let _msg_id = storage
        .append_message(
            session_id,
            None,
            "user",
            vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
        )
        .await
        .unwrap();

    // 5. Update index with message count
    if let Some(mut entry) = index.get(&session_key).await.unwrap() {
        entry.message_count = 1;
        index.insert(session_key.clone(), entry).await.unwrap();
    }

    // 6. Verify everything
    let loaded_entry = index.get(&session_key).await.unwrap().unwrap();
    assert_eq!(loaded_entry.message_count, 1);
    assert_eq!(loaded_entry.session_id, session_id);

    let messages = storage.load_session(session_id).await.unwrap();
    assert_eq!(messages.len(), 2); // session header + 1 message

    // 7. Run maintenance
    let config = MaintenanceConfig {
        mode: MaintenanceMode::Auto,
        prune_after: Duration::from_secs(86400),
        max_sessions: 100,
        rotate_bytes: 10_000_000,
    };
    let report = index.maintenance(&config).await.unwrap();
    assert!(report.is_empty()); // Nothing to do

    // 8. Entry should still exist
    assert!(index.get(&session_key).await.unwrap().is_some());
}
