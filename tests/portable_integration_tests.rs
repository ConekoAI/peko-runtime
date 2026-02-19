//! Integration tests for portable agent module

use pekobot::identity::{storage::KeyStorage, Identity, did::DIDScope};
use pekobot::portable::{
    export_agent, import_agent, ExportOptions, ImportOptions,
};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType};
use std::collections::HashMap;

/// Create a test agent configuration
fn create_test_config(name: &str) -> AgentConfig {
    AgentConfig {
        name: name.to_string(),
        description: Some("Test agent".to_string()),
        tenant: None,
        capabilities: vec![
            AgentCapability {
                name: "test_capability".to_string(),
                version: "1.0".to_string(),
                description: Some("Test capability".to_string()),
                parameters: None,
                required_auth: None,
                estimated_cost: None,
                estimated_duration: None,
            },
        ],
        provider: ProviderConfig {
            provider_type: ProviderType::Ollama,
            api_key: None,
            api_key_env: None,
            base_url: None,
            default_model: "llama3".to_string(),
            models: HashMap::new(),
            timeout_seconds: 30,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        memory: Some(MemoryConfig {
            enable_semantic_search: false,
            embedding_model: None,
            max_entries_per_agent: Some(100),
            default_ttl_seconds: None,
            auto_cleanup: true,
            cleanup_interval_seconds: 3600,
            database_path: None,
        }),
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(100.0),
        default_timeout_seconds: 300,
        coneko: None,
    }
}

#[tokio::test]
async fn test_export_import_roundtrip() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("test.agent");

    // Create test agent
    let config = create_test_config("roundtrip-test");
    let identity = Identity::new("test", DIDScope::Local).await.unwrap();
    let original_did = identity.did.clone();

    // Export
    let export_opts = ExportOptions {
        encrypt: false,
        passphrase: None,
        include_memory: false,
        rotate_keys: false,
        description: None,
        output_path: Some(package_path.to_string_lossy().to_string()),
    };

    let result = export_agent(config.clone(), identity, None, export_opts).await;
    assert!(result.is_ok(), "Export failed: {:?}", result.err());
    assert!(package_path.exists(), "Package file not created");

    // Import
    let import_opts = ImportOptions {
        new_name: Some("imported-agent".to_string()),
        passphrase: None,
        rotate_keys: false,
        import_memory: false,
        skip_validation: false,
        force: true, // Force in case DID exists from previous test
    };

    let result = import_agent(&package_path, import_opts).await;
    assert!(result.is_ok(), "Import failed: {:?}", result.err());

    let import_result = result.unwrap();
    assert_eq!(import_result.name, "imported-agent");
    assert_eq!(import_result.did, original_did, "DID should be preserved");
    assert!(!import_result.keys_rotated);
}

#[tokio::test]
async fn test_export_with_encryption() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("encrypted.agent");

    let config = create_test_config("encrypted-test");
    let identity = Identity::new("test", DIDScope::Local).await.unwrap();
    let original_did = identity.did.clone();

    // Export with encryption
    let export_opts = ExportOptions {
        encrypt: true,
        passphrase: Some("test-passphrase".to_string()),
        include_memory: false,
        rotate_keys: false,
        description: None,
        output_path: Some(package_path.to_string_lossy().to_string()),
    };

    let result = export_agent(config, identity, None, export_opts).await;
    assert!(result.is_ok(), "Encrypted export failed: {:?}", result.err());

    // Import with correct passphrase
    let import_opts = ImportOptions {
        new_name: None,
        passphrase: Some("test-passphrase".to_string()),
        rotate_keys: false,
        import_memory: false,
        skip_validation: false,
        force: true,
    };

    let result = import_agent(&package_path, import_opts).await;
    assert!(result.is_ok(), "Import with passphrase failed: {:?}", result.err());

    let import_result = result.unwrap();
    assert_eq!(import_result.did, original_did);
}

#[tokio::test]
async fn test_import_with_key_rotation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("rotated.agent");

    let config = create_test_config("rotation-test");
    let identity = Identity::new("test", DIDScope::Local).await.unwrap();
    let original_did = identity.did.clone();

    // Export
    let export_opts = ExportOptions {
        encrypt: false,
        passphrase: None,
        include_memory: false,
        rotate_keys: false,
        description: None,
        output_path: Some(package_path.to_string_lossy().to_string()),
    };

    export_agent(config, identity, None, export_opts).await.unwrap();

    // Import with key rotation
    let import_opts = ImportOptions {
        new_name: Some("rotated-agent".to_string()),
        passphrase: None,
        rotate_keys: true, // Generate new keys
        import_memory: false,
        skip_validation: false,
        force: true,
    };

    let result = import_agent(&package_path, import_opts).await;
    assert!(result.is_ok(), "Import with rotation failed: {:?}", result.err());

    let import_result = result.unwrap();
    assert!(import_result.keys_rotated, "Keys should be rotated");
    assert_ne!(import_result.did, original_did, "DID should be different after rotation");
    assert_eq!(import_result.name, "rotated-agent");
}

#[tokio::test]
async fn test_package_inspection() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("inspect.agent");

    let config = create_test_config("inspect-test");
    let identity = Identity::new("test", DIDScope::Local).await.unwrap();

    // Export
    let export_opts = ExportOptions {
        encrypt: false,
        passphrase: None,
        include_memory: false,
        rotate_keys: false,
        description: Some("Test description".to_string()),
        output_path: Some(package_path.to_string_lossy().to_string()),
    };

    export_agent(config.clone(), identity, None, export_opts).await.unwrap();

    // Inspect package
    let info = pekobot::portable::get_package_info(&package_path).await.unwrap();
    
    assert_eq!(info.name, "inspect-test");
    assert_eq!(info.description, Some("Test description".to_string()));
    assert!(!info.encrypted);
    assert!(info.valid);
    assert_eq!(info.capabilities.len(), 1);
    assert!(info.capabilities.contains(&"test_capability".to_string()));
}

#[tokio::test]
async fn test_export_import_with_memory() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_path = temp_dir.path().join("with-memory.agent");
    let memory_path = temp_dir.path().join("memory.db");

    // Create agent with memory
    let config = create_test_config("memory-test");
    let identity = Identity::new("test", DIDScope::Local).await.unwrap();

    // Create and populate memory
    {
        use pekobot::memory::sqlite::SqliteMemory;
        let memory = SqliteMemory::new(
            Some(&memory_path),
            "memory-test",
        ).await.unwrap();

        memory.store(
            "Test memory entry",
            Some(serde_json::json!({"test": true})),
        ).await.unwrap();
    }

    // Export with memory
    let export_opts = ExportOptions {
        encrypt: false,
        passphrase: None,
        include_memory: true,
        rotate_keys: false,
        description: None,
        output_path: Some(package_path.to_string_lossy().to_string()),
    };

    export_agent(config, identity, Some(memory_path.clone()), export_opts).await.unwrap();

    // Import with memory
    let import_opts = ImportOptions {
        new_name: None,
        passphrase: None,
        rotate_keys: false,
        import_memory: true,
        skip_validation: false,
        force: true,
    };

    let result = import_agent(&package_path, import_opts).await;
    assert!(result.is_ok());
    
    let import_result = result.unwrap();
    assert!(import_result.memory_path.is_some(), "Memory path should be set");
    
    // Verify memory file exists
    let mem_path = import_result.memory_path.unwrap();
    assert!(mem_path.exists(), "Memory database should exist");
}

#[test]
fn test_agent_package_file_detection() {
    // Test extension-based detection
    assert!(!pekobot::portable::is_agent_package("test.txt"));
    assert!(!pekobot::portable::is_agent_package("test.tar.gz"));
    
    // Note: File existence and magic byte checks require actual files
    // These are tested in the integration tests above
}
