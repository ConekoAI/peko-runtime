//! Tunnel Management Commands (ADR-035)
//!
//! Provides CLI commands to start, stop, and check the status of the
//! PekoHub tunnel connection.

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Tunnel management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum TunnelCommands {
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
        TunnelCommands::Stop => {
            // In daemon mode, stop the tunnel via IPC
            match peko_core::ipc::DaemonClient::connect().await {
                Ok(client) => match client.tunnel_stop().await {
                    Ok(peko_core::ipc::ResponsePacket::Done { success, .. }) => {
                        if success {
                            println!("🛑 Tunnel stopped.");
                        } else {
                            println!("⚠️  Tunnel stop returned unsuccessful.");
                        }
                    }
                    Ok(other) => {
                        println!(
                            "⚠️  Unexpected response from daemon: {}",
                            other.variant_name()
                        );
                    }
                    Err(e) => {
                        println!("❌ Failed to stop tunnel: {}", e);
                    }
                },
                Err(_) => {
                    println!("🛑 No daemon running. Tunnel is not active.");
                }
            }
            Ok(())
        }
        TunnelCommands::Status { json: json_flag } => {
            let has_cred = peko_core::tunnel::credential::has_pekohub_credential();
            let cred_path = peko_core::tunnel::PekoHubCredential::default_path();

            // Try to check daemon tunnel status
            match peko_core::ipc::DaemonClient::connect().await {
                Ok(client) => match client.tunnel_status().await {
                    Ok(peko_core::ipc::ResponsePacket::TunnelStatus {
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
                        }
                    }
                    Ok(other) => {
                        if json_flag || json {
                            let output = serde_json::json!({
                                "configured": has_cred,
                                "credential_path": cred_path.to_string_lossy().to_string(),
                                "daemon_running": true,
                                "connected": false,
                                "warning": format!("Unexpected response: {}", other.variant_name()),
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
                },
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
                        println!(
                            "  Start with: peko daemon start  (auto-starts tunnel if cred exists)"
                        );
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
    use peko_core::ipc::packet::{RequestPacket, ResponsePacket};

    /// ADR-058 D4: the register PoP is a compact JWS over the
    /// canonical claim set, verifiable against the runtime identity
    /// key (the key behind `runtime_did`).
    #[test]
    fn test_build_register_pop_signs_canonical_claims() {
        let keypair = peko_identity::keys::KeyPair::generate();
        let runtime_did =
            peko_identity::runtime::public_key_to_did_key(&keypair.public_key_bytes());

        let pop = peko_core::tunnel::registration::register_pop(
            "nonce-abc",
            &serde_json::json!(1_900_000_000i64),
            &serde_json::json!("user-42"),
            &runtime_did,
            &keypair.signing_key,
        );
        assert_eq!(pop["nonce"].as_str(), Some("nonce-abc"));
        let jws = pop["jws"].as_str().expect("jws present");

        // Verifies against the key embedded in the claimed runtime DID.
        let key = peko_core::tunnel::did_key_to_verifying_key(&runtime_did).unwrap();
        let (_seg, payload) = peko_core::tunnel::tunnel_channel_signature::jws_verify(&key, jws)
            .expect("PoP JWS must verify against the runtime DID key");
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["nonce"].as_str(), Some("nonce-abc"));
        assert_eq!(payload["runtimeDid"].as_str(), Some(runtime_did.as_str()));
        assert_eq!(payload["owner"].as_str(), Some("user-42"));
        assert_eq!(payload["exp"].as_i64(), Some(1_900_000_000));
        assert!(payload["iat"].as_i64().is_some(), "iat present");

        // A different runtime DID's key must NOT verify the PoP.
        let other = peko_identity::keys::KeyPair::generate();
        let other_did = peko_identity::runtime::public_key_to_did_key(&other.public_key_bytes());
        let other_key = peko_core::tunnel::did_key_to_verifying_key(&other_did).unwrap();
        assert!(
            peko_core::tunnel::tunnel_channel_signature::jws_verify(&other_key, jws).is_err(),
            "PoP must not verify against a different runtime key"
        );
    }

    #[test]
    fn test_tunnel_commands_enum() {
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
