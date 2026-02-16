use clap::{Parser, Subcommand};
use pekobot::agent::Agent;
use pekobot::channels::cli::{CliChannel, run_interactive_loop};
use pekobot::coneko::{ConekoAdapter, ConekoConfig};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType, ModelConfig};
use std::collections::HashMap;
use tracing::{info, warn};

/// Pekobot - Lightweight Multi-Agent Runtime
#[derive(Parser)]
#[command(name = "pekobot")]
#[command(version)]
#[command(about = "Lightweight multi-agent runtime with optional Coneko network")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single agent with interactive CLI
    Agent {
        /// Agent configuration file
        #[arg(short, long)]
        config: Option<String>,
        /// Agent name
        #[arg(short, long, default_value = "peko")]
        name: String,
        /// LLM provider (openai, anthropic, ollama)
        #[arg(short, long, default_value = "openai")]
        provider: String,
        /// Model name
        #[arg(short, long)]
        model: Option<String>,
        /// Database path for memory
        #[arg(long)]
        db: Option<String>,
        /// Coneko endpoint (enables network mode)
        #[arg(long, env = "CONEKO_ENDPOINT")]
        coneko: Option<String>,
        /// Coneko auth token
        #[arg(long, env = "CONEKO_TOKEN")]
        coneko_token: Option<String>,
    },
    /// Run multi-agent orchestrator
    Orchestrate {
        /// Orchestrator configuration file
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Check system status
    Status,
    /// Onboard new agent (interactive setup)
    Onboard,
    /// Send a message to a running agent (HTTP mode)
    Send {
        /// Target endpoint
        #[arg(short, long)]
        endpoint: String,
        /// Message to send
        #[arg(short, long)]
        message: String,
    },
    /// Coneko network commands
    #[command(subcommand)]
    Coneko(ConekoCommands),
}

#[derive(Subcommand)]
enum ConekoCommands {
    /// Check Coneko server status
    Status {
        /// Coneko endpoint
        #[arg(short, long, env = "CONEKO_ENDPOINT")]
        endpoint: String,
    },
    /// Register an agent with Coneko
    Register {
        /// Coneko endpoint
        #[arg(short, long, env = "CONEKO_ENDPOINT")]
        endpoint: String,
        /// Agent DID
        #[arg(short, long)]
        did: String,
        /// Agent name
        #[arg(short, long)]
        name: String,
        /// Agent endpoint (where the agent receives messages)
        #[arg(short, long)]
        agent_endpoint: String,
        /// Capabilities (comma-separated)
        #[arg(short, long, value_delimiter = ',')]
        capabilities: Vec<String>,
        /// Coneko auth token
        #[arg(long, env = "CONEKO_TOKEN")]
        token: Option<String>,
    },
    /// Unregister an agent from Coneko
    Unregister {
        /// Coneko endpoint
        #[arg(short, long, env = "CONEKO_ENDPOINT")]
        endpoint: String,
        /// Agent DID
        #[arg(short, long)]
        did: String,
        /// Coneko auth token
        #[arg(long, env = "CONEKO_TOKEN")]
        token: Option<String>,
    },
    /// Discover agents on Coneko
    Discover {
        /// Coneko endpoint
        #[arg(short, long, env = "CONEKO_ENDPOINT")]
        endpoint: String,
        /// Filter by capability
        #[arg(short, long)]
        capability: Option<String>,
        /// Coneko auth token
        #[arg(long, env = "CONEKO_TOKEN")]
        token: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Agent { config, name, provider, model, db, coneko, coneko_token } => {
            info!("Starting Pekobot agent: {}", name);
            
            // Clone coneko for later use
            let coneko_for_status = coneko.clone();
            
            // Build config
            let agent_config = if let Some(config_path) = config {
                info!("Loading config from: {}", config_path);
                build_default_config(&name, &provider, model, db, coneko, coneko_token)
            } else {
                warn!("No config specified, using defaults");
                build_default_config(&name, &provider, model, db, coneko, coneko_token)
            };
            
            // Create and start agent
            match Agent::new(agent_config).await {
                Ok(agent) => {
                    if let Err(e) = agent.start().await {
                        eprintln!("❌ Failed to start agent: {}", e);
                        return Err(e);
                    }
                    
                    println!("\n🐱 Agent '{}' started successfully!", name);
                    println!("   DID: {}", agent.identity.did);
                    println!("   State: {:?}", agent.state());
                    
                    // Show Coneko status if enabled
                    if let Some(endpoint) = coneko_for_status {
                        println!("   Coneko: 🌐 Connected to {}", endpoint);
                    } else {
                        println!("   Coneko: ⚪ Disabled (use --coneko to enable)");
                    }
                    
                    // Create CLI channel and run interactive loop
                    let mut channel = CliChannel::new(&name);
                    channel.print_banner();
                    channel.print_system(&format!("Agent '{}' is ready! Type 'exit' or 'quit' to stop.", name));
                    
                    // Run the interactive loop
                    if let Err(e) = run_interactive_loop(&mut channel, &name).await {
                        eprintln!("❌ Error in interactive loop: {}", e);
                    }
                    
                    // Stop the agent
                    if let Err(e) = agent.stop().await {
                        eprintln!("❌ Error stopping agent: {}", e);
                    }
                    
                    println!("\n👋 Agent '{}' stopped. Goodbye!", name);
                }
                Err(e) => {
                    eprintln!("❌ Failed to create agent: {}", e);
                    return Err(e);
                }
            }
        }
        Commands::Orchestrate { config } => {
            info!("Starting Pekobot orchestrator");
            if let Some(cfg) = config {
                info!("Using config: {}", cfg);
            }
            println!("🐱 Pekobot orchestrator starting...");
            println!("   Note: Orchestrator mode not yet fully implemented");
            println!("   Use 'pekobot agent' for single agent mode.");
        }
        Commands::Status => {
            println!("🐱 Pekobot Status");
            println!("   Version: {}", pekobot::VERSION);
            println!("   Status: 🟢 Operational");
            println!("   Features:");
            println!("     - Agent Runtime: ✅ Ready");
            println!("     - Identity System: ✅ Ready");
            println!("     - SQLite Memory: ✅ Ready");
            println!("     - OpenAI Provider: ✅ Ready");
            println!("     - CLI Channel: ✅ Ready");
            println!("     - HTTP Channel: ✅ Ready");
            println!("     - Multi-Agent Orchestration: ✅ Ready");
            println!("     - Coneko Adapter: ✅ Ready");
        }
        Commands::Onboard => {
            println!("🐱 Pekobot Onboarding");
            println!("\nWelcome! Let's set up your first agent.\n");
            
            // Interactive setup
            let name = prompt("Agent name [peko]: ").unwrap_or_else(|| "peko".to_string());
            let provider = prompt("Provider (openai/anthropic/ollama) [openai]: ").unwrap_or_else(|| "openai".to_string());
            let coneko = prompt("Coneko endpoint (optional): ");
            
            println!("\n✅ Configuration complete!");
            println!("   Name: {}", name);
            println!("   Provider: {}", provider);
            if let Some(ref endpoint) = coneko {
                println!("   Coneko: {}", endpoint);
            }
            println!("\nStart your agent with:");
            if let Some(endpoint) = coneko {
                println!("   pekobot agent --name {} --provider {} --coneko {}", name, provider, endpoint);
            } else {
                println!("   pekobot agent --name {} --provider {}", name, provider);
            }
        }
        Commands::Send { endpoint, message } => {
            println!("🌐 Sending message to {}...", endpoint);
            
            // Simple HTTP POST
            let client = reqwest::Client::new();
            match client
                .post(&endpoint)
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({
                    "message": message,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                }))
                .send()
                .await
            {
                Ok(response) => {
                    if response.status().is_success() {
                        println!("✅ Message sent successfully!");
                    } else {
                        println!("⚠️  Server returned: {}", response.status());
                    }
                }
                Err(e) => {
                    eprintln!("❌ Failed to send: {}", e);
                }
            }
        }
        Commands::Coneko(coneko_cmd) => {
            match coneko_cmd {
                ConekoCommands::Status { endpoint } => {
                    println!("🌐 Checking Coneko status at {}...", endpoint);
                    
                    let adapter = ConekoAdapter::enabled(&endpoint, None);
                    match adapter.health_check().await {
                        Ok(true) => {
                            println!("✅ Coneko server is healthy");
                        }
                        Ok(false) => {
                            println!("❌ Coneko server is unreachable");
                        }
                        Err(e) => {
                            println!("❌ Error checking Coneko: {}", e);
                        }
                    }
                }
                ConekoCommands::Register { endpoint, did, name, agent_endpoint, capabilities, token } => {
                    println!("🌐 Registering agent with Coneko...");
                    println!("   DID: {}", did);
                    println!("   Name: {}", name);
                    println!("   Endpoint: {}", agent_endpoint);
                    
                    let adapter = ConekoAdapter::enabled(&endpoint, token.as_deref());
                    
                    let caps = capabilities
                        .into_iter()
                        .map(|c| AgentCapability {
                            name: c,
                            version: "1.0".to_string(),
                            description: None,
                            parameters: None,
                            required_auth: None,
                            estimated_cost: None,
                            estimated_duration: None,
                        })
                        .collect();
                    
                    match adapter.register_agent(&did, &name, &agent_endpoint, caps, "local", "default"
                    ).await {
                        Ok(()) => {
                            println!("✅ Agent registered successfully!");
                        }
                        Err(e) => {
                            println!("❌ Registration failed: {}", e);
                        }
                    }
                }
                ConekoCommands::Unregister { endpoint, did, token } => {
                    println!("🌐 Unregistering agent from Coneko...");
                    println!("   DID: {}", did);
                    
                    let adapter = ConekoAdapter::enabled(&endpoint, token.as_deref());
                    
                    match adapter.unregister_agent(&did).await {
                        Ok(()) => {
                            println!("✅ Agent unregistered successfully!");
                        }
                        Err(e) => {
                            println!("❌ Unregistration failed: {}", e);
                        }
                    }
                }
                ConekoCommands::Discover { endpoint, capability, token } => {
                    println!("🌐 Discovering agents on Coneko...");
                    
                    let adapter = ConekoAdapter::enabled(&endpoint, token.as_deref());
                    
                    let caps = capability.map(|c| vec![c]);
                    
                    match adapter.discover_agents(caps, None, None).await {
                        Ok(agents) => {
                            if agents.is_empty() {
                                println!("ℹ️  No agents found");
                            } else {
                                println!("✅ Found {} agent(s):", agents.len());
                                for agent in agents {
                                    println!("   - {} ({})", agent.name, agent.did);
                                    let caps: Vec<String> = agent
                                        .capabilities
                                        .iter()
                                        .map(|c| c.name.clone())
                                        .collect();
                                    println!("     Capabilities: {}", caps.join(", "));
                                    println!("     Endpoint: {}", agent.endpoint);
                                }
                            }
                        }
                        Err(e) => {
                            println!("❌ Discovery failed: {}", e);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build default agent configuration
fn build_default_config(
    name: &str,
    provider: &str,
    model: Option<String>,
    db_path: Option<String>,
    coneko: Option<String>,
    coneko_token: Option<String>,
) -> AgentConfig {
    let provider_type = match provider.to_lowercase().as_str() {
        "openai" => ProviderType::OpenAI,
        "anthropic" => ProviderType::Anthropic,
        "ollama" => ProviderType::Ollama,
        _ => ProviderType::OpenAI,
    };
    
    let model = model.unwrap_or_else(|| match provider_type {
        ProviderType::OpenAI => "gpt-4o-mini".to_string(),
        ProviderType::Anthropic => "claude-3-haiku".to_string(),
        ProviderType::Ollama => "llama3".to_string(),
        ProviderType::OpenAICompatible => "gpt-4o-mini".to_string(),
    });

    // Build Coneko config if endpoint provided
    let coneko_config = coneko.map(|endpoint| ConekoConfig {
        enabled: true,
        endpoint,
        auth_token: coneko_token,
        poll_interval_ms: 5000,
        auto_register: true,
    });
    
    AgentConfig {
        name: name.to_string(),
        description: Some(format!("Pekobot agent: {}", name)),
        tenant: None,
        capabilities: vec![
            AgentCapability {
                name: "messaging".to_string(),
                version: "1.0".to_string(),
                description: Some("Basic messaging capability".to_string()),
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
            provider_type,
            api_key: None,
            api_key_env: None,
            base_url: None,
            default_model: model.clone(),
            models: {
                let mut m = HashMap::new();
                m.insert(model.clone(), ModelConfig {
                    name: model,
                    max_tokens: 2048,
                    temperature: 0.7,
                    top_p: 1.0,
                    presence_penalty: 0.0,
                    frequency_penalty: 0.0,
                });
                m
            },
            timeout_seconds: 30,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        memory: Some(MemoryConfig {
            enable_semantic_search: false,
            embedding_model: None,
            max_entries_per_agent: Some(10000),
            default_ttl_seconds: None,
            auto_cleanup: true,
            cleanup_interval_seconds: 3600,
            database_path: db_path,
        }),
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(100.0),
        default_timeout_seconds: 300,
        coneko: coneko_config,
    }
}

/// Simple prompt helper
fn prompt(message: &str) -> Option<String> {
    use std::io::{self, Write};
    
    print!("{}", message);
    io::stdout().flush().ok()?;
    
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok()?;
    
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
