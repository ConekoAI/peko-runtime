//! Auth command - Manage runtime auth and registry login (ADR-034)

use crate::commands::GlobalPaths;
use anyhow::Result;
use clap::Subcommand;
use peko_core::common::services::CredentialsService;

/// Auth subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AuthCommands {
    /// Show authentication status
    Status,

    // ── ADR-034: Runtime auth management ──
    /// Manage runtime API keys (advanced / hidden)
    #[command(subcommand, hide = true)]
    ApiKey(ApiKeyCommands),
}

/// API key management subcommands (ADR-034)
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ApiKeyCommands {
    /// Create a new API key
    Create {
        /// Name for the key
        #[arg(short, long)]
        name: String,
        /// Scopes (comma-separated: read,write,admin)
        #[arg(short, long, value_delimiter = ',')]
        scopes: Vec<String>,
    },
    /// List API keys
    List,
    /// Revoke an API key
    Revoke {
        /// Key ID to revoke
        key_id: String,
    },
}

/// Handle auth commands
pub fn handle_auth(cmd: AuthCommands, paths: &GlobalPaths, _json: bool) -> Result<()> {
    match cmd {
        AuthCommands::Status => {
            match peko_core::tunnel::HubLogin::load(&paths.config_dir)? {
                Some(login) => println!(
                    "Logged in to {} as user:hub:{}",
                    login.hub_url, login.user_id
                ),
                None => println!("Local user: user:local:local (no Hub login)"),
            }
            Ok(())
        }

        AuthCommands::ApiKey(cmd) => handle_api_key_command(cmd, paths),
    }
}

/// Handle API key management commands (ADR-034)
///
/// # Panics
/// Panics if called from within an async context (nested Runtime::block_on).
/// This function is only called from synchronous CLI command dispatch.
fn handle_api_key_command(cmd: ApiKeyCommands, paths: &GlobalPaths) -> Result<()> {
    let resolver = peko_core::common::paths::PathResolver::with_dirs(
        paths.config_dir.clone(),
        paths.data_dir.clone(),
        paths.cache_dir.clone(),
    );

    // CLI command handlers run in a synchronous context, so we create a
    // temporary runtime to execute async store operations. This is safe
    // because the CLI does not use an existing tokio runtime.
    let rt = tokio::runtime::Runtime::new()?;

    match cmd {
        ApiKeyCommands::Create { name, scopes } => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            let parsed_scopes: Vec<peko_auth::types::ApiKeyScope> =
                scopes.iter().filter_map(|s| s.parse().ok()).collect();
            let (full_key, key_id) = rt.block_on(store.create_key(name, parsed_scopes))?;
            println!("✓ API key created");
            println!("  Key ID: {key_id}");
            println!("  Full key: {full_key}");
            println!("  ⚠ Store this key now — it will not be shown again!");
            Ok(())
        }
        ApiKeyCommands::List => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            let keys = rt.block_on(store.list_keys());
            if keys.is_empty() {
                println!("No API keys configured.");
            } else {
                println!("API keys:");
                for key in keys {
                    let status = if key.enabled { "✓" } else { "✗" };
                    let scopes: Vec<String> = key.scopes.iter().map(|s| s.to_string()).collect();
                    println!(
                        "  {status} {} – {} (scopes: {})",
                        key.id,
                        key.name,
                        scopes.join(", ")
                    );
                }
            }
            Ok(())
        }
        ApiKeyCommands::Revoke { key_id } => {
            let store = peko_auth::api_key::ApiKeyStore::load(&resolver)?;
            if rt.block_on(store.revoke_key(&key_id))? {
                println!("✓ API key {key_id} revoked");
                Ok(())
            } else {
                anyhow::bail!("API key {key_id} not found");
            }
        }
    }
}

fn hub_origin(host: &str) -> Result<String> {
    let url = reqwest::Url::parse(&if host.contains("://") {
        host.to_string()
    } else {
        format!("https://{host}")
    })?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "Use a PekoHub HTTP(S) origin"
    );
    Ok(url.origin().ascii_serialization())
}

/// Authenticate the human account; login does not register principals or move runtime ownership.
pub async fn handle_login(paths: &GlobalPaths, host: &str, api_key: Option<String>) -> Result<()> {
    let origin = hub_origin(host)?;
    let Some(key) = api_key else {
        println!("Create a PekoHub API key at {origin}/profile, then run peko login --hub {origin} --api-key <key>");
        return Ok(());
    };
    let response = reqwest::Client::new()
        .get(format!("{origin}/v1/auth/me"))
        .bearer_auth(&key)
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "PekoHub login rejected: HTTP {}",
        response.status()
    );
    let account: serde_json::Value = response.json().await?;
    let user_id = account
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("PekoHub returned no account ID"))?;
    uuid::Uuid::parse_str(user_id)?;
    CredentialsService::new(paths.clone())?.set_registry_token(
        key,
        origin.clone(),
        account
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    )?;
    peko_core::tunnel::HubLogin {
        user_id: user_id.into(),
        hub_url: origin.clone(),
    }
    .save(&paths.config_dir)?;
    println!("Logged in to {origin} as user:hub:{user_id}");
    Ok(())
}

pub fn handle_logout(paths: &GlobalPaths, host: Option<&str>) -> Result<()> {
    let current = peko_core::tunnel::HubLogin::load(&paths.config_dir)?;
    let origin = match host {
        Some(host) => hub_origin(host)?,
        None => current
            .as_ref()
            .map(|l| l.hub_url.clone())
            .unwrap_or_else(|| "https://pekohub.org".into()),
    };
    CredentialsService::new(paths.clone())?.clear_registry_token(&origin)?;
    if current
        .as_ref()
        .is_some_and(|login| login.hub_url == origin)
    {
        peko_core::tunnel::HubLogin::logout(&paths.config_dir)?;
    }
    println!("Logged out from {origin}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn account_server(status: &str, body: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let response=format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let received = socket.read(&mut buffer).await.unwrap();
            assert!(received > 0);
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        origin
    }
    #[tokio::test]
    async fn login_validates_the_account_before_switching_local_identity() {
        crate::test_support::init_credentials();
        let dir = tempfile::tempdir().unwrap();
        let paths = GlobalPaths::new(
            dir.path().join("config"),
            dir.path().join("data"),
            dir.path().join("cache"),
            "local".into(),
        );
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        peko_core::tunnel::HubLogin {
            user_id: first.clone(),
            hub_url: "https://original".into(),
        }
        .save(&paths.config_dir)
        .unwrap();
        let rejected = account_server("401 Unauthorized", "{}").await;
        assert!(
            handle_login(&paths, &rejected, Some("test-only-key".into()))
                .await
                .is_err()
        );
        assert_eq!(
            super::super::identity_for_config_dir(&paths.config_dir),
            format!("hub:{first}")
        );
        let accepted = account_server(
            "200 OK",
            &serde_json::json!({"id":second,"namespace":"second"}).to_string(),
        )
        .await;
        handle_login(&paths, &accepted, Some("test-only-key".into()))
            .await
            .unwrap();
        assert_eq!(
            super::super::identity_for_config_dir(&paths.config_dir),
            format!("hub:{second}")
        );
        handle_logout(&paths, None).unwrap();
        assert_eq!(
            super::super::identity_for_config_dir(&paths.config_dir),
            "local"
        );
    }
}
