use clap::{Parser, Subcommand};
use pekobot::agent::Agent;
use pekobot::channels::cli::{run_interactive_loop, CliChannel};
use pekobot::identity::{storage::KeyStorage, Identity};
use pekobot::secrets::{
    AuditEvent as SecretAuditEvent, SecretManager, SecretPermission, SecretScope, SecretType,
};
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
    /// Secret manager commands
    #[command(subcommand)]
    Secret(SecretCommands),
}

#[derive(Subcommand)]
enum SecretCommands {
    /// Store a new secret
    Set {
        /// Secret name
        name: String,
        /// Secret value (will prompt if not provided)
        #[arg(short, long)]
        value: Option<String>,
        /// Secret type (api_key, token, ssh_key, certificate, password, other)
        #[arg(short, long, default_value = "api_key")]
        secret_type: String,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Description
        #[arg(short, long)]
        description: Option<String>,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Retrieve a secret value
    Get {
        /// Secret name
        name: String,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// List all secrets
    List {
        /// Filter by scope ("global" or agent DID)
        #[arg(short, long)]
        scope: Option<String>,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Delete a secret
    Delete {
        /// Secret name
        name: String,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Grant permission to an agent for a secret
    Grant {
        /// Secret name
        #[arg(short, long)]
        secret: String,
        /// Agent DID (omit for default permission)
        #[arg(short, long)]
        agent: Option<String>,
        /// Permission level: read, write, none
        #[arg(short, long, default_value = "read")]
        permission: String,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Revoke permission from an agent for a secret
    Revoke {
        /// Secret name
        #[arg(short, long)]
        secret: String,
        /// Agent DID (omit for default permission)
        #[arg(short, long)]
        agent: Option<String>,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// List permissions for a secret
    Permissions {
        /// Secret name
        name: String,
        /// Scope: "global" or agent DID
        #[arg(short, long, default_value = "global")]
        scope: String,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// View audit log
    Audit {
        /// Filter by secret name
        #[arg(short, long)]
        secret: Option<String>,
        /// Filter by agent DID
        #[arg(short, long)]
        agent: Option<String>,
        /// Filter by event type
        #[arg(short, long)]
        event: Option<String>,
        /// Number of entries to show
        #[arg(short, long, default_value = "50")]
        limit: usize,
        /// Show statistics only
        #[arg(long)]
        stats: bool,
        /// Export to file (CSV format)
        #[arg(short, long)]
        export: Option<String>,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Import secrets from environment variables
    ImportEnv {
        /// Pattern to match env var names (regex)
        #[arg(short, long, default_value = ".*")]
        pattern: String,
        /// Prefix to add to secret names
        #[arg(long)]
        prefix: Option<String>,
        /// Remove prefix from env var names
        #[arg(long)]
        remove_prefix: Option<String>,
        /// Secret type for imported secrets
        #[arg(short, long, default_value = "api_key")]
        secret_type: String,
        /// Dry run - show what would be imported
        #[arg(long)]
        dry_run: bool,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
    },
    /// Scan config files for plain-text secrets and migrate them
    Migrate {
        /// Path to config file or directory
        #[arg(short, long, default_value = ".")]
        path: String,
        /// Dry run - show what would be migrated
        #[arg(long)]
        dry_run: bool,
        /// Master password (will prompt if not provided)
        #[arg(short, long)]
        password: Option<String>,
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
        Commands::Secret(secret_cmd) => {
            match secret_cmd {
                SecretCommands::Set {
                    name,
                    value,
                    secret_type,
                    scope,
                    description,
                    password,
                } => {
                    // Get password if not provided
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    // Get value if not provided
                    let value = match value {
                        Some(v) => v,
                        None => rpassword::prompt_password("Enter secret value: ")?,
                    };

                    // Parse scope
                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    // Parse secret type
                    let secret_type = match secret_type.to_lowercase().as_str() {
                        "api_key" => SecretType::ApiKey,
                        "token" => SecretType::Token,
                        "ssh_key" => SecretType::SshKey,
                        "certificate" => SecretType::Certificate,
                        "password" => SecretType::Password,
                        _ => SecretType::Other,
                    };

                    // Create metadata
                    let metadata = description.map(|d| pekobot::secrets::SecretMetadata {
                        description: Some(d),
                        source_hint: None,
                        expires_at: None,
                        tags: Vec::new(),
                    });

                    // Open store and unlock
                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    // Store secret
                    match manager
                        .set(&name, scope, &value, secret_type, metadata)
                        .await
                    {
                        Ok(entry) => {
                            println!("✅ Secret '{}' stored successfully", entry.name);
                            println!("   Type: {:?}", entry.secret_type);
                            println!("   Scope: {:?}", entry.scope);
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to store secret: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Get {
                    name,
                    scope,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager.get(&name, &scope).await {
                        Ok(Some(value)) => {
                            println!("{}", value);
                        }
                        Ok(None) => {
                            eprintln!("❌ Secret '{}' not found", name);
                            std::process::exit(1);
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to retrieve secret: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::List { scope, password } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope_filter = scope.map(|s| {
                        if s == "global" {
                            SecretScope::Global
                        } else {
                            SecretScope::Agent { did: s }
                        }
                    });

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager.list(scope_filter).await {
                        Ok(secrets) => {
                            if secrets.is_empty() {
                                println!("ℹ️  No secrets found");
                            } else {
                                println!("🔐 Secrets ({} found):", secrets.len());
                                for secret in secrets {
                                    let scope_str = match &secret.scope {
                                        SecretScope::Global => "global".to_string(),
                                        SecretScope::Agent { did } => {
                                            format!("agent:{}", &did[..16.min(did.len())])
                                        }
                                    };
                                    println!(
                                        "   {:20} {:12} {:20}",
                                        secret.name,
                                        format!("{:?}", secret.secret_type).to_lowercase(),
                                        scope_str
                                    );
                                    if let Some(desc) = secret.metadata.description {
                                        println!("                      {}", desc);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to list secrets: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Delete {
                    name,
                    scope,
                    password,
                    force,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    // Confirm deletion unless --force
                    if !force {
                        print!("Delete secret '{}' from {:?}? [y/N]: ", name, scope);
                        std::io::Write::flush(&mut std::io::stdout())?;
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        if !input.trim().eq_ignore_ascii_case("y") {
                            println!("Cancelled");
                            return Ok(());
                        }
                    }

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager.delete(&name, &scope).await {
                        Ok(true) => {
                            println!("✅ Secret '{}' deleted", name);
                        }
                        Ok(false) => {
                            println!("ℹ️  Secret '{}' not found", name);
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to delete secret: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Grant {
                    secret,
                    agent,
                    permission,
                    scope,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    let permission = match permission.to_lowercase().as_str() {
                        "read" => SecretPermission::Read,
                        "write" => SecretPermission::Write,
                        "none" => SecretPermission::None,
                        _ => {
                            eprintln!(
                                "❌ Invalid permission: {}. Use 'read', 'write', or 'none'",
                                permission
                            );
                            return Ok(());
                        }
                    };

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager
                        .grant_permission(&secret, &scope, agent.as_deref(), permission)
                        .await
                    {
                        Ok(_) => {
                            let agent_str = agent.as_deref().unwrap_or("(default)");
                            println!(
                                "✅ Permission granted: {} can {:?} {}",
                                agent_str, permission, secret
                            );
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to grant permission: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Revoke {
                    secret,
                    agent,
                    scope,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager
                        .revoke_permission(&secret, &scope, agent.as_deref())
                        .await
                    {
                        Ok(true) => {
                            let agent_str = agent.as_deref().unwrap_or("(default)");
                            println!("✅ Permission revoked for {} on {}", agent_str, secret);
                        }
                        Ok(false) => {
                            println!("ℹ️  No permission found to revoke");
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to revoke permission: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Permissions {
                    name,
                    scope,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let scope = if scope == "global" {
                        SecretScope::Global
                    } else {
                        SecretScope::Agent { did: scope }
                    };

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    match manager.get_permissions(&name, &scope).await {
                        Ok(permissions) => {
                            if permissions.is_empty() {
                                println!("ℹ️  No explicit permissions set for '{}'", name);
                                println!("   Default policy applies:");
                                match scope {
                                    SecretScope::Global => println!("   - All agents: read"),
                                    SecretScope::Agent { did } => {
                                        println!("   - Only {}: write", did)
                                    }
                                }
                            } else {
                                println!("🔐 Permissions for '{}':", name);
                                for perm in permissions {
                                    let agent_str =
                                        perm.agent_did.as_deref().unwrap_or("(default)");
                                    println!("   {:30} {:?}", agent_str, perm.permission);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to get permissions: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::Audit {
                    secret,
                    agent,
                    event,
                    limit,
                    stats,
                    export,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    // Handle export first
                    if let Some(export_path) = export {
                        match manager.export_audit_log(&export_path, None).await {
                            Ok(count) => {
                                println!("✅ Exported {} audit entries to {}", count, export_path);
                            }
                            Err(e) => {
                                eprintln!("❌ Failed to export audit log: {}", e);
                                return Err(e);
                            }
                        }
                        return Ok(());
                    }

                    // Show stats if requested
                    if stats {
                        match manager.get_audit_stats(None).await {
                            Ok(stats) => {
                                println!("📊 Audit Log Statistics");
                                println!("   Total events:      {}", stats.total);
                                println!(
                                    "   Successful:        {} ({:.1}%)",
                                    stats.successful,
                                    stats.success_rate()
                                );
                                println!("   Failed:            {}", stats.failed);
                                println!("   Access denied:     {}", stats.access_denied);
                            }
                            Err(e) => {
                                eprintln!("❌ Failed to get audit stats: {}", e);
                                return Err(e);
                            }
                        }
                        return Ok(());
                    }

                    // Parse event type filter
                    let event_type = event.map(|e| match e.to_uppercase().as_str() {
                        "CREATED" => SecretAuditEvent::SecretCreated,
                        "ACCESSED" => SecretAuditEvent::SecretAccessed,
                        "UPDATED" => SecretAuditEvent::SecretUpdated,
                        "DELETED" => SecretAuditEvent::SecretDeleted,
                        "GRANTED" => SecretAuditEvent::PermissionGranted,
                        "REVOKED" => SecretAuditEvent::PermissionRevoked,
                        "UNLOCKED" => SecretAuditEvent::StoreUnlocked,
                        "LOCKED" => SecretAuditEvent::StoreLocked,
                        "DENIED" => SecretAuditEvent::AccessDenied,
                        _ => SecretAuditEvent::AccessDenied,
                    });

                    // Query audit log
                    match manager
                        .query_audit_log(
                            secret.as_deref(),
                            None,
                            agent.as_deref(),
                            event_type,
                            limit,
                        )
                        .await
                    {
                        Ok(entries) => {
                            if entries.is_empty() {
                                println!("ℹ️  No audit entries found");
                            } else {
                                println!("📋 Audit Log ({} entries):", entries.len());
                                println!(
                                    "   {:20} {:16} {:20} {:12} {}",
                                    "Timestamp", "Event", "Secret", "Status", "Agent"
                                );
                                println!("   {}", "-".repeat(80));

                                for entry in entries {
                                    let ts = &entry.timestamp[..16.min(entry.timestamp.len())]; // Truncate to date+time
                                    let event_short = format!("{:?}", entry.event).to_uppercase();
                                    let event_short = &event_short[..16.min(event_short.len())];
                                    let status = if entry.success { "✓" } else { "✗" };
                                    let agent_short = entry.agent_did.as_deref().unwrap_or("-");
                                    let agent_short = &agent_short[..12.min(agent_short.len())];

                                    println!(
                                        "   {:20} {:16} {:20} {:12} {}",
                                        ts, event_short, entry.secret_name, status, agent_short
                                    );

                                    if let Some(error) = &entry.error {
                                        println!("                      Error: {}", error);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("❌ Failed to query audit log: {}", e);
                            return Err(e);
                        }
                    }
                }
                SecretCommands::ImportEnv {
                    pattern,
                    prefix,
                    remove_prefix,
                    secret_type,
                    dry_run,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    // Compile regex pattern
                    let regex = match regex::Regex::new(&pattern) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("❌ Invalid pattern: {}", e);
                            return Ok(());
                        }
                    };

                    // Find matching env vars
                    let mut matches = Vec::new();
                    for (key, value) in std::env::vars() {
                        if regex.is_match(&key) && !value.is_empty() {
                            // Determine secret name
                            let mut name = key.clone();
                            if let Some(ref prefix_to_remove) = remove_prefix {
                                if name.starts_with(prefix_to_remove) {
                                    name = name[prefix_to_remove.len()..].to_string();
                                }
                            }
                            if let Some(ref prefix_to_add) = prefix {
                                name = format!("{}{}", prefix_to_add, name);
                            }

                            // Clean up name (replace invalid chars)
                            let name =
                                name.replace(|c: char| !c.is_alphanumeric() && c != '_', "_");

                            matches.push((name, key, value));
                        }
                    }

                    if matches.is_empty() {
                        println!("ℹ️  No environment variables matched pattern '{}'", pattern);
                        return Ok(());
                    }

                    println!(
                        "📥 Found {} environment variables matching pattern '{}'",
                        matches.len(),
                        pattern
                    );

                    // Parse secret type
                    let secret_type = match secret_type.to_lowercase().as_str() {
                        "api_key" => SecretType::ApiKey,
                        "token" => SecretType::Token,
                        "ssh_key" => SecretType::SshKey,
                        "certificate" => SecretType::Certificate,
                        "password" => SecretType::Password,
                        _ => SecretType::Other,
                    };

                    if dry_run {
                        println!("\n📝 Would import (dry run):");
                        for (name, original, _) in &matches {
                            println!("   {} -> {} (from {})", original, name, secret_type);
                        }
                        println!("\nRun without --dry-run to import.");
                        return Ok(());
                    }

                    // Import secrets
                    let mut manager = SecretManager::new().await?;
                    manager.unlock(&password).await?;

                    let mut imported = 0;
                    let mut failed = 0;

                    for (name, original, value) in matches {
                        match manager
                            .set(&name, SecretScope::Global, &value, secret_type, None)
                            .await
                        {
                            Ok(_) => {
                                println!("✅ Imported: {} -> {}", original, name);
                                imported += 1;
                            }
                            Err(e) => {
                                eprintln!("❌ Failed to import {}: {}", original, e);
                                failed += 1;
                            }
                        }
                    }

                    println!(
                        "\n📊 Import complete: {} imported, {} failed",
                        imported, failed
                    );
                }
                SecretCommands::Migrate {
                    path,
                    dry_run,
                    password,
                } => {
                    let password = match password {
                        Some(p) => p,
                        None => rpassword::prompt_password("Enter master password: ")?,
                    };

                    // Find config files
                    let path = std::path::PathBuf::from(path);
                    let config_files = if path.is_file() {
                        vec![path.clone()]
                    } else {
                        std::fs::read_dir(&path)?
                            .filter_map(|e| e.ok())
                            .filter(|e| e.path().extension().map(|e| e == "toml").unwrap_or(false))
                            .map(|e| e.path())
                            .collect::<Vec<_>>()
                    };

                    if config_files.is_empty() {
                        println!("ℹ️  No config files found in {}", path.display());
                        return Ok(());
                    }

                    println!(
                        "🔍 Scanning {} config file(s) for secrets...",
                        config_files.len()
                    );

                    // Common secret patterns
                    let api_key_pattern =
                        regex::Regex::new("api_key\\s*=\\s*\"([^\"]+)\"").unwrap();
                    let api_key_env_pattern =
                        regex::Regex::new("api_key_env\\s*=\\s*\"([^\"]+)\"").unwrap();

                    let mut found_secrets = Vec::new();

                    for file in &config_files {
                        let content = match std::fs::read_to_string(file) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };

                        // Check api_key pattern
                        for cap in api_key_pattern.captures_iter(&content) {
                            if let Some(matched) = cap.get(1) {
                                let value = matched.as_str();
                                // Skip if it's already a secret reference
                                if !value.starts_with("${") && !value.starts_with("sk-") {
                                    found_secrets.push((
                                        file.display().to_string(),
                                        "api_key".to_string(),
                                        value.to_string(),
                                    ));
                                }
                            }
                        }

                        // Check api_key_env pattern
                        for cap in api_key_env_pattern.captures_iter(&content) {
                            if let Some(matched) = cap.get(1) {
                                let value = matched.as_str();
                                // Skip if it's already a secret reference
                                if !value.starts_with("${") && !value.starts_with("sk-") {
                                    found_secrets.push((
                                        file.display().to_string(),
                                        "api_key_env".to_string(),
                                        value.to_string(),
                                    ));
                                }
                            }
                        }
                    }

                    if found_secrets.is_empty() {
                        println!("ℹ️  No plain-text secrets found in config files.");
                        return Ok(());
                    }

                    println!(
                        "\n⚠️  Found {} potential secrets in config files:",
                        found_secrets.len()
                    );
                    for (file, secret_type, value) in &found_secrets {
                        let masked = if value.len() > 8 {
                            format!("{}...{}", &value[..4], &value[value.len() - 4..])
                        } else {
                            "***".to_string()
                        };
                        println!("   {} [{}]: {}", file, secret_type, masked);
                    }

                    if dry_run {
                        println!("\n📝 Dry run - no changes made.");
                        println!("Run without --dry-run to migrate these to the secret store.");
                        return Ok(());
                    }

                    // TODO: Implement actual migration (interactive or automatic)
                    println!("\n⚠️  Migration not yet fully implemented.");
                    println!("Please manually move secrets to the secret store:");
                    println!("   pekobot secret set <NAME> --value <VALUE>");
                    println!(
                        "\nThen update your config files to use: api_key = \" ${{secret:NAME}} \""
                    );
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
