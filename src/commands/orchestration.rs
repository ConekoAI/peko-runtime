//! Orchestration layer CLI commands
//!
//! Commands for managing event routing, file watching, and webhooks.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{info, warn};

use crate::orchestration::config::{
    FileWatchConfig, OrchestrationConfig, WebhookRouteConfig,
};
use crate::types::config::PekobotConfig;

/// Orchestration management commands
#[derive(Subcommand)]
pub enum OrchestrationCommands {
    /// List registered event handlers
    Handlers {
        /// Filter by event type
        #[arg(short, long)]
        event_type: Option<String>,
    },

    /// Watch a directory for changes
    Watch {
        /// Path to watch
        path: PathBuf,

        /// Agent to invoke on changes
        #[arg(short, long)]
        agent: String,

        /// File pattern filter (glob)
        #[arg(short, long)]
        pattern: Option<String>,

        /// Watch recursively
        #[arg(short, long, default_value = "true")]
        recursive: bool,

        /// Debounce duration in milliseconds
        #[arg(short, long, default_value = "1000")]
        debounce_ms: u64,
    },

    /// Unwatch a directory
    Unwatch {
        /// Path to stop watching
        path: PathBuf,
    },

    /// Register a webhook route
    WebhookAdd {
        /// Route path (e.g., "/github")
        path: String,

        /// Agent to invoke
        agent: String,

        /// Source identifier
        #[arg(short, long, default_value = "webhook")]
        source: String,

        /// Secret for HMAC verification
        #[arg(short, long)]
        secret: Option<String>,
    },

    /// Remove a webhook route
    WebhookRemove {
        /// Route path to remove
        path: String,
    },

    /// List webhook routes
    WebhookList,

    /// View recent events
    Events {
        /// Number of events to show
        #[arg(short, long, default_value = "50")]
        limit: usize,

        /// Filter by event type
        #[arg(short, long)]
        event_type: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Replay an event by ID
    Replay {
        /// Event ID to replay
        event_id: String,
    },

    /// Show orchestration status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Validate configuration
    Validate,
}

/// Run orchestration command
pub async fn run(
    cmd: OrchestrationCommands,
    config: &PekobotConfig,
    config_path: &PathBuf,
) -> anyhow::Result<()> {
    match cmd {
        OrchestrationCommands::Handlers { event_type } => {
            info!("Listing event handlers");

            if let Some(et) = event_type {
                println!("Handlers for event type: {}", et);
            } else {
                println!("Registered event handlers:");
            }

            // TODO: Query EventRouter for registered handlers
            println!("  (Handler introspection not yet implemented)");

            Ok(())
        }

        OrchestrationCommands::Watch {
            path,
            agent,
            pattern,
            recursive,
            debounce_ms,
        } => {
            info!("Adding file watch: {:?} -> agent: {}", path, agent);

            let watch = FileWatchConfig {
                path: path.clone(),
                agent_id: agent.clone(),
                filter: pattern.clone(),
                recursive,
                debounce_ms,
            };

            // Load current config, modify, and save
            let mut new_config = config.clone();
            new_config.orchestration.add_file_watch(watch);
            new_config.orchestration.file_watcher.enabled = true;

            // Save config
            new_config.to_file(config_path)?;

            println!("Added watch:");
            println!("  Path: {:?}", path);
            println!("  Agent: {}", agent);
            if let Some(p) = pattern {
                println!("  Pattern: {}", p);
            }
            println!("  Recursive: {}", recursive);
            println!("  Debounce: {}ms", debounce_ms);
            println!("\nNote: File watcher will start on next daemon restart");

            Ok(())
        }

        OrchestrationCommands::Unwatch { path } => {
            info!("Removing file watch: {:?}", path);

            let mut new_config = config.clone();
            new_config
                .orchestration
                .file_watcher
                .watches
                .retain(|w| w.path != path);

            // Disable file watcher if no watches remain
            if new_config.orchestration.file_watcher.watches.is_empty() {
                new_config.orchestration.file_watcher.enabled = false;
            }

            new_config.to_file(config_path)?;

            println!("Removed watch: {:?}", path);

            Ok(())
        }

        OrchestrationCommands::WebhookAdd {
            path,
            agent,
            source,
            secret,
        } => {
            info!("Adding webhook route: {} -> agent: {}", path, agent);

            let route = WebhookRouteConfig {
                path: path.clone(),
                agent_id: agent.clone(),
                source: source.clone(),
                secret: secret.clone(),
            };

            let mut new_config = config.clone();
            new_config.orchestration.add_webhook_route(route);
            new_config.orchestration.webhook.enabled = true;

            new_config.to_file(config_path)?;

            println!("Added webhook route:");
            println!("  Path: {}", path);
            println!("  Agent: {}", agent);
            println!("  Source: {}", source);
            if secret.is_some() {
                println!("  Secret: [configured]");
            }
            println!("\nNote: Webhook server will start on next daemon restart");

            Ok(())
        }

        OrchestrationCommands::WebhookRemove { path } => {
            info!("Removing webhook route: {}", path);

            let mut new_config = config.clone();
            new_config
                .orchestration
                .webhook
                .routes
                .retain(|r| r.path != path);

            // Disable webhook if no routes remain
            if new_config.orchestration.webhook.routes.is_empty() {
                new_config.orchestration.webhook.enabled = false;
            }

            new_config.to_file(config_path)?;

            println!("Removed webhook route: {}", path);

            Ok(())
        }

        OrchestrationCommands::WebhookList => {
            println!("Registered webhook routes:");

            if config.orchestration.webhook.routes.is_empty() {
                println!("  (none)");
            } else {
                for route in &config.orchestration.webhook.routes {
                    println!("  {} -> {} (source: {})",
                        route.path,
                        route.agent_id,
                        route.source
                    );
                    if route.secret.is_some() {
                        println!("    [secret configured]");
                    }
                }
            }

            println!("\nWebhook server: {}",
                if config.orchestration.webhook.enabled {
                    format!("enabled on port {}", config.orchestration.webhook.port)
                } else {
                    "disabled".to_string()
                }
            );

            Ok(())
        }

        OrchestrationCommands::Events {
            limit,
            event_type,
            json,
        } => {
            info!("Showing recent events (limit: {})", limit);

            if json {
                println!("{{");
                println!("  \"events\": [],");
                println!("  \"note\": \"Event history requires active daemon\"");
                println!("}}");
            } else {
                println!("Recent events:");
                if let Some(et) = event_type {
                    println!("  (filtered by type: {})", et);
                }
                println!("  (Event history requires active daemon connection)");
            }

            Ok(())
        }

        OrchestrationCommands::Replay { event_id } => {
            info!("Replaying event: {}", event_id);

            // TODO: Query event history and replay
            println!("Replaying event: {}", event_id);
            println!("  (Event replay requires active daemon)");

            Ok(())
        }

        OrchestrationCommands::Status { json } => {
            let status = serde_json::json!({
                "enabled": config.orchestration.enabled,
                "webhook": {
                    "enabled": config.orchestration.webhook.enabled,
                    "port": config.orchestration.webhook.port,
                    "routes_count": config.orchestration.webhook.routes.len(),
                },
                "file_watcher": {
                    "enabled": config.orchestration.file_watcher.enabled,
                    "watches_count": config.orchestration.file_watcher.watches.len(),
                },
                "router": {
                    "max_history": config.orchestration.router.max_history,
                    "log_events": config.orchestration.router.log_events,
                }
            });

            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("Orchestration Status:");
                println!("  Enabled: {}", config.orchestration.enabled);
                println!("\n  Webhook Server:");
                println!("    Status: {}",
                    if config.orchestration.webhook.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!("    Port: {}", config.orchestration.webhook.port);
                println!("    Routes: {}", config.orchestration.webhook.routes.len());
                println!("\n  File Watcher:");
                println!("    Status: {}",
                    if config.orchestration.file_watcher.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!("    Watches: {}", config.orchestration.file_watcher.watches.len());
                println!("\n  Event Router:");
                println!("    Max History: {}", config.orchestration.router.max_history);
                println!("    Log Events: {}", config.orchestration.router.log_events);
            }

            Ok(())
        }

        OrchestrationCommands::Validate => {
            info!("Validating orchestration configuration");

            match config.orchestration.validate() {
                Ok(()) => {
                    println!("✓ Configuration is valid");
                    Ok(())
                }
                Err(e) => {
                    warn!("Configuration validation failed: {}", e);
                    println!("✗ Configuration error: {}", e);
                    Err(e)
                }
            }
        }
    }
}
