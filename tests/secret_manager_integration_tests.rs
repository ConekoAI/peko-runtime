//! Integration tests for Secret Manager
//!
//! Tests the complete workflow:
//! 1. Store creation and unlocking
//! 2. Setting/getting secrets
//! 3. Permission management
//! 4. Audit logging
//! 5. Secret resolution with ${secret:NAME} syntax

use pekobot::secrets::{
    AuditEvent, SecretManager, SecretPermission, SecretResolver, SecretScope, SecretType,
};
use std::collections::HashMap;

#[tokio::test]
async fn test_secret_manager_full_workflow() {
    // Create temporary directory for test store
    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("test-secrets.db");

    println!("🧪 Testing Secret Manager at: {:?}", store_path);

    // =================================================================
    // Phase 1: Store Creation and Unlocking
    // =================================================================
    println!("\n📦 Phase 1: Store Creation and Unlocking");

    let mut manager = SecretManager::open(&store_path).unwrap();
    assert!(!manager.is_unlocked(), "Store should be locked initially");

    // Unlock with master password
    manager.unlock("test-master-password").await.unwrap();
    assert!(manager.is_unlocked(), "Store should be unlocked after unlock()");

    println!("   ✅ Store created and unlocked successfully");

    // =================================================================
    // Phase 2: Storing and Retrieving Secrets
    // =================================================================
    println!("\n🔐 Phase 2: Storing and Retrieving Secrets");

    // Store a global API key
    let entry = manager
        .set(
            "OPENAI_API_KEY",
            SecretScope::Global,
            "sk-test12345",
            SecretType::ApiKey,
            None,
        )
        .await
        .unwrap();

    assert_eq!(entry.name, "OPENAI_API_KEY");
    assert_eq!(entry.secret_type, SecretType::ApiKey);
    assert_eq!(entry.scope, SecretScope::Global);
    println!("   ✅ Stored global secret: {}", entry.name);

    // Store a per-agent secret
    let agent_did = "did:pekobot:local:shopify-bot:abc123";
    let entry2 = manager
        .set(
            "SHOPIFY_TOKEN",
            SecretScope::Agent {
                did: agent_did.to_string(),
            },
            "shpat_xxxxx",
            SecretType::Token,
            None,
        )
        .await
        .unwrap();

    assert_eq!(entry2.name, "SHOPIFY_TOKEN");
    assert_eq!(entry2.scope.as_str(), format!("agent:{}", agent_did));
    println!("   ✅ Stored agent-scoped secret: {}", entry2.name);

    // Retrieve secrets
    let api_key = manager
        .get("OPENAI_API_KEY", &SecretScope::Global)
        .await
        .unwrap();
    assert_eq!(api_key, Some("sk-test12345".to_string()));
    println!("   ✅ Retrieved global secret correctly");

    let shopify_token = manager
        .get(
            "SHOPIFY_TOKEN",
            &SecretScope::Agent {
                did: agent_did.to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(shopify_token, Some("shpat_xxxxx".to_string()));
    println!("   ✅ Retrieved agent-scoped secret correctly");

    // Test secret not found
    let not_found = manager
        .get("NONEXISTENT", &SecretScope::Global)
        .await
        .unwrap();
    assert_eq!(not_found, None);
    println!("   ✅ Non-existent secret returns None");

    // =================================================================
    // Phase 3: Listing Secrets
    // =================================================================
    println!("\n📋 Phase 3: Listing Secrets");

    let all_secrets = manager.list(None).await.unwrap();
    assert_eq!(all_secrets.len(), 2);
    println!("   ✅ Listed all secrets: {}", all_secrets.len());

    let global_secrets = manager.list(Some(SecretScope::Global)).await.unwrap();
    assert_eq!(global_secrets.len(), 1);
    assert_eq!(global_secrets[0].name, "OPENAI_API_KEY");
    println!("   ✅ Listed global secrets only: {}", global_secrets.len());

    // =================================================================
    // Phase 4: Permission Management
    // =================================================================
    println!("\n🔒 Phase 4: Permission Management");

    // Check default permission for global secret (should be Read)
    let perm = manager
        .check_permission("OPENAI_API_KEY", &SecretScope::Global, None)
        .await
        .unwrap();
    assert_eq!(perm, SecretPermission::Read);
    println!("   ✅ Default permission for global secret is Read");

    // Grant write permission to specific agent
    let other_agent = "did:pekobot:local:other-agent:xyz789";
    manager
        .grant_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some(other_agent),
            SecretPermission::Write,
        )
        .await
        .unwrap();
    println!("   ✅ Granted Write permission to agent");

    // Check permission after grant
    let perm = manager
        .check_permission("OPENAI_API_KEY", &SecretScope::Global, Some(other_agent))
        .await
        .unwrap();
    assert_eq!(perm, SecretPermission::Write);
    println!("   ✅ Agent now has Write permission");

    // Revoke permission
    let revoked = manager
        .revoke_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some(other_agent),
        )
        .await
        .unwrap();
    assert!(revoked);
    println!("   ✅ Permission revoked successfully");

    // Check permission after revoke (should fall back to default)
    let perm = manager
        .check_permission("OPENAI_API_KEY", &SecretScope::Global, Some(other_agent))
        .await
        .unwrap();
    assert_eq!(perm, SecretPermission::Read); // Back to default
    println!("   ✅ Permission correctly falls back to default");

    // List permissions
    let perms = manager
        .get_permissions("OPENAI_API_KEY", &SecretScope::Global)
        .await
        .unwrap();
    // After revoke, there should be no explicit permissions (just default policy)
    println!("   ✅ Listed permissions: {}", perms.len());

    // =================================================================
    // Phase 5: Permission-Based Access
    // =================================================================
    println!("\n🛡️ Phase 5: Permission-Based Access");

    // Set default permission to None (deny all)
    manager
        .grant_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            None, // Default permission
            SecretPermission::None,
        )
        .await
        .unwrap();
    println!("   ✅ Set default permission to None");

    // Try to access with denied permission
    let result = manager
        .get_with_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some("some-agent"),
        )
        .await;
    assert!(result.is_err(), "Should fail with permission denied");
    println!("   ✅ Access correctly denied");

    // Grant specific agent read access
    manager
        .grant_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some("authorized-agent"),
            SecretPermission::Read,
        )
        .await
        .unwrap();

    // Now access should work
    let value = manager
        .get_with_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some("authorized-agent"),
        )
        .await
        .unwrap();
    assert_eq!(value, Some("sk-test12345".to_string()));
    println!("   ✅ Access correctly granted to authorized agent");

    // =================================================================
    // Phase 6: Audit Logging
    // =================================================================
    println!("\n📊 Phase 6: Audit Logging");

    // Query audit log
    let audit_entries = manager
        .query_audit_log(None, None, None, None, 100)
        .await
        .unwrap();

    println!("   ✅ Audit log has {} entries", audit_entries.len());
    assert!(
        audit_entries.len() >= 4,
        "Should have at least 4 audit entries (2 creates, 2 grants)"
    );

    // Check for specific events
    let created_events: Vec<_> = audit_entries
        .iter()
        .filter(|e| e.event == AuditEvent::SecretCreated)
        .collect();
    assert_eq!(created_events.len(), 2, "Should have 2 SECRET_CREATED events");
    println!("   ✅ Found {} SECRET_CREATED events", created_events.len());

    let grant_events: Vec<_> = audit_entries
        .iter()
        .filter(|e| e.event == AuditEvent::PermissionGranted)
        .collect();
    assert!(!grant_events.is_empty(), "Should have PERMISSION_GRANTED events");
    println!("   ✅ Found {} PERMISSION_GRANTED events", grant_events.len());

    // Get audit stats
    let stats = manager.get_audit_stats(None).await.unwrap();
    println!("   ✅ Audit stats: {} total, {} successful", stats.total, stats.successful);
    assert!(stats.total >= 4);
    assert!(stats.successful >= 4);

    // =================================================================
    // Phase 7: Secret Resolution
    // =================================================================
    println!("\n🔍 Phase 7: Secret Resolution");

    // Lock and reopen store for resolver test
    manager.lock();
    assert!(!manager.is_unlocked());

    // Create resolver
    let resolver = SecretResolver::open(&store_path).unwrap();
    resolver.unlock("test-master-password").await.unwrap();
    assert!(resolver.is_unlocked().await);
    println!("   ✅ SecretResolver unlocked");

    // Test simple resolution
    let resolved = resolver.resolve("${secret:OPENAI_API_KEY}").await.unwrap();
    assert_eq!(resolved, "sk-test12345");
    println!("   ✅ Resolved ${secret:OPENAI_API_KEY}");

    // Test resolution in context
    let config_value = "Authorization: Bearer ${secret:OPENAI_API_KEY}";
    let resolved = resolver.resolve(config_value).await.unwrap();
    assert_eq!(resolved, "Authorization: Bearer sk-test12345");
    println!("   ✅ Resolved secret in config context");

    // Test agent-scoped resolution
    let resolved = resolver
        .resolve(&format!("${{secret.agent:{}:SHOPIFY_TOKEN}}", agent_did))
        .await
        .unwrap();
    assert_eq!(resolved, "shpat_xxxxx");
    println!("   ✅ Resolved ${{secret.agent:DID:SHOPIFY_TOKEN}}");

    // Test resolution with missing secret
    let result = resolver.resolve("${secret:NONEXISTENT}").await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("not found"));
    assert!(err_msg.contains("pekobot secret set"));
    println!("   ✅ Helpful error message for missing secret");

    // Test environment variable resolution
    std::env::set_var("TEST_API_KEY", "env-test-value");
    let resolved = resolver.resolve("${env:TEST_API_KEY}").await.unwrap();
    assert_eq!(resolved, "env-test-value");
    std::env::remove_var("TEST_API_KEY");
    println!("   ✅ Resolved ${{env:VARNAME}}");

    // =================================================================
    // Phase 8: Secret Update
    // =================================================================
    println!("\n📝 Phase 8: Secret Update");

    // Re-unlock manager
    manager.unlock("test-master-password").await.unwrap();

    // Update existing secret
    let updated = manager
        .set(
            "OPENAI_API_KEY",
            SecretScope::Global,
            "sk-updated67890",
            SecretType::ApiKey,
            None,
        )
        .await
        .unwrap();

    assert_eq!(updated.version, 2, "Version should increment");
    println!("   ✅ Secret updated to version {}", updated.version);

    // Verify new value
    let new_value = manager
        .get("OPENAI_API_KEY", &SecretScope::Global)
        .await
        .unwrap();
    assert_eq!(new_value, Some("sk-updated67890".to_string()));
    println!("   ✅ Retrieved updated value correctly");

    // Check audit log for update event
    let audit_entries = manager
        .query_audit_log(Some("OPENAI_API_KEY"), None, None, Some(AuditEvent::SecretUpdated), 10)
        .await
        .unwrap();
    assert!(!audit_entries.is_empty(), "Should have SECRET_UPDATED event");
    println!("   ✅ SECRET_UPDATED event logged");

    // =================================================================
    // Phase 9: Secret Deletion
    // =================================================================
    println!("\n🗑️ Phase 9: Secret Deletion");

    // Delete secret
    let deleted = manager
        .delete("OPENAI_API_KEY", &SecretScope::Global)
        .await
        .unwrap();
    assert!(deleted);
    println!("   ✅ Secret deleted");

    // Verify deletion
    let not_found = manager
        .get("OPENAI_API_KEY", &SecretScope::Global)
        .await
        .unwrap();
    assert_eq!(not_found, None);
    println!("   ✅ Deleted secret returns None");

    // Check audit log for delete event
    let audit_entries = manager
        .query_audit_log(Some("OPENAI_API_KEY"), None, None, Some(AuditEvent::SecretDeleted), 10)
        .await
        .unwrap();
    assert!(!audit_entries.is_empty(), "Should have SECRET_DELETED event");
    println!("   ✅ SECRET_DELETED event logged");

    // =================================================================
    // Phase 10: Lock/Unlock Security
    // =================================================================
    println!("\n🔒 Phase 10: Lock/Unlock Security");

    // Lock the store
    manager.lock();
    assert!(!manager.is_unlocked());
    println!("   ✅ Store locked");

    // Try to access while locked
    let result = manager.get("SHOPIFY_TOKEN", &SecretScope::Global).await;
    assert!(result.is_err());
    println!("   ✅ Access denied while locked");

    // Unlock again
    manager.unlock("test-master-password").await.unwrap();
    assert!(manager.is_unlocked());
    println!("   ✅ Store unlocked again");

    // Access should work again
    let value = manager
        .get(
            "SHOPIFY_TOKEN",
            &SecretScope::Agent {
                did: agent_did.to_string(),
            },
        )
        .await
        .unwrap();
    assert!(value.is_some());
    println!("   ✅ Access works after re-unlock");

    // =================================================================
    // Summary
    // =================================================================
    println!("\n✅ All Secret Manager integration tests passed!");
    println!("   - Store creation and unlocking");
    println!("   - Storing and retrieving secrets");
    println!("   - Listing secrets with filtering");
    println!("   - Permission management (grant/revoke)");
    println!("   - Permission-based access control");
    println!("   - Audit logging and querying");
    println!("   - Secret resolution (${{secret:NAME}})");
    println!("   - Secret updates with versioning");
    println!("   - Secret deletion");
    println!("   - Lock/unlock security");
}

#[tokio::test]
async fn test_secret_manager_concurrent_access() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let temp_dir = tempfile::tempdir().unwrap();
    let store_path = temp_dir.path().join("concurrent-test.db");

    // Create and unlock store
    let manager = Arc::new(Mutex::new(SecretManager::open(&store_path).unwrap()));
    {
        let mut m = manager.lock().await;
        m.unlock("password").await.unwrap();

        // Store initial secret
        m.set("TEST_KEY", SecretScope::Global, "initial", SecretType::ApiKey, None)
            .await
            .unwrap();
    }

    // Spawn multiple concurrent reads
    let mut handles = vec![];
    for i in 0..10 {
        let manager = Arc::clone(&manager);
        let handle = tokio::spawn(async move {
            let m = manager.lock().await;
            let value = m.get("TEST_KEY", &SecretScope::Global).await.unwrap();
            println!("   Task {} read: {:?}", i, value);
            value
        });
        handles.push(handle);
    }

    // Wait for all tasks
    let results = futures::future::join_all(handles).await;
    for result in results {
        assert_eq!(result.unwrap(), Some("initial".to_string()));
    }

    println!("✅ Concurrent access test passed!");
}
