//! Configuration Management Commands
//!
//! Implements global configuration read/write for `~/.peko/config.toml`.
//! Uses dot-notation path resolution from `common::config_path`.
//!
//! ADR-028: Top-Level Config CLI

use crate::commands::GlobalPaths;
use clap::Subcommand;
use peko_core::common::config_path;
use std::path::PathBuf;

/// Configuration management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ConfigCommands {
    /// Validate a configuration file
    Validate {
        /// Config file path (default: ~/.peko/config.toml)
        file: Option<String>,
    },

    /// Initialize a new configuration
    Init {
        /// Output file
        #[arg(short, long, default_value = "peko.toml")]
        output: String,
        /// Template to use (minimal, full)
        #[arg(short, long, default_value = "minimal")]
        template: String,
    },

    /// Show default configuration values
    Defaults,

    /// Show configuration paths
    Path,

    /// Get a configuration value
    Get {
        /// Key path (e.g., "provider.retry.max_retries")
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

            let Some(contents) = config_template(&template) else {
                anyhow::bail!("Unknown template '{template}' (available: minimal, full)");
            };

            // Atomic write (tmp + rename), same discipline as write_config.
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, contents)?;
            std::fs::rename(&tmp, &path)?;

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
            let defaults = MINIMAL_CONFIG_TEMPLATE;
            if json {
                let value: toml::Value = toml::from_str(defaults)?;
                println!("{}", serde_json::to_string(&value)?);
            } else {
                println!("📋 Default Configuration:\n");
                println!("{defaults}");
            }
            Ok(())
        }
        ConfigCommands::Path => {
            let config_file = paths.config_dir.join("config.toml");
            if json {
                // Build a JSON value and use `serde_json::to_string` so
                // path separators (Windows backslashes) and any other
                // non-JSON-safe characters are correctly escaped. The
                // previous implementation formatted paths directly into
                // a JSON string template via `display()`, which on
                // Windows emitted raw `\` bytes that are not valid
                // JSON (e.g. `{"config_dir": "C:\Users\..."}` is a
                // parse error at column 20). Surfaced by
                // `tests/cli_basics.rs::config_path_json_output` and
                // `Phase E` audit.
                let value = serde_json::json!({
                    "config_dir": paths.config_dir.to_string_lossy(),
                    "data_dir": paths.data_dir.to_string_lossy(),
                    "cache_dir": paths.cache_dir.to_string_lossy(),
                    "config_file": config_file.to_string_lossy(),
                });
                println!("{}", serde_json::to_string(&value)?);
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
//
// The daemon reads `peko.toml` from the config dir (`$PEKO_HOME` or
// `~/.peko`); the only block it consumes today is `[provider.retry]`.
// Models and API keys live in the catalog + vault (`peko model add
// --key ...`), per-peko settings in each peko's `principal.toml`, and
// session-compaction tuning in the `[compaction]` block of
// `~/.peko/config.toml`. Templates carry comments, so they are string
// constants rather than `toml::Value` builders.

const MINIMAL_CONFIG_TEMPLATE: &str = r#"# Peko daemon configuration (peko.toml)
# Everything here is optional — the daemon boots with built-in defaults.
# Models/keys: `peko model add --template <id> --model <wire-id> --key "$KEY"`.
# Per-peko settings: the peko's own principal.toml.

[provider.retry]
max_retries = 5          # transport-level retries for transient errors
retry_delay_ms = 1000    # initial backoff (doubles per attempt)
retry_max_delay_ms = 30000
retry_jitter = 0.1       # ±10% uniform spread; 0 disables jitter
max_attempts = 8         # total transport+engine attempts (shared budget)
"#;

const FULL_CONFIG_TEMPLATE: &str = r#"# Peko configuration — two files, two jobs:
#
#   ~/.peko/peko.toml     — daemon config; only [provider.retry] is consumed
#   ~/.peko/config.toml   — session compaction tuning ([compaction] block)
#                           and `peko config get/set` scratch space
#
# Everything else lives outside both files:
#   models + API keys ... `peko model add` (catalog: ~/.peko/models.toml;
#                         keys: OS keychain vault via `peko credential set`)
#   per-peko settings ... the peko's principal.toml (quota, capabilities,
#                         routing) — edit directly or via `peko quota set`

# ── peko.toml ─────────────────────────────────────────────────────────

[provider.retry]
max_retries = 5
retry_delay_ms = 1000
retry_max_delay_ms = 30000
retry_jitter = 0.1
max_attempts = 8

# ── config.toml (move this block into ~/.peko/config.toml to activate) ──
#
# [compaction]
# enabled = true
# auto_threshold_percent = 85       # trigger at 85% of model context limit
# reserve_tokens = 16384            # tokens reserved for the LLM response
# keep_recent_tokens = 20000        # minimum recent conversation preserved
# max_compactions_per_session = 100
# cooldown_seconds = 60
#
# [compaction.model_limits]         # optional context-window overrides
# openai.gpt-4o = 128000
# kimi.K2.6 = 262144
"#;

fn config_template(template: &str) -> Option<&'static str> {
    match template {
        "minimal" => Some(MINIMAL_CONFIG_TEMPLATE),
        "full" => Some(FULL_CONFIG_TEMPLATE),
        _ => None,
    }
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

        let paths = GlobalPaths::new(config_dir, data_dir, cache_dir, "default".to_string());
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
            key: "daemon.log_level".to_string(),
            value: "debug".to_string(),
            file: None,
        };
        handle_config(cmd, &paths, false).await.unwrap();

        assert!(config_file.exists());
        let contents = std::fs::read_to_string(&config_file).unwrap();
        assert!(contents.contains("debug"));
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
