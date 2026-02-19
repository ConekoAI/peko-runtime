//! Tunnel support for exposing Pekobot via public URLs
//!
//! Supports Cloudflare Tunnel, Tailscale Funnel, ngrok, and custom commands.

use anyhow::{bail, Result};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;

/// Tunnel trait — abstraction for different tunnel providers
#[async_trait::async_trait]
pub trait Tunnel: Send + Sync {
    /// Provider name
    fn name(&self) -> &str;

    /// Start tunnel, expose `local_host:local_port`, return public URL
    async fn start(&self, local_host: &str, local_port: u16) -> Result<String>;

    /// Stop tunnel gracefully
    async fn stop(&self) -> Result<()>;

    /// Check if tunnel is alive
    async fn is_running(&self) -> bool;

    /// Get public URL if running
    fn public_url(&self) -> Option<String>;
}

/// Shared process handle for tunnel implementations
pub(crate) type SharedProcess = Arc<Mutex<Option<TunnelProcess>>>;

/// Tunnel process info
pub(crate) struct TunnelProcess {
    pub child: tokio::process::Child,
    pub public_url: String,
}

/// Create new shared process handle
pub(crate) fn new_shared_process() -> SharedProcess {
    Arc::new(Mutex::new(None))
}

/// Kill shared process
pub(crate) async fn kill_shared_process(proc: &SharedProcess) -> Result<()> {
    let mut guard = proc.lock().await;
    if let Some(ref mut tp) = *guard {
        tp.child.kill().await.ok();
        tp.child.wait().await.ok();
    }
    *guard = None;
    Ok(())
}

// ── Cloudflare Tunnel ─────────────────────────────────────────────────

/// Cloudflare Tunnel using cloudflared
pub struct CloudflareTunnel {
    token: String,
    proc: SharedProcess,
}

impl CloudflareTunnel {
    /// Create new Cloudflare tunnel
    #[must_use] 
    pub fn new(token: String) -> Self {
        Self {
            token,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for CloudflareTunnel {
    fn name(&self) -> &'static str {
        "cloudflare"
    }

    async fn start(&self, _local_host: &str, local_port: u16) -> Result<String> {
        let mut child = Command::new("cloudflared")
            .args([
                "tunnel",
                "--no-autoupdate",
                "run",
                "--token",
                &self.token,
                "--url",
                &format!("http://localhost:{local_port}"),
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Read stderr to find public URL
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture cloudflared stderr"))?;

        let mut reader = tokio::io::BufReader::new(stderr).lines();
        let mut public_url = String::new();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(tokio::time::Duration::from_secs(5), reader.next_line())
                .await
            {
                Ok(Ok(Some(line))) => {
                    tracing::debug!("cloudflared: {}", line);
                    if let Some(idx) = line.find("https://") {
                        let url_part = &line[idx..];
                        let end = url_part
                            .find(|c: char| c.is_whitespace())
                            .unwrap_or(url_part.len());
                        public_url = url_part[..end].to_string();
                        break;
                    }
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => bail!("Error reading cloudflared: {e}"),
                Err(_) => {} // timeout, keep trying
            }
        }

        if public_url.is_empty() {
            child.kill().await.ok();
            bail!("cloudflared did not produce a public URL within 30s");
        }

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        kill_shared_process(&self.proc).await
    }

    async fn is_running(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

    fn public_url(&self) -> Option<String> {
        self.proc
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tp| tp.public_url.clone()))
    }
}

// ── ngrok Tunnel ───────────────────────────────────────────────────────

/// ngrok tunnel
pub struct NgrokTunnel {
    auth_token: String,
    domain: Option<String>,
    proc: SharedProcess,
}

impl NgrokTunnel {
    /// Create new ngrok tunnel
    #[must_use] 
    pub fn new(auth_token: String, domain: Option<String>) -> Self {
        Self {
            auth_token,
            domain,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for NgrokTunnel {
    fn name(&self) -> &'static str {
        "ngrok"
    }

    async fn start(&self, _local_host: &str, local_port: u16) -> Result<String> {
        let mut args = vec![
            "http".to_string(),
            local_port.to_string(),
            "--authtoken".to_string(),
            self.auth_token.clone(),
        ];

        if let Some(ref domain) = self.domain {
            args.push("--domain".to_string());
            args.push(domain.clone());
        }

        let mut child = Command::new("ngrok")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // ngrok API to get public URL
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Try to get URL from ngrok API
        let client = reqwest::Client::new();
        let api_url = "http://localhost:4040/api/tunnels";

        let mut public_url = String::new();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

        while tokio::time::Instant::now() < deadline {
            if let Ok(resp) = client.get(api_url).send().await {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(tunnels) = json.get("tunnels").and_then(|t| t.as_array()) {
                        if let Some(first) = tunnels.first() {
                            if let Some(url) = first.get("public_url").and_then(|u| u.as_str()) {
                                public_url = url.to_string();
                                break;
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }

        if public_url.is_empty() {
            child.kill().await.ok();
            bail!("ngrok did not produce a public URL within 30s");
        }

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        kill_shared_process(&self.proc).await
    }

    async fn is_running(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

    fn public_url(&self) -> Option<String> {
        self.proc
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tp| tp.public_url.clone()))
    }
}

// ── Tailscale Funnel ─────────────────────────────────────────────────

/// Tailscale Funnel tunnel
pub struct TailscaleTunnel {
    funnel: bool,
    hostname: Option<String>,
    proc: SharedProcess,
}

impl TailscaleTunnel {
    /// Create new Tailscale tunnel
    #[must_use] 
    pub fn new(funnel: bool, hostname: Option<String>) -> Self {
        Self {
            funnel,
            hostname,
            proc: new_shared_process(),
        }
    }
}

#[async_trait::async_trait]
impl Tunnel for TailscaleTunnel {
    fn name(&self) -> &'static str {
        "tailscale"
    }

    async fn start(&self, local_host: &str, local_port: u16) -> Result<String> {
        let mut args = vec!["funnel".to_string()];

        if self.funnel {
            args.push("--bg".to_string());
        }

        if let Some(ref hostname) = self.hostname {
            args.push(format!("{hostname}:{local_port}"));
        } else {
            args.push(format!("{local_host}:{local_port}"));
        }

        let child = Command::new("tailscale")
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        // Construct public URL
        let hostname = self.hostname.clone().unwrap_or_else(|| {
            // Get machine name from tailscale
            local_host.to_string()
        });

        let public_url = if self.funnel {
            format!("https://{}.{}", hostname, "ts.net")
        } else {
            format!("http://{hostname}:{local_port}")
        };

        let mut guard = self.proc.lock().await;
        *guard = Some(TunnelProcess {
            child,
            public_url: public_url.clone(),
        });

        Ok(public_url)
    }

    async fn stop(&self) -> Result<()> {
        // Stop funnel with: tailscale funnel --off
        let _ = Command::new("tailscale")
            .args(["funnel", "--off"])
            .output()
            .await;

        kill_shared_process(&self.proc).await
    }

    async fn is_running(&self) -> bool {
        let guard = self.proc.lock().await;
        guard.as_ref().is_some_and(|tp| tp.child.id().is_some())
    }

    fn public_url(&self) -> Option<String> {
        self.proc
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|tp| tp.public_url.clone()))
    }
}

// ── Factory ───────────────────────────────────────────────────────────

/// Tunnel configuration
#[derive(Debug, Clone)]
pub enum TunnelConfig {
    /// No tunnel
    None,
    /// Cloudflare Tunnel
    Cloudflare { token: String },
    /// ngrok
    Ngrok {
        auth_token: String,
        domain: Option<String>,
    },
    /// Tailscale Funnel
    Tailscale {
        funnel: bool,
        hostname: Option<String>,
    },
}

/// Create tunnel from config
#[must_use] 
pub fn create_tunnel(config: &TunnelConfig) -> Option<Box<dyn Tunnel>> {
    match config {
        TunnelConfig::None => None,
        TunnelConfig::Cloudflare { token } => Some(Box::new(CloudflareTunnel::new(token.clone()))),
        TunnelConfig::Ngrok { auth_token, domain } => Some(Box::new(NgrokTunnel::new(
            auth_token.clone(),
            domain.clone(),
        ))),
        TunnelConfig::Tailscale { funnel, hostname } => {
            Some(Box::new(TailscaleTunnel::new(*funnel, hostname.clone())))
        }
    }
}

/// Tunnel manager for easy tunnel lifecycle management
pub struct TunnelManager {
    tunnel: Option<Box<dyn Tunnel>>,
}

impl TunnelManager {
    /// Create new tunnel manager
    #[must_use] 
    pub fn new(config: &TunnelConfig) -> Self {
        Self {
            tunnel: create_tunnel(config),
        }
    }

    /// Start tunnel if configured
    pub async fn start(&self, local_host: &str, local_port: u16) -> Result<Option<String>> {
        if let Some(ref tunnel) = self.tunnel {
            let url = tunnel.start(local_host, local_port).await?;
            tracing::info!("Tunnel {} started: {}", tunnel.name(), url);
            Ok(Some(url))
        } else {
            Ok(None)
        }
    }

    /// Stop tunnel
    pub async fn stop(&self) -> Result<()> {
        if let Some(ref tunnel) = self.tunnel {
            tunnel.stop().await?;
            tracing::info!("Tunnel {} stopped", tunnel.name());
        }
        Ok(())
    }

    /// Get public URL
    #[must_use] 
    pub fn public_url(&self) -> Option<String> {
        self.tunnel.as_ref().and_then(|t| t.public_url())
    }

    /// Check if running
    pub async fn is_running(&self) -> bool {
        if let Some(ref tunnel) = self.tunnel {
            tunnel.is_running().await
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloudflare_tunnel_name() {
        let t = CloudflareTunnel::new("tok".into());
        assert_eq!(t.name(), "cloudflare");
    }

    #[test]
    fn test_ngrok_tunnel_name() {
        let t = NgrokTunnel::new("tok".into(), None);
        assert_eq!(t.name(), "ngrok");
    }

    #[test]
    fn test_tailscale_tunnel_name() {
        let t = TailscaleTunnel::new(false, None);
        assert_eq!(t.name(), "tailscale");
    }

    #[test]
    fn test_create_tunnel_none() {
        let t = create_tunnel(&TunnelConfig::None);
        assert!(t.is_none());
    }

    #[test]
    fn test_create_tunnel_cloudflare() {
        let t = create_tunnel(&TunnelConfig::Cloudflare {
            token: "test".into(),
        });
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "cloudflare");
    }

    #[test]
    fn test_tunnel_manager_none() {
        let mgr = TunnelManager::new(&TunnelConfig::None);
        assert!(mgr.public_url().is_none());
    }
}
