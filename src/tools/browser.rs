//! Browser automation tool using Vercel's agent-browser CLI
//!
//! Provides AI-optimized web browsing with semantic element selection,
//! accessibility snapshots, and JSON output for LLM integration.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

use crate::tools::Tool;

/// Browser automation tool
pub struct BrowserTool {
    allowed_domains: Vec<String>,
    session_name: Option<String>,
}

/// Response from agent-browser --json commands
#[derive(Debug, Deserialize)]
struct AgentBrowserResponse {
    success: bool,
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Supported browser actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    /// Navigate to a URL
    Open { url: String },
    /// Get accessibility snapshot with refs
    Snapshot {
        #[serde(default)]
        interactive_only: bool,
        #[serde(default)]
        compact: bool,
        #[serde(default)]
        depth: Option<u32>,
    },
    /// Click an element by ref or selector
    Click { selector: String },
    /// Fill a form field
    Fill { selector: String, value: String },
    /// Type text into focused element
    Type { selector: String, text: String },
    /// Get text content of element
    GetText { selector: String },
    /// Get page title
    GetTitle,
    /// Get current URL
    GetUrl,
    /// Take screenshot
    Screenshot {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        full_page: bool,
    },
    /// Wait for element or time
    Wait {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        ms: Option<u64>,
        #[serde(default)]
        text: Option<String>,
    },
    /// Press a key
    Press { key: String },
    /// Hover over element
    Hover { selector: String },
    /// Scroll page
    Scroll {
        direction: String,
        #[serde(default)]
        pixels: Option<u32>,
    },
    /// Check if element is visible
    IsVisible { selector: String },
    /// Close browser
    Close,
}

impl BrowserTool {
    /// Create a new browser tool
    #[must_use]
    pub fn new(allowed_domains: Vec<String>, session_name: Option<String>) -> Self {
        Self {
            allowed_domains: normalize_domains(allowed_domains),
            session_name,
        }
    }

    /// Check if agent-browser CLI is available
    pub async fn is_available() -> bool {
        Command::new("agent-browser")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Validate URL against allowlist
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        let url = url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        // Allow file:// URLs for local testing
        if url.starts_with("file://") {
            return Ok(());
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "Browser tool enabled but no allowed_domains configured. \
                Configure allowed_domains in agent settings."
            );
        }

        let host = extract_host(url)?;

        if is_private_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' not in browser.allowed_domains");
        }

        Ok(())
    }

    /// Execute an agent-browser command
    async fn run_command(&self, args: &[&str]) -> anyhow::Result<AgentBrowserResponse> {
        let mut cmd = Command::new("agent-browser");

        // Add session if configured
        if let Some(ref session) = self.session_name {
            cmd.arg("--session").arg(session);
        }

        // Add --json for machine-readable output
        cmd.args(args).arg("--json");

        debug!("Running: agent-browser {} --json", args.join(" "));

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            debug!("agent-browser stderr: {}", stderr);
        }

        // Parse JSON response
        if let Ok(resp) = serde_json::from_str::<AgentBrowserResponse>(&stdout) {
            return Ok(resp);
        }

        // Fallback for non-JSON output
        if output.status.success() {
            Ok(AgentBrowserResponse {
                success: true,
                data: Some(json!({ "output": stdout.trim() })),
                error: None,
            })
        } else {
            Ok(AgentBrowserResponse {
                success: false,
                data: None,
                error: Some(stderr.trim().to_string()),
            })
        }
    }

    /// Execute a browser action
    async fn execute_action(&self, action: BrowserAction) -> anyhow::Result<serde_json::Value> {
        match action {
            BrowserAction::Open { url } => {
                self.validate_url(&url)?;
                let resp = self.run_command(&["open", &url]).await?;
                self.to_json(resp)
            }

            BrowserAction::Snapshot {
                interactive_only,
                compact,
                depth,
            } => {
                let mut args = vec!["snapshot"];
                if interactive_only {
                    args.push("-i");
                }
                if compact {
                    args.push("-c");
                }
                let depth_str;
                if let Some(d) = depth {
                    args.push("-d");
                    depth_str = d.to_string();
                    args.push(&depth_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_json(resp)
            }

            BrowserAction::Click { selector } => {
                let resp = self.run_command(&["click", &selector]).await?;
                self.to_json(resp)
            }

            BrowserAction::Fill { selector, value } => {
                let resp = self.run_command(&["fill", &selector, &value]).await?;
                self.to_json(resp)
            }

            BrowserAction::Type { selector, text } => {
                let resp = self.run_command(&["type", &selector, &text]).await?;
                self.to_json(resp)
            }

            BrowserAction::GetText { selector } => {
                let resp = self.run_command(&["get", "text", &selector]).await?;
                self.to_json(resp)
            }

            BrowserAction::GetTitle => {
                let resp = self.run_command(&["get", "title"]).await?;
                self.to_json(resp)
            }

            BrowserAction::GetUrl => {
                let resp = self.run_command(&["get", "url"]).await?;
                self.to_json(resp)
            }

            BrowserAction::Screenshot { path, full_page } => {
                let mut args = vec!["screenshot"];
                if let Some(ref p) = path {
                    args.push(p);
                }
                if full_page {
                    args.push("--full");
                }
                let resp = self.run_command(&args).await?;
                self.to_json(resp)
            }

            BrowserAction::Wait { selector, ms, text } => {
                let mut args = vec!["wait"];
                let ms_str;
                if let Some(ref sel) = selector {
                    args.push(sel);
                } else if let Some(millis) = ms {
                    ms_str = millis.to_string();
                    args.push(&ms_str);
                } else if let Some(ref t) = text {
                    args.push("--text");
                    args.push(t);
                }
                let resp = self.run_command(&args).await?;
                self.to_json(resp)
            }

            BrowserAction::Press { key } => {
                let resp = self.run_command(&["press", &key]).await?;
                self.to_json(resp)
            }

            BrowserAction::Hover { selector } => {
                let resp = self.run_command(&["hover", &selector]).await?;
                self.to_json(resp)
            }

            BrowserAction::Scroll { direction, pixels } => {
                let mut args = vec!["scroll", &direction];
                let px_str;
                if let Some(px) = pixels {
                    px_str = px.to_string();
                    args.push(&px_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_json(resp)
            }

            BrowserAction::IsVisible { selector } => {
                let resp = self.run_command(&["is", "visible", &selector]).await?;
                self.to_json(resp)
            }

            BrowserAction::Close => {
                let resp = self.run_command(&["close"]).await?;
                self.to_json(resp)
            }
        }
    }

    fn to_json(&self, resp: AgentBrowserResponse) -> anyhow::Result<serde_json::Value> {
        if resp.success {
            Ok(json!({
                "success": true,
                "data": resp.data,
            }))
        } else {
            Ok(json!({
                "success": false,
                "error": resp.error,
            }))
        }
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &'static str {
        "browser"
    }

    fn description(&self) -> &'static str {
        "Web browser automation using agent-browser CLI. Supports navigation, clicking, \
        filling forms, taking screenshots, and getting accessibility snapshots. \
        Use 'snapshot' to get interactive elements with refs (@e1, @e2), then use refs \
        for precise element interaction. Allowed domains only."
    }

    fn llm_description(&self) -> String {
        "Web browser automation for interactive web pages. \
        Use when: pages require JavaScript, logging into sites, filling forms, clicking buttons, taking screenshots. \
        Don't use when: you just need to read static content (use `fetch` instead), or searching for information (use `web_search`). \
        Tip: Use 'snapshot' first to get element refs (@e1, @e2), then use those refs for clicking/filling."
            .to_string()
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Check if agent-browser is available
        if !Self::is_available().await {
            return Ok(json!({
                "success": false,
                "error": "agent-browser CLI not found. Install with: npm install -g agent-browser"
            }));
        }

        // Parse action from params
        let action_str = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let action = match action_str {
            "open" => {
                let url = params
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'url' for open action"))?;
                BrowserAction::Open { url: url.into() }
            }
            "snapshot" => BrowserAction::Snapshot {
                interactive_only: params
                    .get("interactive_only")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                compact: params
                    .get("compact")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                depth: params
                    .get("depth")
                    .and_then(serde_json::Value::as_u64)
                    .map(|d| u32::try_from(d).unwrap_or(u32::MAX)),
            },
            "click" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for click"))?;
                BrowserAction::Click {
                    selector: selector.into(),
                }
            }
            "fill" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for fill"))?;
                let value = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'value' for fill"))?;
                BrowserAction::Fill {
                    selector: selector.into(),
                    value: value.into(),
                }
            }
            "type" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for type"))?;
                let text = params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'text' for type"))?;
                BrowserAction::Type {
                    selector: selector.into(),
                    text: text.into(),
                }
            }
            "get_text" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for get_text"))?;
                BrowserAction::GetText {
                    selector: selector.into(),
                }
            }
            "get_title" => BrowserAction::GetTitle,
            "get_url" => BrowserAction::GetUrl,
            "screenshot" => BrowserAction::Screenshot {
                path: params
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                full_page: params
                    .get("full_page")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            },
            "wait" => BrowserAction::Wait {
                selector: params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                ms: params.get("ms").and_then(serde_json::Value::as_u64),
                text: params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            },
            "press" => {
                let key = params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'key' for press"))?;
                BrowserAction::Press { key: key.into() }
            }
            "hover" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for hover"))?;
                BrowserAction::Hover {
                    selector: selector.into(),
                }
            }
            "scroll" => {
                let direction = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'direction' for scroll"))?;
                BrowserAction::Scroll {
                    direction: direction.into(),
                    pixels: params
                        .get("pixels")
                        .and_then(serde_json::Value::as_u64)
                        .map(|p| u32::try_from(p).unwrap_or(u32::MAX)),
                }
            }
            "is_visible" => {
                let selector = params
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for is_visible"))?;
                BrowserAction::IsVisible {
                    selector: selector.into(),
                }
            }
            "close" => BrowserAction::Close,
            _ => {
                return Ok(json!({
                    "success": false,
                    "error": format!("Unknown action: {action_str}")
                }));
            }
        };

        self.execute_action(action).await
    }
}

// Helper functions
fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    domains
        .into_iter()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

fn extract_host(url_str: &str) -> anyhow::Result<String> {
    let url = url_str.trim();
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);

    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme);

    if host.is_empty() {
        anyhow::bail!("Invalid URL: no host");
    }

    Ok(host.to_lowercase())
}

fn is_private_host(host: &str) -> bool {
    let private_patterns = [
        "localhost",
        "127.",
        "10.",
        "192.168.",
        "172.16.",
        "172.17.",
        "172.18.",
        "172.19.",
        "172.20.",
        "172.21.",
        "172.22.",
        "172.23.",
        "172.24.",
        "172.25.",
        "172.26.",
        "172.27.",
        "172.28.",
        "172.29.",
        "172.30.",
        "172.31.",
        "0.0.0.0",
        "::1",
        "[::1]",
    ];

    private_patterns
        .iter()
        .any(|p| host.starts_with(p) || host == *p)
}

fn host_matches_allowlist(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|pattern| {
        if pattern == "*" {
            return true;
        }
        if pattern.starts_with("*.") {
            let suffix = &pattern[1..];
            host.ends_with(suffix) || host == &pattern[2..]
        } else {
            host == pattern || host.ends_with(&format!(".{pattern}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_domains() {
        let domains = vec![
            "  Example.COM  ".into(),
            "docs.example.com".into(),
            String::new(),
        ];
        let normalized = normalize_domains(domains);
        assert_eq!(normalized, vec!["example.com", "docs.example.com"]);
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_host("https://Sub.Example.COM:8080/").unwrap(),
            "sub.example.com"
        );
    }

    #[test]
    fn test_is_private_host() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("192.168.1.1"));
        assert!(!is_private_host("example.com"));
    }

    #[test]
    fn test_host_matches_allowlist() {
        let allowed = vec!["example.com".into()];
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(host_matches_allowlist("sub.example.com", &allowed));
        assert!(!host_matches_allowlist("other.com", &allowed));
    }

    #[test]
    fn test_browser_tool_creation() {
        let tool = BrowserTool::new(vec!["example.com".into()], None);
        assert_eq!(tool.name(), "browser");
    }
}
