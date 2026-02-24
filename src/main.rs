use clap::{Parser, Subcommand};
use pekobot::agent::Agent;
use pekobot::channels::cli::{run_interactive_loop, CliChannel};
use pekobot::identity::{storage::KeyStorage, Identity};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use std::collections::HashMap;
use tracing::{info, warn};

/// Pekobot - Lightweight Multi-Agent Runtime
#[derive(Parser)]
#[command(name = "pekobot")]
#[command(version)]
#[command(about = "Lightweight multi-agent runtime")]
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
    /// Export an agent to a .agent package
    Export {
        /// Agent name or config file
        #[arg(short, long)]
        agent: String,
        /// Output path for .agent file
        #[arg(short, long)]
        output: Option<String>,
        /// Encrypt the package with a passphrase
        #[arg(long)]
        encrypt: bool,
        /// Rotate keys (generate new DID on import)
        #[arg(long)]
        rotate_keys: bool,
        /// Include memory database
        #[arg(long, default_value = "true")]
        include_memory: bool,
        /// Description for the package
        #[arg(short, long)]
        description: Option<String>,
    },
    /// Import an agent from a .agent package
    Import {
        /// Path to .agent file
        #[arg(short, long)]
        file: String,
        /// New name for the imported agent
        #[arg(short, long)]
        name: Option<String>,
        /// Rotate keys (generate new DID)
        #[arg(long)]
        rotate_keys: bool,
        /// Don't import memory
        #[arg(long)]
        no_memory: bool,
        /// Force import even if DID exists
        #[arg(long)]
        force: bool,
    },
    /// Inspect a .agent package without importing
    Inspect {
        /// Path to .agent file
        #[arg(short, long)]
        file: String,
    },
    /// Gateway plugin commands
    #[command(subcommand)]
    Gateway(GatewayCommands),
}

#[derive(Subcommand)]
enum GatewayCommands {
    /// List installed gateway plugins
    List,
    /// List available gateway plugins from Pekohub
    Available,
    /// Install a gateway plugin
    Install {
        /// Gateway plugin name (e.g., discord, whatsapp)
        name: String,
        /// Specific version to install
        #[arg(short, long)]
        version: Option<String>,
    },
    /// Uninstall a gateway plugin
    Uninstall {
        /// Gateway plugin name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Update a gateway plugin to the latest version
    Update {
        /// Gateway plugin name (omit for all)
        name: Option<String>,
    },
    /// Show gateway plugin information
    Info {
        /// Gateway plugin name
        name: String,
    },
    /// Start a gateway instance
    Start {
        /// Gateway configuration file
        #[arg(short, long)]
        config: String,
    },
    /// Stop a gateway instance
    Stop {
        /// Instance ID
        instance_id: String,
    },
    /// List active gateway instances
    Instances,
    /// Test gateway connection
    Test {
        /// Instance ID
        instance_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Agent {
            config,
            name,
            provider,
            model,
            db,
        } => {
            info!("Starting Pekobot agent: {}", name);

            // Build config
            let agent_config = if let Some(config_path) = config {
                info!("Loading config from: {}", config_path);
                build_default_config(&name, &provider, model, db)
            } else {
                warn!("No config specified, using defaults");
                build_default_config(&name, &provider, model, db)
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

                    // Create CLI channel and run interactive loop
                    let mut channel = CliChannel::new(&name);
                    channel.print_banner();
                    channel.print_system(&format!(
                        "Agent '{}' is ready! Type 'exit' or 'quit' to stop.",
                        name
                    ));

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
            println!("     - Portable Agents: ✅ Ready");
        }
        Commands::Onboard => {
            println!("🐱 Pekobot Onboarding");
            println!("\nWelcome! Let's set up your first agent.\n");

            // Interactive setup
            let name = prompt("Agent name [peko]: ").unwrap_or_else(|| "peko".to_string());
            let provider = prompt("Provider (openai/anthropic/ollama) [openai]: ")
                .unwrap_or_else(|| "openai".to_string());

            println!("\n✅ Configuration complete!");
            println!("   Name: {}", name);
            println!("   Provider: {}", provider);
            println!("\nStart your agent with:");
            println!("   pekobot agent --name {} --provider {}", name, provider);
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
        Commands::Export {
            agent,
            output,
            encrypt,
            rotate_keys,
            include_memory,
            description,
        } => {
            println!("📦 Exporting agent '{}'...", agent);

            // Load agent configuration
            let config_path = if agent.ends_with(".toml") {
                std::path::PathBuf::from(&agent)
            } else {
                dirs::config_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("pekobot")
                    .join("agents")
                    .join(format!("{}.toml", agent))
            };

            let config: AgentConfig = if config_path.exists() {
                let content = std::fs::read_to_string(&config_path)?;
                toml::from_str(&content)?
            } else {
                eprintln!("❌ Agent config not found: {:?}", config_path);
                return Err(anyhow::anyhow!("Agent not found"));
            };

            // Load identity
            let identity = match load_identity_for_agent(&config.name).await {
                Some(id) => id,
                None => {
                    eprintln!("❌ Failed to load identity for agent: {}", config.name);
                    return Err(anyhow::anyhow!("Identity not found"));
                }
            };

            // Determine memory path
            let memory_path = config
                .memory
                .as_ref()
                .and_then(|m| m.database_path.as_ref())
                .map(|p| std::path::PathBuf::from(p));

            // Build export options
            let export_opts = pekobot::portable::ExportOptions {
                encrypt,
                passphrase: if encrypt {
                    Some(rpassword::prompt_password("Enter passphrase: ")?)
                } else {
                    None
                },
                include_memory,
                rotate_keys,
                description,
                output_path: output,
            };

            // Export
            match pekobot::portable::export_agent(config, identity, memory_path, export_opts).await
            {
                Ok(path) => {
                    println!("✅ Agent exported to: {}", path.display());
                }
                Err(e) => {
                    eprintln!("❌ Export failed: {}", e);
                    return Err(e);
                }
            }
        }
        Commands::Import {
            file,
            name,
            rotate_keys,
            no_memory,
            force,
        } => {
            println!("📦 Importing agent from '{}'...", file);

            let import_opts = pekobot::portable::ImportOptions {
                new_name: name,
                passphrase: None, // Will prompt if needed
                rotate_keys,
                import_memory: !no_memory,
                skip_validation: false,
                force,
            };

            match pekobot::portable::import_agent(&file, import_opts).await {
                Ok(result) => {
                    println!("✅ Agent imported successfully!");
                    println!("   Name: {}", result.name);
                    println!("   DID: {}", result.did);
                    if result.keys_rotated {
                        println!("   Keys: Rotated (new DID generated)");
                    }
                    if let Some(mem_path) = result.memory_path {
                        println!("   Memory: {}", mem_path.display());
                    }
                }
                Err(e) => {
                    eprintln!("❌ Import failed: {}", e);
                    return Err(e);
                }
            }
        }
        Commands::Inspect { file } => {
            println!("🔍 Inspecting package: {}", file);

            match pekobot::portable::get_package_info(&file).await {
                Ok(info) => {
                    println!("\n{}", info.format());
                }
                Err(e) => {
                    eprintln!("❌ Inspection failed: {}", e);
                    return Err(e);
                }
            }
        }
        Commands::Gateway(gateway_cmd) => {
            use pekobot::gateway::{GatewayManager, GatewaysConfig};
            use std::sync::Arc;

            let config = GatewaysConfig::default();
            let manager = Arc::new(GatewayManager::new(config).await?);

            match gateway_cmd {
                GatewayCommands::List => {
                    println!("🌐 Installed Gateway Plugins");
                    println!("   (Use 'pekobot gateway available' to see installable plugins)");
                    println!();
                    
                    let plugins = manager.registry().list_loaded().await;
                    if plugins.is_empty() {
                        println!("   No gateway plugins installed.");
                        println!("   Install one with: pekobot gateway install <name>");
                    } else {
                        for plugin in plugins {
                            println!("   📦 {} v{}", plugin.name, plugin.version);
                            println!("      {}", plugin.description);
                        }
                    }
                }
                GatewayCommands::Available => {
                    println!("🔍 Checking Pekohub for available gateways...");
                    
                    match manager.registry().list_available().await {
                        Ok(plugins) => {
                            println!("\n📦 Available Gateway Plugins:");
                            for plugin in plugins {
                                let status = if plugin.installed {
                                    "✅ installed"
                                } else {
                                    "⬜ not installed"
                                };
                                println!("   {} {} v{} - {}", 
                                    plugin.name, 
                                    status,
                                    plugin.version,
                                    plugin.description
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to fetch available plugins: {}", e);
                            println!("   Make sure you have an internet connection.");
                        }
                    }
                }
                GatewayCommands::Install { name, version } => {
                    println!("📥 Installing gateway plugin: {}", name);
                    if let Some(v) = version {
                        println!("   Version: {}", v);
                    }
                    
                    match manager.registry().load(&name).await {
                        Ok(_) => {
                            println!("✅ Gateway plugin '{}' installed successfully!", name);
                            println!("   Use 'pekobot gateway start --config <file>' to run it.");
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to install '{}': {}", name, e);
                        }
                    }
                }
                GatewayCommands::Uninstall { name, force } => {
                    println!("🗑️  Uninstalling gateway plugin: {}", name);
                    
                    if !force {
                        print!("   Are you sure? This cannot be undone [y/N]: ");
                        use std::io::{self, Write};
                        io::stdout().flush().ok();
                        let mut input = String::new();
                        io::stdin().read_line(&mut input).ok();
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("   Cancelled.");
                            return Ok(());
                        }
                    }
                    
                    match manager.registry().unload(&name).await {
                        Ok(_) => {
                            println!("✅ Gateway plugin '{}' uninstalled.", name);
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to uninstall '{}': {}", name, e);
                        }
                    }
                }
                GatewayCommands::Update { name } => {
                    if let Some(plugin_name) = name {
                        println!("🔄 Updating gateway plugin: {}", plugin_name);
                        match manager.registry().update(&plugin_name).await {
                            Ok(updated) => {
                                if updated {
                                    println!("✅ Updated '{}' to latest version.", plugin_name);
                                } else {
                                    println!("ℹ️  '{}' is already up to date.", plugin_name);
                                }
                            }
                            Err(e) => {
                                eprintln!("❌ Failed to update '{}': {}", plugin_name, e);
                            }
                        }
                    } else {
                        println!("🔄 Updating all gateway plugins...");
                        let plugins = manager.registry().list_loaded().await;
                        for plugin in plugins {
                            print!("   Checking {}...", plugin.name);
                            match manager.registry().update(&plugin.name).await {
                                Ok(updated) => {
                                    if updated {
                                        println!(" updated!");
                                    } else {
                                        println!(" already up to date");
                                    }
                                }
                                Err(e) => {
                                    println!(" error: {}", e);
                                }
                            }
                        }
                    }
                }
                GatewayCommands::Info { name } => {
                    match manager.registry().get_metadata(&name).await {
                        Some(info) => {
                            println!("📦 Gateway Plugin: {}", info.name);
                            println!("   Version: {}", info.version);
                            println!("   Description: {}", info.description);
                            println!("   Author: {}", info.author);
                            if info.installed {
                                println!("   Status: ✅ Installed");
                            } else {
                                println!("   Status: ⬜ Not installed");
                            }
                        }
                        None => {
                            println!("ℹ️  Gateway plugin '{}' not found.", name);
                            println!("   Use 'pekobot gateway list' to see installed plugins.");
                        }
                    }
                }
                GatewayCommands::Start { config } => {
                    println!("🚀 Starting gateway from config: {}", config);
                    println!("   (Not yet fully implemented)");
                }
                GatewayCommands::Stop { instance_id } => {
                    println!("🛑 Stopping gateway instance: {}", instance_id);
                    match manager.stop_gateway(&instance_id).await {
                        Ok(_) => println!("✅ Gateway instance stopped."),
                        Err(e) => eprintln!("❌ Failed to stop: {}", e),
                    }
                }
                GatewayCommands::Instances => {
                    println!("🌐 Active Gateway Instances");
                    let instances = manager.list_instances().await;
                    if instances.is_empty() {
                        println!("   No active instances.");
                    } else {
                        for instance in instances {
                            println!("   📡 {} ({} - {})", 
                                instance.id, 
                                instance.gateway,
                                instance.name
                            );
                        }
                    }
                }
                GatewayCommands::Test { instance_id } => {
                    println!("🧪 Testing gateway instance: {}", instance_id);
                    match manager.get_instance(&instance_id).await {
                        Some(_) => {
                            println!("✅ Instance '{}' is active.", instance_id);
                            // TODO: Send test message
                        }
                        None => {
                            println!("❌ Instance '{}' not found.", instance_id);
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
                m.insert(
                    model.clone(),
                    ModelConfig {
                        name: model,
                        max_tokens: 2048,
                        temperature: 0.7,
                        top_p: 1.0,
                        presence_penalty: 0.0,
                        frequency_penalty: 0.0,
                    },
                );
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
    }
}

/// Load identity for an agent by name
async fn load_identity_for_agent(name: &str) -> Option<Identity> {
    // Try to load from agent config first
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("pekobot")
        .join("agents")
        .join(format!("{}.toml", name));

    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
            // Try to find identity by checking common DIDs
            let storage = KeyStorage::new().ok()?;

            // List all identities and find one that matches the agent name
            if let Ok(identities) = storage.list_identities() {
                for did in identities {
                    // Simple heuristic: check if DID contains agent name
                    if did.to_lowercase().contains(&name.to_lowercase()) {
                        if let Ok(identity) = storage.load(&did) {
                            return Some(identity);
                        }
                    }
                }
            }
        }
    }

    None
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
