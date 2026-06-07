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
    paths: &GlobalPaths,
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

            // For now, start the tunnel in foreground mode
            // In daemon mode, this would be spawned as a background task
            let mut client = TunnelClient::new(cred);
            client.on_request(|msg, handle| {
                tracing::info!("Received tunnel message: {:?}", msg);
                // TODO: Dispatch proxied requests to the IPC server / daemon
                let _ = handle;
            });

            println!("✅ Tunnel connected (Ctrl+C to disconnect)");
            client.run().await;

            Ok(())
        }
        TunnelCommands::Stop => {
            // In the current architecture, the tunnel runs as a foreground task.
            // Stopping is done by sending a signal or Ctrl+C.
            // Future: implement daemon-integrated tunnel with PID file.
            println!("🛑 Tunnel stop is not yet implemented for foreground mode.");
            println!("   Use Ctrl+C to disconnect when running `peko tunnel start`.");
            Ok(())
        }
        TunnelCommands::Status { json: json_flag } => {
            let has_cred = crate::tunnel::credential::has_pekohub_credential();
            let cred_path = crate::tunnel::PekoHubCredential::default_path();

            if json_flag || json {
                let output = serde_json::json!({
                    "configured": has_cred,
                    "credential_path": cred_path.to_string_lossy().to_string(),
                    "connected": false, // TODO: query actual connection state
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("📡 Tunnel Status:");
                if has_cred {
                    println!("  Credential: ✅ Found at {}", cred_path.display());
                } else {
                    println!("  Credential: ❌ Not found at {}", cred_path.display());
                }
                println!("  Connected:  ❌ Not connected");
                println!();
                println!("  Start with: peko tunnel start");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tunnel_commands_enum() {
        let _cmd = TunnelCommands::Start { credential: None };
        let _cmd = TunnelCommands::Stop;
        let _cmd = TunnelCommands::Status { json: true };
    }
}
