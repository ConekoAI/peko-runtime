//! Orchestration layer CLI commands
//!
//! Commands for managing event routing, file watching, and webhooks.

use clap::Subcommand;
use std::path::PathBuf;
use tracing::{info, warn};

use crate::types::config::{
    ExternalSource, FileWatchConfig, PekobotConfig, SourceDetection, WebhookRouteConfig,
};

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

    /// Add an external source (unified ingress)
    IngressAdd {
        /// Source name (e.g., "github", "discord")
        name: String,

        /// Agent to invoke
        #[arg(short, long)]
        agent: String,

        /// Detection header name
        #[arg(long)]
        header: Option<String>,

        /// Detection payload field path (e.g., "event.type")
        #[arg(long)]
        payload_field: Option<String>,

        /// User-Agent substring to match
        #[arg(long)]
        user_agent: Option<String>,

        /// Expected header/payload value (optional)
        #[arg(long)]
        value: Option<String>,
    },

    /// Remove an external source
    IngressRemove {
        /// Source name to remove
        name: String,
    },

    /// List external sources
    IngressList,

    /// Enable unified external ingress
    IngressEnable {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },

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
                println!("Handlers for event type: {et}");
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
            println!("  Path: {path:?}");
            println!("  Agent: {agent}");
            if let Some(p) = pattern {
                println!("  Pattern: {p}");
            }
            println!("  Recursive: {recursive}");
            println!("  Debounce: {debounce_ms}ms");
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

            println!("Removed watch: {path:?}");

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
            println!("  Path: {path}");
            println!("  Agent: {agent}");
            println!("  Source: {source}");
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

            println!("Removed webhook route: {path}");

            Ok(())
        }

        OrchestrationCommands::WebhookList => {
            println!("Registered webhook routes:");

            if config.orchestration.webhook.routes.is_empty() {
                println!("  (none)");
            } else {
                for route in &config.orchestration.webhook.routes {
                    println!(
                        "  {} -> {} (source: {})",
                        route.path, route.agent_id, route.source
                    );
                    if route.secret.is_some() {
                        println!("    [secret configured]");
                    }
                }
            }

            println!(
                "\nWebhook server: {}",
                if config.orchestration.webhook.enabled {
                    format!("enabled on port {}", config.orchestration.webhook.port)
                } else {
                    "disabled".to_string()
                }
            );

            Ok(())
        }

        OrchestrationCommands::IngressAdd {
            name,
            agent,
            header,
            payload_field,
            user_agent,
            value,
        } => {
            info!("Adding external source: {} -> agent: {}", name, agent);

            // Determine detection method
            let detection = if let Some(header_name) = header {
                SourceDetection::Header {
                    name: header_name,
                    value_prefix: value,
                }
            } else if let Some(field_path) = payload_field {
                SourceDetection::PayloadField {
                    path: field_path,
                    value,
                }
            } else if let Some(ua) = user_agent {
                SourceDetection::UserAgent { contains: ua }
            } else {
                return Err(anyhow::anyhow!(
                    "Must specify one of: --header, --payload-field, or --user-agent"
                ));
            };

            let source = ExternalSource {
                name: name.clone(),
                detection,
                agent_id: agent.clone(),
                verification: None,
                transform: None,
            };

            let mut new_config = config.clone();
            new_config.orchestration.add_external_source(source);
            new_config.orchestration.external_ingress.enabled = true;

            new_config.to_file(config_path)?;

            println!("Added external source:");
            println!("  Name: {name}");
            println!("  Agent: {agent}");
            println!("\nNote: External ingress will start on next daemon restart");
            println!(
                "  Configure external services to POST to: http://your-host:{}/webhook/ingress",
                new_config.orchestration.external_ingress.port
            );

            Ok(())
        }

        OrchestrationCommands::IngressRemove { name } => {
            info!("Removing external source: {}", name);

            let mut new_config = config.clone();
            new_config
                .orchestration
                .external_ingress
                .sources
                .retain(|s| s.name != name);

            // Disable if no sources remain
            if new_config.orchestration.external_ingress.sources.is_empty() {
                new_config.orchestration.external_ingress.enabled = false;
            }

            new_config.to_file(config_path)?;

            println!("Removed external source: {name}");

            Ok(())
        }

        OrchestrationCommands::IngressList => {
            println!("Registered external sources (unified ingress):");

            if config.orchestration.external_ingress.sources.is_empty() {
                println!("  (none)");
            } else {
                for source in &config.orchestration.external_ingress.sources {
                    println!("  {} -> {}", source.name, source.agent_id);
                    match &source.detection {
                        SourceDetection::Header { name, value_prefix } => {
                            if let Some(prefix) = value_prefix {
                                println!("    [header: {name}={prefix}*]");
                            } else {
                                println!("    [header: {name}]");
                            }
                        }
                        SourceDetection::PayloadField { path, value } => {
                            if let Some(val) = value {
                                println!("    [payload: {path}={val}]");
                            } else {
                                println!("    [payload: {path}]");
                            }
                        }
                        SourceDetection::UserAgent { contains } => {
                            println!("    [user-agent: contains '{contains}']");
                        }
                    }
                }
            }

            println!(
                "\nExternal ingress: {}",
                if config.orchestration.external_ingress.enabled {
                    format!(
                        "enabled on port {}",
                        config.orchestration.external_ingress.port
                    )
                } else {
                    "disabled".to_string()
                }
            );
            println!(
                "  Endpoint: {}",
                config.orchestration.external_ingress.endpoint
            );

            Ok(())
        }

        OrchestrationCommands::IngressEnable { port } => {
            info!("Enabling external ingress on port {}", port);

            let mut new_config = config.clone();
            new_config.orchestration.external_ingress.enabled = true;
            new_config.orchestration.external_ingress.port = port;

            new_config.to_file(config_path)?;

            println!("External ingress enabled on port {port}");
            println!(
                "  Endpoint: {}",
                new_config.orchestration.external_ingress.endpoint
            );
            println!("\nConfigure external services to POST to:");
            println!("  http://your-host:{port}/webhook/ingress");
            println!("\nAdd sources with: peko orchestration ingress-add");

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
                    println!("  (filtered by type: {et})");
                }
                println!("  (Event history requires active daemon connection)");
            }

            Ok(())
        }

        OrchestrationCommands::Replay { event_id } => {
            info!("Replaying event: {}", event_id);

            // TODO: Query event history and replay
            println!("Replaying event: {event_id}");
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
                "external_ingress": {
                    "enabled": config.orchestration.external_ingress.enabled,
                    "port": config.orchestration.external_ingress.port,
                    "endpoint": config.orchestration.external_ingress.endpoint,
                    "sources_count": config.orchestration.external_ingress.sources.len(),
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
                println!(
                    "    Status: {}",
                    if config.orchestration.webhook.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!("    Port: {}", config.orchestration.webhook.port);
                println!("    Routes: {}", config.orchestration.webhook.routes.len());
                println!("\n  External Ingress (Unified):");
                println!(
                    "    Status: {}",
                    if config.orchestration.external_ingress.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!("    Port: {}", config.orchestration.external_ingress.port);
                println!(
                    "    Endpoint: {}",
                    config.orchestration.external_ingress.endpoint
                );
                println!(
                    "    Sources: {}",
                    config.orchestration.external_ingress.sources.len()
                );
                println!("\n  File Watcher:");
                println!(
                    "    Status: {}",
                    if config.orchestration.file_watcher.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                println!(
                    "    Watches: {}",
                    config.orchestration.file_watcher.watches.len()
                );
                println!("\n  Event Router:");
                println!(
                    "    Max History: {}",
                    config.orchestration.router.max_history
                );
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
                    println!("✗ Configuration error: {e}");
                    Err(e)
                }
            }
        }
    }
}
