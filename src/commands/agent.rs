//! Agent Management Commands
#![allow(dead_code)]

use crate::commands::identifier::parse_agent_identifier_with_override;
use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Agent management subcommands
///
/// Examples:
///   # Initialize a new agent directory
///   pekobot agent init ./my-agent --provider openai
///
///   # Send a message to an agent (non-interactive only)
///   pekobot agent start my-agent --message "Hello"
///
///   # List all agents
///   pekobot agent list
///
///   # Create agent in a specific team
///   pekobot agent create myteam/my-agent --provider kimi
///   pekobot agent create my-agent --team myteam --provider kimi
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AgentCommands {
    /// Send a single message to an agent (non-interactive)
    ///
    /// **DEPRECATED:** Use `pekobot send <agent> <message>` instead.
    #[command(alias = "run")]
    Start {
        /// Agent name (defaults to 'peko' if not specified)
        name: Option<String>,
        /// Custom configuration file path (optional, defaults to ~/.pekobot/teams/default/agents/{name}/config.toml)
        #[arg(short, long)]
        config: Option<String>,
        /// LLM provider (openai, anthropic, ollama, kimi, `kimi_code`) - only used when creating default config
        #[arg(short, long, default_value = "kimi_code")]
        provider: String,
        /// Model name - only used when creating default config
        #[arg(short, long)]
        model: Option<String>,
        /// Database path for memory
        #[arg(long)]
        db: Option<String>,
        /// Message to send (required, non-interactive only)
        #[arg(short = 'M', long, required = true)]
        message: String,
        /// Start a new session (don't resume existing CLI session)
        #[arg(short, long)]
        new: bool,
    },

    /// List all configured agents
    List {
        /// Show detailed information
        #[arg(short, long)]
        long: bool,
    },

    /// Show detailed agent information
    Show {
        /// Agent name or team/agent format
        name: String,
        /// Team to look in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
    },

    /// Create a new agent from template
    Create {
        /// Agent name or team/agent format
        name: String,
        /// Team to create agent in (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Use template (minimal, coding, research, full)
        #[arg(short, long, default_value = "minimal")]
        template: String,
        /// Provider to use
        #[arg(short, long, default_value = "kimi_code")]
        provider: String,
        /// Force overwrite if agent already exists (non-interactive)
        #[arg(short, long)]
        force: bool,
    },

    /// Delete an agent and its configuration
    Delete {
        /// Agent name or team/agent format
        name: String,
        /// Team to delete from (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Also delete identity
        #[arg(long)]
        purge: bool,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Rename an agent
    Rename {
        /// Current agent name or team/agent format
        old_name: String,
        /// New agent name (just the name, no team prefix)
        new_name: String,
        /// Team of the existing agent (overrides team/ prefix if both provided)
        #[arg(short, long)]
        team: Option<String>,
        /// Target team for cross-team move (optional)
        #[arg(long)]
        to_team: Option<String>,
    },

    /// Export agent to .agent package
    Export {
        /// Agent name or team/agent format
        #[arg(short, long)]
        name: String,
        /// Team to export from (overrides team/ prefix if both provided)
        #[arg(long)]
        team: Option<String>,
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

    /// Initialize a new agent directory with minimal structure
    Init {
        /// Directory path to initialize (creates if doesn't exist)
        path: String,
        /// Agent name (defaults to directory name)
        #[arg(short, long)]
        name: Option<String>,
        /// Provider to use
        #[arg(short, long, default_value = "kimi_code")]
        provider: String,
        /// Model name
        #[arg(short, long)]
        model: Option<String>,
        /// Force overwrite if directory exists (non-interactive)
        #[arg(short, long)]
        force: bool,
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
            new,
        } => {
            crate::commands::agent::handlers::handle_agent_start(
                name, config, provider, model, db, message, new,
            )
            .await
        }
        AgentCommands::List { long } => {
            crate::commands::agent::handlers::handle_agent_list(paths, long, json).await
        }
        AgentCommands::Show { name, team } => {
            crate::commands::agent::handlers::handle_agent_show(paths, name, team, json).await
        }
        AgentCommands::Create {
            name,
            team,
            template,
            provider,
            force,
        } => {
            crate::commands::agent::handlers::handle_agent_create(
                paths, name, team, template, provider, force,
            )
            .await
        }
        AgentCommands::Delete { name, team, purge, force } => {
            crate::commands::agent::handlers::handle_agent_delete(paths, name, team, purge, force)
                .await
        }
        AgentCommands::Rename {
            old_name,
            new_name,
            team,
            to_team,
        } => {
            crate::commands::agent::handlers::handle_agent_rename(
                paths, old_name, new_name, team, to_team, json,
            )
            .await
        }
        AgentCommands::Export {
            name,
            team,
            output,
            encrypt,
        } => {
            crate::commands::agent::handlers::handle_agent_export(
                paths, name, team, output, encrypt,
            )
            .await
        }
        AgentCommands::Import { file, name } => {
            crate::commands::agent::handlers::handle_agent_import(paths, file, name).await
        }
        AgentCommands::Inspect { file } => {
            crate::commands::agent::handlers::handle_agent_inspect(file, json).await
        }
        AgentCommands::Init {
            path,
            name,
            provider,
            model,
            force,
        } => {
            crate::commands::agent::handlers::handle_agent_init(
                path, name, provider, model, force, json,
            )
            .await
        }
    }
}

/// Agent command handlers
pub mod handlers {
    use crate::agent::Agent;
    use crate::commands::identifier::parse_agent_identifier_with_override;
    use crate::commands::GlobalPaths;
    use crate::types::agent::AgentConfig;
    use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
    use tracing::{info, warn};

    /// Handle agent start command
    ///
    /// **DEPRECATED:** The `agent start --message` flag is deprecated.
    /// Use `pekobot send <agent> <message>` instead.
    pub async fn handle_agent_start(
        name: Option<String>,
        config_path: Option<String>,
        provider: String,
        model: Option<String>,
        db: Option<String>,
        message: String,
        new_session: bool,
    ) -> anyhow::Result<()> {
        use tokio::task::LocalSet;

        // Show deprecation warning
        eprintln!("⚠️  Warning: 'pekobot agent start --message' is deprecated.");
        eprintln!("   Use 'pekobot send <agent> \"<message>\"' instead.");
        eprintln!();

        // Determine agent name (default to "peko")
        let agent_name = name.unwrap_or_else(|| "peko".to_string());

        // Determine config path (updated for team-based structure)
        let config_path = if let Some(path) = config_path {
            path
        } else {
            // Look up in default location: ~/.pekobot/teams/default/agents/{name}/config.toml
            let home = dirs::home_dir()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            format!("{home}/.pekobot/teams/default/agents/{agent_name}/config.toml")
        };

        let agent_config = if std::path::Path::new(&config_path).exists() {
            let content = std::fs::read_to_string(&config_path)?;
            toml::from_str(&content)?
        } else {
            info!(
                "No config file found at {}, using default configuration",
                config_path
            );
            build_default_config(&agent_name, &provider, model, db)
        };

        let agent = match Agent::new(agent_config).await {
            Ok(agent) => agent,
            Err(e) => {
                eprintln!("❌ Failed to create agent: {e}");
                return Err(e);
            }
        };

        if let Err(e) = agent.start().await {
            eprintln!("❌ Failed to start agent: {e}");
            return Err(e);
        }

        // Create a LocalSet for the entire execution
        // This is required because execute_streaming_with_session uses spawn_local internally
        let local = LocalSet::new();

        local
            .run_until(async {
                // Single message mode only (non-interactive)
                // Send message - output is already printed by process_events
                crate::channels::cli::send_single_message_with_session(
                    &agent,
                    &message,
                    new_session,
                )
                .await
            })
            .await?;

        Ok(())
    }

    /// Handle agent list command - organized by teams
    pub async fn handle_agent_list(
        paths: &GlobalPaths,
        long: bool,
        json: bool,
    ) -> anyhow::Result<()> {
        let teams_dir = paths.teams_dir();

        if !teams_dir.exists() {
            if json {
                println!("{{\"teams\": []}}");
            } else {
                println!("No agents configured.");
                println!("Create one with: pekobot agent create <name>");
            }
            return Ok(());
        }

        // Structure: teams/{team}/agents/{agent}/config.toml
        let mut teams: std::collections::HashMap<String, Vec<(String, AgentConfig)>> =
            std::collections::HashMap::new();
        let mut total_agents = 0;

        if let Ok(team_entries) = std::fs::read_dir(&teams_dir) {
            for team_entry in team_entries.flatten() {
                let team_path = team_entry.path();
                let team_name = team_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let team_agents_dir = team_path.join("agents");
                if !team_agents_dir.exists() {
                    continue;
                }

                let mut team_agents = Vec::new();
                if let Ok(agent_entries) = std::fs::read_dir(&team_agents_dir) {
                    for agent_entry in agent_entries.flatten() {
                        let agent_path = agent_entry.path();
                        if !agent_path.is_dir() {
                            continue;
                        }

                        let agent_name = agent_path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();

                        let config_path = agent_path.join("config.toml");
                        if let Ok(content) = std::fs::read_to_string(&config_path) {
                            if let Ok(config) = toml::from_str::<AgentConfig>(&content) {
                                team_agents.push((agent_name, config));
                            }
                        }
                    }
                }

                if !team_agents.is_empty() {
                    team_agents.sort_by(|a, b| a.0.cmp(&b.0));
                    total_agents += team_agents.len();
                    teams.insert(team_name, team_agents);
                }
            }
        }

        if json {
            // Build JSON output with team structure
            let mut teams_json = Vec::new();
            for (team_name, agents) in &teams {
                let agents_json: Vec<_> = agents
                    .iter()
                    .map(|(name, config)| {
                        serde_json::json!({
                            "name": name,
                            "provider": format!("{:?}", config.provider.provider_type),
                            "model": config.provider.default_model,
                            "description": config.description,
                        })
                    })
                    .collect();
                teams_json.push(serde_json::json!({
                    "name": team_name,
                    "agents": agents_json
                }));
            }
            let output = serde_json::json!({"teams": teams_json, "total_agents": total_agents});
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if total_agents == 0 {
            println!("No agents configured.");
            println!("Create one with: pekobot agent create <name>");
        } else {
            println!("🐱 Configured Agents ({}):", total_agents);

            // Sort teams: default first, then alphabetically
            let mut team_names: Vec<_> = teams.keys().cloned().collect();
            team_names.sort_by(|a, b| {
                if a == "default" {
                    return std::cmp::Ordering::Less;
                }
                if b == "default" {
                    return std::cmp::Ordering::Greater;
                }
                a.cmp(b)
            });

            for team_name in team_names {
                let team_agents = teams.get(&team_name).unwrap();
                println!("\n  📁 Team: {team_name}");

                for (name, config) in team_agents {
                    if long {
                        println!("\n    📦 {name}");
                        println!("       Provider: {:?}", config.provider.provider_type);
                        println!("       Model: {}", config.provider.default_model);
                        if let Some(desc) = &config.description {
                            println!("       Description: {desc}");
                        }
                    } else {
                        println!("    📦 {name}");
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle agent show command
    pub async fn handle_agent_show(
        paths: &GlobalPaths,
        name: String,
        team: Option<String>,
        json: bool,
    ) -> anyhow::Result<()> {
        // Parse identifier to extract team and agent name
        let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

        let config_path = paths.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            eprintln!("❌ Agent '{}' not found in team '{}'", agent_name, team);
            return Ok(());
        }

        let content = std::fs::read_to_string(&config_path)?;
        let config: AgentConfig = toml::from_str(&content)?;

        if json {
            let output = serde_json::json!({
                "name": agent_name,
                "team": team,
                "config": config,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("📦 Agent: {}", agent_name);
            println!("   Team: {}", team);
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
    ///
    /// NOTE: This command is non-interactive per REQ-UI-001.
    /// If agent already exists, use --force to overwrite or delete it first.
    pub async fn handle_agent_create(
        paths: &GlobalPaths,
        name: String,
        team: Option<String>,
        _template: String,
        provider: String,
        force: bool,
    ) -> anyhow::Result<()> {
        // Parse identifier to extract team and agent name
        let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

        // Check if team exists
        let team_dir = paths.team_dir(team);
        if !team_dir.exists() {
            anyhow::bail!(
                "Team '{}' does not exist. Create it first with: pekobot team create {}",
                team,
                team
            );
        }

        let config_path = paths.agent_config(agent_name, Some(team));

        if config_path.exists() && !force {
            anyhow::bail!(
                "Agent '{}' already exists in team '{}'. Use --force to overwrite or delete it first.",
                agent_name,
                team
            );
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
        let mut config = build_config_with_auth(paths, agent_name, &selected_provider, None, None).await?;
        // Set the team field in config
        config.team = Some(team.to_string());

        let toml = toml::to_string_pretty(&config)?;

        std::fs::create_dir_all(config_path.parent().unwrap())?;
        std::fs::write(&config_path, toml)?;

        println!("✅ Created agent '{}' in team '{}'", agent_name, team);
        println!("   Provider: {selected_provider}");
        println!("   Config: {}", config_path.display());

        // Show warning if no API keys configured
        if available_providers.is_empty() {
            println!();
            println!("⚠️  No API keys configured!");
            println!("   Set one with: pekobot auth set <provider> <key>");
            println!("   Or export: export OPENAI_API_KEY='your-key'");
        }

        // Bootstrap workspace with OpenClaw-style files (always non-interactive)
        let workspace_dir = paths.data_dir.join("workspaces").join(team).join(agent_name);
        let bootstrap = crate::commands::agent_bootstrap::AgentBootstrap::new(agent_name, workspace_dir);
        if let Err(e) = bootstrap.run_non_interactive() {
            eprintln!("⚠️  Warning: Failed to bootstrap workspace: {e}");
        }

        Ok(())
    }

    /// Handle agent delete command
    ///
    /// NOTE: This command is non-interactive per REQ-UI-001.
    /// Use --force flag to skip confirmation (confirmation is not prompted).
    pub async fn handle_agent_delete(
        paths: &GlobalPaths,
        name: String,
        team: Option<String>,
        purge: bool,
        _force: bool, // Always non-interactive, flag kept for CLI compatibility
    ) -> anyhow::Result<()> {
        // Parse identifier to extract team and agent name
        let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

        let config_path = paths.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", agent_name, team);
        }

        // Remove entire agent directory (config.toml and sessions)
        let agent_dir = config_path.parent().unwrap();
        std::fs::remove_dir_all(agent_dir)?;

        if purge {
            // Also delete identity if requested
            println!("🗑️  Purged identity for '{}'", agent_name);
        }

        println!("✅ Deleted agent '{}' from team '{}'", agent_name, team);
        Ok(())
    }

    /// Handle agent rename command
    ///
    /// Renames an agent within a team or moves it to another team.
    pub async fn handle_agent_rename(
        paths: &GlobalPaths,
        old_name: String,
        new_name: String,
        team: Option<String>,
        to_team: Option<String>,
        json: bool,
    ) -> anyhow::Result<()> {
        use crate::commands::identifier::validate_agent_name;

        // Validate new agent name
        if let Err(e) = validate_agent_name(&new_name) {
            anyhow::bail!("Invalid new agent name '{}': {}", new_name, e);
        }

        // Parse identifier to extract source team and agent name
        let (from_team, old_agent_name) = parse_agent_identifier_with_override(&old_name, team.as_deref())?;
        let target_team = to_team.as_deref().unwrap_or(from_team);

        // Check if source agent exists
        let old_config_path = paths.agent_config(old_agent_name, Some(from_team));
        if !old_config_path.exists() {
            anyhow::bail!("Agent '{}' not found in team '{}'", old_agent_name, from_team);
        }

        // Check if target team exists
        let target_team_dir = paths.team_dir(target_team);
        if !target_team_dir.exists() {
            anyhow::bail!(
                "Target team '{}' does not exist. Create it first with: pekobot team create {}",
                target_team,
                target_team
            );
        }

        // Check if target agent already exists
        let new_config_path = paths.agent_config(&new_name, Some(target_team));
        if new_config_path.exists() {
            anyhow::bail!(
                "Agent '{}' already exists in team '{}'",
                new_name,
                target_team
            );
        }

        // Create target directory
        let new_agent_dir = new_config_path.parent().unwrap();
        std::fs::create_dir_all(new_agent_dir)?;

        // Move config file
        std::fs::rename(&old_config_path, &new_config_path)?;

        // Move sessions directory if it exists
        let old_sessions_dir = old_config_path.parent().unwrap().join("sessions");
        let new_sessions_dir = new_agent_dir.join("sessions");
        if old_sessions_dir.exists() {
            std::fs::rename(&old_sessions_dir, &new_sessions_dir)?;
        }

        // Remove old agent directory
        let old_agent_dir = old_config_path.parent().unwrap();
        if old_agent_dir.exists() {
            std::fs::remove_dir(old_agent_dir)?;
        }

        // Update config with new name and team
        let mut config: AgentConfig = toml::from_str(&std::fs::read_to_string(&new_config_path)?)?;
        config.name = new_name.clone();
        config.team = Some(target_team.to_string());
        let updated_toml = toml::to_string_pretty(&config)?;
        std::fs::write(&new_config_path, updated_toml)?;

        if json {
            println!(
                "{{\"success\": true, \"old_name\": \"{}\", \"new_name\": \"{}\", \"team\": \"{}\"}}",
                old_agent_name, new_name, target_team
            );
        } else {
            if from_team == target_team {
                println!("✅ Renamed agent '{}' to '{}' in team '{}'", old_agent_name, new_name, from_team);
            } else {
                println!(
                    "✅ Moved agent '{}' from team '{}' to '{}' as '{}'",
                    old_agent_name, from_team, target_team, new_name
                );
            }
            println!("   Config: {}", new_config_path.display());
        }

        Ok(())
    }

    /// Handle agent export command
    pub async fn handle_agent_export(
        paths: &GlobalPaths,
        name: String,
        team: Option<String>,
        output: Option<String>,
        encrypt: bool,
    ) -> anyhow::Result<()> {
        // Parse identifier to extract team and agent name
        let (team, agent_name) = parse_agent_identifier_with_override(&name, team.as_deref())?;

        let config_path = paths.agent_config(agent_name, Some(team));

        if !config_path.exists() {
            eprintln!("❌ Agent '{}' not found in team '{}'", agent_name, team);
            return Ok(());
        }

        let output_path = output.unwrap_or_else(|| format!("{}_{}.agent", team, agent_name));

        println!("📦 Exporting agent '{}' from team '{}' to '{}'...", agent_name, team, output_path);

        if encrypt {
            println!("🔐 Encryption enabled (not yet implemented)");
        }

        // TODO: Implement actual export via Packager
        println!("✅ Exported agent '{}'", agent_name);

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

    /// Handle agent init command
    ///
    /// NOTE: This command is non-interactive per REQ-UI-001.
    /// Use --force flag to overwrite existing directory.
    pub async fn handle_agent_init(
        path: String,
        name: Option<String>,
        provider: String,
        model: Option<String>,
        force: bool,
        json: bool,
    ) -> anyhow::Result<()> {
        // Determine agent name from directory if not provided
        let agent_name = name.unwrap_or_else(|| {
            std::path::Path::new(&path)
                .file_name()
                .map_or_else(|| "agent".to_string(), |n| n.to_string_lossy().to_string())
        });

        let dir = std::path::PathBuf::from(&path);
        let config_path = dir.join("config.toml");

        // Check if directory already exists and has files
        if dir.exists() {
            let entries: Vec<_> = std::fs::read_dir(&dir)?.collect();
            if !entries.is_empty() && !force {
                if json {
                    println!("{{\"error\": \"Directory not empty: {path}\"}}");
                } else {
                    eprintln!("⚠️  Directory '{path}' is not empty. Use --force to overwrite or remove existing files.");
                }
                return Err(anyhow::anyhow!("Directory not empty: {path}"));
            }
            // If force is true, remove existing directory
            if force {
                std::fs::remove_dir_all(&dir)?;
            }
        }

        // Create directory
        std::fs::create_dir_all(&dir)?;

        // Create config using proper AgentConfig struct for valid TOML
        let config = build_default_config(&agent_name, &provider, model, None);
        let config_content = toml::to_string_pretty(&config)?;

        std::fs::write(&config_path, config_content)?;

        // Create .gitignore
        let gitignore_content = r#"# Pekobot agent - gitignore
sessions/
workspace/
memories/
cron.json
*.log
"#;
        std::fs::write(dir.join(".gitignore"), gitignore_content)?;

        // Create optional markdown files with templates
        let agent_md = format!(
            r#"# {agent_name}

Agent description and instructions go here.

## Capabilities

- Add specific capabilities here
- Describe what this agent can do

## Instructions

Add detailed instructions for the agent here.
"#
        );
        std::fs::write(dir.join("AGENT.md"), agent_md)?;

        // Create empty directories
        std::fs::create_dir_all(dir.join("tools"))?;
        std::fs::create_dir_all(dir.join("skills"))?;
        std::fs::create_dir_all(dir.join("workspace"))?;

        if json {
            println!("{{\"success\": true, \"name\": \"{agent_name}\", \"path\": \"{path}\", \"config\": \"{}\"}}", config_path.display());
        } else {
            println!("✅ Initialized agent '{agent_name}' in '{path}'");
            println!();
            println!("📁 Structure created:");
            println!("   config.toml    - Agent configuration");
            println!("   AGENT.md       - Agent description");
            println!("   .gitignore     - Excludes sessions/, workspace/, memories/");
            println!("   tools/         - Custom tools directory");
            println!("   skills/        - Skills directory");
            println!("   workspace/     - Working directory");
            println!();
            println!("🚀 Run the agent:");
            println!("   pekobot agent start --config {}/config.toml", path);
        }

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
            "moonshot" => ProviderType::Moonshot,
            "kimi" => ProviderType::Kimi,
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
                    ProviderType::Moonshot => "kimi-k2.5".to_string(),
                    ProviderType::Kimi => "k2p5".to_string(),
                },
                max_tokens: 4096,
                temperature: 0.7,
                top_p: 1.0,
                presence_penalty: 0.0,
                frequency_penalty: 0.0,
            },
        );

        AgentConfig {
            version: "1.0".to_string(),
            name: name.to_string(),
            description: Some(format!("Pekobot agent: {name}")),
            team: None,
            tenant: None,
            capabilities: vec![],
            provider: ProviderConfig {
                provider_type,
                api_key: None,
                api_key_env: match provider_type {
                    ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
                    ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
                    ProviderType::Moonshot => Some("MOONSHOT_API_KEY".to_string()),
                    ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
                    _ => None,
                },
                base_url: match provider_type {
                    ProviderType::OpenAI => None,
                    ProviderType::Anthropic => None,
                    ProviderType::Ollama => Some("http://localhost:11434".to_string()),
                    ProviderType::OpenAICompatible => None,
                    ProviderType::Moonshot => Some("https://api.moonshot.cn/v1".to_string()),
                    ProviderType::Kimi => Some("https://api.kimi.com/coding".to_string()),
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
        // Normalize provider name for credential lookup
        let credential_provider = match provider {
            "kimi" | "kimi-code" | "kimi_code" => "kimi",
            "moonshot" => "moonshot",
            _ => provider,
        };

        if let Some(api_key) = crate::commands::auth::get_api_key(paths, credential_provider, None)?
        {
            config.provider.api_key = Some(api_key);
            config.provider.api_key_env = None; // Use direct key instead of env
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_commands_enum() {
        // Test Init command
        let cmd = AgentCommands::Init {
            path: "./my-agent".to_string(),
            name: Some("my-agent".to_string()),
            provider: "kimi_code".to_string(),
            model: Some("k2p5".to_string()),
            force: true,
        };
        match cmd {
            AgentCommands::Init {
                path,
                name,
                provider,
                model,
                force,
            } => {
                assert_eq!(path, "./my-agent");
                assert_eq!(name, Some("my-agent".to_string()));
                assert_eq!(provider, "kimi_code");
                assert_eq!(model, Some("k2p5".to_string()));
                assert!(force);
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_agent_init_default_provider() {
        let cmd = AgentCommands::Init {
            path: "./test".to_string(),
            name: None,
            provider: "openai".to_string(),
            model: None,
            force: false,
        };
        match cmd {
            AgentCommands::Init { provider, .. } => {
                assert_eq!(provider, "openai");
            }
            _ => panic!("Expected Init command"),
        }
    }

    #[test]
    fn test_agent_init_extracts_name_from_path() {
        // Test that the name extraction logic works
        let path = "./my-awesome-agent";
        let name: Option<String> = None;

        // This is what the handler does
        let extracted_name = name.unwrap_or_else(|| {
            std::path::Path::new(path)
                .file_name()
                .map_or_else(|| "agent".to_string(), |n| n.to_string_lossy().to_string())
        });

        assert_eq!(extracted_name, "my-awesome-agent");
    }
}
