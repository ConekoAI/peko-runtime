//! Auth command - Manage API keys and credentials (ADR-034)

use crate::commands::GlobalPaths;
use crate::common::services::CredentialsService;
use anyhow::Result;
use clap::Subcommand;
use std::io::Write;

/// Auth subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum AuthCommands {
    /// Add a new API key
    Set {
        /// Provider (openai, anthropic, kimi, etc.)
        provider: String,
        /// API key value (or omit to enter interactively)
        key: Option<String>,
    },

    /// List configured credentials
    List {
        /// Show full keys (not masked)
        #[arg(long)]
        show: bool,
    },

    /// Remove a credential
    Remove {
        /// Provider name
        provider: String,
    },

    /// Test a credential
    Test {
        /// Provider name (test all if omitted)
        provider: Option<String>,
    },

    /// Log in to the PekoHub registry
    Login {
        /// Registry host (default: pekohub.ai)
        #[arg(long, default_value = "pekohub.ai")]
        registry: String,
        /// Log in with an API key instead of OAuth
        #[arg(long)]
        api_key: Option<String>,
    },

    /// Log out from the PekoHub registry
    Logout {
        /// Registry host to log out from
        #[arg(long, default_value = "pekohub.ai")]
        registry: String,
    },

    /// Show authentication status
    Status,

    // ── ADR-034: Runtime auth management ──
    /// Manage runtime API keys
    #[command(subcommand)]
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

/// Mask API key for display
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

/// Normalize provider name (handle aliases)
fn normalize_provider(provider: &str) -> &str {
    match provider.to_lowercase().as_str() {
        "kimi" | "kimi-code" | "kimi_code" => "kimi",
        "moonshot" | "moonshotai" => "moonshot",
        _ => provider,
    }
}

/// Handle auth commands
pub fn handle_auth(cmd: AuthCommands, paths: &GlobalPaths, _json: bool) -> Result<()> {
    let service = CredentialsService::new(paths.clone());

    match cmd {
        AuthCommands::Set { provider, key } => {
            let canonical_provider = normalize_provider(&provider);

            // Get API key interactively if not provided
            let api_key = if let Some(k) = key {
                k
            } else {
                print!("Enter API key for {provider}: ");
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                input.trim().to_string()
            };

            if api_key.is_empty() {
                anyhow::bail!("API key cannot be empty");
            }

            service.set(canonical_provider, api_key)?;

            println!("✓ API key saved for {canonical_provider}");
            println!("  Location: {}", service.credentials_path().display());

            Ok(())
        }

        AuthCommands::List { show } => {
            let providers = service.list_providers()?;

            if providers.is_empty() {
                println!("No LLM provider credentials configured.");
                println!("  Use 'peko auth set <provider> <key>' to add one.");
            } else {
                println!("Configured credentials:");
                println!();

                for provider in providers {
                    if let Some(cred) = service.get(&provider)? {
                        let key_display = if show {
                            cred.api_key
                        } else {
                            mask_key(&cred.api_key)
                        };
                        println!("  {provider}: {key_display}");
                    }
                }

                println!();
                if !show {
                    println!("  Use --show to display full keys");
                }
            }

            // Show registry status
            println!();
            print_registry_status(&service, show)?;

            Ok(())
        }

        AuthCommands::Remove { provider } => {
            if service.remove(&provider)? {
                println!("✓ Removed credential for {provider}");
            } else {
                println!("✗ No credential found for {provider}");
            }

            Ok(())
        }

        AuthCommands::Test { provider } => {
            let providers_to_test = match provider {
                Some(p) => vec![p],
                None => service.list_providers()?,
            };

            if providers_to_test.is_empty() {
                println!("No credentials configured to test.");
                return Ok(());
            }

            println!("Testing credentials...");
            println!();

            for provider in providers_to_test {
                match service.test_provider(&provider)? {
                    Some(valid) => {
                        if valid {
                            println!("  ✓ {provider}: Valid format");
                        } else {
                            println!("  ⚠ {provider}: Invalid key format");
                        }
                    }
                    None => {
                        println!("  ✗ {provider}: Not found");
                    }
                }
            }

            Ok(())
        }

        AuthCommands::Login { registry, api_key } => {
            eprintln!("⚠ Warning: `peko auth login` is deprecated. Use `peko login` instead.");
            if let Some(key) = api_key {
                // Store the API key directly as a Bearer token
                service.set_registry_token(key, registry.clone(), None)?;
                println!("✓ Logged in to {registry}");
                println!("  Token stored in {}", service.credentials_path().display());
            } else {
                println!("To log in to PekoHub, visit:");
                println!("  https://{registry}/api/v1/auth/github/authorize");
                println!("Or generate an API key at:");
                println!("  https://{registry}/profile");
                println!();
                println!("Then run: peko login --api-key <your-key>");
            }
            Ok(())
        }

        AuthCommands::Logout { registry } => {
            eprintln!("⚠ Warning: `peko auth logout` is deprecated. Use `peko logout` instead.");
            match service.clear_registry_token()? {
                true => println!("✓ Logged out from {registry}"),
                false => println!("✗ Not logged in to {registry}"),
            }
            Ok(())
        }

        AuthCommands::Status => {
            let providers = service.list_providers()?;

            // LLM providers
            if providers.is_empty() {
                println!("No LLM provider credentials configured.");
            } else {
                println!("LLM provider credentials:");
                for provider in &providers {
                    if let Some(cred) = service.get(provider)? {
                        let masked = mask_key(&cred.api_key);
                        println!("  ✓ {provider}: {masked}");
                    }
                }
            }

            println!();
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
            let parsed_scopes: Vec<crate::auth::types::ApiKeyScope> = scopes
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect();
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
            match rt.block_on(store.revoke_key(&key_id))? {
                true => {
                    println!("✓ API key {key_id} revoked");
                    Ok(())
                }
                false => {
                    anyhow::bail!("API key {key_id} not found");
                }
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
            println!("    Run 'peko auth login --api-key <key>' to log in");
        }
    }
    Ok(())
}

/// Handle top-level `peko login` command
pub fn handle_login(paths: &GlobalPaths, host: &str, api_key: Option<String>) -> Result<()> {
    let service = CredentialsService::new(paths.clone());

    if let Some(key) = api_key {
        // Store the API key directly as a Bearer token
        service.set_registry_token(key, host.to_string(), None)?;
        println!("✓ Logged in to {host}");
        println!("  Token stored in {}", service.credentials_path().display());
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
    let service = CredentialsService::new(paths.clone());
    match service.clear_registry_token()? {
        true => println!("✓ Logged out from {host}"),
        false => println!("✗ Not logged in to {host}"),
    }
    Ok(())
}

/// Get API key for a provider (used by agent creation)
pub fn get_api_key(paths: &GlobalPaths, provider: &str) -> Result<Option<String>> {
    let service = CredentialsService::new(paths.clone());
    service.get_api_key(provider)
}

/// Auto-detect available providers
pub fn detect_available_providers(paths: &GlobalPaths) -> Result<Vec<String>> {
    let service = CredentialsService::new(paths.clone());
    service.list_providers()
}
