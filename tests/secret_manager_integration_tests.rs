//! Integration tests for Secret Manager

use pekobot::secrets::{
    AuditEvent, SecretManager, SecretPermission, SecretResolver, SecretScope, SecretType,
};

#[tokio::test]
async fn test_secret_manager_basic_workflow() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-secrets.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    assert!(!manager.is_unlocked());

    // Unlock
    manager.unlock("test-password").await.unwrap();
    assert!(manager.is_unlocked());

    // Store a secret
    let entry = manager
        .set(
            "TEST_API_KEY",
            SecretScope::Global,
            "sk-test12345",
            SecretType::ApiKey,
            None,
        )
        .await
        .unwrap();

    assert_eq!(entry.name, "TEST_API_KEY");
    assert_eq!(entry.secret_type, SecretType::ApiKey);

    // Retrieve
    let value = manager.get("TEST_API_KEY", &SecretScope::Global).await.unwrap();
    assert_eq!(value, Some("sk-test12345".to_string()));

    // Lock
    manager.lock();
    assert!(!manager.is_unlocked());
}

#[tokio::test]
async fn test_secret_manager_agent_scoped() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-agent.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    let agent_did = "did:pekobot:local:test-agent:abc123";

    // Store agent-scoped secret
    let entry = manager
        .set(
            "AGENT_SECRET",
            SecretScope::Agent {
                did: agent_did.to_string(),
            },
            "agent-value",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    assert_eq!(entry.name, "AGENT_SECRET");

    // Retrieve
    let value = manager
        .get(
            "AGENT_SECRET",
            &SecretScope::Agent {
                did: agent_did.to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(value, Some("agent-value".to_string()));
}

#[tokio::test]
async fn test_secret_manager_list() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-list.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    manager
        .set("KEY1", SecretScope::Global, "value1", SecretType::ApiKey, None)
        .await
        .unwrap();
    manager
        .set("KEY2", SecretScope::Global, "value2", SecretType::Token, None)
        .await
        .unwrap();

    let secrets = manager.list(Some(SecretScope::Global)).await.unwrap();
    assert_eq!(secrets.len(), 2);
}

#[tokio::test]
async fn test_secret_manager_delete() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-delete.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    manager
        .set("TO_DELETE", SecretScope::Global, "value", SecretType::ApiKey, None)
        .await
        .unwrap();

    let deleted = manager.delete("TO_DELETE", &SecretScope::Global).await.unwrap();
    assert!(deleted);

    let not_found = manager.get("TO_DELETE", &SecretScope::Global).await.unwrap();
    assert_eq!(not_found, None);
}

#[tokio::test]
async fn test_secret_manager_permissions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-perms.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    manager
        .set("SECRET", SecretScope::Global, "value", SecretType::ApiKey, None)
        .await
        .unwrap();

    // Check default permission
    let perm = manager
        .check_permission("SECRET", &SecretScope::Global, None)
        .await
        .unwrap();
    assert_eq!(perm, SecretPermission::Read);

    // Grant write permission
    manager
        .grant_permission(
            "SECRET",
            &SecretScope::Global,
            Some("test-agent"),
            SecretPermission::Write,
        )
        .await
        .unwrap();

    let perm = manager
        .check_permission("SECRET", &SecretScope::Global, Some("test-agent"))
        .await
        .unwrap();
    assert_eq!(perm, SecretPermission::Write);

    // Revoke permission
    let revoked = manager
        .revoke_permission("SECRET", &SecretScope::Global, Some("test-agent"))
        .await
        .unwrap();
    assert!(revoked);
}

#[tokio::test]
async fn test_secret_manager_audit_log() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-audit.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    manager
        .set("AUDIT_TEST", SecretScope::Global, "value", SecretType::ApiKey, None)
        .await
        .unwrap();

    let value = manager.get("AUDIT_TEST", &SecretScope::Global).await.unwrap();
    assert!(value.is_some());

    // Query audit log
    let entries = manager
        .query_audit_log(None, None, None, None, 10)
        .await
        .unwrap();
    assert!(!entries.is_empty());

    // Check for created event
    let created_events: Vec<_> = entries
        .iter()
        .filter(|e| e.event == AuditEvent::SecretCreated)
        .collect();
    assert!(!created_events.is_empty());
}

#[tokio::test]
async fn test_secret_resolver_basic() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-resolver.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    manager
        .set("RESOLVE_KEY", SecretScope::Global, "resolved-value", SecretType::ApiKey, None)
        .await
        .unwrap();

    manager.lock();

    let resolver = SecretResolver::open(&store_path).unwrap();
    resolver.unlock("password").await.unwrap();

    let resolved = resolver.resolve("${secret:RESOLVE_KEY}").await.unwrap();
    assert_eq!(resolved, "resolved-value");

    // Test in context
    let config = "Bearer ${secret:RESOLVE_KEY}";
    let resolved = resolver.resolve(config).await.unwrap();
    assert_eq!(resolved, "Bearer resolved-value");
}

#[tokio::test]
async fn test_secret_manager_update() {
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-update.db");

    let mut manager = SecretManager::open(&store_path).unwrap();
    manager.unlock("password").await.unwrap();

    let entry = manager
        .set("UPDATE_KEY", SecretScope::Global, "v1", SecretType::ApiKey, None)
        .await
        .unwrap();
    assert_eq!(entry.version, 1);

    let updated = manager
        .set("UPDATE_KEY", SecretScope::Global, "v2", SecretType::ApiKey, None)
        .await
        .unwrap();
    assert_eq!(updated.version, 2);

    let value = manager.get("UPDATE_KEY", &SecretScope::Global).await.unwrap();
    assert_eq!(value, Some("v2".to_string()));
}
