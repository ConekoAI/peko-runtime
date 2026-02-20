//! Example: Export and Import Agent Workflow
//!
//! Demonstrates how to:
//! 1. Create an agent with memory
//! 2. Export it to a .agent package
//! 3. Import it on another "machine" (simulated)
//! 4. Verify the imported agent has the same capabilities

use pekobot::agent::Agent;
use pekobot::identity::Identity;
use pekobot::portable::{export_agent, import_agent, ExportOptions, ImportOptions};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("🐱 Pekobot Portable Agent Example\n");

    // Step 1: Create a test agent with some configuration
    println!("Step 1: Creating test agent...");
    let config = create_test_config();
    let identity = Identity::new("test-agent", pekobot::identity::did::DIDScope::Local).await?;
    let did = identity.did.clone();

    println!("   Created agent: {}", config.name);
    println!("   DID: {}", did);

    // Step 2: Create a temporary memory database
    let temp_dir = tempfile::tempdir()?;
    let memory_path = temp_dir.path().join("memory.db");

    // Store some test data in memory
    {
        use pekobot::memory::sqlite::SqliteMemory;
        let memory = SqliteMemory::new(&memory_path, "test-agent")?;

        memory
            .store(
                "Hello, I am a test agent!",
                Some(serde_json::json!({
                    "test": true,
                    "created": chrono::Utc::now().to_rfc3339(),
                })),
            )
            .await?;

        println!("   Stored test memory entry");
    }

    // Step 3: Export the agent
    println!("\nStep 2: Exporting agent to .agent package...");
    let export_opts = ExportOptions {
        encrypt: false, // For demo, skip encryption
        passphrase: None,
        include_memory: true,
        rotate_keys: false,
        description: Some("Test agent for portable workflow demo".to_string()),
        output_path: Some("./test-agent-export.agent".to_string()),
    };

    let package_path =
        export_agent(config.clone(), identity, Some(memory_path), export_opts).await?;

    println!("   ✅ Exported to: {}", package_path.display());
    println!(
        "   Package size: {} bytes",
        std::fs::metadata(&package_path)?.len()
    );

    // Step 4: Inspect the package
    println!("\nStep 3: Inspecting package...");
    let info = pekobot::portable::get_package_info(&package_path).await?;
    println!("{}", info.format());

    // Step 5: Import the agent (simulating moving to another machine)
    println!("\nStep 4: Importing agent (simulating new machine)...");
    let import_opts = ImportOptions {
        new_name: Some("imported-test-agent".to_string()),
        passphrase: None,
        rotate_keys: false,
        import_memory: true,
        skip_validation: false,
        force: false,
    };

    let result = import_agent(&package_path, import_opts).await?;

    println!("   ✅ Import successful!");
    println!("   New name: {}", result.name);
    println!("   DID preserved: {}", result.did);
    println!("   Config path: {}", result.config_path.display());
    if let Some(mem_path) = result.memory_path {
        println!("   Memory path: {}", mem_path.display());
    }

    // Step 6: Verify the imported agent
    println!("\nStep 5: Verifying imported agent...");
    let imported_config: AgentConfig = {
        let content = std::fs::read_to_string(&result.config_path)?;
        toml::from_str(&content)?
    };

    assert_eq!(
        imported_config.capabilities.len(),
        config.capabilities.len()
    );
    println!(
        "   ✅ Capabilities match: {}",
        imported_config.capabilities.len()
    );

    assert_eq!(imported_config.name, "imported-test-agent");
    println!("   ✅ Name updated correctly");

    // Cleanup
    println!("\n🧹 Cleaning up...");
    std::fs::remove_file(&package_path)?;
    std::fs::remove_file(&result.config_path)?;
    if let Some(mem_path) = result.memory_path {
        let _ = std::fs::remove_file(mem_path);
    }
    println!("   Removed temporary files");

    println!("\n✨ Example completed successfully!");
    println!("\nYou can now:");
    println!("   pekobot export --agent my-agent --output ./my-agent.agent --encrypt");
    println!("   pekobot import --file ./my-agent.agent --name imported-agent");
    println!("   pekobot inspect --file ./my-agent.agent");

    Ok(())
}

fn create_test_config() -> AgentConfig {
    AgentConfig {
        name: "test-agent".to_string(),
        description: Some("A test agent for the portable workflow example".to_string()),
        tenant: Some("demo".to_string()),
        capabilities: vec![
            AgentCapability {
                name: "messaging".to_string(),
                version: "1.0".to_string(),
                description: Some("Send and receive messages".to_string()),
                parameters: None,
                required_auth: None,
                estimated_cost: None,
                estimated_duration: None,
            },
            AgentCapability {
                name: "task_execution".to_string(),
                version: "1.0".to_string(),
                description: Some("Execute tasks".to_string()),
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
            base_url: Some("http://localhost:11434".to_string()),
            default_model: "llama3".to_string(),
            models: HashMap::new(),
            timeout_seconds: 30,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        memory: Some(MemoryConfig {
            enable_semantic_search: true,
            embedding_model: Some("all-MiniLM-L6-v2".to_string()),
            max_entries_per_agent: Some(1000),
            default_ttl_seconds: None,
            auto_cleanup: true,
            cleanup_interval_seconds: 3600,
            database_path: None,
        }),
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(50.0),
        default_timeout_seconds: 300,
        coneko: None,
    }
}
