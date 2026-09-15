//! Runtime identity commands (ADR-032).
//!
//! The legacy `KnownRuntimes` trust registry commands (`list`,
//! `register`, `trust`, `remove`) were removed in the ADR-057
//! cleanup: `TrustLevel` was never consulted by any access decision,
//! and the direct-transport fields the registry carried were dead.

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Runtime identity subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum RuntimeCommands {
    /// Show this runtime's DID
    Id,
    /// Show runtime metadata
    Info,
}

/// Handle runtime commands
pub async fn handle_runtime(
    cmd: RuntimeCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        RuntimeCommands::Id => {
            let client = peko_core::ipc::DaemonClient::connect().await?;
            let packet = peko_core::ipc::RequestPacket::RuntimeId { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                peko_core::ipc::ResponsePacket::RuntimeId { did, .. } => {
                    if json {
                        println!("{}", serde_json::json!({"did": did}));
                    } else {
                        println!("{}", did);
                    }
                    Ok(())
                }
                peko_core::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Info => {
            let client = peko_core::ipc::DaemonClient::connect().await?;
            let packet = peko_core::ipc::RequestPacket::RuntimeInfo { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                peko_core::ipc::ResponsePacket::RuntimeInfo { metadata, .. } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&metadata)?);
                    } else {
                        println!("Runtime ID: {}", metadata.runtime_id);
                        println!("Display Name: {}", metadata.display_name);
                        println!("Version: {}", metadata.version);
                        println!("Created: {}", metadata.created_at);
                        println!("Last Seen: {}", metadata.last_seen_at);
                        println!("Capabilities: {}", metadata.capabilities.join(", "));
                        println!("Host OS: {}", metadata.host_info.os);
                        println!("Host Arch: {}", metadata.host_info.arch);
                        println!("Hostname: {}", metadata.host_info.hostname);
                    }
                    Ok(())
                }
                peko_core::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}
