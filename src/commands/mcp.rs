//! MCP server management commands.
//!
//! Provides a Claude Code-style workflow for managing MCP servers, especially
//! remote SSE servers that require OAuth authentication:
//!
//!   peko ext mcp add myremote --url http://localhost:.../sse --oauth-client-id ...
//!   peko ext mcp auth myremote
//!   peko ext mcp list
//!   peko ext mcp remove myremote

use crate::commands::GlobalPaths;
use crate::common::vault::Vault;
use crate::extensions::mcp::protocol::{
    config::{McpAuthConfig, McpConfig, McpServerConfig, TransportType},
    oauth::OAuthFlow,
};
use anyhow::{Context, Result};
use clap::Subcommand;
use std::collections::HashMap;
use std::path::PathBuf;

/// MCP server management commands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum McpCommands {
    /// Add or update an MCP server configuration in `mcp.toml`.
    Add {
        /// Unique server name
        name: String,
        /// SSE endpoint URL
        #[arg(long)]
        url: Option<String>,
        /// Stdio command to execute
        #[arg(long, conflicts_with = "url")]
        command: Option<String>,
        /// Arguments for the stdio command
        #[arg(long)]
        args: Vec<String>,
        /// Static bearer token sent as `Authorization: Bearer <token>`
        #[arg(long)]
        bearer_token: Option<String>,
        /// OAuth client identifier
        #[arg(long)]
        oauth_client_id: Option<String>,
        /// OAuth authorization endpoint URL
        #[arg(long)]
        authorization_endpoint: Option<String>,
        /// OAuth token endpoint URL
        #[arg(long)]
        token_endpoint: Option<String>,
        /// OAuth scopes to request
        #[arg(long)]
        scope: Vec<String>,
        /// Additional static headers as `Name: Value`
        #[arg(long)]
        header: Vec<String>,
        /// Whether to auto-start the server (default true)
        #[arg(long)]
        auto_start: Option<bool>,
    },

    /// Authenticate an SSE MCP server using OAuth PKCE.
    Auth {
        /// Server name as configured in `mcp.toml`
        name: String,
        /// Authorization code for headless completion
        #[arg(long)]
        code: Option<String>,
        /// PKCE verifier for headless completion
        #[arg(long)]
        verifier: Option<String>,
    },

    /// List configured MCP servers
    List,

    /// Remove an MCP server configuration
    Remove { name: String },
}

/// Execute an MCP subcommand.
pub async fn execute(cmd: McpCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        McpCommands::Add {
            name,
            url,
            command,
            args,
            bearer_token,
            oauth_client_id,
            authorization_endpoint,
            token_endpoint,
            scope,
            header,
            auto_start,
        } => {
            add_cmd(
                paths,
                name,
                url,
                command,
                args,
                bearer_token,
                oauth_client_id,
                authorization_endpoint,
                token_endpoint,
                scope,
                header,
                auto_start,
            )
            .await
        }
        McpCommands::Auth {
            name,
            code,
            verifier,
        } => auth_cmd(paths, name, code, verifier).await,
        McpCommands::List => list_cmd(paths).await,
        McpCommands::Remove { name } => remove_cmd(paths, name).await,
    }
}

async fn add_cmd(
    paths: &GlobalPaths,
    name: String,
    url: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    bearer_token: Option<String>,
    oauth_client_id: Option<String>,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    scope: Vec<String>,
    header: Vec<String>,
    auto_start: Option<bool>,
) -> Result<()> {
    let (transport, endpoint) = match (url, command.clone()) {
        (Some(url), None) => (TransportType::Sse, Some(url)),
        (None, Some(_command)) => (TransportType::Stdio, None),
        (Some(_), Some(_)) => unreachable!("clap conflicts_with prevents both"),
        (None, None) => anyhow::bail!("Either --url or --command must be provided"),
    };

    let headers = parse_headers(&header)?;

    let auth = McpAuthConfig {
        bearer_token,
        oauth_client_id,
        authorization_endpoint,
        token_endpoint,
        scopes: scope,
        headers,
    };

    let mut config = load_mcp_config(paths.mcp_config()).await?;

    // Remove existing entry with the same name before appending the new one.
    config.remove_server(&name);

    let server_config = McpServerConfig {
        name: name.clone(),
        transport,
        command,
        args,
        env: HashMap::new(),
        cwd: None,
        endpoint,
        auto_start: auto_start.unwrap_or(true),
        health_check_interval_secs: 30,
        max_restarts: 0,
        init_timeout_secs: 30,
        tool_timeout_secs: 60,
        reserved_parameters: crate::extensions::framework::services::ReservedParamsConfig::new(),
        bundle: false,
        bundled_path: None,
        auth,
    };
    config.add_server(server_config);

    save_mcp_config(paths.mcp_config(), &config).await?;
    println!(
        "Added MCP server '{}' to {}",
        name,
        paths.mcp_config().display()
    );
    notify_daemon_reload().await;
    Ok(())
}

async fn auth_cmd(
    paths: &GlobalPaths,
    name: String,
    code: Option<String>,
    verifier: Option<String>,
) -> Result<()> {
    let config = load_mcp_config(paths.mcp_config()).await?;
    let server_config = config.get_server(&name).cloned().with_context(|| {
        format!(
            "MCP server '{}' not found in {}",
            name,
            paths.mcp_config().display()
        )
    })?;

    if server_config.transport != TransportType::Sse {
        anyhow::bail!("OAuth is only supported for SSE transport servers");
    }

    let vault =
        Vault::load(paths.resolver().vault()).with_context(|| "failed to load credential vault")?;

    let token = match (code, verifier) {
        (Some(code), Some(verifier)) => {
            OAuthFlow::authorize_with_code(&server_config.auth, &code, &verifier, "")
                .await
                .with_context(|| "failed to exchange authorization code")?
        }
        (None, None) => OAuthFlow::authorize(&server_config.auth, &name)
            .await
            .with_context(|| "OAuth authorization flow failed")?,
        _ => anyhow::bail!("--code and --verifier must be provided together"),
    };

    vault
        .set_oauth_token(&name, &token)
        .with_context(|| "failed to store OAuth token in vault")?;

    println!(
        "Authenticated '{}'. Access token expires at {:?}.",
        name, token.expires_at
    );
    notify_daemon_reload().await;
    Ok(())
}

async fn list_cmd(paths: &GlobalPaths) -> Result<()> {
    let config = load_mcp_config(paths.mcp_config()).await?;

    if config.servers.is_empty() {
        println!(
            "No MCP servers configured in {}",
            paths.mcp_config().display()
        );
        return Ok(());
    }

    println!("MCP servers in {}:", paths.mcp_config().display());
    for server in &config.servers {
        let transport = match server.transport {
            TransportType::Stdio => "stdio",
            TransportType::Sse => "sse",
        };
        let endpoint = server
            .endpoint
            .as_deref()
            .or(server.command.as_deref())
            .unwrap_or("-");
        let auth_hint = if server.auth.is_empty() {
            "no auth"
        } else if server.auth.oauth_client_id.is_some() {
            "oauth"
        } else if server.auth.bearer_token.is_some() {
            "bearer"
        } else {
            "headers"
        };
        println!(
            "  {} [{}] {} (auto_start={}, {})",
            server.name, transport, endpoint, server.auto_start, auth_hint
        );
    }
    Ok(())
}

async fn remove_cmd(paths: &GlobalPaths, name: String) -> Result<()> {
    let mut config = load_mcp_config(paths.mcp_config()).await?;
    if config.remove_server(&name).is_none() {
        anyhow::bail!(
            "MCP server '{}' not found in {}",
            name,
            paths.mcp_config().display()
        );
    }

    save_mcp_config(paths.mcp_config(), &config).await?;
    println!(
        "Removed MCP server '{}' from {}",
        name,
        paths.mcp_config().display()
    );

    // Also drop any stored OAuth token for this server.
    match Vault::load(paths.resolver().vault()) {
        Ok(vault) => {
            let _ = vault.delete_oauth_token(&name);
        }
        Err(e) => {
            tracing::warn!("Failed to load vault while removing OAuth token: {e}");
        }
    }

    notify_daemon_reload().await;
    Ok(())
}

async fn load_mcp_config(path: PathBuf) -> Result<McpConfig> {
    if path.exists() {
        McpConfig::from_file(&path)
            .await
            .with_context(|| format!("Failed to read MCP config from {path:?}"))
    } else {
        Ok(McpConfig::default())
    }
}

async fn save_mcp_config(path: PathBuf, config: &McpConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let content = config
        .to_toml()
        .with_context(|| format!("Failed to serialize MCP config to TOML"))?;
    tokio::fs::write(&path, content)
        .await
        .with_context(|| format!("Failed to write MCP config to {path:?}"))?;
    Ok(())
}

fn parse_headers(raw: &[String]) -> Result<HashMap<String, String>> {
    let mut headers = HashMap::new();
    for item in raw {
        let (name, value) = item
            .split_once(':')
            .with_context(|| format!("Invalid header '{item}', expected Name:Value"))?;
        headers.insert(name.trim().to_string(), value.trim().to_string());
    }
    Ok(headers)
}

/// Tell the running daemon to re-read `mcp.toml` and the vault.
async fn notify_daemon_reload() {
    let Ok(client) = crate::ipc::DaemonClient::connect().await else {
        return;
    };
    match client.mcp_reload().await {
        Ok(crate::ipc::ResponsePacket::McpReloaded { servers_count, .. }) => {
            tracing::info!("Daemon reloaded MCP config ({servers_count} servers)");
        }
        Ok(_) => {
            tracing::warn!("Daemon returned unexpected response to MCP reload");
        }
        Err(e) => {
            eprintln!("Daemon MCP reload failed: {e}");
        }
    }
}
