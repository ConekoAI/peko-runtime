//! Auth command - Manage runtime auth and registry login (ADR-034)

use crate::commands::GlobalPaths;
use crate::common::services::CredentialsService;
use anyhow::Result;
use clap::Subcommand;

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

/// Mask a token for display
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

/// Handle auth commands
pub fn handle_auth(cmd: AuthCommands, paths: &GlobalPaths, _json: bool) -> Result<()> {
    match cmd {
        AuthCommands::Status => {
            let service = CredentialsService::new(paths.clone())?;
            print_registry_status(&service, false)?;
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
    let resolver = crate::common::paths::PathResolver::with_dirs(
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
            let store = crate::auth::api_key::ApiKeyStore::load(&resolver)?;
            let parsed_scopes: Vec<crate::auth::types::ApiKeyScope> =
                scopes.iter().filter_map(|s| s.parse().ok()).collect();
            let (full_key, key_id) = rt.block_on(store.create_key(name, parsed_scopes))?;
            println!("✓ API key created");
            println!("  Key ID: {key_id}");
            println!("  Full key: {full_key}");
            println!("  ⚠ Store this key now — it will not be shown again!");
            Ok(())
        }
        ApiKeyCommands::List => {
            let store = crate::auth::api_key::ApiKeyStore::load(&resolver)?;
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
            let store = crate::auth::api_key::ApiKeyStore::load(&resolver)?;
            if rt.block_on(store.revoke_key(&key_id))? {
                println!("✓ API key {key_id} revoked");
                Ok(())
            } else {
                anyhow::bail!("API key {key_id} not found");
            }
        }
    }
}

/// Print registry login status
fn print_registry_status(service: &CredentialsService, show: bool) -> Result<()> {
    match service.get_registry_token()? {
        Some(cred) => {
            let token_display = if show {
                cred.token.clone()
            } else {
                mask_key(&cred.token)
            };
            println!("Registry login status:");
            println!("  ✓ Logged in to {}", cred.registry_host);
            if let Some(ns) = &cred.user_namespace {
                println!("  Namespace: {ns}");
            }
            println!("  Token: {token_display}");
        }
        None => {
            println!("Registry login status:");
            println!("  ✗ Not logged in to registry");
            println!("    Run 'peko login --api-key <key>' to log in");
        }
    }
    Ok(())
}

/// Handle top-level `peko login` command
pub fn handle_login(paths: &GlobalPaths, host: &str, api_key: Option<String>) -> Result<()> {
    let service = CredentialsService::new(paths.clone())?;

    if let Some(key) = api_key {
        // Store the API key directly as a Bearer token
        service.set_registry_token(key, host.to_string(), None)?;
        println!("✓ Logged in to {host}");
        println!("  Token stored in {}", service.vault_path().display());
    } else {
        println!("To log in to PekoHub, visit:");
        println!("  https://{host}/api/v1/auth/github/authorize");
        println!("Or generate an API key at:");
        println!("  https://{host}/profile");
        println!();
        println!("Then run: peko login --api-key <your-key>");
    }
    Ok(())
}

/// Handle top-level `peko logout` command
pub fn handle_logout(paths: &GlobalPaths, host: &str) -> Result<()> {
    let service = CredentialsService::new(paths.clone())?;
    if service.clear_registry_token(host)? {
        println!("✓ Logged out from {host}")
    } else {
        println!("✗ Not logged in to {host}")
    }
    Ok(())
}
