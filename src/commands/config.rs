//! Configuration Management Commands
//!
//! Implements global configuration read/write for `~/.pekobot/config.toml`.
//! Uses dot-notation path resolution from `common::config_path`.
//!
//! ADR-028: Top-Level Config CLI

use crate::commands::GlobalPaths;
use crate::common::config_path;
use clap::Subcommand;
use std::path::PathBuf;

/// Configuration management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ConfigCommands {
    /// Validate a configuration file
    Validate {
        /// Config file path (default: ~/.pekobot/config.toml)
        file: Option<String>,
    },

    /// Initialize a new configuration
    Init {
        /// Output file
        #[arg(short, long, default_value = "pekobot.toml")]
        output: String,
        /// Template to use (minimal, full, agent)
        #[arg(short, long, default_value = "minimal")]
        template: String,
    },

    /// Show default configuration values
    Defaults,

    /// Show configuration paths
    Path,

    /// Get a configuration value
    Get {
        /// Key path (e.g., "daemon.bind_address" or "defaults.provider")
        key: String,
        /// Config file to read from
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Set a configuration value
    Set {
        /// Key path
        key: String,
        /// Value to set
        value: String,
        /// Config file to modify
        #[arg(short, long)]
        file: Option<String>,
    },
}

/// Resolve the config file path: explicit `--file` argument, or default.
fn resolve_config_path(paths: &GlobalPaths, file: Option<String>) -> PathBuf {
    file.map(PathBuf::from)
        .unwrap_or_else(|| paths.config_dir.join("config.toml"))
}

/// Read the global config TOML, returning an empty table if the file does not exist.
fn read_config(path: &PathBuf) -> anyhow::Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let contents = std::fs::read_to_string(path)?;
    let value: toml::Value = toml::from_str(&contents)?;
    Ok(value)
}

/// Write the global config TOML atomically (tmp + rename).
fn write_config(path: &PathBuf, value: &toml::Value) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;

    let contents = toml::to_string_pretty(value)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Handle config commands
pub async fn handle_config(
    cmd: ConfigCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        ConfigCommands::Validate { file } => {
            let path = resolve_config_path(paths, file);
            if !path.exists() {
                anyhow::bail!("Config file not found: {}", path.display());
            }
            let contents = std::fs::read_to_string(&path)?;
            let _: toml::Value = toml::from_str(&contents)
                .map_err(|e| anyhow::anyhow!("Invalid TOML in {}: {e}", path.display()))?;

            if json {
                println!("{{\"valid\": true, \"file\": \"{}\"}}", path.display());
            } else {
                println!("✓ Valid TOML: {}", path.display());
            }
            Ok(())
        }
        ConfigCommands::Init { output, template } => {
            let path = PathBuf::from(&output);
            if path.exists() {
                anyhow::bail!("File already exists: {}", path.display());
            }

            let default_config = match template.as_str() {
                "full" => full_config_template(),
                "agent" => agent_config_template(),
                _ => minimal_config_template(),
            };

            write_config(&path, &default_config)?;

            if json {
                println!(
                    "{{\"success\": true, \"file\": \"{}\", \"template\": \"{template}\"}}",
                    path.display()
                );
            } else {
                println!(
                    "📝 Created config: {} (template: {template})",
                    path.display()
                );
            }
            Ok(())
        }
        ConfigCommands::Defaults => {
            let defaults = minimal_config_template();
            if json {
                println!("{}", serde_json::json!(defaults));
            } else {
                println!("📋 Default Configuration:\n");
                println!("{}", toml::to_string_pretty(&defaults)?);
            }
            Ok(())
        }
        ConfigCommands::Path => {
            let config_file = paths.config_dir.join("config.toml");
            if json {
                println!(
                    "{{\"config_dir\": \"{}\", \"data_dir\": \"{}\", \"cache_dir\": \"{}\", \"config_file\": \"{}\"}}",
                    paths.config_dir.display(),
                    paths.data_dir.display(),
                    paths.cache_dir.display(),
                    config_file.display(),
                );
            } else {
                println!("📁 Configuration Paths:");
                println!("  Config dir: {}", paths.config_dir.display());
                println!("  Data dir:   {}", paths.data_dir.display());
                println!("  Cache dir:  {}", paths.cache_dir.display());
                println!("  Config file: {}", config_file.display());
            }
            Ok(())
        }
        ConfigCommands::Get { key, file } => {
            let path = resolve_config_path(paths, file);
            let config = read_config(&path)?;
            let value = config_path::get_toml_value(&config, &key)?;
            let formatted = config_path::format_toml_value(&value)?;

            if json {
                println!(
                    "{{\"key\": \"{key}\", \"value\": {}}}",
                    serde_json::to_string(&formatted)?
                );
            } else {
                println!("{formatted}");
            }
            Ok(())
        }
        ConfigCommands::Set { key, value, file } => {
            let path = resolve_config_path(paths, file);
            let config = read_config(&path)?;
            let updated = config_path::set_toml_value(config, &key, &value)?;
            write_config(&path, &updated)?;

            if json {
                println!(
                    "{{\"success\": true, \"key\": \"{key}\", \"value\": {}}}",
                    serde_json::to_string(&value)?
                );
            } else {
                println!("✅ Set '{key}' = '{value}' in {}", path.display());
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Config templates
// ---------------------------------------------------------------------------

fn minimal_config_template() -> toml::Value {
    toml::toml! {
        [daemon]
        bind_address = "127.0.0.1:11435"
        log_level = "info"

        [defaults]
        provider = "minimax"
        model = "gpt-4o-mini"
        temperature = 0.7
        max_tokens = 2048
    }
    .into()
}

fn full_config_template() -> toml::Value {
    toml::toml! {
        [daemon]
        bind_address = "127.0.0.1:11435"
        log_level = "info"

        [defaults]
        provider = "minimax"
        model = "gpt-4o-mini"
        temperature = 0.7
        max_tokens = 2048

        [paths]
        sessions = "~/.pekobot/sessions"
        registry = "~/.pekobot/registry"

        [security]
        strip_env_vars = ["*_API_KEY", "*_SECRET", "*_TOKEN", "*_PASSWORD"]
    }
    .into()
}

fn agent_config_template() -> toml::Value {
    toml::toml! {
        [agent]
        name = "my-agent"
        description = "A helpful assistant"

        [agent.provider]
        type = "openai"
        model = "gpt-4o-mini"
        temperature = 0.7
        max_tokens = 2048
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_paths() -> (GlobalPaths, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let config_dir = temp.path().join("config");
        let data_dir = temp.path().join("data");
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        let resolver = crate::common::paths::PathResolver::with_dirs(
            config_dir.clone(),
            data_dir.clone(),
            cache_dir.clone(),
        );

        let paths = GlobalPaths {
            config_dir: config_dir.clone(),
            data_dir: data_dir.clone(),
            cache_dir: cache_dir.clone(),
            resolver: resolver.clone(),
            services: crate::common::services::ServiceContainer::new(resolver),
            user: "default".to_string(),
        };
        (paths, temp)
    }

    #[tokio::test]
    async fn test_config_get_existing_key() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        let mut file = std::fs::File::create(&config_file).unwrap();
        file.write_all(b"name = \"test\"\n").unwrap();

        let cmd = ConfigCommands::Get {
            key: "name".to_string(),
            file: None,
        };
        // Should not panic / error
        handle_config(cmd, &paths, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_config_get_missing_key_errors() {
        let (paths, _temp) = temp_paths();
        let cmd = ConfigCommands::Get {
            key: "does.not.exist".to_string(),
            file: None,
        };
        let result = handle_config(cmd, &paths, false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_config_set_creates_file() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        assert!(!config_file.exists());

        let cmd = ConfigCommands::Set {
            key: "daemon.bind_address".to_string(),
            value: "0.0.0.0:8080".to_string(),
            file: None,
        };
        handle_config(cmd, &paths, false).await.unwrap();

        assert!(config_file.exists());
        let contents = std::fs::read_to_string(&config_file).unwrap();
        assert!(contents.contains("0.0.0.0:8080"));
    }

    #[tokio::test]
    async fn test_config_set_updates_existing() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        let mut file = std::fs::File::create(&config_file).unwrap();
        file.write_all(b"name = \"old\"\n").unwrap();

        let cmd = ConfigCommands::Set {
            key: "name".to_string(),
            value: "new".to_string(),
            file: None,
        };
        handle_config(cmd, &paths, false).await.unwrap();

        let contents = std::fs::read_to_string(&config_file).unwrap();
        assert!(contents.contains("new"));
    }

    #[tokio::test]
    async fn test_config_validate_valid_toml() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        let mut file = std::fs::File::create(&config_file).unwrap();
        file.write_all(b"name = \"test\"\n").unwrap();

        let cmd = ConfigCommands::Validate { file: None };
        handle_config(cmd, &paths, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_config_validate_invalid_toml() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        let mut file = std::fs::File::create(&config_file).unwrap();
        file.write_all(b"not valid toml [[[").unwrap();

        let cmd = ConfigCommands::Validate { file: None };
        let result = handle_config(cmd, &paths, false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_config_path_outputs() {
        let (paths, _temp) = temp_paths();
        let cmd = ConfigCommands::Path;
        handle_config(cmd, &paths, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_config_defaults_outputs() {
        let (paths, _temp) = temp_paths();
        let cmd = ConfigCommands::Defaults;
        handle_config(cmd, &paths, false).await.unwrap();
    }

    #[tokio::test]
    async fn test_config_set_json_output() {
        let (paths, _temp) = temp_paths();
        let cmd = ConfigCommands::Set {
            key: "name".to_string(),
            value: "test".to_string(),
            file: None,
        };
        handle_config(cmd, &paths, true).await.unwrap();
    }

    #[tokio::test]
    async fn test_config_get_json_output() {
        let (paths, _temp) = temp_paths();
        let config_file = paths.config_dir.join("config.toml");
        let mut file = std::fs::File::create(&config_file).unwrap();
        file.write_all(b"name = \"test\"\n").unwrap();

        let cmd = ConfigCommands::Get {
            key: "name".to_string(),
            file: None,
        };
        handle_config(cmd, &paths, true).await.unwrap();
    }
}
