//! Coneko Network Integration Example
//!
//! Demonstrates how to connect a Pekobot agent to the Coneko
//! coordination network for cross-network discovery and messaging.

use pekobot::{
    coneko::{client::ConekoClient, registry::UnifiedRegistry, ConekoConfig},
    Agent, Config,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🌐 Coneko Network Integration Example\n");
    println!("=====================================\n");

    // Configuration
    let coneko_endpoint =
        std::env::var("CONEKO_ENDPOINT").unwrap_or_else(|_| "http://localhost:8080".to_string());

    let coneko_token = std::env::var("CONEKO_TOKEN").ok();

    println!("📡 Coneko Endpoint: {}", coneko_endpoint);
    println!(
        "🔑 Auth Token: {}\n",
        if coneko_token.is_some() {
            "Set"
        } else {
            "Not set"
        }
    );

    // Create Coneko client
    let client = ConekoClient::new(&coneko_endpoint, coneko_token.as_deref())?;

    // Check Coneko server health
    println!("🏥 Checking Coneko server health...");
    match client.health_check().await {
        Ok(healthy) => {
            if healthy {
                println!("   ✅ Coneko server is healthy\n");
            } else {
                println!("   ⚠️  Coneko server reports unhealthy\n");
            }
        }
        Err(e) => {
            println!("   ❌ Failed to connect: {}\n", e);
            println!("   (Continuing with local-only mode)\n");
        }
    }

    // Create agent with Coneko integration
    let coneko_config = ConekoConfig {
        enabled: true,
        endpoint: coneko_endpoint.clone(),
        auth_token: coneko_token.clone(),
        auto_register: true,
        poll_interval_seconds: 30,
    };

    let agent_config = Config::agent("networked-agent")
        .with_description("An agent connected to the Coneko network")
        .with_capabilities(vec!["messaging".to_string(), "task_execution".to_string()])
        .with_coneko(coneko_config)
        .with_memory(true)
        .build();

    let agent = Agent::new(agent_config).await?;
    agent.start().await?;

    println!("🤖 Agent created and started:");
    println!("   Name: {}", agent.name());
    println!("   DID: {}", agent.did());
    println!();

    // Register agent with Coneko (if enabled)
    if let Ok(agent_endpoint) = std::env::var("AGENT_ENDPOINT") {
        println!("📤 Registering agent with Coneko...");

        let capabilities = vec!["messaging".to_string(), "task_execution".to_string()];

        match client
            .register_agent(
                agent.did(),
                agent.name(),
                &agent_endpoint,
                capabilities,
                None, // tenant
                None, // metadata
            )
            .await
        {
            Ok(_) => println!("   ✅ Agent registered successfully\n"),
            Err(e) => println!("   ❌ Registration failed: {}\n", e),
        }

        // Discover other agents
        println!("🔍 Discovering agents on Coneko network...");
        match client.discover_agents(None, None).await {
            Ok(agents) => {
                println!("   Found {} agents:\n", agents.len());
                for agent_info in agents {
                    println!("   • {}", agent_info.name);
                    println!("     DID: {}", agent_info.did);
                    println!("     Endpoint: {}", agent_info.endpoint);
                    println!("     Capabilities: {:?}\n", agent_info.capabilities);
                }
            }
            Err(e) => println!("   ❌ Discovery failed: {}\n", e),
        }
    } else {
        println!("⚠️  AGENT_ENDPOINT not set, skipping registration");
        println!("   (Set this env var to enable Coneko registration)\n");
    }

    // Demonstrate unified registry (local + Coneko)
    println!("📚 Creating unified registry...");
    let unified = UnifiedRegistry::new(pekobot::a2a::registry::create_registry().0, client);
    println!("   ✅ Unified registry created\n");

    // Search for agents with specific capability
    println!("🔍 Searching for agents with 'messaging' capability...");
    match unified
        .search_by_capability("messaging", pekobot::coneko::registry::CapabilityMatch::Any)
        .await
    {
        Ok(results) => {
            println!("   Found {} agents\n", results.len());
        }
        Err(e) => println!("   Search error: {}\n", e),
    }

    // Cleanup
    println!("🧹 Cleaning up...");
    if std::env::var("AGENT_ENDPOINT").is_ok() {
        let client = ConekoClient::new(&coneko_endpoint, coneko_token.as_deref())?;
        match client.unregister_agent(agent.did()).await {
            Ok(_) => println!("   ✅ Agent unregistered"),
            Err(e) => println!("   ⚠️  Unregistration failed: {}", e),
        }
    }

    agent.stop().await?;
    println!("   ✅ Agent stopped\n");

    println!("👋 Coneko integration example complete!");

    Ok(())
}
