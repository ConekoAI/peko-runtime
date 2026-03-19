//! Send Command - Unified message sending to agents
//!
//! This command replaces the deprecated `agent start --message` and `session send` commands
//! with a unified, top-level interface for sending messages to agents.
//!
//! Per ADR-013, agents are stateless and cold-start on every request. This command
//! performs a cold-start sequence: load config, load session, instantiate tools,
//! run agentic loop, then exit and free resources.
//!
//! Examples:
//!   pekobot send myagent "What is the weather?"
//!   pekobot send myteam/myagent "Hello"
//!   pekobot send myagent --team myteam "Hello"
//!   pekobot send myagent "Hello" --session sess_xxx
//!   pekobot send myagent --new "Start fresh"
//!   echo "Hello" | pekobot send myagent --stdin
//!   pekobot send myagent --file prompt.txt

use crate::agent::Agent;
use crate::commands::identifier::parse_agent_identifier_with_override;
use crate::commands::GlobalPaths;
use crate::types::agent::AgentConfig;
use crate::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use anyhow::Result;
use clap::Args;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::info;

/// Send a message to an agent (unified command)
///
/// This is the primary way to interact with agents. Agents cold-start on each
/// request, load their configuration and session history, process the message,
/// then exit and free all resources.
///
/// The message can be provided as an argument, from a file, or via stdin.
/// Agent can be specified as just "name" (uses default team) or "team/name".
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct SendArgs {
    /// Agent name or team/agent format (e.g., "myagent" or "myteam/myagent")
    pub agent: String,

    /// Message to send (optional if --file or --stdin is used)
    pub message: Option<String>,

    /// Team to look in (overrides team/ prefix if both provided)
    #[arg(short, long)]
    pub team: Option<String>,

    /// Custom configuration file path (optional)
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<String>,

    /// Specific session ID to use (optional, uses active session if not provided)
    #[arg(short, long, value_name = "SESSION")]
    pub session: Option<String>,

    /// Start a new session (don't resume existing)
    #[arg(short, long)]
    pub new: bool,

    /// Read message from file
    #[arg(short, long, value_name = "PATH", conflicts_with = "stdin")]
    pub file: Option<String>,

    /// Read message from stdin
    #[arg(long, conflicts_with = "file")]
    pub stdin: bool,

    /// LLM provider override (openai, anthropic, ollama, kimi, kimi_code)
    #[arg(short, long)]
    pub provider: Option<String>,

    /// Model name override
    #[arg(short, long)]
    pub model: Option<String>,
}

/// Handle the send command
pub async fn handle_send(args: SendArgs, paths: &GlobalPaths, _json: bool) -> Result<()> {
    // Resolve the message content
    let message = resolve_message(&args).await?;

    // Parse agent identifier to extract team and agent name
    let (team, agent_name) = parse_agent_identifier_with_override(&args.agent, args.team.as_deref())?;

    // Determine config path
    let config_path = resolve_config_path(agent_name, args.config.as_ref(), team, paths)?;

    info!("Sending message to agent '{}' in team '{}'", agent_name, team);
    info!("Config path: {}", config_path.display());

    // Load or build agent configuration
    let agent_config = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)?;
        toml::from_str(&content)?
    } else {
        info!(
            "No config file found at {}, using default configuration",
            config_path.display()
        );
        let provider = args.provider.as_deref().unwrap_or("kimi_code");
        build_default_config(agent_name, provider, args.model, None)
    };

    // Create and start the agent (cold-start)
    let agent = match Agent::new(agent_config).await {
        Ok(agent) => agent,
        Err(e) => {
            eprintln!("❌ Failed to create agent: {e}");
            return Err(e);
        }
    };

    if let Err(e) = agent.start().await {
        eprintln!("❌ Failed to start agent: {e}");
        return Err(e);
    }

    // Execute in a LocalSet for spawn_local compatibility
    let local = tokio::task::LocalSet::new();

    local
        .run_until(async {
            // Handle session selection:
            // --new: Force new session
            // --session: Use specific session (TODO: implement session switching)
            // Neither: Resume active session (or create new if none exists)
            let new_session = args.new;

            if let Some(ref session_id) = args.session {
                info!("Using session: {}", session_id);
                // TODO: Implement session switching when specific session ID provided
                // For now, we still use the default behavior but log the intent
            }

            crate::channels::cli::send_single_message_with_session(&agent, &message, new_session)
                .await
        })
        .await?;

    Ok(())
}

/// Resolve message from various sources (argument, file, or stdin)
async fn resolve_message(args: &SendArgs) -> Result<String> {
    // Priority: --stdin > --file > positional argument
    if args.stdin {
        let mut buffer = String::new();
        std::io::stdin().read_line(&mut buffer)?;
        Ok(buffer.trim().to_string())
    } else if let Some(ref file_path) = args.file {
        match std::fs::read_to_string(file_path) {
            Ok(content) => Ok(content),
            Err(e) => {
                anyhow::bail!("Failed to read message file '{}': {}", file_path, e);
            }
        }
    } else if let Some(ref message) = args.message {
        Ok(message.clone())
    } else {
        anyhow::bail!(
            "Message is required. Provide it as an argument, use --file, or --stdin.\n\
             Examples:\n  \
             pekobot send myagent \"Hello\"\n  \
             pekobot send myagent --file prompt.txt\n  \
             echo \"Hello\" | pekobot send myagent --stdin"
        );
    }
}

/// Resolve the configuration file path
fn resolve_config_path(
    agent_name: &str,
    config_override: Option<&String>,
    team: &str,
    paths: &GlobalPaths,
) -> Result<PathBuf> {
    if let Some(path) = config_override {
        Ok(PathBuf::from(path))
    } else {
        // Default location: ~/.pekobot/teams/{team}/agents/{name}/config.toml
        Ok(paths.agent_config(agent_name, Some(team)))
    }
}

/// Build default agent config
fn build_default_config(
    name: &str,
    provider: &str,
    model: Option<String>,
    _db: Option<String>,
) -> AgentConfig {
    let provider_type = match provider.to_lowercase().as_str() {
        "openai" => ProviderType::OpenAI,
        "anthropic" => ProviderType::Anthropic,
        "ollama" => ProviderType::Ollama,
        "moonshot" => ProviderType::Moonshot,
        "kimi" => ProviderType::Kimi,
        _ => ProviderType::OpenAI,
    };

    let default_model = model.unwrap_or_else(|| "default".to_string());

    let mut models = HashMap::new();
    models.insert(
        "default".to_string(),
        ModelConfig {
            name: match provider_type {
                ProviderType::OpenAI => "gpt-4o-mini".to_string(),
                ProviderType::Anthropic => "claude-3-sonnet".to_string(),
                ProviderType::Ollama => "llama3.2".to_string(),
                ProviderType::OpenAICompatible => "default".to_string(),
                ProviderType::Moonshot => "moonshot-v1-8k".to_string(),
                ProviderType::Kimi => "k2p5".to_string(),
            },
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    );

    AgentConfig {
        version: "1.0".to_string(),
        name: name.to_string(),
        description: Some(format!("Pekobot agent: {name}")),
        team: None,
        tenant: None,
        capabilities: vec![],
        provider: ProviderConfig {
            provider_type,
            api_key: None,
            api_key_env: match provider_type {
                ProviderType::OpenAI => Some("OPENAI_API_KEY".to_string()),
                ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".to_string()),
                ProviderType::Moonshot => Some("MOONSHOT_API_KEY".to_string()),
                ProviderType::Kimi => Some("KIMI_API_KEY".to_string()),
                _ => None,
            },
            base_url: match provider_type {
                ProviderType::OpenAI => None,
                ProviderType::Anthropic => None,
                ProviderType::Ollama => Some("http://localhost:11434".to_string()),
                ProviderType::OpenAICompatible => None,
                ProviderType::Moonshot => Some("https://api.moonshot.cn/v1".to_string()),
                ProviderType::Kimi => Some("https://api.kimi.com/coding".to_string()),
            },
            default_model,
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        memory: None,
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(100.0),
        default_timeout_seconds: 300,
        workspace: None,
        prompt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_default_config() {
        let config = build_default_config("test-agent", "openai", None, None);
        assert_eq!(config.name, "test-agent");
        assert!(matches!(
            config.provider.provider_type,
            ProviderType::OpenAI
        ));
    }

    #[test]
    fn test_resolve_config_path_with_override() {
        let paths = GlobalPaths {
            config_dir: PathBuf::from("/home/test/.pekobot"),
            data_dir: PathBuf::from("/home/test/.local/share/pekobot"),
            cache_dir: PathBuf::from("/home/test/.cache/pekobot"),
            resolver: crate::common::paths::PathResolver::with_dirs(
                PathBuf::from("/home/test/.pekobot"),
                PathBuf::from("/home/test/.local/share/pekobot"),
                PathBuf::from("/home/test/.cache/pekobot"),
            ),
        };

        let result =
            resolve_config_path("myagent", Some(&"/custom/config.toml".to_string()), "default", &paths)
                .unwrap();
        assert_eq!(result, PathBuf::from("/custom/config.toml"));
    }

    #[test]
    fn test_resolve_config_path_default_team() {
        let paths = GlobalPaths {
            config_dir: PathBuf::from("/home/test/.pekobot"),
            data_dir: PathBuf::from("/home/test/.local/share/pekobot"),
            cache_dir: PathBuf::from("/home/test/.cache/pekobot"),
            resolver: crate::common::paths::PathResolver::with_dirs(
                PathBuf::from("/home/test/.pekobot"),
                PathBuf::from("/home/test/.local/share/pekobot"),
                PathBuf::from("/home/test/.cache/pekobot"),
            ),
        };

        let result = resolve_config_path("myagent", None, "default", &paths).unwrap();
        assert_eq!(
            result,
            PathBuf::from("/home/test/.pekobot/teams/default/agents/myagent/config.toml")
        );
    }

    #[test]
    fn test_resolve_config_path_custom_team() {
        let paths = GlobalPaths {
            config_dir: PathBuf::from("/home/test/.pekobot"),
            data_dir: PathBuf::from("/home/test/.local/share/pekobot"),
            cache_dir: PathBuf::from("/home/test/.cache/pekobot"),
            resolver: crate::common::paths::PathResolver::with_dirs(
                PathBuf::from("/home/test/.pekobot"),
                PathBuf::from("/home/test/.local/share/pekobot"),
                PathBuf::from("/home/test/.cache/pekobot"),
            ),
        };

        let result = resolve_config_path("myagent", None, "myteam", &paths).unwrap();
        assert_eq!(
            result,
            PathBuf::from("/home/test/.pekobot/teams/myteam/agents/myagent/config.toml")
        );
    }
}
