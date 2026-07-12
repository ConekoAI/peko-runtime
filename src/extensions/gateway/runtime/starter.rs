//! Gateway Runtime Starter
//!
//! Implements `ExtensionRuntimeStarter` for gateway extensions.
//! Extracted from `src/ipc/server.rs` to eliminate hardcoded type dispatch.

use super::adapter::{GatewayFlavor, GatewayRuntimeAdapter};
use super::router::GatewayRouter;
use crate::common::process::{ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
use crate::daemon::background_runtime::starter::{ExtensionRuntimeStarter, StarterContext};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

/// Starter for gateway extensions.
///
/// Reads gateway manifests and launches out-of-process or external gateway
/// runtimes via `BackgroundRuntimeManager`.
#[derive(Debug)]
pub struct GatewayRuntimeStarter;

impl GatewayRuntimeStarter {
    /// Create a new gateway runtime starter
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for GatewayRuntimeStarter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ExtensionRuntimeStarter for GatewayRuntimeStarter {
    fn extension_type(&self) -> &'static str {
        "gateway"
    }

    async fn start(&self, extension_id: &str, ctx: &StarterContext) -> anyhow::Result<()> {
        let ext_dir = ctx.data_dir.join("extensions").join(extension_id);
        let manifest_path = ext_dir.join("manifest.yaml");

        let content = tokio::fs::read_to_string(&manifest_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read gateway manifest: {e}"))?;

        let manifest: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse gateway manifest: {e}"))?;

        let config = manifest
            .get("config")
            .ok_or_else(|| anyhow::anyhow!("Gateway manifest missing 'config' section"))?;

        let gateway_type = manifest
            .get("gateway_type")
            .and_then(|v| v.as_str())
            .unwrap_or("out-of-process");

        let router = GatewayRouter::new(Arc::clone(&ctx.principal_service));

        // Parse and register routing configuration from manifest
        let routing_config = parse_gateway_routing_config(config);
        router
            .register_gateway(extension_id, routing_config)
            .await?;

        if gateway_type == "out-of-process" {
            let command = config
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Gateway config missing 'command'"))?;
            let args: Vec<String> = config
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let env: HashMap<String, String> = config
                .get("env")
                .and_then(|v| v.as_mapping())
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| {
                            Some((k.as_str()?.to_string(), v.as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let mut process_config = ProcessSpawnConfig::new(command)
                .args(args.clone())
                .cwd(&ext_dir);
            for (key, value) in &env {
                process_config = process_config.env(key.clone(), value.clone());
            }

            let spawn_config = RuntimeSpawnConfig::Process(process_config);
            let adapter = Arc::new(GatewayRuntimeAdapter::new(
                Arc::new(router),
                GatewayFlavor::OutOfProcess {
                    command: command.to_string(),
                    args: args.clone(),
                    env: HashMap::new(),
                    cwd: Some(ext_dir.clone()),
                },
            ));

            ctx.background_runtime_manager
                .start(
                    extension_id.to_string(),
                    spawn_config,
                    adapter,
                    RestartPolicy::default(),
                )
                .await?;
        } else if gateway_type == "external" {
            let endpoint = config
                .get("endpoint")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("External gateway config missing 'endpoint'"))?;
            let webhook_secret = config
                .get("webhook_secret")
                .and_then(|v| v.as_str())
                .map(String::from);

            let spawn_config = RuntimeSpawnConfig::External {
                endpoint: endpoint.to_string(),
                connect_timeout: std::time::Duration::from_secs(10),
            };
            let adapter = Arc::new(GatewayRuntimeAdapter::new(
                Arc::new(router),
                GatewayFlavor::External {
                    endpoint: endpoint.to_string(),
                    webhook_secret,
                },
            ));

            ctx.background_runtime_manager
                .start(
                    extension_id.to_string(),
                    spawn_config,
                    adapter,
                    RestartPolicy::default(),
                )
                .await?;
        } else {
            anyhow::bail!("Unknown gateway_type: {}", gateway_type);
        }

        info!("Gateway '{}' started successfully", extension_id);
        Ok(())
    }
}

/// Parse gateway routing configuration from manifest config section
fn parse_gateway_routing_config(config: &serde_yaml::Value) -> super::router::GatewayRoutingConfig {
    use super::router::GatewayRoutingConfig;

    let default_agent = config
        .get("routing")
        .and_then(|r| r.get("default_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("assistant")
        .to_string();

    let channel_map: HashMap<String, String> = config
        .get("routing")
        .and_then(|r| r.get("channel_map"))
        .and_then(|v| v.as_mapping())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let dm_agents: HashMap<String, String> = config
        .get("routing")
        .and_then(|r| r.get("dm_agents"))
        .and_then(|v| v.as_mapping())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.as_str()?.to_string(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();

    GatewayRoutingConfig {
        default_agent,
        channel_map,
        dm_agents,
    }
}
