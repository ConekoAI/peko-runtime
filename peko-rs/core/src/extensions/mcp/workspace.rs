//! Workspace-resident MCP server scanner (ADR-047 §2.3).
//!
//! Phase 2 PR 2 deletes the framework-coupled `McpAdapter`. The
//! per-extension `register_adapter` path is gone — MCP servers are now
//! files inside the principal's workspace
//! (`<workspace>/mcp/<id>/server.json` or
//! `<workspace>/mcp/<id>/manifest.yaml`). The
//! [`load_workspace_mcp_servers`] walker reads each server's config
//! and registers it with the global [`McpManager`].
//!
//! No caching of the tool list: a single MCP server can add or remove
//! tools at runtime, so the agent always asks the manager for the
//! current set when assembling its tool bag.
//!
//! Server lifecycle is unchanged from the framework path — the
//! framework's `McpServerInitHandler` did *not* auto-start servers
//! (the comment on `McpServerInitHandler::handle` explains why);
//! `McpToolProxy::call_with_auto_start` starts the server on first
//! tool call.

use crate::extensions::mcp::protocol::config::McpServerConfig;
use crate::extensions::mcp::protocol::manager::McpManager;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, warn};

/// Scan `<workspace>/mcp/<id>/server.json` and
/// `<workspace>/mcp/<id>/manifest.yaml`, registering each server with
/// `manager`. Returns the number of newly-added servers.
///
/// Servers whose `name` already exists on the manager are skipped
/// (matching `McpManager::add_server_config`'s "false if already
/// exists" contract).
///
/// The walker accepts both nested (one server per subdirectory) and
/// flat (one server per file) layouts — the nested layout is the
/// canonical workspace shape from ADR-047 §2.1; the flat layout is
/// accepted for parity with the legacy extensions directory.
pub async fn load_workspace_mcp_servers(
    mcp_dir: &Path,
    manager: &Arc<TokioRwLock<McpManager>>,
) -> anyhow::Result<usize> {
    if !mcp_dir.exists() {
        debug!(
            "MCP workspace dir does not exist, skipping: {}",
            mcp_dir.display()
        );
        return Ok(0);
    }

    let mut entries = match tokio::fs::read_dir(mcp_dir).await {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Failed to read MCP workspace dir {}: {e}",
                mcp_dir.display()
            );
            return Ok(0);
        }
    };

    let mut dir_entries = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        dir_entries.push(entry);
    }

    let mut loaded = 0usize;
    for entry in dir_entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        let path = entry.path();
        let manifest_path = if path.is_dir() {
            let server_json = path.join("server.json");
            let manifest_yaml = path.join("manifest.yaml");
            if server_json.exists() {
                server_json
            } else if manifest_yaml.exists() {
                manifest_yaml
            } else {
                continue;
            }
        } else if path.is_file() {
            path.clone()
        } else {
            continue;
        };

        match parse_server_manifest(&manifest_path).await {
            Ok(config) => {
                let mgr = manager.read().await;
                match mgr.add_server_config(config).await {
                    Ok(true) => loaded += 1,
                    Ok(false) => debug!(
                        server = %manifest_path.display(),
                        "MCP server config already registered"
                    ),
                    Err(e) => warn!(
                        server = %manifest_path.display(),
                        error = %e,
                        "Failed to register MCP server config"
                    ),
                }
            }
            Err(e) => warn!(
                path = %manifest_path.display(),
                error = %e,
                "Failed to parse MCP server manifest"
            ),
        }
    }

    Ok(loaded)
}

/// Parse a single MCP server manifest (`server.json` or
/// `manifest.yaml`) into an [`McpServerConfig`].
///
/// The MCP Registry `server.json` format is normalised through
/// `registry_to_mcp_server_config`; the unified `manifest.yaml` shape
/// (with a top-level `mcp_servers:` map) takes the first entry.
async fn parse_server_manifest(path: &Path) -> anyhow::Result<McpServerConfig> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if file_name == "server.json" {
        let registry: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("invalid server.json: {e}"))?;
        registry_to_mcp_server_config(&registry, path)
    } else {
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("invalid manifest.yaml: {e}"))?;
        unified_yaml_to_mcp_server_config(&yaml, path)
    }
}

/// Parse-only walk used by ad-hoc tooling (e.g., MCP config validation
/// in tests). Mirrors [`load_workspace_mcp_servers`] but does **not**
/// require a [`McpManager`]; it just enumerates the MCP servers visible
/// at `dir` and returns their parsed configs. Each entry is
/// `(server_name, config_path)`.
pub async fn discover_workspace_mcp_servers(
    dir: &Path,
) -> anyhow::Result<Vec<(String, std::path::PathBuf)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let manifest_path = if path.is_dir() {
            let server_json = path.join("server.json");
            let manifest_yaml = path.join("manifest.yaml");
            if server_json.exists() {
                server_json
            } else if manifest_yaml.exists() {
                manifest_yaml
            } else {
                continue;
            }
        } else if path.is_file() {
            path.clone()
        } else {
            continue;
        };

        match parse_server_manifest(&manifest_path).await {
            Ok(cfg) => out.push((cfg.name, manifest_path)),
            Err(_) => out.push((manifest_path.display().to_string(), manifest_path)),
        }
    }
    Ok(out)
}

/// Convert an MCP Registry `server.json` value into an
/// [`McpServerConfig`]. Looks at top-level `transport` or
/// `packages[0].transport` for the command/args/endpoint and
/// injects the server `name` if absent.
fn registry_to_mcp_server_config(
    registry: &serde_json::Value,
    path: &Path,
) -> anyhow::Result<McpServerConfig> {
    let server_name = registry
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let transport = registry
        .get("transport")
        .or_else(|| {
            registry
                .get("packages")
                .and_then(|p| p.as_array())
                .and_then(|arr| arr.first())
                .and_then(|pkg| pkg.get("transport"))
        })
        .ok_or_else(|| anyhow::anyhow!("server.json missing transport"))?;

    let transport_type = transport
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("stdio");

    let mut obj = serde_json::Map::new();
    obj.insert("name".to_string(), serde_json::json!(server_name));
    obj.insert("transport".to_string(), serde_json::json!(transport_type));
    if let Some(cmd) = transport.get("command").and_then(|v| v.as_str()) {
        obj.insert("command".to_string(), serde_json::json!(cmd));
    }
    if let Some(args) = transport.get("args").and_then(|v| v.as_array()) {
        let collected: Vec<_> = args
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !collected.is_empty() {
            obj.insert("args".to_string(), serde_json::json!(collected));
        }
    }
    if let Some(endpoint) = transport.get("endpoint").and_then(|v| v.as_str()) {
        obj.insert("endpoint".to_string(), serde_json::json!(endpoint));
    }
    if let Some(parent) = path.parent() {
        obj.insert(
            "cwd".to_string(),
            serde_json::json!(parent.to_string_lossy()),
        );
    }

    let cfg: McpServerConfig = serde_json::from_value(serde_json::Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("failed to parse McpServerConfig: {e}"))?;
    Ok(cfg)
}

/// Convert a unified `manifest.yaml` with a top-level `mcp_servers:`
/// map into the first [`McpServerConfig`].
fn unified_yaml_to_mcp_server_config(
    yaml: &serde_yaml::Value,
    _path: &Path,
) -> anyhow::Result<McpServerConfig> {
    let mcp_servers = yaml
        .get("mcp_servers")
        .ok_or_else(|| anyhow::anyhow!("manifest.yaml missing mcp_servers"))?;
    let mapping = mcp_servers
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers must be a mapping"))?;

    let (server_name, server_value) = mapping
        .iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers is empty"))?;

    let server_name = server_name
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("mcp_servers key must be a string"))?;
    let mut server_json = serde_json::to_value(server_value)
        .map_err(|e| anyhow::anyhow!("failed to convert mcp_servers value: {e}"))?;
    if let Some(obj) = server_json.as_object_mut() {
        if !obj.contains_key("name") {
            obj.insert("name".to_string(), serde_json::json!(server_name));
        }
    }
    let cfg: McpServerConfig = serde_json::from_value(server_json)
        .map_err(|e| anyhow::anyhow!("failed to parse McpServerConfig: {e}"))?;
    Ok(cfg)
}

/// Render the MCP server context block used for `{{mcp_context}}` in
/// the system prompt. Replaces the framework-coupled `McpContextHandler`
/// hook (Phase 2 PR 2 deletes the hook path). Format is unchanged so
/// existing prompt templates that reference `{{mcp_context}}` see the
/// same Markdown body they always did.
///
/// Returns the empty string when no servers are configured — the
/// `PromptRenderer`'s `remove_missing=true` placeholder substitution
/// strips the placeholder in that case.
pub async fn render_mcp_prompt_context(
    manager: &Arc<TokioRwLock<McpManager>>,
) -> String {
    let mgr = manager.read().await;
    let server_states = mgr.list_server_prompt_context().await;
    drop(mgr);

    if server_states.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "## MCP Servers".to_string(),
        "The following Model Context Protocol (MCP) servers are configured. \
         When a server is offline, its tools are still listed but will be \
         started automatically if you invoke one."
            .to_string(),
        String::new(),
    ];

    for state in server_states {
        let status_label = if state.running {
            if state.healthy {
                "running"
            } else {
                "running (unhealthy)"
            }
        } else {
            "offline"
        };

        let info = state
            .server_info
            .as_ref()
            .map(|s| format!(" — {s}"))
            .unwrap_or_default();
        lines.push(format!("- **{}** ({}){}", state.name, status_label, info));

        if let Some(instructions) = state.instructions {
            lines.push(format!("  - Instructions: {instructions}"));
        }

        if !state.tools.is_empty() {
            let tool_names: Vec<String> = state.tools.iter().map(|t| t.name.clone()).collect();
            lines.push(format!("  - Tools: {}", tool_names.join(", ")));
        }

        if !state.resources.is_empty() {
            let resource_names: Vec<String> = state
                .resources
                .iter()
                .map(|r| format!("{} ({})", r.uri, r.name))
                .collect();
            lines.push(format!("  - Resources: {}", resource_names.join(", ")));
        }

        if !state.prompts.is_empty() {
            let prompt_names: Vec<String> = state
                .prompts
                .iter()
                .map(|p| {
                    let args = p.arguments.as_ref().map_or(String::new(), |args| {
                        let names: Vec<String> = args.iter().map(|a| a.name.clone()).collect();
                        if names.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", names.join(", "))
                        }
                    });
                    format!("{}{}", p.name, args)
                })
                .collect();
            lines.push(format!("  - Prompts: {}", prompt_names.join(", ")));
        }
    }

    lines.join("\n")
}

/// Adapter that lets the engine's `PromptRenderer` consume the
/// global `McpManager` via the trait port
/// `peko_engine::McpPromptContextProvider`. Production code at
/// `principal/agent_runner.rs` constructs one of these per agent
/// and binds it via `Agent::with_mcp_context_provider`.
///
/// Lives in `peko_core` (not in `peko_engine`) because the engine
/// crate does not depend on the `McpManager` runtime type — the
/// trait port keeps the engine decoupled from MCP internals.
pub struct McpManagerPromptContextProvider {
    manager: Arc<TokioRwLock<McpManager>>,
}

impl McpManagerPromptContextProvider {
    /// Wrap a shared `McpManager` so the renderer can read its
    /// server list at prompt-render time.
    pub fn new(manager: Arc<TokioRwLock<McpManager>>) -> Self {
        Self { manager }
    }
}

#[async_trait::async_trait]
impl peko_engine::McpPromptContextProvider for McpManagerPromptContextProvider {
    async fn render_mcp_context(&self) -> String {
        render_mcp_prompt_context(&self.manager).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn load_skips_missing_dir() {
        let manager = Arc::new(TokioRwLock::new(McpManager::new(Default::default())));
        let n = load_workspace_mcp_servers(
            std::path::Path::new("/nonexistent/mcp"),
            &manager,
        )
        .await
        .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn load_reads_nested_server_json() {
        let tmp = TempDir::new().unwrap();
        let server_dir = tmp.path().join("filesystem");
        std::fs::create_dir_all(&server_dir).unwrap();
        let server_json = serde_json::json!({
            "name": "filesystem",
            "version": "1.0.0",
            "transport": {
                "type": "stdio",
                "command": "echo",
                "args": ["hello"]
            }
        });
        std::fs::write(server_dir.join("server.json"), server_json.to_string()).unwrap();

        let manager = Arc::new(TokioRwLock::new(McpManager::new(Default::default())));
        let n = load_workspace_mcp_servers(tmp.path(), &manager).await.unwrap();
        assert_eq!(n, 1);

        let mgr = manager.read().await;
        let state = mgr.get_server_config("filesystem").await.unwrap();
        assert_eq!(state.command.as_deref(), Some("echo"));
    }

    #[tokio::test]
    async fn load_reads_unified_manifest_yaml() {
        let tmp = TempDir::new().unwrap();
        let server_dir = tmp.path().join("web");
        std::fs::create_dir_all(&server_dir).unwrap();
        let yaml = "\
mcp_servers:
  web:
    transport: sse
    endpoint: http://localhost:8080/sse
    auto_start: true
";
        std::fs::write(server_dir.join("manifest.yaml"), yaml).unwrap();

        let manager = Arc::new(TokioRwLock::new(McpManager::new(Default::default())));
        let n = load_workspace_mcp_servers(tmp.path(), &manager).await.unwrap();
        assert_eq!(n, 1);

        let mgr = manager.read().await;
        let state = mgr.get_server_config("web").await.unwrap();
        assert_eq!(state.endpoint.as_deref(), Some("http://localhost:8080/sse"));
        assert!(state.auto_start);
    }

    #[tokio::test]
    async fn load_dedups_already_registered() {
        let tmp = TempDir::new().unwrap();
        let server_dir = tmp.path().join("filesystem");
        std::fs::create_dir_all(&server_dir).unwrap();
        let server_json = serde_json::json!({
            "name": "filesystem",
            "version": "1.0.0",
            "transport": {"type": "stdio", "command": "echo"}
        });
        std::fs::write(server_dir.join("server.json"), server_json.to_string()).unwrap();

        let manager = Arc::new(TokioRwLock::new(McpManager::new(Default::default())));
        let n1 = load_workspace_mcp_servers(tmp.path(), &manager).await.unwrap();
        let n2 = load_workspace_mcp_servers(tmp.path(), &manager).await.unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
    }
}