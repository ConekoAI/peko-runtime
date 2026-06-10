//! Tunnel Management Commands (ADR-035)
//!
//! Provides CLI commands to start, stop, and check the status of the
//! PekoHub tunnel connection.

use crate::commands::GlobalPaths;
use crate::tunnel::{load_pekohub_credential, TunnelClient};
use clap::Subcommand;
use std::path::PathBuf;

/// Tunnel management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum TunnelCommands {
    /// Start the tunnel connection to PekoHub
    Start {
        /// Path to PekoHub credential file (default: ~/.peko/pekohub.toml)
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

/// Handle tunnel commands
pub async fn handle_tunnel(
    cmd: TunnelCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
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

            let mut client = TunnelClient::new(cred);
            client.on_request(|msg, _handle| {
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
