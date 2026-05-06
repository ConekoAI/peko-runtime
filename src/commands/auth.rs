//! Auth command - Manage API keys and credentials

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
                println!("No credentials configured.");
                println!("  Use 'pekobot auth set <provider> <key>' to add one.");
                return Ok(());
            }

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
    }
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
