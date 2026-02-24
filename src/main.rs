use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use pekobot::agent::Agent;
use pekobot::channels::cli::{run_interactive_loop, CliChannel};
use pekobot::identity::did::DIDScope;
use pekobot::identity::{storage::KeyStorage, Identity};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use tracing::{info, warn};

// ============================================================================
// CLI Structure - Phase 1 Implementation
// ============================================================================

/// Pekobot - Lightweight Multi-Agent Runtime
#[derive(Parser)]
#[command(name = "pekobot")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Lightweight multi-agent runtime")]
#[command(propagate_version = true)]
struct Cli {
    /// Configuration directory override
    #[arg(long, global = true, env = "PEKOBOT_CONFIG_DIR")]
    config_dir: Option<PathBuf>,
    
    /// Data directory override
    #[arg(long, global = true, env = "PEKOBOT_DATA_DIR")]
    data_dir: Option<PathBuf>,
    
    /// Cache directory override  
    #[arg(long, global = true, env = "PEKOBOT_CACHE_DIR")]
    cache_dir: Option<PathBuf>,
    
    /// Output results as JSON
    #[arg(long, global = true)]
    json: bool,
    
    /// Suppress non-error output
    #[arg(short, long, global = true)]
    quiet: bool,
    
    /// Enable verbose logging
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Agent management commands
    #[command(subcommand)]
    Agent(AgentCommands),
    
    /// Tool management commands (Pekohub integration)
    #[command(subcommand)]
    Tool(ToolCommands),
    
    /// Session management commands
    #[command(subcommand)]
    Session(SessionCommands),
    
    /// Configuration management
    #[command(subcommand)]
    Config(ConfigCommands),
    
    /// System diagnostics and maintenance
    #[command(subcommand)]
    System(SystemCommands),
    
    /// Cron job management
    #[command(subcommand)]
    Cron(CronCommands),
    
    /// Gateway plugin management
    #[command(subcommand)]
    Gateway(GatewayCommands),
    
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

// ============================================================================
// Agent Commands - Phase 1
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum AgentCommands {
    /// Run an agent interactively (legacy compatibility)
    #[command(alias = "run")]
    Start {
        /// Agent configuration file
        #[arg(short, long)]
        config: Option<String>,
        /// Agent name
        #[arg(short, long, default_value = "peko")]
        name: String,
        /// LLM provider (openai, anthropic, ollama, kimi)
        #[arg(short, long, default_value = "openai")]
        provider: String,
        /// Model name
        #[arg(short, long)]
        model: Option<String>,
        /// Database path for memory
        #[arg(long)]
        db: Option<String>,
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

// ============================================================================
// Tool Commands - Phase 1  
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum ToolCommands {
    /// List installed tools
    List {
        /// Show all details
        #[arg(short, long)]
        long: bool,
    },
    
    /// Search Pekohub registry
    Search {
        /// Search query
        query: String,
        /// Limit results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    
    /// Install tool from Pekohub
    Install {
        /// Tool name
        name: String,
        /// Specific version
        #[arg(long)]
        version: Option<String>,
        /// Force reinstall if exists
        #[arg(short, long)]
        force: bool,
    },
    
    /// Uninstall a tool
    Uninstall {
        /// Tool name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
    
    /// Show tool information
    Info {
        /// Tool name
        name: String,
    },
}

// ============================================================================
// Gateway Commands
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum GatewayCommands {
    /// List installed gateway plugins
    List,
    
    /// Search available gateways on Pekohub
    Search {
        /// Search query
        query: Option<String>,
    },
    
    /// Install a gateway plugin
    Install {
        /// Gateway name
        name: String,
        /// Specific version
        #[arg(long)]
        version: Option<String>,
    },
    
    /// Show gateway information
    Info {
        /// Gateway name
        name: String,
    },
}

// ============================================================================
// Session Commands - Phase 2
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum SessionCommands {
    /// List active sessions
    List {
        /// Show all sessions (including inactive)
        #[arg(long)]
        all: bool,
        /// Filter by agent name
        #[arg(short, long)]
        agent: Option<String>,
    },
    
    /// Show session details and history
    Show {
        /// Session ID
        id: String,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },
    
    /// Send message to a session
    Send {
        /// Session ID
        id: String,
        /// Message content
        message: String,
    },
    
    /// Terminate a session
    Kill {
        /// Session ID
        id: String,
        /// Force immediate termination
        #[arg(short, long)]
        force: bool,
    },
}

// ============================================================================
// Config Commands - Phase 2
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum ConfigCommands {
    /// Validate a configuration file
    Validate {
        /// Config file path (default: pekobot.toml in current dir)
        file: Option<String>,
    },
    
    /// Initialize a new configuration
    Init {
        /// Output file
        #[arg(short, long, default_value = "pekobot.toml")]
        output: String,
        /// Template to use (minimal, full, agent)
        #[arg(short, long, default_value = "minimal")]
        template: String,
    },
    
    /// Show default configuration values
    Defaults,
    
    /// Show configuration paths
    Path,
    
    /// Get a configuration value
    Get {
        /// Key path (e.g., "agent.name" or "provider.api_key")
        key: String,
        /// Config file to read from
        #[arg(short, long)]
        file: Option<String>,
    },
    
    /// Set a configuration value
    Set {
        /// Key path
        key: String,
        /// Value to set
        value: String,
        /// Config file to modify
        #[arg(short, long)]
        file: Option<String>,
    },
}

// ============================================================================
// System Commands - Phase 2
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum SystemCommands {
    /// Show detailed system status
    Status {
        /// Include resource usage
        #[arg(long)]
        resources: bool,
    },
    
    /// Show system information
    Info,
    
    /// Run health check diagnostics
    Doctor {
        /// Fix issues automatically where possible
        #[arg(long)]
        fix: bool,
    },
    
    /// Clean up temporary files and cache
    Clean {
        /// Remove all tool caches
        #[arg(long)]
        tools: bool,
        /// Remove old logs
        #[arg(long)]
        logs: bool,
        /// Remove everything (full reset)
        #[arg(long)]
        all: bool,
    },
    
    /// Update Pekobot to latest version
    Update {
        /// Check for updates only
        #[arg(long)]
        check: bool,
    },
}

// ============================================================================
// Cron Commands
// ============================================================================

#[derive(Subcommand)]
#[command(disable_version_flag = true)]
enum CronCommands {
    /// List all cron jobs
    List {
        /// Show all jobs including disabled
        #[arg(long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    
    /// Add a new cron job
    Add {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Schedule expression (cron format: "0 9 * * *")
        #[arg(short, long)]
        schedule: String,
        /// Timezone (e.g., "America/Los_Angeles")
        #[arg(short, long)]
        timezone: Option<String>,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Execution mode: main or isolated
        #[arg(short, long, default_value = "main")]
        execution: String,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
        /// Delete after successful run (one-shot)
        #[arg(long)]
        delete_after_run: bool,
    },
    
    /// Add a one-shot job at specific time
    At {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// ISO timestamp (e.g., "2026-02-25T14:00:00Z")
        #[arg(short, long)]
        at: String,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },
    
    /// Add a recurring interval job
    Every {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Interval (e.g., "5m", "1h", "30s")
        #[arg(short, long)]
        interval: String,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },
    
    /// Remove a cron job
    Remove {
        /// Job ID
        job_id: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },
    
    /// Run a job immediately (manual execution)
    Run {
        /// Job ID
        job_id: String,
    },
    
    /// Show job run history
    History {
        /// Job ID
        job_id: String,
        /// Limit results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
}

// ============================================================================
// Global Paths Helper
// ============================================================================

struct GlobalPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    _cache_dir: PathBuf,
}

impl GlobalPaths {
    fn from_cli(cli: &Cli) -> Self {
        let config_dir = cli.config_dir.clone()
            .or_else(|| dirs::config_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| PathBuf::from(".").join(".pekobot"));
            
        let data_dir = cli.data_dir.clone()
            .or_else(|| dirs::data_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| config_dir.clone());
            
        let cache_dir = cli.cache_dir.clone()
            .or_else(|| dirs::cache_dir().map(|d| d.join("pekobot")))
            .unwrap_or_else(|| data_dir.join("cache"));
        
        // Ensure directories exist
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
        
        Self { config_dir, data_dir, _cache_dir: cache_dir }
    }
    
    fn agents_dir(&self) -> PathBuf {
        self.config_dir.join("agents")
    }
    
    fn agent_config(&self, name: &str) -> PathBuf {
        self.agents_dir().join(format!("{}.toml", name))
    }
    
    fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    
    // Initialize logging
    init_logging(cli.verbose, cli.quiet);
    
    // Set up global paths
    let paths = GlobalPaths::from_cli(&cli);
    
    match cli.command {
        Commands::Agent(cmd) => handle_agent(cmd, &paths, cli.json).await,
        Commands::Tool(cmd) => handle_tool(cmd, &paths, cli.json).await,
        Commands::Session(cmd) => handle_session(cmd, &paths, cli.json).await,
        Commands::Config(cmd) => handle_config(cmd, &paths, cli.json).await,
        Commands::System(cmd) => handle_system(cmd, &paths, cli.json).await,
        Commands::Cron(cmd) => handle_cron(cmd, &paths, cli.json).await,
        Commands::Gateway(cmd) => handle_gateway(cmd, &paths, cli.json).await,
        Commands::Completions { shell } => {
            let mut cmd = <Cli as clap::CommandFactory>::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
            Ok(())
        }
    }
}

fn init_logging(verbosity: u8, quiet: bool) {
    if quiet {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .init();
        return;
    }
    
    let level = match verbosity {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    
    tracing_subscriber::fmt()
        .with_max_level(level)
        .init();
}

// ============================================================================
// Agent Handlers - Phase 1 (Working Implementation)
// ============================================================================

async fn handle_agent(
    cmd: AgentCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        AgentCommands::Start { config, name, provider, model, db } => {
            handle_agent_start(config, name, provider, model, db).await
        }
        AgentCommands::List { long } => {
            handle_agent_list(paths, long, json).await
        }
        AgentCommands::Show { name } => {
            handle_agent_show(paths, name, json).await
        }
        AgentCommands::Create { name, template, provider, yes } => {
            handle_agent_create(paths, name, template, provider, yes).await
        }
        AgentCommands::Delete { name, purge, force } => {
            handle_agent_delete(paths, name, purge, force).await
        }
        AgentCommands::Export { name, output, encrypt } => {
            handle_agent_export(paths, name, output, encrypt).await
        }
        AgentCommands::Import { file, name } => {
            handle_agent_import(paths, file, name).await
        }
        AgentCommands::Inspect { file } => {
            handle_agent_inspect(file, json).await
        }
    }
}

async fn handle_agent_start(
    config_path: Option<String>,
    name: String,
    provider: String,
    model: Option<String>,
    db: Option<String>,
) -> anyhow::Result<()> {
    info!("Starting Pekobot agent: {}", name);
    
    let agent_config = if let Some(path) = config_path {
        info!("Loading config from: {}", path);
        let content = std::fs::read_to_string(&path)?;
        toml::from_str(&content)?
    } else {
        warn!("No config specified, using defaults");
        build_default_config(&name, &provider, model, db)
    };

    match Agent::new(agent_config).await {
        Ok(agent) => {
            if let Err(e) = agent.start().await {
                eprintln!("❌ Failed to start agent: {}", e);
                return Err(e);
            }

            println!("\n🐱 Agent '{}' started successfully!", name);
            println!("   DID: {}", agent.identity.did);
            println!("   State: {:?}", agent.state());

            let mut channel = CliChannel::new(&name);
            channel.print_banner();
            channel.print_system(&format!(
                "Agent '{}' is ready! Type 'exit' or 'quit' to stop.",
                name
            ));

            if let Err(e) = run_interactive_loop(&mut channel, &name).await {
                eprintln!("❌ Error in interactive loop: {}", e);
            }

            if let Err(e) = agent.stop().await {
                eprintln!("❌ Error stopping agent: {}", e);
            }

            println!("\n👋 Agent '{}' stopped. Goodbye!", name);
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Failed to create agent: {}", e);
            Err(e)
        }
    }
}

async fn handle_agent_list(
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
        println!("{}", output);
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
                        println!("\n  📦 {}", name);
                        println!("     Provider: {:?}", config.provider.provider_type);
                        println!("     Model: {}", config.provider.default_model);
                        if let Some(desc) = &config.description {
                            println!("     Description: {}", desc);
                        }
                        // Try to get DID
                        if let Some(identity) = load_identity_for_agent(&name).await {
                            println!("     DID: {}", identity.did);
                        }
                    } else {
                        println!("  📦 {} (invalid config)", name);
                    }
                }
            } else {
                print!("  📦 {}", name);
                // Quick status indicator
                if let Ok(content) = std::fs::read_to_string(&config_path) {
                    if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                        print!(" ({:?})", config.provider.provider_type);
                    }
                }
                println!();
            }
        }
    }
    
    Ok(())
}

async fn handle_agent_show(
    paths: &GlobalPaths,
    name: String,
    json: bool,
) -> anyhow::Result<()> {
    let config_path = paths.agent_config(&name);
    
    if !config_path.exists() {
        return Err(anyhow::anyhow!("Agent '{}' not found", name));
    }
    
    let content = std::fs::read_to_string(&config_path)?;
    
    if json {
        // Output raw config as JSON
        let config: AgentConfig = toml::from_str(&content)?;
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        let config: AgentConfig = toml::from_str(&content)?;
        println!("🐱 Agent: {}", name);
        println!("   Config: {}", config_path.display());
        
        if let Some(desc) = &config.description {
            println!("   Description: {}", desc);
        }
        
        println!("\n   Provider: {:?}", config.provider.provider_type);
        println!("   Model: {}", config.provider.default_model);
        println!("   Timeout: {}s", config.provider.timeout_seconds);
        
        if let Some(memory) = &config.memory {
            println!("\n   Memory:");
            println!("     Semantic Search: {}", memory.enable_semantic_search);
            if let Some(max) = memory.max_entries_per_agent {
                println!("     Max Entries: {}", max);
            }
        }
        
        // Try to load identity
        if let Some(identity) = load_identity_for_agent(&name).await {
            println!("\n   Identity:");
            println!("     DID: {}", identity.did);
        }
        
        println!("\n   Capabilities ({}):", config.capabilities.len());
        for cap in &config.capabilities {
            println!("     - {} v{}", cap.name, cap.version);
        }
    }
    
    Ok(())
}

async fn handle_agent_create(
    paths: &GlobalPaths,
    name: String,
    template: String,
    provider: String,
    yes: bool,
) -> anyhow::Result<()> {
    let config_path = paths.agent_config(&name);
    
    // Check if agent already exists
    if config_path.exists() && !yes {
        print!("Agent '{}' already exists. Overwrite? [y/N]: ", name);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }
    
    // Ensure directories exist
    std::fs::create_dir_all(paths.agents_dir())?;
    
    // Generate config from template
    let config_content = generate_config_from_template(&name, &template, &provider
    );
    
    // Write config
    std::fs::write(&config_path, config_content)?;
    
    // Create or load identity
    let storage = KeyStorage::new()?;
    let identity = if let Some(existing) = load_identity_for_agent(&name).await {
        println!("   Using existing identity");
        existing
    } else {
        let new_id = Identity::generate(DIDScope::Local, None)?;
        storage.store(&new_id)?;
        new_id
    };
    
    println!("✅ Created agent '{}'", name);
    println!("   Config: {}", config_path.display());
    println!("   DID: {}", identity.did);
    println!("   Template: {}", template);
    println!("\nRun with: pekobot agent start --name {}", name);
    
    Ok(())
}

async fn handle_agent_delete(
    paths: &GlobalPaths,
    name: String,
    purge: bool,
    force: bool,
) -> anyhow::Result<()> {
    let config_path = paths.agent_config(&name);
    
    if !config_path.exists() {
        return Err(anyhow::anyhow!("Agent '{}' not found", name));
    }
    
    if !force {
        print!("Delete agent '{}'? This cannot be undone. [y/N]: ", name);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }
    
    // Delete config
    std::fs::remove_file(&config_path)?;
    
    if purge {
        // Also delete identity
        if let Some(identity) = load_identity_for_agent(&name).await {
            let storage = KeyStorage::new()?;
            let _ = storage.delete(&identity.did);
        }
        println!("✅ Agent '{}' deleted (including identity)", name);
    } else {
        println!("✅ Agent '{}' deleted", name);
        println!("   Identity preserved. Use --purge to remove completely.");
    }
    
    Ok(())
}

async fn handle_agent_export(
    paths: &GlobalPaths,
    name: String,
    output: Option<String>,
    encrypt: bool,
) -> anyhow::Result<()> {
    println!("📦 Exporting agent '{}'...", name);
    
    let config_path = paths.agent_config(&name);
    
    if !config_path.exists() {
        return Err(anyhow::anyhow!("Agent '{}' not found", name));
    }
    
    let config: AgentConfig = {
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content)?
    };
    
    let identity = match load_identity_for_agent(&name).await {
        Some(id) => id,
        None => return Err(anyhow::anyhow!("Identity not found for agent: {}", name)),
    };
    
    let memory_path = config
        .memory
        .as_ref()
        .and_then(|m| m.database_path.as_ref())
        .map(PathBuf::from);
    
    let export_opts = pekobot::portable::ExportOptions {
        encrypt,
        passphrase: if encrypt {
            Some(rpassword::prompt_password("Enter passphrase: ")?)
        } else {
            None
        },
        include_memory: true,
        rotate_keys: false,
        description: None,
        output_path: output,
    };
    
    match pekobot::portable::export_agent(config, identity, memory_path, export_opts).await {
        Ok(path) => {
            println!("✅ Agent exported to: {}", path.display());
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Export failed: {}", e);
            Err(e)
        }
    }
}

async fn handle_agent_import(
    _paths: &GlobalPaths,
    file: String,
    name: Option<String>,
) -> anyhow::Result<()> {
    println!("📦 Importing agent from '{}'...", file);
    
    // Check if file exists
    if !std::path::Path::new(&file).exists() {
        return Err(anyhow::anyhow!("File not found: {}", file));
    }
    
    let import_opts = pekobot::portable::ImportOptions {
        new_name: name,
        passphrase: None, // Will prompt if encrypted
        rotate_keys: false,
        import_memory: true,
        skip_validation: false,
        force: false,
    };
    
    match pekobot::portable::import_agent(&file, import_opts).await {
        Ok(result) => {
            println!("✅ Agent imported successfully!");
            println!("   Name: {}", result.name);
            println!("   DID: {}", result.did);
            if result.keys_rotated {
                println!("   Keys: Rotated (new DID)");
            }
            if let Some(mem_path) = result.memory_path {
                println!("   Memory: {}", mem_path.display());
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Import failed: {}", e);
            Err(e)
        }
    }
}

async fn handle_agent_inspect(file: String, json: bool) -> anyhow::Result<()> {
    if !std::path::Path::new(&file).exists() {
        return Err(anyhow::anyhow!("File not found: {}", file));
    }
    
    match pekobot::portable::inspect_agent(&file, None).await {
        Ok((manifest, validation)) => {
            if json {
                // Manually construct JSON since ValidationResult doesn't implement Serialize
                let errors: Vec<String> = validation.errors.iter()
                    .map(|e| format!("{:?}", e))
                    .collect();
                let warnings: Vec<String> = validation.warnings.iter()
                    .map(|w| format!("{:?}", w))
                    .collect();
                
                let output = serde_json::json!({
                    "name": manifest.agent.name,
                    "version": manifest.agent.version,
                    "did": manifest.agent.did,
                    "description": manifest.agent.description,
                    "created_at": manifest.agent.created_at,
                    "export_format": manifest.agent.export_format,
                    "validation": {
                        "valid": validation.valid,
                        "errors": errors,
                        "warnings": warnings
                    }
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("🔍 Package: {}", file);
                println!("\n📦 Agent: {}", manifest.agent.name);
                println!("   Version: {}", manifest.agent.version);
                println!("   DID: {}", manifest.agent.did);
                if let Some(desc) = &manifest.agent.description {
                    println!("   Description: {}", desc);
                }
                println!("   Created: {}", manifest.agent.created_at);
                println!("   Format: {}", manifest.agent.export_format);
                
                println!("\n✅ Validation:");
                println!("   Valid: {}", validation.valid);
                if !validation.errors.is_empty() {
                    println!("   Errors:");
                    for error in &validation.errors {
                        println!("     - {:?}", error);
                    }
                }
                if !validation.warnings.is_empty() {
                    println!("   Warnings:");
                    for warning in &validation.warnings {
                        println!("     - {:?}", warning);
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Inspection failed: {}", e);
            Err(e)
        }
    }
}

// ============================================================================
// Tool Handlers - Phase 1 (Working Implementation)
// ============================================================================

async fn handle_tool(
    cmd: ToolCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    use pekobot::tool_registry::{
        MultiRegistryConfig, UnifiedToolRegistry, RegistryBackend
    };
    
    // Set up registry with Pekohub as primary
    let config = MultiRegistryConfig {
        primary: RegistryBackend::Pekohub {
            url: "https://tools.coneko.ai".to_string(),
            api_key: None,
        },
        fallbacks: vec![RegistryBackend::local_only()],
        cache_dir: paths.tools_dir(),
        allow_source_builds: true,
        timeout_secs: 60,
    };
    
    let registry = UnifiedToolRegistry::new(config)?;
    
    match cmd {
        ToolCommands::List { long } => {
            handle_tool_list(&registry, paths, long, json).await
        }
        ToolCommands::Search { query, limit } => {
            handle_tool_search(&registry, &query, limit, json).await
        }
        ToolCommands::Install { name, version, force } => {
            handle_tool_install(&registry, &name, version, force, json).await
        }
        ToolCommands::Uninstall { name, force } => {
            handle_tool_uninstall(paths, &name, force).await
        }
        ToolCommands::Info { name } => {
            handle_tool_info(&registry, &name, json).await
        }
    }
}

async fn handle_tool_list(
    _registry: &pekobot::tool_registry::UnifiedToolRegistry,
    paths: &GlobalPaths,
    long: bool,
    json: bool,
) -> anyhow::Result<()> {
    let tools_dir = paths.tools_dir();
    
    let mut tools = Vec::new();
    
    // Scan installed tools in cache directory
    if tools_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&tools_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name() {
                        tools.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    
    tools.sort();
    
    if json {
        let output = serde_json::json!({
            "tools": tools,
            "count": tools.len(),
            "cache_dir": tools_dir.to_string_lossy().to_string()
        });
        println!("{}", output);
    } else if tools.is_empty() {
        println!("No tools installed.");
        println!("Search with: pekobot tool search <query>");
        println!("Install with: pekobot tool install <name>");
    } else {
        println!("🔧 Installed Tools ({}):", tools.len());
        for tool in tools {
            if long {
                println!("  📦 {}", tool);
                // Try to read manifest
                let manifest_path = tools_dir.join(&tool).join("manifest.toml");
                if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                    if let Ok(manifest) = toml::from_str::<pekobot::tool_registry::ToolManifest>(&content
                    ) {
                        println!("     Version: {}", manifest.tool.version);
                        println!("     {}", manifest.tool.description);
                    }
                }
            } else {
                println!("  📦 {}", tool);
            }
        }
    }
    
    Ok(())
}

async fn handle_tool_search(
    _registry: &pekobot::tool_registry::UnifiedToolRegistry,
    query: &str,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    println!("🔍 Searching Pekohub for '{}'...", query);
    
    // For now, search is a stub that queries the Pekohub API
    // In full implementation, this would call registry.search_tools()
    
    // Mock results for Phase 1
    let available_tools = vec![
        ("calendar", "1.2.0", "Google Calendar and Outlook integration"),
        ("email", "1.0.5", "Send and receive emails"),
        ("web_search", "2.1.0", "Web search via Brave or DuckDuckGo"),
        ("browser", "1.0.0", "Browser automation and scraping"),
        ("document", "1.3.0", "PDF and document processing"),
        ("social_media", "0.9.0", "Post to Twitter/X, LinkedIn"),
        ("slack", "1.0.0", "Slack integration"),
        ("github", "2.0.0", "GitHub API integration"),
    ];
    
    let filtered: Vec<_> = available_tools
        .into_iter()
        .filter(|(name, _, desc)| {
            name.to_lowercase().contains(&query.to_lowercase()) ||
            desc.to_lowercase().contains(&query.to_lowercase())
        })
        .take(limit)
        .collect();
    
    if json {
        let results: Vec<_> = filtered.iter()
            .map(|(name, version, desc)| {
                serde_json::json!({
                    "name": name,
                    "version": version,
                    "description": desc
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else if filtered.is_empty() {
        println!("No tools found matching '{}'.", query);
    } else {
        println!("Found {} tool(s):", filtered.len());
        for (name, version, desc) in filtered {
            println!("  📦 {} v{} - {}", name, version, desc);
            println!("     Install: pekobot tool install {}", name);
        }
    }
    
    Ok(())
}

async fn handle_tool_install(
    registry: &pekobot::tool_registry::UnifiedToolRegistry,
    name: &str,
    version: Option<String>,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    println!("📥 Installing tool '{}'...", name);
    
    if let Some(v) = &version {
        println!("   Version: {}", v);
    }
    
    // Check if already installed
    let tools_dir = registry.get_cache_dir().join(name);
    if tools_dir.exists() && !force {
        return Err(anyhow::anyhow!(
            "Tool '{}' already installed. Use --force to reinstall.",
            name
        ));
    }
    
    // Try to load from registry
    match registry.load_tool(name, version.as_deref()).await {
        Ok(path) => {
            if json {
                let output = serde_json::json!({
                    "success": true,
                    "name": name,
                    "version": version.unwrap_or_else(|| "latest".to_string()),
                    "path": path.to_string_lossy().to_string()
                });
                println!("{}", output);
            } else {
                println!("✅ Tool '{}' installed successfully!", name);
                println!("   Location: {}", path.display());
            }
            Ok(())
        }
        Err(e) => {
            if json {
                let output = serde_json::json!({
                    "success": false,
                    "error": e.to_string()
                });
                println!("{}", output);
            } else {
                eprintln!("❌ Failed to install '{}': {}", name, e);
            }
            Err(e)
        }
    }
}

async fn handle_tool_uninstall(
    paths: &GlobalPaths,
    name: &str,
    force: bool,
) -> anyhow::Result<()> {
    let tool_dir = paths.tools_dir().join(name);
    
    if !tool_dir.exists() {
        return Err(anyhow::anyhow!("Tool '{}' is not installed", name));
    }
    
    if !force {
        print!("Uninstall tool '{}'? [y/N]: ", name);
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }
    
    std::fs::remove_dir_all(&tool_dir)?;
    println!("✅ Tool '{}' uninstalled.", name);
    
    Ok(())
}

async fn handle_tool_info(
    registry: &pekobot::tool_registry::UnifiedToolRegistry,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    // Try to read local manifest first
    let manifest_path = registry.get_cache_dir().join(name).join("manifest.toml");
    
    if manifest_path.exists() {
        let content = std::fs::read_to_string(&manifest_path)?;
        let manifest: pekobot::tool_registry::ToolManifest = toml::from_str(&content)?;
        
        if json {
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        } else {
            println!("📦 Tool: {}", manifest.tool.name);
            println!("   Version: {}", manifest.tool.version);
            println!("   Description: {}", manifest.tool.description);
            if let Some(author) = &manifest.tool.author {
                println!("   Author: {}", author);
            }
            if let Some(license) = &manifest.tool.license {
                println!("   License: {}", license);
            }
            println!("   Capabilities: {}", manifest.capabilities.provides.join(", "));
        }
    } else {
        println!("ℹ️  Tool '{}' not found locally.", name);
        println!("   Search: pekobot tool search {}", name);
        println!("   Install: pekobot tool install {}", name);
    }
    
    Ok(())
}

// ============================================================================
// Gateway Handlers
// ============================================================================

async fn handle_gateway(
    cmd: GatewayCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    use pekobot::gateway::{GatewayManager, GatewaysConfig};
    use std::sync::Arc;
    
    let config = GatewaysConfig::default();
    let manager = Arc::new(GatewayManager::new(config).await?);
    
    match cmd {
        GatewayCommands::List => {
            let plugins = manager.registry().list_loaded().await;
            
            if json {
                println!("{{\"gateways\": []}}");
            } else if plugins.is_empty() {
                println!("🌐 No gateway plugins installed.");
                println!("   Search: pekobot gateway search");
            } else {
                println!("🌐 Installed Gateways ({}):", plugins.len());
                for plugin in plugins {
                    println!("  📦 {} v{}", plugin.name, plugin.version);
                }
            }
        }
        GatewayCommands::Search { query } => {
            print!("🔍 Searching for gateways");
            if let Some(q) = &query {
                print!(" matching '{}'", q);
            }
            println!("...");
            
            match manager.registry().list_available().await {
                Ok(plugins) => {
                    let filtered: Vec<_> = plugins.into_iter()
                        .filter(|p| {
                            query.as_ref().map_or(true, |q| {
                                p.name.to_lowercase().contains(&q.to_lowercase())
                            })
                        })
                        .collect();
                    
                    if filtered.is_empty() {
                        println!("   No gateways found.");
                    } else {
                        for plugin in filtered {
                            let status = if plugin.installed { "✅" } else { "⬜" };
                            println!("   {} {} v{}", status, plugin.name, plugin.version);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("❌ Search failed: {}", e);
                }
            }
        }
        GatewayCommands::Install { name, version } => {
            println!("📥 Installing gateway: {}", name);
            if let Some(v) = &version {
                println!("   Version: {}", v);
            }
            
            match manager.registry().load(&name).await {
                Ok(_) => println!("✅ Gateway '{}' installed!", name),
                Err(e) => eprintln!("❌ Failed: {}", e),
            }
        }
        GatewayCommands::Info { name } => {
            match manager.registry().get_metadata(&name).await {
                Some(info) => {
                    println!("📦 Gateway: {}", info.name);
                    println!("   Version: {}", info.version);
                    println!("   {}", info.description);
                }
                None => println!("ℹ️  Gateway '{}' not found.", name),
            }
        }
    }
    
    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

fn generate_config_from_template(name: &str, template: &str, provider: &str) -> String {
    let (provider_type, default_model) = match provider.to_lowercase().as_str() {
        "openai" => ("open_a_i", "gpt-4o-mini"),
        "anthropic" => ("anthropic", "claude-3-haiku"),
        "ollama" => ("ollama", "llama3"),
        "kimi" => ("openai_compatible", "kimi-k2.5"),
        _ => ("open_a_i", "gpt-4o-mini"),
    };

    let capabilities = match template {
        "coding" => r#"[[capabilities]]
name = "filesystem"
version = "1.0"

[[capabilities]]
name = "process"
version = "1.0"

[[capabilities]]
name = "code_assist"
version = "1.0""#,
        "research" => r#"[[capabilities]]
name = "web_search"
version = "1.0"

[[capabilities]]
name = "browser"
version = "1.0"

[[capabilities]]
name = "fetch"
version = "1.0""#,
        "full" => r#"[[capabilities]]
name = "messaging"
version = "1.0"

[[capabilities]]
name = "filesystem"
version = "1.0"

[[capabilities]]
name = "process"
version = "1.0"

[[capabilities]]
name = "web_search"
version = "1.0"

[[capabilities]]
name = "browser"
version = "1.0""#,
        _ => r#"[[capabilities]]
name = "messaging"
version = "1.0"

[[capabilities]]
name = "task_execution"
version = "1.0""#,
    };

    format!(r#"# Pekobot Agent Configuration
name = "{}"
description = "A {} agent"
auto_accept_trusted = false
approval_threshold = 100.0
default_timeout_seconds = 300

[provider]
provider_type = "{}"
api_key = "${{env:{}_API_KEY}}"
default_model = "{}"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.{}]
name = "{}"
max_tokens = 2048
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

{}

[memory]
enable_semantic_search = false
max_entries_per_agent = 10000
auto_cleanup = true
cleanup_interval_seconds = 3600
"#,
        name,
        template,
        provider_type,
        provider_type.to_uppercase().replace("_A_I", "AI"),
        default_model,
        default_model,
        default_model,
        capabilities
    )
}

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
        "kimi" => ProviderType::OpenAICompatible,
        _ => ProviderType::OpenAI,
    };

    let model = model.unwrap_or_else(|| match provider_type {
        ProviderType::OpenAI => "gpt-4o-mini".to_string(),
        ProviderType::Anthropic => "claude-3-haiku".to_string(),
        ProviderType::Ollama => "llama3".to_string(),
        ProviderType::OpenAICompatible => "kimi-k2.5".to_string(),
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

async fn load_identity_for_agent(name: &str) -> Option<Identity> {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pekobot")
        .join("agents")
        .join(format!("{}.toml", name));

    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(_config) = toml::from_str::<AgentConfig>(&content) {
            let storage = KeyStorage::new().ok()?;
            
            if let Ok(identities) = storage.list_identities() {
                for did in identities {
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

// ============================================================================
// Session Handlers - Phase 2
// ============================================================================

async fn handle_session(
    cmd: SessionCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        SessionCommands::List { all, agent } => {
            if json {
                println!("{{\"sessions\": []}}");
            } else {
                println!("💬 Active Sessions:");
                if let Some(agent_name) = agent {
                    println!("   (Filtered by agent: {})", agent_name);
                }
                if all {
                    println!("   (Including inactive)");
                }
                println!("   Note: Session tracking requires running daemon");
                println!("   No active sessions found in this process.");
            }
        }
        SessionCommands::Show { id, history } => {
            println!("💬 Session: {}", id);
            if history {
                println!("   (Showing full history)");
            }
            println!("   Note: Session details require running daemon or API server");
        }
        SessionCommands::Send { id, message } => {
            println!("📨 Sending to session {}: {}", id, message);
            println!("   Note: Session messaging requires running daemon");
        }
        SessionCommands::Kill { id, force } => {
            if force {
                println!("💀 Force killing session {}...", id);
            } else {
                println!("🛑 Stopping session {}...", id);
            }
            println!("   Note: Session management requires running daemon");
        }
    }
    Ok(())
}

// ============================================================================
// Config Handlers - Phase 2
// ============================================================================

async fn handle_config(
    cmd: ConfigCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        ConfigCommands::Validate { file } => {
            let config_path = file.map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("pekobot.toml"));
            
            if !config_path.exists() {
                return Err(anyhow::anyhow!("Config file not found: {:?}", config_path));
            }
            
            let content = std::fs::read_to_string(&config_path)?;
            
            match toml::from_str::<AgentConfig>(&content) {
                Ok(_) => {
                    if json {
                        println!("{{\"valid\": true}}");
                    } else {
                        println!("✅ Config is valid: {}", config_path.display());
                    }
                }
                Err(e) => {
                    if json {
                        println!("{{\"valid\": false, \"error\": \"{}\"}}", e);
                    } else {
                        println!("❌ Config is invalid: {}", e);
                    }
                    std::process::exit(1);
                }
            }
        }
        ConfigCommands::Init { output, template } => {
            let config = generate_config_from_template("my-agent", &template, "openai");
            
            std::fs::write(&output, config)?;
            
            if json {
                println!("{{\"created\": \"{}\"}}", output);
            } else {
                println!("✅ Created config: {}", output);
                println!("   Template: {}", template);
            }
        }
        ConfigCommands::Defaults => {
            let defaults = build_default_config("example", "openai", None, None);
            if json {
                println!("{}", serde_json::to_string_pretty(&defaults)?);
            } else {
                println!("📝 Default Configuration Values:");
                println!("   Provider: OpenAI");
                println!("   Model: gpt-4o-mini");
                println!("   Timeout: 30s");
                println!("   Retries: 3");
                println!("\n   See 'pekobot config init' for full example.");
            }
        }
        ConfigCommands::Path => {
            if json {
                let output = serde_json::json!({
                    "config_dir": paths.config_dir,
                    "data_dir": paths.data_dir,
                });
                println!("{}", output);
            } else {
                println!("📁 Configuration Paths:");
                println!("   Config: {}", paths.config_dir.display());
                println!("   Data:   {}", paths.data_dir.display());
                println!("\n   Override with:");
                println!("     --config-dir <path>  or  PEKOBOT_CONFIG_DIR");
                println!("     --data-dir <path>    or  PEKOBOT_DATA_DIR");
            }
        }
        ConfigCommands::Get { key, file } => {
            let config_path = file.map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("pekobot.toml"));
            
            let _content = std::fs::read_to_string(&config_path)?;
            
            // Simple key access - in full impl would use toml::Value traversal
            println!("🔍 {} = (would read from {:?})", key, config_path);
            println!("   Note: Config get requires TOML value traversal (not yet implemented)");
        }
        ConfigCommands::Set { key, value, file } => {
            let config_path = file.map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("pekobot.toml"));
            
            println!("✏️  Setting {} = {:?} in {:?}", key, value, config_path);
            println!("   Note: Config set requires TOML manipulation (not yet implemented)");
        }
    }
    Ok(())
}

// ============================================================================
// System Handlers - Phase 2
// ============================================================================

async fn handle_system(
    cmd: SystemCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        SystemCommands::Status { resources } => {
            if json {
                let output = serde_json::json!({
                    "version": pekobot::VERSION,
                    "status": "operational",
                    "resources": resources
                });
                println!("{}", output);
            } else {
                println!("🐱 Pekobot Status");
                println!("   Version: {}", pekobot::VERSION);
                println!("   Status: 🟢 Operational");
                
                if resources {
                    println!("\n   Resources:");
                    println!("     (Resource monitoring not yet implemented)");
                }
            }
        }
        SystemCommands::Info => {
            if json {
                let output = serde_json::json!({
                    "version": pekobot::VERSION,
                    "config_dir": paths.config_dir.to_string_lossy().to_string(),
                    "data_dir": paths.data_dir.to_string_lossy().to_string(),
                });
                println!("{}", output);
            } else {
                println!("🐱 Pekobot System Information");
                println!("   Version: {}", pekobot::VERSION);
                println!("   Config: {}", paths.config_dir.display());
                println!("   Data: {}", paths.data_dir.display());
                
                // Check directories exist
                println!("\n   Directory Status:");
                for (name, path) in [
                    ("Config", &paths.config_dir),
                    ("Data", &paths.data_dir),
                ] {
                    let status = if path.exists() { "✅" } else { "❌" };
                    println!("     {} {}: {}", status, name, path.display());
                }
            }
        }
        SystemCommands::Doctor { fix } => {
            println!("🏥 Running health check...");
            
            let mut issues = Vec::new();
            
            // Check config directory
            if !paths.config_dir.exists() {
                issues.push("Config directory does not exist");
                if fix {
                    std::fs::create_dir_all(&paths.config_dir)?;
                    println!("   ✅ Created config directory");
                }
            }
            
            // Check data directory
            if !paths.data_dir.exists() {
                issues.push("Data directory does not exist");
                if fix {
                    std::fs::create_dir_all(&paths.data_dir)?;
                    println!("   ✅ Created data directory");
                }
            }
            
            if issues.is_empty() {
                println!("✅ All checks passed!");
            } else if !fix {
                println!("⚠️  Issues found ({}):", issues.len());
                for issue in issues {
                    println!("   - {}", issue);
                }
                println!("\n   Run with --fix to attempt automatic repair.");
            }
        }
        SystemCommands::Clean { tools, logs, all } => {
            if all {
                println!("🧹 Cleaning all temporary data...");
                let tools_dir = paths.tools_dir();
                if tools_dir.exists() {
                    std::fs::remove_dir_all(&tools_dir)?;
                    println!("   ✅ Removed tool cache");
                }
            } else {
                if tools {
                    println!("🧹 Cleaning tool cache...");
                    let tools_dir = paths.tools_dir();
                    if tools_dir.exists() {
                        std::fs::remove_dir_all(&tools_dir)?;
                        println!("   ✅ Removed tool cache");
                    }
                }
                if logs {
                    println!("🧹 Cleaning old logs...");
                    println!("   (Log cleanup not yet implemented)");
                }
                if !tools && !logs {
                    println!("🧹 Cleaning unused cache...");
                    println!("   (Specify --tools, --logs, or --all)");
                }
            }
        }
        SystemCommands::Update { check } => {
            if check {
                println!("🔍 Checking for updates...");
            } else {
                println!("🔄 Updating Pekobot...");
            }
            println!("   Note: Self-update not yet implemented");
            println!("   Please use your package manager or reinstall.");
        }
    }
    Ok(())
}

// ============================================================================
// Cron Handlers
// ============================================================================

async fn handle_cron(
    cmd: CronCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    use pekobot::cron::{CronJob, CronScheduler, DeliveryMode, ExecutionTarget, ScheduleKind};
    use chrono::Utc;
    use uuid::Uuid;
    
    let cron_db = paths.data_dir.join("cron.db");
    let scheduler = CronScheduler::new(&cron_db)?;
    
    match cmd {
        CronCommands::List { all, json: output_json } => {
            let jobs = scheduler.list_jobs(all)?;
            
            if json || output_json {
                let job_list: Vec<serde_json::Value> = jobs
                    .into_iter()
                    .map(|j| {
                        serde_json::json!({
                            "id": j.id,
                            "name": j.name,
                            "schedule": j.schedule.display(),
                            "target": match j.target {
                                ExecutionTarget::Main => "main",
                                ExecutionTarget::Isolated => "isolated",
                            },
                            "enabled": j.enabled,
                            "next_run": j.next_run.to_rfc3339(),
                            "run_count": j.run_count,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "jobs": job_list }));
            } else {
                if jobs.is_empty() {
                    println!("No cron jobs scheduled.");
                    println!("Add one with: pekobot cron add --name <name> --schedule '<expr>' --message '<msg>'");
                } else {
                    println!("⏰ Cron Jobs ({}):", jobs.len());
                    for job in jobs {
                        let status = if job.enabled { "🟢" } else { "⚪" };
                        let target = match job.target {
                            ExecutionTarget::Main => "main",
                            ExecutionTarget::Isolated => "iso",
                        };
                        println!("  {} {} [{}] - {}", status, job.name, target, job.schedule.display());
                        println!("     Next: {} | Runs: {}", 
                            job.next_run.format("%Y-%m-%d %H:%M UTC"),
                            job.run_count);
                    }
                }
            }
        }
        
        CronCommands::Add { name, schedule, timezone, agent, execution, message, announce, delete_after_run } => {
            // Parse cron expression
            let schedule_kind = ScheduleKind::Cron { 
                expr: schedule, 
                tz: timezone 
            };
            
            let target = match execution.as_str() {
                "isolated" => ExecutionTarget::Isolated,
                _ => ExecutionTarget::Main,
            };
            
            let delivery = if announce {
                DeliveryMode::Announce { channel: None, to: None, best_effort: true }
            } else {
                DeliveryMode::None
            };
            
            let now = Utc::now();
            let next_run = scheduler.calculate_next_run(&schedule_kind, now)?;
            
            let job = CronJob {
                id: Uuid::new_v4().to_string(),
                name,
                schedule: schedule_kind,
                target,
                agent_id: agent,
                message,
                delivery,
                delete_after_run,
                enabled: true,
                created_at: now,
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
            };
            
            scheduler.add_job(&job)?;
            
            if json {
                println!("{}", serde_json::json!({
                    "success": true,
                    "job_id": job.id,
                    "next_run": job.next_run.to_rfc3339()
                }));
            } else {
                println!("✅ Created cron job '{}'", job.name);
                println!("   ID: {}", job.id);
                println!("   Next run: {}", job.next_run.format("%Y-%m-%d %H:%M UTC"));
            }
        }
        
        CronCommands::At { name, at, agent, message, announce } => {
            let schedule_kind = ScheduleKind::At { at };
            
            let delivery = if announce {
                DeliveryMode::Announce { channel: None, to: None, best_effort: true }
            } else {
                DeliveryMode::None
            };
            
            let now = Utc::now();
            let next_run = scheduler.calculate_next_run(&schedule_kind, now)?;
            
            let job = CronJob {
                id: Uuid::new_v4().to_string(),
                name,
                schedule: schedule_kind,
                target: ExecutionTarget::Main,
                agent_id: agent,
                message,
                delivery,
                delete_after_run: true, // One-shot jobs auto-delete
                enabled: true,
                created_at: now,
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
            };
            
            scheduler.add_job(&job)?;
            
            if json {
                println!("{}", serde_json::json!({
                    "success": true,
                    "job_id": job.id,
                    "next_run": job.next_run.to_rfc3339()
                }));
            } else {
                println!("✅ Created one-shot job '{}'", job.name);
                println!("   ID: {}", job.id);
                println!("   Runs at: {}", job.next_run.format("%Y-%m-%d %H:%M UTC"));
            }
        }
        
        CronCommands::Every { name, interval, agent, message, announce } => {
            // Parse interval (e.g., "5m", "1h", "30s")
            let every_ms = parse_interval(&interval)?;
            let schedule_kind = ScheduleKind::Every { every_ms };
            
            let delivery = if announce {
                DeliveryMode::Announce { channel: None, to: None, best_effort: true }
            } else {
                DeliveryMode::None
            };
            
            let now = Utc::now();
            let next_run = scheduler.calculate_next_run(&schedule_kind, now)?;
            
            let job = CronJob {
                id: Uuid::new_v4().to_string(),
                name,
                schedule: schedule_kind,
                target: ExecutionTarget::Main,
                agent_id: agent,
                message,
                delivery,
                delete_after_run: false,
                enabled: true,
                created_at: now,
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
            };
            
            scheduler.add_job(&job)?;
            
            if json {
                println!("{}", serde_json::json!({
                    "success": true,
                    "job_id": job.id,
                    "next_run": job.next_run.to_rfc3339()
                }));
            } else {
                println!("✅ Created interval job '{}'", job.name);
                println!("   ID: {}", job.id);
                println!("   Every: {} | Next: {}", 
                    format_duration(every_ms),
                    job.next_run.format("%Y-%m-%d %H:%M UTC"));
            }
        }
        
        CronCommands::Remove { job_id, force } => {
            if !force {
                print!("Delete job '{}'? [y/N]: ", job_id);
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            
            let deleted = scheduler.delete_job(&job_id)?;
            if deleted {
                if json {
                    println!("{}", serde_json::json!({ "success": true }));
                } else {
                    println!("✅ Deleted job {}", job_id);
                }
            } else {
                return Err(anyhow::anyhow!("Job {} not found", job_id));
            }
        }
        
        CronCommands::Run { job_id } => {
            let job = scheduler.get_job(&job_id)?
                .ok_or_else(|| anyhow::anyhow!("Job {} not found", job_id))?;
            
            // Note: Actual execution would require the agent runtime
            // This is a placeholder that shows the job details
            if json {
                println!("{}", serde_json::json!({
                    "success": true,
                    "job_id": job.id,
                    "message": "Job queued for execution",
                    "note": "Manual execution requires running agent"
                }));
            } else {
                println!("⏳ Queued job '{}' for execution", job.name);
                println!("   Message: {}", job.message);
                println!("   Note: Requires running agent to execute");
            }
        }
        
        CronCommands::History { job_id, limit } => {
            let runs = scheduler.get_run_history(&job_id, limit)?;
            
            if json {
                let run_list: Vec<serde_json::Value> = runs
                    .into_iter()
                    .map(|r| serde_json::json!({
                        "id": r.id,
                        "started_at": r.started_at.to_rfc3339(),
                        "finished_at": r.finished_at.map(|t| t.to_rfc3339()),
                        "status": r.status,
                    }))
                    .collect();
                println!("{}", serde_json::json!({ "runs": run_list }));
            } else {
                if runs.is_empty() {
                    println!("No run history for job {}.", job_id);
                } else {
                    println!("📜 Run history for {} (last {} runs):", job_id, runs.len());
                    for run in runs {
                        let status = match run.status.as_str() {
                            "success" => "✅",
                            "failed" => "❌",
                            _ => "⏳",
                        };
                        println!("  {} {} - {}", status, run.id, run.started_at.format("%Y-%m-%d %H:%M UTC"));
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_interval(interval: &str) -> anyhow::Result<u64> {
    let interval = interval.trim();
    let (num, unit) = if let Some(pos) = interval.find(|c: char| !c.is_ascii_digit()) {
        interval.split_at(pos)
    } else {
        return Err(anyhow::anyhow!("Invalid interval format. Use like: 5m, 1h, 30s"));
    };
    
    let num: u64 = num.parse()
        .map_err(|_| anyhow::anyhow!("Invalid number in interval"))?;
    
    let ms = match unit {
        "s" | "sec" | "secs" => num * 1000,
        "m" | "min" | "mins" => num * 60 * 1000,
        "h" | "hr" | "hrs" => num * 60 * 60 * 1000,
        "d" | "day" | "days" => num * 24 * 60 * 60 * 1000,
        _ => return Err(anyhow::anyhow!("Invalid interval unit: {}. Use s, m, h, or d", unit)),
    };
    
    Ok(ms)
}

fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else if ms < 3_600_000 {
        format!("{}m", ms / 60_000)
    } else if ms < 86_400_000 {
        format!("{}h", ms / 3_600_000)
    } else {
        format!("{}d", ms / 86_400_000)
    }
}
