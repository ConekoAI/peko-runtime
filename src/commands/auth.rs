//! Auth command - Manage API keys and credentials

use crate::commands::GlobalPaths;
use anyhow::{Context, Result};
use clap::Subcommand;
use std::collections::HashMap;
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
        #[arg(trailing_var_arg = true)]
        key: Option<String>,
        /// Profile name (default: "default")
        #[arg(short, long)]
        profile: Option<String>,
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
        /// Profile name (default: "default")
        #[arg(short, long)]
        profile: Option<String>,
    },

    /// Test a credential
    Test {
        /// Provider name (test all if omitted)
        provider: Option<String>,
        /// Profile name
        #[arg(short, long)]
        profile: Option<String>,
    },
}

/// Credential entry
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Credential {
    pub provider: String,
    pub profile: String,
    pub api_key: String,
    pub created_at: String,
}

/// Credentials store
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct CredentialsStore {
    pub version: u32,
    pub credentials: HashMap<String, Credential>, // key: "provider:profile"
}

impl CredentialsStore {
    fn key(provider: &str, profile: &str) -> String {
        format!("{}:{}", provider, profile)
    }

    pub fn get(&self, provider: &str, profile: &str) -> Option<&Credential> {
        self.credentials.get(&Self::key(provider, profile))
    }

    pub fn set(&mut self, provider: &str, profile: &str, api_key: String) {
        let credential = Credential {
            provider: provider.to_string(),
            profile: profile.to_string(),
            api_key,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.credentials.insert(Self::key(provider, profile), credential);
    }

    pub fn remove(&mut self, provider: &str, profile: &str) -> bool {
        self.credentials.remove(&Self::key(provider, profile)).is_some()
    }

    pub fn list_for_provider(&self, provider: &str) -> Vec<&Credential> {
        self.credentials
            .values()
            .filter(|c| c.provider == provider)
            .collect()
    }

    pub fn list_all(&self) -> Vec<&Credential> {
        self.credentials.values().collect()
    }

    pub fn providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self
            .credentials
            .values()
            .map(|c| c.provider.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        providers.sort();
        providers
    }
}

/// Load credentials from file
fn load_credentials(paths: &GlobalPaths) -> Result<CredentialsStore> {
    let path = paths.config_dir.join("credentials.json");
    
    if !path.exists() {
        return Ok(CredentialsStore {
            version: 1,
            credentials: HashMap::new(),
        });
    }

    let content = std::fs::read_to_string(&path)?;
    let store: CredentialsStore = serde_json::from_str(&content)?;
    Ok(store)
}

/// Save credentials to file with restricted permissions
fn save_credentials(paths: &GlobalPaths, store: &CredentialsStore) -> Result<()> {
    let path = paths.config_dir.join("credentials.json");
    
    // Ensure config dir exists
    std::fs::create_dir_all(&paths.config_dir)?;
    
    let content = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, content)?;
    
    // Set restrictive permissions (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    
    Ok(())
}

/// Mask API key for display
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "***".to_string()
    } else {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    }
}

/// Handle auth commands
pub async fn handle_auth(
    cmd: AuthCommands,
    paths: &GlobalPaths,
    _json: bool,
) -> Result<()> {
    match cmd {
        AuthCommands::Set { provider, key, profile } => {
            let profile = profile.unwrap_or_else(|| "default".to_string());
            
            // Get API key interactively if not provided
            let api_key = match key {
                Some(k) => k,
                None => {
                    print!("Enter API key for {} (profile: {}): ", provider, profile);
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    input.trim().to_string()
                }
            };

            if api_key.is_empty() {
                anyhow::bail!("API key cannot be empty");
            }

            // Load, update, save
            let mut store = load_credentials(paths)?;
            store.set(&provider, &profile, api_key);
            save_credentials(paths, &store)?;

            println!("✓ API key saved for {} (profile: {})", provider, profile);
            println!("  Location: {}", paths.config_dir.join("credentials.json").display());
            
            Ok(())
        }

        AuthCommands::List { show } => {
            let store = load_credentials(paths)?;
            
            if store.credentials.is_empty() {
                println!("No credentials configured.");
                println!("  Use 'pekobot auth set <provider> <key>' to add one.");
                return Ok(());
            }

            println!("Configured credentials:");
            println!();
            
            for provider in store.providers() {
                println!("  {}:", provider);
                let creds = store.list_for_provider(&provider);
                for cred in creds {
                    let key_display = if show {
                        &cred.api_key
                    } else {
                        &mask_key(&cred.api_key)
                    };
                    println!("    - {}: {}", cred.profile, key_display);
                }
            }
            
            println!();
            if !show {
                println!("  Use --show to display full keys");
            }
            
            Ok(())
        }

        AuthCommands::Remove { provider, profile } => {
            let profile = profile.unwrap_or_else(|| "default".to_string());
            
            let mut store = load_credentials(paths)?;
            if store.remove(&provider, &profile) {
                save_credentials(paths, &store)?;
                println!("✓ Removed credential for {} (profile: {})", provider, profile);
            } else {
                println!("✗ No credential found for {} (profile: {})", provider, profile);
            }
            
            Ok(())
        }

        AuthCommands::Test { provider, profile } => {
            let store = load_credentials(paths)?;
            
            let providers_to_test = match provider {
                Some(p) => vec![p],
                None => store.providers(),
            };
            
            if providers_to_test.is_empty() {
                println!("No credentials configured to test.");
                return Ok(());
            }

            println!("Testing credentials...");
            println!();
            
            for provider in providers_to_test {
                let profile_name = profile.as_deref().unwrap_or("default");
                
                match store.get(&provider, profile_name) {
                    Some(cred) => {
                        // Simple test - just verify key format
                        let valid = match provider.as_str() {
                            "openai" => cred.api_key.starts_with("sk-"),
                            "anthropic" => cred.api_key.starts_with("sk-ant-"),
                            _ => cred.api_key.len() > 10,
                        };
                        
                        if valid {
                            println!("  ✓ {} ({}): Valid format", provider, profile_name);
                        } else {
                            println!("  ⚠ {} ({}): Invalid key format", provider, profile_name);
                        }
                    }
                    None => {
                        println!("  ✗ {} ({}): Not found", provider, profile_name);
                    }
                }
            }
            
            Ok(())
        }
    }
}

/// Get API key for a provider (used by agent creation)
pub fn get_api_key(paths: &GlobalPaths, provider: &str, profile: Option<&str>) -> Result<Option<String>> {
    let store = load_credentials(paths)?;
    let profile = profile.unwrap_or("default");
    
    Ok(store.get(provider, profile).map(|c| c.api_key.clone()))
}

/// Auto-detect available providers
pub fn detect_available_providers(paths: &GlobalPaths) -> Result<Vec<String>> {
    let store = load_credentials(paths)?;
    Ok(store.providers())
}
