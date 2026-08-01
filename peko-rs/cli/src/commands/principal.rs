//! Principal management commands
//!
//! Principals are top-level AI actors that own identity, memory, intent,
//! governance, capabilities, and thin Markdown agent prompts. This module
//! implements the `peko principal` CLI surface.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::commands::GlobalPaths;
use peko_auth::{subject_from_string_with_default_user, Subject};
use peko_core::common::authority::TierPath;
use peko_core::common::paths::PathResolver;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_core::principal::config::{
    PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
    PrincipalMemoryConfig, PrincipalRoutingConfig,
};
use peko_core::principal::memory::{DefaultPrincipalMemory, PrincipalMemory};
use peko_core::principal::{
    factory::{DefaultPrincipalRouterFactory, PrincipalMemoryFactory},
    router::{ChannelContext, ChannelKind},
    PrincipalManager,
};
use peko_extension_api::Capabilities;

/// Subcommands for `peko principal`.
#[derive(Subcommand)]
pub enum PrincipalCommands {
    /// Create a new Principal
    Create {
        /// Principal name
        name: String,

        /// Configured model id to pin this principal to (see
        /// `peko model list`). Required: there is no runtime default
        /// model, so an unpinned principal fails every send with
        /// "no model configured".
        #[arg(long, value_name = "MODEL_ID")]
        model: String,

        /// Bypass the overwrite guard. Without `--force`,
        /// `peko principal create <existing>` refuses with a clear
        /// error — protects identity, agents, memory, and session
        /// history from a one-keystroke wipe (see Bug 2 in
        /// scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md).
        /// With `--force`, `create` will proceed past the guard but
        /// the existing on-disk principal is NOT destroyed — to
        /// fully replace a principal, run `peko principal remove`
        /// first.
        #[arg(short, long)]
        force: bool,
    },

    /// List Principals
    List,

    /// Show Principal configuration and agent prompts
    Show {
        /// Principal name
        name: String,
    },

    /// Remove a Principal and all its data
    Remove {
        /// Principal name
        name: String,

        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
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

        /// Override trust pinning if the package's DID differs from a
        /// previously imported package with the same name.
        #[arg(short, long)]
        force: bool,

        /// Skip the preview confirmation prompt
        #[arg(long)]
        yes: bool,
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

        /// Skip the preview confirmation prompt
        #[arg(long)]
        yes: bool,

        /// Allow pulling an unsigned package
        #[arg(long)]
        allow_unsigned: bool,

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

    /// Mint a signed invite link for a Principal (share with one friend)
    Invite {
        /// Principal name
        name: String,

        /// Comma-separated permissions to grant via the invite
        /// (e.g. `chat`). Defaults to `chat` when omitted.
        #[arg(long, value_name = "SCOPE", value_delimiter = ',')]
        scope: Vec<String>,

        /// Time-to-live, parsed as a duration string (e.g. `7d`, `24h`,
        /// `30m`). Defaults to `7d`. Hard-capped at 30 days by the
        /// daemon.
        #[arg(long, value_name = "TTL", default_value = "7d")]
        ttl: String,
    },

    /// Revoke a previously minted invite token
    RevokeInvite {
        /// Principal name
        name: String,

        /// The `jti` (UUID) printed by `peko principal invite`
        jti: String,
    },

    /// Manage agents (prompts) inside a Principal
    #[command(subcommand)]
    Agent(PrincipalAgentCommands),
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

/// Dispatch `peko principal` commands.
pub async fn handle_principal(
    cmd: PrincipalCommands,
    paths: &GlobalPaths,
    _json: bool,
) -> Result<()> {
    match cmd {
        PrincipalCommands::Create { name, model, force } => {
            create_principal(&name, &model, force, paths).await
        }
        PrincipalCommands::List => list_principals(paths).await,
        PrincipalCommands::Show { name } => show_principal(&name, paths, _json).await,
        PrincipalCommands::Remove { name, yes } => remove_principal(&name, yes, paths).await,
        PrincipalCommands::Send { name, message } => {
            send_to_principal(&name, &message, paths).await
        }
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
            force,
            yes,
        } => import_principal(&file_path, name, allow_unsigned, force, yes).await,
        PrincipalCommands::Push {
            name,
            registry_host,
            registry_token,
        } => push_principal(&name, registry_host, registry_token).await,
        PrincipalCommands::Pull {
            registry_ref,
            name,
            force,
            yes,
            allow_unsigned,
            registry_host,
            registry_token,
        } => {
            pull_principal(
                &registry_ref,
                name,
                force,
                yes,
                allow_unsigned,
                registry_host,
                registry_token,
            )
            .await
        }
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
        PrincipalCommands::Invite { name, scope, ttl } => {
            mint_invite(&name, scope, &ttl).await
        }
        PrincipalCommands::RevokeInvite { name, jti } => revoke_invite(&name, &jti).await,
        PrincipalCommands::Agent(PrincipalAgentCommands::List { name }) => {
            list_principal_agents(&name, paths).await
        }
        PrincipalCommands::Agent(PrincipalAgentCommands::Show { name, agent }) => {
            show_principal_agent(&name, &agent, paths).await
        }
    }
}

async fn create_principal(
    name: &str,
    model_id: &str,
    force: bool,
    paths: &GlobalPaths,
) -> Result<()> {
    use peko_providers::catalog::ModelCatalog;

    let manager = build_manager(paths);

    // Refuse to silently overwrite an existing principal — see Bug 2 in
    // scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
    // Without this guard, a non-technical user running
    //   peko principal create scout --model …
    // a second time wipes identity, agents, memory, and session history
    // without any prompt. `--force` is the explicit override.
    let shared_layout = paths.resolver().principal_layout(name).shared;
    if shared_layout.config_file.exists() && !force {
        anyhow::bail!(
            "principal '{name}' already exists at {}.\n\
             Refusing to overwrite an existing principal.\n\
             To replace it, remove it first and then re-create:\n  \
             peko principal remove {name}\n  \
             peko principal create {name} --model <MODEL_ID>",
            shared_layout.root.display()
        );
    }

    // Enforce the model pin at creation: there is no runtime default
    // model, so an unpinned principal would fail every send with
    // "no model configured". Validate against the catalog now so a
    // typo'd id fails here instead of at first use.
    let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
    let catalog = ModelCatalog::load_or_init(&cat_path).await?;
    if catalog.get_enabled(model_id).await.is_none() {
        anyhow::bail!(
            "model '{model_id}' is not in the catalog (or is disabled).\n\
             Add one with: peko model add --template <anthropic|openai|ollama|...> --model <wire-id> --key \"$YOUR_KEY\"\n\
             Or list configured models: peko model list"
        );
    }

    // Prepare the workspace and default agent prompt before registering the
    // Principal, because `PrincipalManager::create` loads and validates the
    // agent prompts immediately.
    //
    // Phase B: agents live in the Shared tier
    // (`{config_dir}/principals/{name}/agents/`). The CLI resolves
    // via the typed `SharedLayout` — the `RuntimeAuthority` accessor
    // requires a `PrincipalId`, but at create time the principal
    // doesn't exist yet, so we use the resolver directly. The CLI
    // operates from `Subject::User`, which is entitled to write its
    // own principal's shared state.
    let agents_dir = paths.resolver().principal_layout(name).shared.agents_dir;
    tokio::fs::create_dir_all(&agents_dir).await?;
    let prompt_path = agents_dir.join("primary.md");
    let prompt_body = default_agent_prompt(name);
    tokio::fs::write(&prompt_path, prompt_body).await?;

    let mut config = default_principal_config(name);
    config.preferred_model_id = Some(model_id.to_string());
    let principal = manager.create(config).await?;

    println!(
        "Created principal '{}' at {} (model: {model_id})",
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

async fn show_principal(name: &str, paths: &GlobalPaths, json: bool) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    let (display_name, did, preferred_model_id) = {
        let config = principal.config.read().await;
        (
            config
                .identity
                .display_name
                .clone()
                .unwrap_or_else(|| config.name.clone()),
            config.did.clone(),
            config.preferred_model_id.clone(),
        )
    };

    if json {
        // Structured view — same shape as `peko log --json` for
        // downstream tooling. `path` is the absolute workspace path;
        // `agents` is an array of {name, description, prompt_path}.
        // Bug 3 in scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md
        // notes that previously `--json` was silently ignored here.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AgentView<'a> {
            name: &'a str,
            description: &'a str,
            prompt_path: &'a std::path::PathBuf,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ShowView<'a> {
            name: &'a str,
            display_name: &'a str,
            did: Option<&'a str>,
            workspace: &'a std::path::Path,
            preferred_model_id: Option<&'a str>,
            agents: Vec<AgentView<'a>>,
        }
        let agents: Vec<AgentView> = principal
            .agent_prompts
            .iter()
            .map(|(agent_name, prompt)| AgentView {
                name: agent_name,
                description: prompt.frontmatter.description.as_deref().unwrap_or(""),
                prompt_path: &prompt.path,
            })
            .collect();
        let view = ShowView {
            name: &principal.config.read().await.name,
            display_name: &display_name,
            did: did.as_ref().map(|d| d.0.as_str()),
            workspace: &principal.workspace_path,
            preferred_model_id: preferred_model_id.as_deref(),
            agents,
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let did_str = did.map(|d| d.0).unwrap_or_else(|| "(none)".to_string());

    println!("Principal: {}", display_name);
    println!("  DID:     {}", did_str);
    println!("  Workspace: {}", principal.workspace_path.display());

    // Show where the root agent will route its LLM calls. Surface the
    // pinned configured model so users can confirm their override took
    // effect without trawling config files.
    let model_line = match preferred_model_id {
        Some(id) => format!("{id} (per-principal)"),
        None => "(none — run `peko model add` and pin a model)".to_string(),
    };
    println!("  Model: {model_line}");

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

async fn remove_principal(name: &str, yes: bool, paths: &GlobalPaths) -> Result<()> {
    let manager = build_manager(paths);
    // Load first so the principal is registered in the in-memory manager,
    // and so a missing principal fails with a clear error before prompting.
    let _principal = load_principal(name, &manager, paths).await?;

    if !yes && !confirm_prompt(&format!("Remove principal '{name}' and all its data?"))? {
        println!("Remove cancelled.");
        return Ok(());
    }

    manager.remove(name).await?;
    println!("Removed principal '{name}'");
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
        .receive(
            principal.id.clone(),
            peer,
            message.to_string(),
            channel,
            None,
        )
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
            name, output_path, ..
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
    force: bool,
    yes: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;

    // Always preview first. The preview is read-only and surfaces bundled
    // agents, extensions, signature status, validation issues, and the
    // capabilities that the package's extensions require.
    let preview = client
        .principal_import_preview(file_path, name.clone(), allow_unsigned, force)
        .await?;

    let preview = decode_preview_response(preview, "import")?;

    // Default to granting nothing, even for signed packages. A signature
    // verifies integrity, not intent; the user must opt in to each requested
    // capability (or pre-grant them via `peko capability grant`).
    let default_select_all = false;
    let selected_capabilities = if yes {
        apply_capability_defaults(&preview.required_capabilities, default_select_all)
    } else {
        prompt_capability_selection(&preview.required_capabilities, default_select_all)?
    };

    render_import_preview(&preview, &selected_capabilities);

    if !preview.validation_errors.is_empty() && !force {
        anyhow::bail!(
            "Package has validation errors. Use --force to import anyway, or fix the package."
        );
    }

    if !yes && !confirm_prompt("Import this principal?")? {
        println!("Import cancelled.");
        return Ok(());
    }

    let response = client
        .principal_import(
            file_path,
            name,
            allow_unsigned,
            force,
            true,
            selected_capabilities,
        )
        .await?;

    match response {
        ResponsePacket::PrincipalImported {
            name, config_path, ..
        } => {
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

/// Internal summary of a principal package preview response, used to keep
/// the rendering and selection code tidy.
struct PrincipalImportPreview {
    name: String,
    version: String,
    did: String,
    description: Option<String>,
    agents: Vec<String>,
    extensions: Vec<String>,
    required_capabilities: Vec<String>,
    signed: bool,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
}

/// Decode either a local import preview response or a remote pull preview
/// response into the shared `PrincipalImportPreview` struct.
fn decode_preview_response(
    response: ResponsePacket,
    operation: &str,
) -> Result<PrincipalImportPreview> {
    match response {
        ResponsePacket::PrincipalImportPreviewed {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
            ..
        }
        | ResponsePacket::PrincipalPullPreviewed {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
            ..
        } => Ok(PrincipalImportPreview {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
        }),
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to preview principal {operation}: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

fn render_import_preview(preview: &PrincipalImportPreview, selected_capabilities: &[String]) {
    println!("Principal import preview:");
    println!("  Name:        {}", preview.name);
    println!("  Version:     {}", preview.version);
    println!("  DID:         {}", preview.did);
    if let Some(desc) = &preview.description {
        println!("  Description: {desc}");
    }
    println!(
        "  Signed:      {}",
        if preview.signed { "yes" } else { "no" }
    );

    if preview.agents.is_empty() {
        println!("  Agents:      (none)");
    } else {
        println!("  Agents:");
        for agent in &preview.agents {
            println!("    - {agent}");
        }
    }

    if preview.extensions.is_empty() {
        println!("  Extensions:  (none)");
    } else {
        println!("  Extensions:");
        for ext in &preview.extensions {
            println!("    - {ext}");
        }
    }

    if preview.required_capabilities.is_empty() {
        println!("  Required capabilities: (none)");
    } else {
        println!("  Required capabilities:");
        for cap in &preview.required_capabilities {
            let mark = if selected_capabilities.contains(cap) {
                "[x]"
            } else {
                "[ ]"
            };
            println!("    {mark} {cap}");
        }
    }

    if !preview.validation_warnings.is_empty() {
        println!("  Warnings:");
        for warning in &preview.validation_warnings {
            println!("    ⚠️  {warning}");
        }
    }

    if !preview.validation_errors.is_empty() {
        println!("  Errors:");
        for error in &preview.validation_errors {
            println!("    ❌ {error}");
        }
    }
}

/// Return the full required capability list if `select_all` is true,
/// otherwise an empty list. Used by `--yes` to accept defaults.
fn apply_capability_defaults(required: &[String], select_all: bool) -> Vec<String> {
    if select_all {
        required.to_vec()
    } else {
        Vec::new()
    }
}

/// Interactively prompt the user to toggle each required capability.
/// `default_enabled` controls the default answer for each item.
fn prompt_capability_selection(required: &[String], default_enabled: bool) -> Result<Vec<String>> {
    if required.is_empty() {
        return Ok(Vec::new());
    }

    println!("\nSelect capabilities to grant to the imported Principal:");
    let mut selected = Vec::new();
    for cap in required {
        let default_label = if default_enabled { "Y/n" } else { "y/N" };
        print!("  Grant {cap}? [{default_label}] ");
        std::io::Write::flush(&mut std::io::stdout())
            .with_context(|| "failed to flush capability prompt")?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .with_context(|| "failed to read capability selection")?;
        let answer = answer.trim();
        let enabled = if answer.is_empty() {
            default_enabled
        } else {
            answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
        };
        if enabled {
            selected.push(cap.clone());
        }
    }
    Ok(selected)
}

/// Ask a yes/no question and return the answer.
fn confirm_prompt(message: &str) -> Result<bool> {
    print!("{message} [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())
        .with_context(|| "failed to flush confirmation prompt")?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .with_context(|| "failed to read confirmation")?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
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
    yes: bool,
    allow_unsigned: bool,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> Result<()> {
    let client = DaemonClient::connect().await?;

    // Always preview first so the user can review capabilities before the
    // remote package is imported.
    let preview = client
        .principal_pull_preview(
            registry_ref,
            name.clone(),
            force,
            registry_host.clone(),
            registry_token.clone(),
        )
        .await?;

    let preview = decode_preview_response(preview, "pull")?;

    // Default to granting nothing, even for signed packages. A signature
    // verifies integrity, not intent; the user must opt in to each requested
    // capability (or pre-grant them via `peko capability grant`).
    let selected_capabilities = if yes {
        apply_capability_defaults(&preview.required_capabilities, false)
    } else {
        prompt_capability_selection(&preview.required_capabilities, false)?
    };

    render_import_preview(&preview, &selected_capabilities);

    if !preview.validation_errors.is_empty() && !force {
        anyhow::bail!(
            "Package has validation errors. Use --force to import anyway, or fix the package."
        );
    }

    if !yes && !confirm_prompt("Pull and import this principal?")? {
        println!("Pull cancelled.");
        return Ok(());
    }

    let response = client
        .principal_pull(
            registry_ref,
            name,
            force,
            true,
            selected_capabilities,
            allow_unsigned,
            registry_host,
            registry_token,
        )
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

fn parse_permission(value: &str) -> Result<peko_auth::Permission> {
    match value.to_lowercase().as_str() {
        "chat" => Ok(peko_auth::Permission::Chat),
        "view_settings" | "view-settings" | "viewsettings" => {
            Ok(peko_auth::Permission::ViewSettings)
        }
        "manage_settings" | "manage-settings" | "managesettings" => {
            Ok(peko_auth::Permission::ManageSettings)
        }
        "manage_extensions" | "manage-extensions" | "manageextensions" => {
            Ok(peko_auth::Permission::ManageExtensions)
        }
        "manage_members" | "manage-members" | "managemembers" => {
            Ok(peko_auth::Permission::ManageMembers)
        }
        "expose" => Ok(peko_auth::Permission::Expose),
        "delete" => Ok(peko_auth::Permission::Delete),
        other => anyhow::bail!("Unknown permission: {other}"),
    }
}

async fn grant_permission(name: &str, subject_str: &str, permission_str: &str) -> Result<()> {
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
            println!("Granted {:?} on '{}' to {}", permission, name, subject);
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

async fn revoke_permission(name: &str, subject_str: &str, permission_str: &str) -> Result<()> {
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
            println!("Revoked {:?} on '{}' from {}", permission, name, subject);
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

/// Parse a human-friendly duration string (`30m`, `24h`, `7d`) into
/// seconds. Used by `peko principal invite --ttl <value>`. Bare
/// integers are treated as seconds.
fn parse_ttl_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("TTL cannot be empty");
    }
    let (num, unit) = if let Some(rest) = s.strip_suffix('d') {
        (rest, 'd')
    } else if let Some(rest) = s.strip_suffix('h') {
        (rest, 'h')
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 'm')
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 's')
    } else {
        (s, 's')
    };
    let n: u64 = num.parse().map_err(|_| anyhow::anyhow!("Invalid TTL: {s:?}"))?;
    let secs = match unit {
        's' => n,
        'm' => n.checked_mul(60).ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        'h' => n
            .checked_mul(60 * 60)
            .ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        'd' => n
            .checked_mul(24 * 60 * 60)
            .ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        _ => unreachable!(),
    };
    Ok(secs)
}

async fn mint_invite(name: &str, scope: Vec<String>, ttl: &str) -> Result<()> {
    // Default to `chat` when the caller doesn't pass `--scope` so a
    // bare `peko principal invite alice` still produces a usable link.
    let scope_strs: Vec<String> = if scope.is_empty() {
        vec!["chat".to_string()]
    } else {
        scope
    };
    let permissions: Vec<peko_auth::Permission> = scope_strs
        .iter()
        .map(|s| parse_permission(s))
        .collect::<Result<_>>()?;
    let ttl_secs = parse_ttl_duration(ttl)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_mint_invite(name, permissions, ttl_secs)
        .await?;

    match response {
        ResponsePacket::PrincipalInviteMinted {
            name,
            token,
            url,
            claims,
            ..
        } => {
            println!("Minted invite for principal '{name}'");
            println!("  jti:  {}", claims.jti);
            println!("  exp:  {} ({}s from now)", claims.exp, claims.exp.timestamp() - chrono::Utc::now().timestamp());
            println!("  scope: {:?}", claims.scope);
            println!();
            println!("Share this URL with one friend:");
            println!("  {url}");
            // The token itself is also printed so the owner can use
            // it in scripts / curl examples. Anyone holding the
            // token can chat until the owner revokes it.
            println!();
            println!("Token only:");
            println!("  {token}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to mint invite: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn revoke_invite(name: &str, jti: &str) -> Result<()> {
    // Reject obviously bad JTIs client-side so the user gets a clean
    // error instead of a daemon roundtrip.
    if uuid::Uuid::parse_str(jti).is_err() {
        anyhow::bail!("Invalid jti: {jti:?} (expected a UUID)");
    }

    let client = DaemonClient::connect().await?;
    let response = client.principal_revoke_invite(name, jti).await?;

    match response {
        ResponsePacket::PrincipalInviteRevoked { name, jti, .. } => {
            println!("Revoked invite {jti} on principal '{name}'.");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to revoke invite: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn list_principal_agents(name: &str, paths: &GlobalPaths) -> Result<()> {
    let agents_dir = paths
        .authority()
        .shared_agents_dir(&paths.principal_id_for(name).ok_or_else(|| {
            anyhow::anyhow!("principal '{name}' not found")
        })?)
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
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

async fn show_principal_agent(name: &str, agent: &str, paths: &GlobalPaths) -> Result<()> {
    let agents_dir = paths
        .authority()
        .shared_agents_dir(&paths.principal_id_for(name).ok_or_else(|| {
            anyhow::anyhow!("principal '{name}' not found")
        })?)
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
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

async fn load_principal(
    name: &str,
    manager: &PrincipalManager,
    paths: &GlobalPaths,
) -> Result<Arc<peko_core::principal::Principal>> {
    if let Some(p) = manager.get_by_name(name).await {
        return Ok(p);
    }

    let config_path = paths
        .authority()
        .shared_config(&paths.principal_id_for(name).ok_or_else(|| {
            anyhow::anyhow!("principal '{name}' not found")
        })?)
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
    if !config_path.exists() {
        anyhow::bail!("principal '{name}' not found");
    }

    manager
        .load(&config_path)
        .await
        .context("failed to load principal")
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
        resolver.clone(),
        Arc::new(CliPrincipalMemoryFactory {
            resolver,
        }),
        Arc::new(DefaultPrincipalRouterFactory),
    )
}

/// Minimal safe built-in extension bundle for a freshly-created Principal.
///
/// With deny-all semantics an empty allowlist would leave the root agent
/// unable to do anything useful. This starter set is intentionally small:
/// file/tools, shell execution, task management, the Agent tool, and the
/// agent_catalog needed to discover subagents.
fn starter_extensions() -> Capabilities {
    Capabilities::starter_bundle()
}

fn default_principal_config(name: &str) -> PrincipalConfig {
    PrincipalConfig {
        name: name.to_string(),
        did: None,
        owner: Subject::User("local".to_string()),
        identity: PrincipalIdentityConfig {
            display_name: Some(name.to_string()),
            description: Some(format!("The {name} Principal")),
            avatar: None,
        },
        intent: PrincipalIntentConfig::default(),
        governance: PrincipalGovernanceConfig::default(),
        memory: PrincipalMemoryConfig::default(),
        routing: PrincipalRoutingConfig::default(),
        capabilities: starter_extensions(),
        exposure: peko_auth::Exposure::Private,
        status: None,
        permissions: Vec::new(),
        // Principals must be created with a configured model. The CLI
        // `peko principal create` path will require `--model` and set
        // this field; a default of `None` is only used by legacy tests.
        preferred_model_id: None,
        transport_preference: Default::default(),
        quota: None,
    }
}

fn default_agent_prompt(name: &str) -> String {
    // The `{{{{...}}}}` quadruple-braces emit the literal `{{memory}}`
    // placeholder that `SystemPromptBuilder::build` substitutes for the
    // principal's MEMORY.md content. Doubled braces are needed because
    // `format!` treats `{...}` as a substitution argument and `{{` as
    // a literal `{`.
    format!(
        "---\nname: primary\ndescription: \"Default assistant for {name}\"\n---\n\n\
        You are {name}, a helpful AI assistant. Respond to the caller's message concisely.\n\n\
        {{{{memory}}}}\n"
    )
}

/// Memory factory that places Principal memory under the Local tier root.
///
/// **Phase A.** Memory now lives at `{data_dir}/principals/{name}/local/`
/// (Local tier), not `{data_dir}/principals/{name}/memory/`. The factory
/// takes a `PathResolver` so the runtime writer and the IPC resolver agree
/// on the same path — the previous hand-rolled join caused silent
/// session-export loss.
struct CliPrincipalMemoryFactory {
    resolver: peko_core::common::paths::PathResolver,
}

#[async_trait::async_trait]
impl PrincipalMemoryFactory for CliPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &peko_subject::PrincipalId,
        workspace_path: &Path,
    ) -> Arc<dyn peko_core::principal::PrincipalMemory> {
        let name = workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let local_root = self.resolver.principal_layout(&name).local.root;
        let _ = tokio::fs::create_dir_all(&local_root).await;
        let memory = DefaultPrincipalMemory::new(local_root);
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{from_cli, Cli, Commands};
    use clap::Parser;
    use peko_auth::Permission;

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
    fn principal_import_parses_yes_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "import",
            "/tmp/pkg.principal",
            "--name",
            "renamed",
            "--allow-unsigned",
            "--force",
            "--yes",
        ])
        .expect("should parse principal import with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Import {
                file_path,
                name,
                allow_unsigned,
                force,
                yes,
            }) => {
                assert_eq!(file_path, "/tmp/pkg.principal");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(allow_unsigned);
                assert!(force);
                assert!(yes);
            }
            _other => panic!("expected Principal import command"),
        }
    }

    #[test]
    fn principal_import_without_yes_defaults() {
        let cli = Cli::try_parse_from(["peko", "principal", "import", "/tmp/pkg.principal"])
            .expect("should parse principal import without --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Import { yes, .. }) => {
                assert!(!yes);
            }
            _other => panic!("expected Principal import command"),
        }
    }

    #[test]
    fn principal_pull_parses_yes_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "pull",
            "owner/principal:1.0.0",
            "--name",
            "renamed",
            "--force",
            "--yes",
        ])
        .expect("should parse principal pull with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Pull {
                registry_ref,
                name,
                force,
                yes,
                ..
            }) => {
                assert_eq!(registry_ref, "owner/principal:1.0.0");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(force);
                assert!(yes);
            }
            _other => panic!("expected Principal pull command"),
        }
    }

    #[test]
    fn principal_pull_without_yes_defaults() {
        let cli = Cli::try_parse_from(["peko", "principal", "pull", "owner/principal:1.0.0"])
            .expect("should parse principal pull without --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Pull { yes, .. }) => {
                assert!(!yes);
            }
            _other => panic!("expected Principal pull command"),
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
    fn principal_remove_parses() {
        let cli = Cli::try_parse_from(["peko", "principal", "remove", "myprincipal", "--yes"])
            .expect("should parse principal remove with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Remove { name, yes }) => {
                assert_eq!(name, "myprincipal");
                assert!(yes);
            }
            _other => panic!("expected Principal remove command"),
        }
    }

    #[test]
    fn default_agent_prompt_contains_name() {
        let prompt = default_agent_prompt("spot");
        assert!(prompt.contains("spot"));
        // The strict agent adapter requires both `name` and `description`
        // in the frontmatter; the generated default must parse cleanly.
        assert!(
            prompt.contains("name: primary"),
            "default agent prompt frontmatter must include a name; got: {prompt}"
        );
        assert!(
            prompt.contains("description:"),
            "default agent prompt frontmatter must include a description; got: {prompt}"
        );
        // The default supervisor template must opt in to the
        // `{{memory}}` placeholder so casual users see their MEMORY.md
        // content without authoring a custom template.
        assert!(
            prompt.contains("{{memory}}"),
            "default agent prompt must include the {{memory}} placeholder; got: {prompt}"
        );
    }

    #[test]
    fn principal_create_requires_model_flag() {
        // Model-first: `--model` is required at creation so every
        // principal is pinned to a configured model from day one —
        // there is no runtime default to fall back to.
        let result = Cli::try_parse_from(["peko", "principal", "create", "alice"]);
        assert!(result.is_err(), "create without --model should fail");

        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "create",
            "alice",
            "--model",
            "anthropic-haiku",
        ])
        .expect("should parse principal create with --model");
        match cli.command {
            Commands::Principal(PrincipalCommands::Create { name, model, force }) => {
                assert_eq!(name, "alice");
                assert_eq!(model, "anthropic-haiku");
                assert!(!force);
            }
            _other => panic!("expected Principal create command"),
        }
    }

    #[tokio::test]
    async fn create_principal_rejects_unknown_model() {
        let dir = tempfile::tempdir().unwrap();

        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Empty catalog → any model id is rejected at creation, before
        // the workspace is written.
        let result = create_principal("alice", "no-such-model", false, &paths).await;
        let err = result.expect_err("unknown model must fail creation");
        assert!(
            format!("{err:#}").contains("no-such-model"),
            "error should name the offending model id: {err:#}"
        );
    }

    #[test]
    fn principal_create_parses_force_flag() {
        // `--force` is the explicit override for the overwrite guard —
        // see Bug 2 in scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "create",
            "scout",
            "--model",
            "anthropic-haiku",
            "--force",
        ])
        .expect("should parse principal create with --force");

        match cli.command {
            Commands::Principal(PrincipalCommands::Create { name, model, force }) => {
                assert_eq!(name, "scout");
                assert_eq!(model, "anthropic-haiku");
                assert!(force);
            }
            _other => panic!("expected Principal create command"),
        }
    }

    /// Bug 2 regression: a second `create` against an existing workspace
    /// must refuse without `--force`. Previously it silently wiped
    /// identity / agents / memory / sessions.
    #[tokio::test]
    async fn create_principal_refuses_overwrite_without_force() {
        use peko_providers::catalog::ModelCatalog;

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Seed the catalog with a single valid model so the creation
        // path can proceed past the catalog check.
        let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
        std::fs::create_dir_all(paths.config_dir.clone()).unwrap();
        std::fs::write(
            &cat_path,
            r#"
version = "4.0"

[entries.demo-model]
id = "demo-model"
display_name = "Demo"
template_id = "anthropic"
api_format = "anthropic_messages"
base_url = "https://example.invalid"
model_id = "demo"
context_window = 100000
credential_id = "00000000-0000-0000-0000-000000000000"
requires_key = true
enabled = true
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
        )
        .unwrap();

        // First create: success.
        create_principal("scout", "demo-model", false, &paths)
            .await
            .expect("first create should succeed");

        // Capture a sentinel file inside the workspace that the
        // overwrite (if it happened) would destroy. This proves the
        // refusal is real and not a "succeeded silently" trap.
        let shared = paths.resolver().principal_layout("scout").shared;
        std::fs::write(shared.agents_dir.join("sentinel.md"), "do-not-delete")
            .expect("write sentinel");
        assert!(shared.agents_dir.join("sentinel.md").exists());

        // Second create without --force: must refuse.
        let err = create_principal("scout", "demo-model", false, &paths)
            .await
            .expect_err("second create without --force must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already exists") && msg.contains("remove"),
            "error must name the guard + the recovery path: {msg}"
        );

        // Sentinel must still exist — nothing was overwritten.
        assert!(
            shared.agents_dir.join("sentinel.md").exists(),
            "overwrite guard must not destroy workspace data"
        );

        // With --force: the guard is bypassed and the call succeeds.
        // (Note: --force currently does NOT wipe existing on-disk state;
        // the doc-comment on `Create { force }` and the bail message
        // tell the user to `principal remove` first for a clean
        // replacement. This regression only asserts the guard was
        // bypassed, not that data was destroyed — destruction semantics
        // are a follow-up tracked in the e2e report.)
        create_principal("scout", "demo-model", true, &paths)
            .await
            .expect("create with --force should succeed past the guard");
    }
}
