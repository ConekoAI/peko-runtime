//! Principal management commands
//!
//! Principals are top-level AI actors that own identity, memory, intent,
//! governance, capabilities, and thin Markdown agent prompts. This module
//! implements the `peko principal` CLI surface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::auth::{Subject, subject_from_string_with_default_user};
use crate::commands::GlobalPaths;
use crate::common::paths::PathResolver;
use crate::ipc::{DaemonClient, ResponsePacket};
use crate::principal::{
    config::{
        PrincipalCapabilities, PrincipalConfig, PrincipalGovernanceConfig,
        PrincipalIdentityConfig, PrincipalIntentConfig, PrincipalMemoryConfig,
        PrincipalRoutingConfig,
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

    /// Export a Principal to a `.principal` package
    Export {
        /// Principal name
        name: String,

        /// Output file path (defaults to `<name>.principal`)
        #[arg(short, long)]
        output: Option<String>,

        /// Include session history in the package
        #[arg(long)]
        include_sessions: bool,

        /// Embed extension packages referenced by the Principal
        #[arg(long)]
        with_extensions: bool,
    },

    /// Import a Principal from a `.principal` package
    Import {
        /// Path to the `.principal` package
        file_path: String,

        /// Rename the imported Principal
        #[arg(short, long)]
        name: Option<String>,

        /// Allow importing an unsigned package
        #[arg(long)]
        allow_unsigned: bool,
    },

    /// Push a Principal package to a registry
    Push {
        /// Principal name
        name: String,

        /// Registry host (defaults to workspace config)
        #[arg(long)]
        registry_host: Option<String>,

        /// Registry auth token
        #[arg(long)]
        registry_token: Option<String>,
    },

    /// Pull a Principal package from a registry and import it
    Pull {
        /// Registry reference (e.g. `owner/principal:version`)
        registry_ref: String,

        /// Rename the imported Principal
        #[arg(short, long)]
        name: Option<String>,

        /// Overwrite an existing Principal with the same name
        #[arg(short, long)]
        force: bool,

        /// Registry host (defaults to workspace config)
        #[arg(long)]
        registry_host: Option<String>,

        /// Registry auth token
        #[arg(long)]
        registry_token: Option<String>,
    },

    /// Grant a permission on a Principal
    Permit {
        /// Principal name
        name: String,

        /// Subject to grant permission to (e.g. `user:alice`, `public`)
        subject: String,

        /// Permission to grant (e.g. `chat`, `manage_settings`)
        permission: String,
    },

    /// Revoke a permission from a Principal
    Revoke {
        /// Principal name
        name: String,

        /// Subject to revoke permission from
        subject: String,

        /// Permission to revoke
        permission: String,
    },

    /// List permissions on a Principal
    Permissions {
        /// Principal name
        name: String,
    },

    /// Manage agents (prompts) inside a Principal
    #[command(subcommand)]
    Agent(PrincipalAgentCommands),

    /// Inspect Principal memory
    #[command(subcommand)]
    Memory(PrincipalMemoryCommands),
}

/// Subcommands for `peko principal agent`.
#[derive(Subcommand)]
pub enum PrincipalAgentCommands {
    /// List agent prompts in a Principal
    List {
        /// Principal name
        name: String,
    },

    /// Show an agent prompt
    Show {
        /// Principal name
        name: String,

        /// Agent prompt name
        agent: String,
    },
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
        PrincipalCommands::Export {
            name,
            output,
            include_sessions,
            with_extensions,
        } => export_principal(&name, output, include_sessions, with_extensions).await,
        PrincipalCommands::Import {
            file_path,
            name,
            allow_unsigned,
        } => import_principal(&file_path, name, allow_unsigned).await,
        PrincipalCommands::Push {
            name,
            registry_host,
            registry_token,
        } => push_principal(&name, registry_host, registry_token).await,
        PrincipalCommands::Pull {
            registry_ref,
            name,
            force,
            registry_host,
            registry_token,
        } => pull_principal(&registry_ref,
            name,
            force,
            registry_host,
            registry_token,
        )
        .await,
        PrincipalCommands::Permit {
            name,
            subject,
            permission,
        } => grant_permission(&name, &subject, &permission).await,
        PrincipalCommands::Revoke {
            name,
            subject,
            permission,
        } => revoke_permission(&name, &subject, &permission).await,
        PrincipalCommands::Permissions { name } => list_permissions(&name).await,
        PrincipalCommands::Agent(PrincipalAgentCommands::List { name }) => {
            list_principal_agents(&name, paths).await
        }
        PrincipalCommands::Agent(PrincipalAgentCommands::Show { name, agent }) => {
            show_principal_agent(&name, &agent, paths).await
        }
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
        name,
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

    let (display_name, did) = {
        let config = principal.config.read().await;
        (
            config.identity.display_name.clone().unwrap_or_else(|| config.name.clone()),
            config.did.clone(),
        )
    };
    let did_str = did.map(|d| d.0).unwrap_or_else(|| "(none)".to_string());

    println!("Principal: {}", display_name);
    println!("  DID:     {}", did_str);
    println!("  Workspace: {}", principal.workspace_path.display());
    println!("  Agents:");
    for (agent_name, prompt) in &principal.agent_prompts {
        let desc = prompt
            .frontmatter
            .description
            .as_deref()
            .unwrap_or("(no description)");
        println!("    - {} ({}): {desc}", agent_name, prompt.path.display());
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

async fn export_principal(
    name: &str,
    output: Option<String>,
    include_sessions: bool,
    with_extensions: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_export(name, output, include_sessions, with_extensions)
        .await?;

    match response {
        ResponsePacket::PrincipalExported {
            name,
            output_path,
            ..
        } => {
            println!("Exported principal '{name}' to {output_path}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to export principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn import_principal(
    file_path: &str,
    name: Option<String>,
    allow_unsigned: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_import(file_path, name, allow_unsigned)
        .await?;

    match response {
        ResponsePacket::PrincipalImported { name, config_path, .. } => {
            println!("Imported principal '{name}' at {config_path}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to import principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn push_principal(
    name: &str,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_push(name, registry_host, registry_token)
        .await?;

    match response {
        ResponsePacket::PrincipalPushed { name, digest, .. } => {
            println!("Pushed principal '{name}' (digest {digest})");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to push principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn pull_principal(
    registry_ref: &str,
    name: Option<String>,
    force: bool,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_pull(registry_ref, name, force, registry_host, registry_token)
        .await?;

    match response {
        ResponsePacket::PrincipalPulled {
            name,
            version,
            digest,
            ..
        } => {
            println!("Pulled principal '{name}' {version} (digest {digest})");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to pull principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

fn parse_permission(value: &str) -> Result<crate::auth::Permission> {
    match value.to_lowercase().as_str() {
        "chat" => Ok(crate::auth::Permission::Chat),
        "view_settings" | "view-settings" | "viewsettings" => {
            Ok(crate::auth::Permission::ViewSettings)
        }
        "manage_settings" | "manage-settings" | "managesettings" => {
            Ok(crate::auth::Permission::ManageSettings)
        }
        "manage_extensions" | "manage-extensions" | "manageextensions" => {
            Ok(crate::auth::Permission::ManageExtensions)
        }
        "manage_members" | "manage-members" | "managemembers" => {
            Ok(crate::auth::Permission::ManageMembers)
        }
        "expose" => Ok(crate::auth::Permission::Expose),
        "delete" => Ok(crate::auth::Permission::Delete),
        other => anyhow::bail!("Unknown permission: {other}"),
    }
}

async fn grant_permission(
    name: &str,
    subject_str: &str,
    permission_str: &str,
) -> Result<()> {
    let subject = subject_from_string_with_default_user(subject_str);
    let permission = parse_permission(permission_str)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_grant_permission(name, subject.clone(), permission.clone())
        .await?;

    match response {
        ResponsePacket::PrincipalPermissionGranted {
            name,
            subject,
            permission,
            ..
        } => {
            println!(
                "Granted {:?} on '{}' to {}",
                permission,
                name,
                subject
            );
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to grant permission: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn revoke_permission(
    name: &str,
    subject_str: &str,
    permission_str: &str,
) -> Result<()> {
    let subject = subject_from_string_with_default_user(subject_str);
    let permission = parse_permission(permission_str)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_revoke_permission(name, subject.clone(), permission.clone())
        .await?;

    match response {
        ResponsePacket::PrincipalPermissionRevoked {
            name,
            subject,
            permission,
            ..
        } => {
            println!(
                "Revoked {:?} on '{}' from {}",
                permission,
                name,
                subject
            );
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to revoke permission: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn list_permissions(name: &str) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client.principal_permissions(name).await?;

    match response {
        ResponsePacket::PrincipalPermissions { permissions, .. } => {
            if permissions.is_empty() {
                println!("No permissions granted on principal '{name}'.");
                return Ok(());
            }
            println!("Permissions on principal '{name}':");
            for grant in permissions {
                println!(
                    "  {:?} for {} (granted by {} at {})",
                    grant.permission, grant.subject, grant.granted_by, grant.granted_at
                );
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to list permissions: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn list_principal_agents(name: &str, paths: &GlobalPaths) -> Result<()> {
    let agents_dir = paths.principal_agents_dir(name);
    if !agents_dir.exists() {
        println!("No agents found for principal '{name}'.");
        return Ok(());
    }

    let mut entries = tokio::fs::read_dir(&agents_dir).await?;
    let mut found = false;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            println!("{stem}");
            found = true;
        }
    }

    if !found {
        println!("No agents found for principal '{name}'.");
    }
    Ok(())
}

async fn show_principal_agent(
    name: &str,
    agent: &str,
    paths: &GlobalPaths,
) -> Result<()> {
    let agents_dir = paths.principal_agents_dir(name);
    let mut candidates = vec![agents_dir.join(format!("{agent}.md"))];
    if !agent.ends_with(".md") {
        candidates.push(agents_dir.join(format!("{agent}.toml")));
    }

    let path = candidates.into_iter().find(|p| p.exists());
    let path = match path {
        Some(p) => p,
        None => {
            anyhow::bail!("Agent '{agent}' not found in principal '{name}'");
        }
    };

    let content = tokio::fs::read_to_string(&path).await?;
    println!("{}", content);
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

    let resolver = PathResolver::from_overrides(
        Some(paths.config_dir.clone()),
        Some(paths.data_dir.clone()),
        Some(paths.cache_dir.clone()),
    );

    PrincipalManager::with_path_resolver(
        root,
        resolver,
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
        exposure: crate::tunnel::protocol::InstanceExposure::Private,
        permissions: Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Permission;
    use crate::commands::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn parse_permission_maps_common_names() {
        assert_eq!(parse_permission("chat").unwrap(), Permission::Chat);
        assert_eq!(
            parse_permission("view-settings").unwrap(),
            Permission::ViewSettings
        );
        assert_eq!(
            parse_permission("ManageSettings").unwrap(),
            Permission::ManageSettings
        );
        assert_eq!(parse_permission("EXPOSE").unwrap(), Permission::Expose);
    }

    #[test]
    fn parse_permission_rejects_unknown() {
        assert!(parse_permission("fly").is_err());
    }

    #[test]
    fn principal_permit_parses_positional_args() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "permit",
            "myprincipal",
            "user:alice",
            "chat",
        ])
        .expect("should parse principal permit");

        match cli.command {
            Commands::Principal(PrincipalCommands::Permit {
                name,
                subject,
                permission,
            }) => {
                assert_eq!(name, "myprincipal");
                assert_eq!(subject, "user:alice");
                assert_eq!(permission, "chat");
            }
            _other => panic!("expected Principal permit command"),
        }
    }

    #[test]
    fn principal_agent_show_parses() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "agent",
            "show",
            "myprincipal",
            "primary",
        ])
        .expect("should parse principal agent show");

        match cli.command {
            Commands::Principal(PrincipalCommands::Agent(PrincipalAgentCommands::Show {
                name,
                agent,
            })) => {
                assert_eq!(name, "myprincipal");
                assert_eq!(agent, "primary");
            }
            _other => panic!("expected Principal agent show command"),
        }
    }

    #[test]
    fn default_agent_prompt_contains_name() {
        let prompt = default_agent_prompt("spot");
        assert!(prompt.contains("spot"));
    }
}
