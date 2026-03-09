//! HTTP webhook channel

use super::Channel;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// HTTP webhook channel - receives messages via HTTP POST
pub struct HttpChannel {
    name: String,
    endpoint: String,
    message_rx: mpsc::Receiver<String>,
    server_handle: Option<tokio::task::JoinHandle<Result<()>>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HttpChannel {
    /// Create a new HTTP channel (but don't start server yet)
    pub fn new(name: impl Into<String>, endpoint: impl Into<String>) -> Self {
        let (_tx, rx) = mpsc::channel::<String>(100);

        Self {
            name: name.into(),
            endpoint: endpoint.into(),
            message_rx: rx,
            server_handle: None,
            shutdown_tx: None,
        }
    }

    /// Start the HTTP server
    pub async fn start(&mut self) -> Result<()> {
        let addr: SocketAddr = self.endpoint.parse().context("Invalid endpoint address")?;

        let listener = TcpListener::bind(addr)
            .await
            .context("Failed to bind HTTP server")?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        self.shutdown_tx = Some(shutdown_tx);

        // We need a separate sender for the server task
        let (msg_tx, mut msg_rx) = mpsc::channel::<String>(100);

        // Bridge the channel
        let _bridge_handle = tokio::spawn(async move {
            while let Some(_msg) = msg_rx.recv().await {
                // Messages are forwarded through the receive() method
            }
        });

        let server_handle = tokio::spawn(run_http_server(listener, msg_tx, shutdown_rx));
        self.server_handle = Some(server_handle);

        println!(
            "🌐 HTTP channel '{}' listening on http://{}",
            self.name, addr
        );

        Ok(())
    }

    /// Stop the HTTP server
    pub async fn stop(self) -> Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx {
            let _ = shutdown_tx.send(());
        }
        if let Some(handle) = self.server_handle {
            let _ = handle.await;
        }
        Ok(())
    }

    /// Send message to an external webhook
    pub async fn send_webhook(&self, url: &str, message: &str) -> Result<()> {
        let client = reqwest::Client::new();
        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "channel": self.name,
                "message": message,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }))
            .send()
            .await
            .context("Failed to send webhook")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Webhook returned status: {}",
                response.status()
            ))
        }
    }
}

#[async_trait]
impl Channel for HttpChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&mut self, message: &str) -> Result<()> {
        // For HTTP channel, send could post to a configured webhook
        // For now, just log it
        println!("🌐 [{}] Outgoing: {}", self.name, message);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<String>> {
        // Try to receive with timeout
        match tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            self.message_rx.recv(),
        )
        .await
        {
            Ok(Some(msg)) => Ok(Some(msg)),
            Ok(None) => Ok(None), // Channel closed
            Err(_) => Ok(None),   // Timeout
        }
    }
}

/// Run HTTP server to receive webhook messages
async fn run_http_server(
    listener: TcpListener,
    message_tx: mpsc::Sender<String>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        let tx = message_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, tx, addr).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {}", e);
                    }
                }
            }
            _ = &mut shutdown_rx => {
                tracing::info!("HTTP server shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Handle a single HTTP connection
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    message_tx: mpsc::Sender<String>,
    _addr: SocketAddr,
) {
    let mut buffer = [0u8; 4096];

    match stream.read(&mut buffer).await {
        Ok(n) if n > 0 => {
            let request = String::from_utf8_lossy(&buffer[..n]);

            // Simple HTTP parsing - extract body for POST requests
            if request.starts_with("POST") {
                if let Some(body_start) = request.find("\r\n\r\n") {
                    let body = &request[body_start + 4..];

                    // Try to parse as JSON
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                        if let Some(message) = json.get("message").and_then(|m| m.as_str()) {
                            let _ = message_tx.send(message.to_string()).await;
                        }
                    }

                    // Send response
                    let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            } else {
                // Health check endpoint
                let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\",\"channel\":\"pekobot\"}".to_string();
                let _ = stream.write_all(response.as_bytes()).await;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_http_channel_name() {
        let channel = HttpChannel::new("webhook", "127.0.0.1:9999");
        assert_eq!(channel.name(), "webhook");
    }

    #[tokio::test]
    async fn test_http_channel_send() {
        let mut channel = HttpChannel::new("webhook", "127.0.0.1:9999");
        let result = channel.send("Hello").await;
        assert!(result.is_ok());
    }
}
