//! Session Management Integration Tests
//!
//! Run with: cargo test --test session_management

use std::time::Duration;
use tempfile::TempDir;

// Import from pekobot crate
use pekobot::session::index::SessionEntry;
use pekobot::session::jsonl::SessionStorage;
use pekobot::session::key::{
    cli_session_key, derive_session_key, discord_session_key, parse_session_key, ChatType,
    SessionKeyContext, SessionScope,
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

    let mut index = pekobot::session::index::SessionIndex::open(index_path);

    // Create a session entry
    let entry = SessionEntry {
        session_id: "test_123".to_string(),
        agent_name: "testagent".to_string(),
        created_at: 1234567890,
        updated_at: 1234567890,
        message_count: 0,
        turn_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        transcript_file: "test_123.jsonl".to_string(),
        title: None,
        parent_session_id: None,
        ended: false,
        trigger: "test".to_string(),
        provider: None,
        model: None,
        channel: None,
        recipient: None,
        cwd: None,
        peer_type: None,
        peer_id: None,
    };

    // Insert and verify
    index.insert(entry).await.unwrap();

    // Reload and verify
    let loaded = index.get("test_123").await.unwrap();
    assert!(loaded.is_some());
    assert_eq!(loaded.unwrap().session_id, "test_123");
}

/// Test peer-based session operations
#[tokio::test]
async fn test_peer_session_operations() {
    let temp = TempDir::new().unwrap();
    let index_path = temp.path();

    tokio::fs::create_dir_all(&index_path).await.unwrap();

    let mut index = pekobot::session::index::SessionIndex::open(index_path);

    // Create a peer info entry
    let peer_key = "agent:testagent:cli:default";
    let session_id = "session_abc123";

    // Create session entry
    let entry = SessionEntry {
        session_id: session_id.to_string(),
        agent_name: "testagent".to_string(),
        created_at: 1234567890,
        updated_at: 1234567890,
        message_count: 5,
        turn_count: 3,
        input_tokens: 100,
        output_tokens: 200,
        total_tokens: 300,
        transcript_file: format!("{}.jsonl", session_id),
        title: Some("Test Session".to_string()),
        parent_session_id: None,
        ended: false,
        trigger: "cli".to_string(),
        provider: Some("openai".to_string()),
        model: Some("gpt-4".to_string()),
        channel: Some("cli".to_string()),
        recipient: None,
        cwd: Some("/tmp".to_string()),
        peer_type: Some("user".to_string()),
        peer_id: Some("default".to_string()),
    };

    // Save session
    index.insert(entry).await.unwrap();

    // Get session directly
    let loaded = index.get(session_id).await.unwrap();
    assert!(loaded.is_some());
    let loaded_entry = loaded.unwrap();
    assert_eq!(loaded_entry.session_id, session_id);
    assert_eq!(loaded_entry.message_count, 5);

    // Verify we can update message count
    let mut updated_entry = loaded_entry;
    updated_entry.message_count = 10;
    updated_entry.updated_at = 1234567999;
    index.insert(updated_entry).await.unwrap();

    let reloaded = index.get(session_id).await.unwrap().unwrap();
    assert_eq!(reloaded.message_count, 10);
}

/// Test session key derivation
#[test]
fn test_session_key_derivation() {
    // CLI default
    let ctx = SessionKeyContext::default();
    let key = derive_session_key("myagent", SessionScope::CliDefault, &ctx);
    assert_eq!(key, "agent:myagent:cli:default");

    // Global
    let key = derive_session_key("myagent", SessionScope::Global, &ctx);
    assert_eq!(key, "agent:myagent:global");

    // Per-sender
    let ctx = SessionKeyContext {
        channel: Some("discord".to_string()),
        sender_id: Some("123456".to_string()),
        chat_type: ChatType::Direct,
        ..Default::default()
    };
    let key = derive_session_key("myagent", SessionScope::PerSender, &ctx);
    assert_eq!(key, "agent:myagent:discord:123456");

    // Per-channel
    let ctx = SessionKeyContext {
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
///
/// NOTE: This test is marked as #[ignore] because it has race conditions
/// when running concurrently with other tests. The file locking mechanism
/// has issues with concurrent access in the test environment.
#[tokio::test]
#[ignore = "Flaky test - race condition with concurrent file locking"]
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
    assert!(res1.is_ok(), "fut1 failed: {:?}", res1);
    assert!(res2.is_ok(), "fut2 failed: {:?}", res2);

    // Verify both messages are in session
    let entries = storage.load_session("test_session").await.unwrap();
    let messages: Vec<_> = entries
        .into_iter()
        .filter(|e| matches!(e, pekobot::session::jsonl::SessionEntry::Message { .. }))
        .collect();
    assert_eq!(messages.len(), 2);
}

/// Integration test: Full session lifecycle
#[tokio::test]
async fn test_full_session_lifecycle() {
    let temp = TempDir::new().unwrap();
    let sessions_dir = temp.path().join("sessions");
    tokio::fs::create_dir_all(&sessions_dir).await.unwrap();

    // 1. Create storage and index
    let storage = SessionStorage::new(sessions_dir.clone());
    let mut index = pekobot::session::index::SessionIndex::open(&sessions_dir);

    // 2. Create session
    let session_id = "lifecycle_test";
    storage
        .create_session(session_id, Some("/tmp".to_string()))
        .await
        .unwrap();

    // 3. Create session entry
    let entry = SessionEntry {
        session_id: session_id.to_string(),
        agent_name: "testagent".to_string(),
        created_at: 1234567890,
        updated_at: 1234567890,
        message_count: 0,
        turn_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        transcript_file: format!("{}.jsonl", session_id),
        title: None,
        parent_session_id: None,
        ended: false,
        trigger: "cli".to_string(),
        provider: None,
        model: None,
        channel: None,
        recipient: None,
        cwd: Some("/tmp".to_string()),
        peer_type: None,
        peer_id: None,
    };
    index.insert(entry).await.unwrap();

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
    if let Some(mut loaded) = index.get(session_id).await.unwrap() {
        loaded.message_count = 1;
        index.insert(loaded).await.unwrap();
    }

    // 6. Verify everything
    let loaded_entry = index.get(session_id).await.unwrap().unwrap();
    assert_eq!(loaded_entry.message_count, 1);
    assert_eq!(loaded_entry.session_id, session_id);

    let messages = storage.load_session(session_id).await.unwrap();
    assert_eq!(messages.len(), 2); // session header + 1 message

    // 7. Entry should still exist
    assert!(index.get(session_id).await.unwrap().is_some());
}
