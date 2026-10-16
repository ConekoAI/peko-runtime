//! Auth command - Manage runtime auth and registry login (ADR-034)

use crate::commands::GlobalPaths;
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

/// Handle auth commands
pub fn handle_auth(cmd: AuthCommands, paths: &GlobalPaths, _json: bool) -> Result<()> {
    match cmd {
        AuthCommands::Status => {
            // ADR-081 D3: the terminal is always the local user; a registered
            // runtime gives it a Hub alias.
            let local = tokio::runtime::Runtime::new()?.block_on(
                peko_core::tunnel::registration::local_user(paths.resolver()),
            );
            match local.hub_address() {
                Some(hub) => println!("Local user: {} = {hub}", local.address),
                None => println!(
                    "Local user: {} (runtime not registered with PekoHub)",
                    local.address
                ),
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
