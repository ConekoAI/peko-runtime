//! Tunnel Management Commands (ADR-035)
//!
//! Provides CLI commands to start, stop, and check the status of the
//! PekoHub tunnel connection.

use anyhow::Context;
use crate::commands::GlobalPaths;
use crate::tunnel::{load_pekohub_credential, TunnelClient};
use clap::Subcommand;
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

/// Tunnel management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum TunnelCommands {
    /// Set up PekoHub tunnel credentials
    Setup {
        /// PekoHub URL (default: wss://pekohub.org/v1/tunnel)
        #[arg(short, long)]
        url: Option<String>,
        /// API key from PekoHub (can also be set via PEKOHUB_API_KEY env var)
        #[arg(short, long, env = "PEKOHUB_API_KEY")]
        api_key: Option<String>,
    },

    /// Start the tunnel connection to PekoHub
    Start {
        /// Path to PekoHub credential file (default: ~/.peko/runtime/pekohub.toml)
        #[arg(short, long)]
        credential: Option<PathBuf>,
    },

    /// Stop the tunnel connection
    Stop,

    /// Show tunnel status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Handle tunnel setup
async fn handle_tunnel_setup(
    url: Option<String>,
    api_key: Option<String>,
    _json: bool,
    paths: &GlobalPaths,
) -> anyhow::Result<()> {
    let cred_path = crate::tunnel::PekoHubCredential::default_path();
    let vault = crate::common::vault::Vault::load(paths.resolver().vault())
        .context("Failed to load credential vault")?;

    // Check if credential already exists
    if cred_path.exists() {
        let existing = crate::tunnel::PekoHubCredential::from_file(&cred_path)?;
        anyhow::bail!(
            "PekoHub credential already exists at: {}\n\
             Runtime ID: {}\n\
             Use `peko tunnel start` to connect, or delete the file to reconfigure.",
            cred_path.display(),
            existing.runtime_id
        );
    }

    // Generate new ed25519 keypair
    let keypair = crate::identity::keys::KeyPair::generate();
    let public_key_bytes = keypair.public_key_bytes();
    let private_key_bytes = keypair.private_key_bytes();

    // Create did:key from public key
    let runtime_did = crate::runtime::identity::public_key_to_did_key(&public_key_bytes);

    // Determine hub URL
    let hub_url = url.unwrap_or_else(|| "wss://pekohub.org/v1/tunnel".to_string());

    // API key is required for setup
    let api_key = api_key.ok_or_else(|| {
        anyhow::anyhow!(
            "API key is required. Provide it with --api-key or set the PEKOHUB_API_KEY environment variable."
        )
    })?;

    // Validate the API key with a lightweight HTTP ping before saving credentials
    let http_url = hub_url
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .replace("/v1/tunnel", "");
    let validate_url = format!("{}/v1/ping", http_url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&validate_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            println!("   API key validated successfully.");
        }
        Ok(r) => {
            anyhow::bail!(
                "API key validation failed: HTTP {}. Please check your API key and hub URL.",
                r.status()
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to validate API key: {}. Please check your network connection and hub URL.",
                e
            );
        }
    }

    // Register this runtime's DID with PekoHub's allowlist before we
    // save the credential. PekoHub's tunnel handshake (issue #1) now
    // requires a row in the `runtimes` table or it will close the
    // WebSocket with 1008. Registration is idempotent (`upsert`) so
    // re-running setup is safe.
    let register_url = format!("{}/v1/runtimes/register", http_url);
    let register_resp = client
        .post(&register_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::json!({
            "runtime_did": runtime_did,
            "display_name": "peko-runtime",
        }))
        .send()
        .await;
    match register_resp {
        Ok(r) if r.status().is_success() => {
            println!("   Runtime registered with PekoHub allowlist.");
        }
        Ok(r) => {
            // 4xx is fatal — PekoHub explicitly rejected the registration
            // (e.g. API key lacks the right scope, or runtime DID is
            // already owned by another user). 5xx is treated as a soft
            // warning so setup still succeeds and the operator can
            // retry registration manually.
            if r.status().is_client_error() {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                anyhow::bail!(
                    "PekoHub rejected runtime registration: HTTP {status}. {body}"
                );
            } else {
                eprintln!(
                    "   ⚠️  Runtime registration failed: HTTP {}. The credential was saved; \
                     re-run `peko tunnel setup` once PekoHub is reachable to complete registration.",
                    r.status()
                );
            }
        }
        Err(e) => {
            eprintln!(
                "   ⚠️  Could not reach PekoHub to register the runtime: {}. \
                 The credential was saved; re-run `peko tunnel setup` once PekoHub is reachable \
                 to complete registration.",
                e
            );
        }
    }

    // Store the private key in the encrypted vault.
    vault
        .set_tunnel_private_key(&runtime_did, &BASE64.encode(private_key_bytes))
        .context("Failed to store tunnel private key in vault")?;
    println!("   Private key stored securely in vault.");

    // Create credential (no raw private_key)
    let credential = crate::tunnel::PekoHubCredential {
        url: hub_url.clone(),
        runtime_id: runtime_did.clone(),
    };

    // Save credential to file
    credential.save_to_file(&cred_path)?;

    println!("✅ PekoHub tunnel configured successfully.");
    println!("   Credential file: {}", cred_path.display());
    println!("   Hub URL:         {}", hub_url);
    println!("   Runtime DID:     {}", runtime_did);
    println!();
    println!("   Start the tunnel with: peko tunnel start");
    println!("   Or start the daemon:   peko daemon start");

    Ok(())
}

/// Handle tunnel commands
pub async fn handle_tunnel(
    cmd: TunnelCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        TunnelCommands::Setup { url, api_key } => {
            handle_tunnel_setup(url, api_key, json, paths).await
        }
        TunnelCommands::Start { credential } => {
            let cred_path = credential.as_deref();
            let cred = match load_pekohub_credential(cred_path)? {
                Some(c) => c,
                None => {
                    let path = cred_path.map_or_else(
                        || crate::tunnel::PekoHubCredential::default_path(),
                        PathBuf::from,
                    );
                    anyhow::bail!(
                        "No PekoHub credential found at: {}\n\
                         Run `peko tunnel setup` to configure, or provide --credential",
                        path.display()
                    );
                }
            };

            println!("🔗 Starting PekoHub tunnel...");
            println!("   URL: {}", cred.url);
            println!("   Runtime ID: {}", cred.runtime_id);

            // Try to connect to the daemon and use its AppState for dispatch.
            // If the daemon is not running, fall back to a limited mode.
            let daemon_running = crate::ipc::DaemonClient::connect().await.is_ok();

            if daemon_running {
                println!("   Mode: daemon-integrated (full service dispatch)");
                // The tunnel is already running in the daemon if credentials exist.
                // This foreground command is mainly for debugging / manual override.
                println!("   Note: Daemon already manages the tunnel. Use `peko tunnel status` to check.");
                println!("   Forcing foreground tunnel connection...");
            } else {
                println!("   Mode: standalone (daemon not running)");
                println!("   Warning: Chat requests will not be dispatched without the daemon.");
            }

            let vault = crate::common::vault::Vault::load(paths.resolver().vault())
                .context("Failed to load credential vault")?;
            let mut client = TunnelClient::new(cred).with_vault(std::sync::Arc::new(vault));
            client.on_request(|msg, _handle| async move {
                tracing::info!("Received tunnel message (no dispatcher): {:?}", msg);
            });

            println!("✅ Tunnel connected (Ctrl+C to disconnect)");
            client.run().await;

            Ok(())
        }
        TunnelCommands::Stop => {
            // In daemon mode, stop the tunnel via IPC
            match crate::ipc::DaemonClient::connect().await {
                Ok(client) => {
                    match client.tunnel_stop().await {
                        Ok(crate::ipc::ResponsePacket::Done { success, .. }) => {
                            if success {
                                println!("🛑 Tunnel stopped.");
                            } else {
                                println!("⚠️  Tunnel stop returned unsuccessful.");
                            }
                        }
                        Ok(other) => {
                            println!("⚠️  Unexpected response from daemon: {:?}", other);
                        }
                        Err(e) => {
                            println!("❌ Failed to stop tunnel: {}", e);
                        }
                    }
                }
                Err(_) => {
                    println!("🛑 No daemon running. Tunnel is not active.");
                }
            }
            Ok(())
        }
        TunnelCommands::Status { json: json_flag } => {
            let has_cred = crate::tunnel::credential::has_pekohub_credential();
            let cred_path = crate::tunnel::PekoHubCredential::default_path();

            // Try to check daemon tunnel status
            match crate::ipc::DaemonClient::connect().await {
                Ok(client) => {
                    match client.tunnel_status().await {
                        Ok(crate::ipc::ResponsePacket::TunnelStatus {
                            configured,
                            daemon_running,
                            connected,
                            ..
                        }) => {
                            if json_flag || json {
                                let output = serde_json::json!({
                                    "configured": configured,
                                    "credential_path": cred_path.to_string_lossy().to_string(),
                                    "daemon_running": daemon_running,
                                    "connected": connected,
                                });
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            } else {
                                println!("📡 Tunnel Status:");
                                if configured {
                                    println!("  Credential: ✅ Found at {}", cred_path.display());
                                } else {
                                    println!("  Credential: ❌ Not found at {}", cred_path.display());
                                }
                                if daemon_running {
                                    println!("  Daemon:     ✅ Running");
                                } else {
                                    println!("  Daemon:     ❌ Not running");
                                }
                                if connected {
                                    println!("  Tunnel:     ✅ Connected");
                                } else {
                                    println!("  Tunnel:     ❌ Not connected");
                                }
                                println!();
                                println!("  Start with: peko daemon start  (auto-starts tunnel if cred exists)");
                                println!("  Or:         peko tunnel start   (foreground mode)");
                            }
                        }
                        Ok(other) => {
                            if json_flag || json {
                                let output = serde_json::json!({
                                    "configured": has_cred,
                                    "credential_path": cred_path.to_string_lossy().to_string(),
                                    "daemon_running": true,
                                    "connected": false,
                                    "warning": format!("Unexpected response: {:?}", other),
                                });
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            } else {
                                println!("📡 Tunnel Status:");
                                println!("  Daemon:     ✅ Running");
                                println!("  Warning:    Unexpected response from daemon");
                            }
                        }
                        Err(e) => {
                            if json_flag || json {
                                let output = serde_json::json!({
                                    "configured": has_cred,
                                    "credential_path": cred_path.to_string_lossy().to_string(),
                                    "daemon_running": true,
                                    "connected": false,
                                    "error": format!("Failed to get status: {}", e),
                                });
                                println!("{}", serde_json::to_string_pretty(&output)?);
                            } else {
                                println!("📡 Tunnel Status:");
                                println!("  Daemon:     ✅ Running");
                                println!("  Error:      Failed to get status: {}", e);
                            }
                        }
                    }
                }
                Err(_) => {
                    if json_flag || json {
                        let output = serde_json::json!({
                            "configured": has_cred,
                            "credential_path": cred_path.to_string_lossy().to_string(),
                            "daemon_running": false,
                            "connected": false,
                        });
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    } else {
                        println!("📡 Tunnel Status:");
                        if has_cred {
                            println!("  Credential: ✅ Found at {}", cred_path.display());
                        } else {
                            println!("  Credential: ❌ Not found at {}", cred_path.display());
                        }
                        println!("  Daemon:     ❌ Not running");
                        println!("  Tunnel:     ❌ Not connected");
                        println!();
                        println!("  Start with: peko daemon start  (auto-starts tunnel if cred exists)");
                        println!("  Or:         peko tunnel start   (foreground mode)");
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::packet::{RequestPacket, ResponsePacket};

    #[test]
    fn test_tunnel_commands_enum() {
        let _cmd = TunnelCommands::Setup { url: None, api_key: None };
        let _cmd = TunnelCommands::Start { credential: None };
        let _cmd = TunnelCommands::Stop;
        let _cmd = TunnelCommands::Status { json: true };
    }

    #[test]
    fn test_tunnel_stop_request_serialization() {
        let req = RequestPacket::TunnelStop { request_id: 700 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TunnelStop { request_id } => {
                assert_eq!(request_id, 700);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_status_request_serialization() {
        let req = RequestPacket::TunnelStatus { request_id: 701 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TunnelStatus { request_id } => {
                assert_eq!(request_id, 701);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_status_response_serialization() {
        let resp = ResponsePacket::TunnelStatus {
            request_id: 702,
            configured: true,
            daemon_running: true,
            connected: false,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TunnelStatus {
                request_id,
                configured,
                daemon_running,
                connected,
            } => {
                assert_eq!(request_id, 702);
                assert!(configured);
                assert!(daemon_running);
                assert!(!connected);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_request_ids() {
        let req_stop = RequestPacket::TunnelStop { request_id: 1 };
        assert_eq!(req_stop.request_id(), 1);

        let req_status = RequestPacket::TunnelStatus { request_id: 2 };
        assert_eq!(req_status.request_id(), 2);

        let resp = ResponsePacket::TunnelStatus {
            request_id: 3,
            configured: false,
            daemon_running: true,
            connected: false,
        };
        assert_eq!(resp.request_id(), 3);
    }
}
