//! Principal management commands
//!
//! Principals are top-level AI actors that own identity, memory, intent,
//! governance, capabilities, and thin Markdown agent prompts. This module
//! implements the `peko principal` CLI surface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::auth::Subject;
use crate::commands::GlobalPaths;
use crate::principal::{
    config::{
        AgentRole, PrincipalAgentRef, PrincipalCapabilities, PrincipalConfig,
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    },
    factory::{DefaultPrincipalRouterFactory, PrincipalMemoryFactory},
    memory::{DefaultPrincipalMemory, PrincipalMemory},
    router::{ChannelContext, ChannelKind},
    PrincipalManager,
};

/// Subcommands for `peko principal`.
#[derive(Subcommand)]
pub enum PrincipalCommands {
    /// Create a new Principal
    Create {
        /// Principal name
        name: String,
    },

    /// List Principals
    List,

    /// Show Principal configuration and agent prompts
    Show {
        /// Principal name
        name: String,
    },

    /// Send a message to a Principal
    Send {
        /// Principal name
        name: String,

        /// Message to send
        message: String,
    },

    /// Inspect Principal memory
    #[command(subcommand)]
    Memory(PrincipalMemoryCommands),
}

/// Subcommands for `peko principal memory`.
#[derive(Subcommand)]
pub enum PrincipalMemoryCommands {
    /// List sessions
    Session {
        /// Principal name
        name: String,
    },
}

/// Dispatch `peko principal` commands.
pub async fn handle_principal(
    cmd: PrincipalCommands,
    paths: &GlobalPaths,
    _json: bool,
) -> Result<()> {
    match cmd {
        PrincipalCommands::Create { name } => create_principal(&name, paths).await,
        PrincipalCommands::List => list_principals(paths).await,
        PrincipalCommands::Show { name } => show_principal(&name, paths).await,
        PrincipalCommands::Send { name, message } => send_to_principal(&name, &message, paths).await,
        PrincipalCommands::Memory(PrincipalMemoryCommands::Session { name }) => {
            list_principal_sessions(&name, paths).await
        }
    }
}

async fn create_principal(name: &str, paths: &GlobalPaths) -> Result<()> {
    let manager = build_manager(paths);

    // Prepare the workspace and default agent prompt before registering the
    // Principal, because `PrincipalManager::create` loads and validates the
    // agent prompts immediately.
    let workspace_path = paths.principal_dir(name);
    let agents_dir = workspace_path.join("agents");
    tokio::fs::create_dir_all(&agents_dir).await?;
    let prompt_path = agents_dir.join("primary.md");
    let prompt_body = default_agent_prompt(name);
    tokio::fs::write(&prompt_path, prompt_body).await?;

    let config = default_principal_config(name);
    let principal = manager.create(config).await?;

    println!(
        "Created principal '{}' at {}",
        principal.config.name,
        principal.workspace_path.display()
    );
    Ok(())
}

async fn list_principals(paths: &GlobalPaths) -> Result<()> {
    let root = paths.principals_root_dir();
    if !root.exists() {
        println!("No principals found.");
        return Ok(());
    }

    let mut entries = tokio::fs::read_dir(root).await?;
    let mut found = false;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            let config_path = path.join("principal.toml");
            if config_path.exists() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                println!("{name}");
                found = true;
            }
        }
    }

    if !found {
        println!("No principals found.");
    }
    Ok(())
}

async fn show_principal(name: &str, paths: &GlobalPaths) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    println!("Principal: {}", principal.config.name);
    println!("  DID:     {}", principal.did().0);
    println!("  Workspace: {}", principal.workspace_path.display());
    println!("  Agents:");
    for agent_ref in &principal.config.agents {
        let prompt = principal.agent_prompt(&agent_ref.name);
        let desc = prompt
            .and_then(|p| p.frontmatter.description.as_deref())
            .unwrap_or("(no description)");
        println!("    - {} ({}): {desc}", agent_ref.name, agent_ref.prompt.display());
    }
    Ok(())
}

async fn send_to_principal(name: &str, message: &str, paths: &GlobalPaths) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    let peer = Subject::User(paths.user().to_string());
    let channel = ChannelContext {
        kind: ChannelKind::Cli,
        streaming: false,
    };

    let response = manager
        .receive(principal.id.clone(), peer, message.to_string(), channel)
        .await
        .context("principal receive failed")?;

    println!("{}", response.content);
    Ok(())
}

async fn list_principal_sessions(name: &str, paths: &GlobalPaths) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    let sessions = principal.memory.list_sessions().await?;
    if sessions.is_empty() {
        println!("No sessions found for principal '{name}'.");
        return Ok(());
    }

    for s in sessions {
        let title = s.title.as_deref().unwrap_or("untitled");
        println!("{} [{}] {}", s.session_id, s.peer, title);
    }
    Ok(())
}

async fn load_principal(
    name: &str,
    manager: &PrincipalManager,
    paths: &GlobalPaths,
) -> Result<Arc<crate::principal::Principal>> {
    if let Some(p) = manager.get_by_name(name).await {
        return Ok(p);
    }

    let config_path = paths.principal_config(name);
    if !config_path.exists() {
        anyhow::bail!("principal '{name}' not found");
    }

    manager.load(&config_path).await.context("failed to load principal")
}

fn build_manager(paths: &GlobalPaths) -> PrincipalManager {
    let root = paths.principals_root_dir();
    let _ = std::fs::create_dir_all(&root);

    PrincipalManager::new(
        root,
        Arc::new(CliPrincipalMemoryFactory {
            data_dir: paths.data_dir.clone(),
        }),
        Arc::new(DefaultPrincipalRouterFactory),
    )
}

fn default_principal_config(name: &str) -> PrincipalConfig {
    PrincipalConfig {
        name: name.to_string(),
        did: None,
        owner: Subject::User("default".to_string()),
        identity: PrincipalIdentityConfig {
            display_name: Some(name.to_string()),
            description: Some(format!("The {name} Principal")),
            avatar: None,
        },
        intent: PrincipalIntentConfig::default(),
        governance: PrincipalGovernanceConfig::default(),
        memory: PrincipalMemoryConfig::default(),
        routing: PrincipalRoutingConfig::default(),
        capabilities: PrincipalCapabilities::default(),
        agents: vec![PrincipalAgentRef {
            name: "primary".to_string(),
            prompt: PathBuf::from("agents/primary.md"),
            role: AgentRole::Default,
        }],
    }
}

fn default_agent_prompt(name: &str) -> String {
    format!(
        "---\ndescription: \"Default assistant for {name}\"\n---\n\n\
        You are {name}, a helpful AI assistant. Respond to the caller's message concisely.\n"
    )
}

/// Memory factory that places Principal memory under the data directory,
/// outside the config directory where `principal.toml` lives.
struct CliPrincipalMemoryFactory {
    data_dir: PathBuf,
}

#[async_trait::async_trait]
impl PrincipalMemoryFactory for CliPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &crate::principal::PrincipalId,
        workspace_path: &Path,
    ) -> Arc<dyn crate::principal::PrincipalMemory> {
        let name = workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let memory_dir = self.data_dir.join("principals").join(name).join("memory");
        let _ = tokio::fs::create_dir_all(&memory_dir).await;
        let memory = DefaultPrincipalMemory::new(memory_dir);
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}
