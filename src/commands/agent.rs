//! Agent Management Commands
#![allow(dead_code)]

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Agent management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AgentCommands {
    /// Run an agent interactively
    #[command(alias = "run")]
    Start {
        /// Agent name (defaults to 'peko' if not specified)
        name: Option<String>,
        /// Custom configuration file path (optional, defaults to ~/.config/pekobot/agents/{name}.toml)
        #[arg(short, long)]
        config: Option<String>,
        /// LLM provider (openai, anthropic, ollama, kimi) - only used when creating default config
        #[arg(short, long, default_value = "openai")]
        provider: String,
        /// Model name - only used when creating default config
        #[arg(short, long)]
        model: Option<String>,
        /// Database path for memory
        #[arg(long)]
        db: Option<String>,
        /// Send a single message and exit (non-interactive mode)
        #[arg(short = 'M', long)]
        message: Option<String>,
    },

    /// List all configured agents
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Show detailed agent information
    Show {
        /// Agent name
        name: String,
    },

    /// Create a new agent from template
    Create {
        /// Agent name
        name: String,
        /// Use template (minimal, coding, research, full)
        #[arg(short, long, default_value = "minimal")]
        template: String,
        /// Provider to use
        #[arg(short, long, default_value = "openai")]
        provider: String,
        /// Skip confirmation
        #[arg(short, long)]
        yes: bool,
    },

    /// Delete an agent and its configuration
    Delete {
        /// Agent name
        name: String,
        /// Also delete identity
        #[arg(long)]
        purge: bool,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Export agent to .agent package
    Export {
        /// Agent name
        #[arg(short, long)]
        name: String,
        /// Output path
        #[arg(short, long)]
        output: Option<String>,
        /// Encrypt with passphrase
        #[arg(long)]
        encrypt: bool,
    },

    /// Import agent from .agent package
    Import {
        /// Path to .agent file
        #[arg(short, long)]
        file: String,
        /// New name for imported agent
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Inspect .agent package without importing
    Inspect {
        /// Path to .agent file
        file: String,
    },
}

/// Handle agent commands
pub async fn handle_agent(
    cmd: AgentCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        AgentCommands::Start {
            name,
            config,
            provider,
            model,
            db,
            message,
        } => {
            crate::commands::agent::handlers::handle_agent_start(name, config, provider, model, db, message)
                .await
        }
        AgentCommands::List { long } => {
            crate::commands::agent::handlers::handle_agent_list(paths, long, json).await
        }
        AgentCommands::Show { name } => {
            crate::commands::agent::handlers::handle_agent_show(paths, name, json).await
        }
        AgentCommands::Create {
            name,
            template,
            provider,
            yes,
        } => {
            crate::commands::agent::handlers::handle_agent_create(
                paths, name, template, provider, yes,
            )
            .await
        }
        AgentCommands::Delete { name, purge, force } => {
            crate::commands::agent::handlers::handle_agent_delete(paths, name, purge, force).await
        }
        AgentCommands::Export {
            name,
            output,
            encrypt,
        } => {
            crate::commands::agent::handlers::handle_agent_export(paths, name, output, encrypt)
                .await
        }
        AgentCommands::Import { file, name } => {
            crate::commands::agent::handlers::handle_agent_import(paths, file, name).await
        }
        AgentCommands::Inspect { file } => {
            crate::commands::agent::handlers::handle_agent_inspect(file, json).await
        }
    }
}

/// Agent command handlers
pub mod handlers {
    use crate::agent::Agent;
    use crate::channels::cli::{run_interactive_loop_with_agent, CliChannel};
    use crate::commands::GlobalPaths;
    use crate::types::agent::AgentConfig;
    use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
    use std::io::{self, Write};
    use tracing::{info, warn};

    /// Handle agent start command
    pub async fn handle_agent_start(
        name: Option<String>,
        config_path: Option<String>,
        provider: String,
        model: Option<String>,
        db: Option<String>,
        message: Option<String>,
    ) -> anyhow::Result<()> {
        // Determine agent name (default to "peko")
        let agent_name = name.unwrap_or_else(|| "peko".to_string());
        
        // Determine config path
        let config_path = if let Some(path) = config_path {
            path
        } else {
            // Look up in default location: ~/.config/pekobot/agents/{name}.toml
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{}/.config/pekobot/agents/{}.toml", home, agent_name)
        };
        
        info!("Starting Pekobot agent: {}", agent_name);
        info!("Loading config from: {}", config_path);

        let agent_config = if std::path::Path::new(&config_path).exists() {
            let content = std::fs::read_to_string(&config_path)?;
            toml::from_str(&content)?
        } else {
            warn!("Config file not found: {}", config_path);
            warn!("Using default configuration");
            build_default_config(&agent_name, &provider, model, db)
        };

        match Agent::new(agent_config).await {
            Ok(agent) => {
                if let Err(e) = agent.start().await {
                    eprintln!("❌ Failed to start agent: {e}");
                    return Err(e);
                }

                println!("\n🐱 Agent '{}' started successfully!", agent_name);
                println!("   DID: {}", agent.identity.did);
                println!("   State: {:?}", agent.state());

                // Check if we're in single-message mode
                if let Some(msg) = message {
                    // Single message mode
                    println!();
                    if let Err(e) = crate::channels::cli::send_single_message(&agent, &msg
                    ).await {
                        eprintln!("❌ Error processing message: {e}");
                    }
                } else {
                    // Interactive mode
                    let mut channel = CliChannel::new(&agent_name);
                    
                    if let Err(e) = run_interactive_loop_with_agent(&mut channel, &agent_name, &agent
                    ).await {
                        eprintln!("❌ Error in interactive loop: {e}");
                    }
                }

                if let Err(e) = agent.stop().await {
                    eprintln!("❌ Error stopping agent: {e}");
                }

                println!("\n👋 Agent '{}' stopped. Goodbye!", agent_name);
                Ok(())
            }
            Err(e) => {
                eprintln!("❌ Failed to create agent: {e}");
                Err(e)
            }
        }
    }

    /// Handle agent list command
    pub async fn handle_agent_list(
        paths: &GlobalPaths,
        long: bool,
        json: bool,
    ) -> anyhow::Result<()> {
        let agents_dir = paths.agents_dir();

        if !agents_dir.exists() {
            if json {
                println!("{{\"agents\": []}}");
            } else {
                println!("No agents configured.");
                println!("Create one with: pekobot agent create <name>");
            }
            return Ok(());
        }

        let mut agents = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&agents_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.path().file_stem() {
                    let name = name.to_string_lossy().to_string();
                    agents.push(name);
                }
            }
        }

        agents.sort();

        if json {
            let output = serde_json::json!({"agents": agents});
            println!("{output}");
        } else if agents.is_empty() {
            println!("No agents configured.");
            println!("Create one with: pekobot agent create <name>");
        } else {
            println!("🐱 Configured Agents ({}):", agents.len());
            for name in agents {
                let config_path = paths.agent_config(&name);
                if long {
                    if let Ok(content) = std::fs::read_to_string(&config_path) {
                        if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                            println!("\n  📦 {name}");
                            println!("     Provider: {:?}", config.provider.provider_type);
                            println!("     Model: {}", config.provider.default_model);
                            if let Some(desc) = &config.description {
                                println!("     Description: {desc}");
                            }
                        } else {
                            println!("  📦 {name} (parse error)");
                        }
                    } else {
                        println!("  📦 {name} (no config)");
                    }
                } else {
                    println!("  📦 {name}");
                }
            }
        }

        Ok(())
    }

    /// Handle agent show command
    pub async fn handle_agent_show(
        paths: &GlobalPaths,
        name: String,
        json: bool,
    ) -> anyhow::Result<()> {
        let config_path = paths.agent_config(&name);

        if !config_path.exists() {
            eprintln!("❌ Agent '{name}' not found");
            return Ok(());
        }

        let content = std::fs::read_to_string(&config_path)?;
        let config: AgentConfig = toml::from_str(&content)?;

        if json {
            let output = serde_json::json!({
                "name": name,
                "config": config,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("📦 Agent: {name}");
            println!("   Config: {}", config_path.display());
            println!("   Provider: {:?}", config.provider.provider_type);
            println!("   Model: {}", config.provider.default_model);
            if let Some(desc) = &config.description {
                println!("   Description: {desc}");
            }
        }

        Ok(())
    }

    /// Handle agent create command with bootstrapping and auto-detected credentials
    pub async fn handle_agent_create(
        paths: &GlobalPaths,
        name: String,
        template: String,
        provider: String,
        yes: bool,
    ) -> anyhow::Result<()> {
        let config_path = paths.agent_config(&name);

        if config_path.exists() && !yes {
            print!("Agent '{name}' already exists. Overwrite? [y/N] ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return Ok(());
            }
        }

        // Auto-detect available providers from stored credentials
        let available_providers =
            crate::commands::auth::detect_available_providers(paths).unwrap_or_default();

        // Determine the best provider to use
        let selected_provider = if available_providers.contains(&provider) {
            provider.clone()
        } else if !available_providers.is_empty() && provider == "openai" {
            // Default to first available if openai requested but not configured
            let first = available_providers[0].clone();
            println!("⚠️  '{provider}' not configured. Using '{first}' instead.");
            first
        } else {
            provider.clone()
        };

        // Build config with stored credentials if available
        let config = build_config_with_auth(paths, &name, &selected_provider, None, None).await?;
        let toml = toml::to_string_pretty(&config)?;

        std::fs::create_dir_all(paths.agents_dir())?;
        std::fs::write(&config_path, toml)?;

        println!("✅ Created agent '{name}'");
        println!("   Provider: {selected_provider}");
        println!("   Config: {}", config_path.display());

        // Show warning if no API keys configured
        if available_providers.is_empty() {
            println!();
            println!("⚠️  No API keys configured!");
            println!("   Set one with: pekobot auth set <provider> <key>");
            println!("   Or export: export OPENAI_API_KEY='your-key'");
        }

        // Bootstrap workspace with OpenClaw-style files
        let workspace_dir = paths.data_dir.join("workspaces").join(&name);
        if let Err(e) =
            crate::commands::agent_bootstrap::bootstrap_agent_workspace(&name, &workspace_dir)
        {
            eprintln!("⚠️  Warning: Failed to bootstrap workspace: {e}");
        }

        Ok(())
    }

    /// Handle agent delete command
    pub async fn handle_agent_delete(
        paths: &GlobalPaths,
        name: String,
        purge: bool,
        force: bool,
    ) -> anyhow::Result<()> {
        let config_path = paths.agent_config(&name);

        if !config_path.exists() {
            eprintln!("❌ Agent '{name}' not found");
            return Ok(());
        }

        if !force {
            print!("Delete agent '{name}'? This cannot be undone. [y/N] ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if !input.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return Ok(());
            }
        }

        std::fs::remove_file(&config_path)?;

        if purge {
            // Also delete identity if requested
            println!("🗑️  Purged identity for '{name}'");
        }

        println!("✅ Deleted agent '{name}'");
        Ok(())
    }

    /// Handle agent export command
    pub async fn handle_agent_export(
        paths: &GlobalPaths,
        name: String,
        output: Option<String>,
        encrypt: bool,
    ) -> anyhow::Result<()> {
        let config_path = paths.agent_config(&name);

        if !config_path.exists() {
            eprintln!("❌ Agent '{name}' not found");
            return Ok(());
        }

        let output_path = output.unwrap_or_else(|| format!("{name}.agent"));

        println!("📦 Exporting agent '{name}' to '{output_path}'...");

        if encrypt {
            println!("🔐 Encryption enabled (not yet implemented)");
        }

        // TODO: Implement actual export via Packager
        println!("✅ Exported agent '{name}'");

        Ok(())
    }

    /// Handle agent import command
    pub async fn handle_agent_import(
        _paths: &GlobalPaths,
        file: String,
        name: Option<String>,
    ) -> anyhow::Result<()> {
        if !std::path::Path::new(&file).exists() {
            eprintln!("❌ File not found: {file}");
            return Ok(());
        }

        let agent_name = name.unwrap_or_else(|| {
            std::path::Path::new(&file).file_stem().map_or_else(
                || "imported".to_string(),
                |s| s.to_string_lossy().to_string(),
            )
        });

        println!("📥 Importing '{file}' as '{agent_name}'...");

        // TODO: Implement actual import via Unpackager
        println!("✅ Imported agent '{agent_name}'");

        Ok(())
    }

    /// Handle agent inspect command
    pub async fn handle_agent_inspect(file: String, _json: bool) -> anyhow::Result<()> {
        if !std::path::Path::new(&file).exists() {
            eprintln!("❌ File not found: {file}");
            return Ok(());
        }

        println!("🔍 Inspecting '{file}'...");

        // TODO: Implement actual inspection via Unpackager
        println!("Package format: .agent");

        Ok(())
    }

    /// Build default agent config
    fn build_default_config(
        name: &str,
        provider: &str,
        model: Option<String>,
        _db: Option<String>,
    ) -> AgentConfig {
        use std::collections::HashMap;

        let provider_type = match provider.to_lowercase().as_str() {
            "openai" => ProviderType::OpenAI,
            "anthropic" => ProviderType::Anthropic,
            "ollama" => ProviderType::Ollama,
            "kimi" => ProviderType::Kimi,
            "kimi_code" => ProviderType::KimiCode,
            "kimi-code" => ProviderType::KimiCode,
            _ => ProviderType::OpenAI,
        };

        let default_model = model.unwrap_or_else(|| "default".to_string());

        let mut models = HashMap::new();
        models.insert(
            "default".to_string(),
            ModelConfig {
                name: match provider_type {
                    ProviderType::OpenAI => "gpt-4o-mini".to_string(),
                    ProviderType::Anthropic => "claude-3-sonnet".to_string(),
                    ProviderType::Ollama => "llama3.2".to_string(),
                    ProviderType::OpenAICompatible => "default".to_string(),
                    ProviderType::Kimi => "kimi-k2.5".to_string(),
                    ProviderType::KimiCode => "k2p5".to_string(),
                },
                max_tokens: 4096,
                temperature: 0.7,
                top_p: 1.0,
                presence_penalty: 0.0,
                frequency_penalty: 0.0,
            },
        );

        AgentConfig {
            name: name.to_string(),
            description: Some(format!("Pekobot agent: {name}")),
            tenant: None,
            capabilities: vec![],
            provider: ProviderConfig {
                provider_type,
                api_key: None,
                api_key_env: match provider_type {
                    ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
                    ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
                    ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
                    ProviderType::KimiCode => Some("KIMI_API_KEY".to_string()),
                    _ => None,
                },
                base_url: match provider_type {
                    ProviderType::OpenAI => None,
                    ProviderType::Anthropic => None,
                    ProviderType::Ollama => Some("http://localhost:11434".to_string()),
                    ProviderType::OpenAICompatible => None,
                    ProviderType::Kimi => Some("https://api.moonshot.cn/v1".to_string()),
                    ProviderType::KimiCode => Some("https://api.kimi.com/coding".to_string()),
                },
                default_model,
                models,
                timeout_seconds: 60,
                max_retries: 3,
                retry_delay_ms: 1000,
            },
            memory: None,
            tools: None,
            channels: None,
            auto_accept_trusted: false,
            approval_threshold: Some(100.0),
            default_timeout_seconds: 300,
            workspace: None,
            prompt: None,
        }
    }

    /// Build config with stored credentials if available
    async fn build_config_with_auth(
        paths: &GlobalPaths,
        name: &str,
        provider: &str,
        model: Option<String>,
        db: Option<String>,
    ) -> anyhow::Result<AgentConfig> {
        let mut config = build_default_config(name, provider, model, db);

        // Try to get stored API key
        if let Some(api_key) = crate::commands::auth::get_api_key(paths, provider, None)? {
            config.provider.api_key = Some(api_key);
            config.provider.api_key_env = None; // Use direct key instead of env
        }

        Ok(config)
    }
}
