//! Registry management commands
//!
//! Provides `peko registry set-default`, `get-default`, and `list`.

use crate::commands::GlobalPaths;
use crate::registry::config::load_from_config_dir;
use anyhow::Result;
use clap::Subcommand;

/// Registry management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum RegistryCommands {
    /// Set the default registry host
    SetDefault {
        /// Registry host (e.g., pekohub.org or localhost:3000)
        host: String,
    },

    /// Get the current default registry host
    GetDefault,

    /// List configured registry sources
    List,
}

/// Handle registry commands
pub fn handle_registry(cmd: RegistryCommands, paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        RegistryCommands::SetDefault { host } => {
            let config_path = paths.config_dir.join("config.toml");

            // Read existing config or start fresh
            let content = if config_path.exists() {
                std::fs::read_to_string(&config_path)?
            } else {
                String::new()
            };

            // Parse as TOML to manipulate
            let mut doc: toml::Value = if content.trim().is_empty() {
                toml::Value::Table(toml::map::Map::new())
            } else {
                content.parse().unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()))
            };

            // Set [registry].default
            let table = doc.as_table_mut().unwrap();
            let registry = table
                .entry("registry")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            registry.insert("default".to_string(), toml::Value::String(host.clone()));

            // Ensure the source is also in sources list
            let sources = registry
                .entry("sources")
                .or_insert_with(|| toml::Value::Array(Vec::new()))
                .as_array_mut()
                .unwrap();

            // Check if host already exists in sources
            let exists = sources.iter().any(|s| {
                s.get("url")
                    .and_then(|u| u.as_str())
                    .map(|u| u == host)
                    .unwrap_or(false)
            });

            if !exists {
                let mut new_source = toml::map::Map::new();
                new_source.insert("url".to_string(), toml::Value::String(host.clone()));
                new_source.insert("priority".to_string(), toml::Value::Integer(1));
                sources.push(toml::Value::Table(new_source));
            }

            // Write back
            std::fs::create_dir_all(&paths.config_dir)?;
            std::fs::write(&config_path, doc.to_string())?;

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "success": true,
                        "default_registry": host,
                        "config_path": config_path.to_string_lossy(),
                    })
                );
            } else {
                println!("✓ Default registry set to: {host}");
                println!("  Config: {}", config_path.display());
            }

            Ok(())
        }

        RegistryCommands::GetDefault => {
            let config = load_from_config_dir(&paths.config_dir);

            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "default_registry": config.default,
                    })
                );
            } else {
                println!("{}", config.default);
            }

            Ok(())
        }

        RegistryCommands::List => {
            let config = load_from_config_dir(&paths.config_dir);

            if json {
                let sources: Vec<_> = config
                    .sources
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "url": s.url,
                            "priority": s.priority,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "default": config.default,
                        "sources": sources,
                    })
                );
            } else {
                println!("Default registry: {}", config.default);
                println!();
                if config.sources.is_empty() {
                    println!("No configured sources.");
                } else {
                    println!("Configured sources:");
                    for source in &config.sources {
                        println!("  {} (priority: {})", source.url, source.priority);
                    }
                }
            }

            Ok(())
        }
    }
}
