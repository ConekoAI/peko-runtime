//! Capability authority management commands
//!
//! Capabilities are the fine-grained authority grants that control what a
//! Principal is allowed to do.  This module implements the `peko capability`
//! CLI surface, which mutates a Principal's `[capabilities] grants` array in
//! `principal.toml` via the daemon.
//!
//! - `peko capability grant --principal <name> <capability>`
//! - `peko capability revoke --principal <name> <capability>`
//! - `peko capability list --principal <name>`

use anyhow::Result;
use clap::Subcommand;

use crate::ipc::{DaemonClient, ResponsePacket};

/// Subcommands for `peko capability`.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum CapabilityCommands {
    /// Grant a capability to a Principal
    Grant {
        /// Principal name
        #[arg(long, value_name = "NAME")]
        principal: String,

        /// Capability to grant (e.g. `tool:Read`, `agent:researcher`)
        capability: String,
    },

    /// Revoke a capability from a Principal
    Revoke {
        /// Principal name
        #[arg(long, value_name = "NAME")]
        principal: String,

        /// Capability to revoke
        capability: String,
    },

    /// List capabilities granted to a Principal
    List {
        /// Principal name
        #[arg(long, value_name = "NAME")]
        principal: String,
    },
}

/// Handle `peko capability` subcommands.
pub async fn handle_capability(command: CapabilityCommands, json: bool) -> Result<()> {
    match command {
        CapabilityCommands::Grant {
            principal,
            capability,
        } => {
            let response = capability_grant(&principal, &capability).await?;
            match response {
                ResponsePacket::CapabilityGranted {
                    capability,
                    message,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "capability": capability,
                                "principal": principal,
                                "message": message,
                            })
                        );
                    } else {
                        println!("✅ {message}");
                    }
                }
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                other => anyhow::bail!(crate::ipc::unexpected_response(&other)),
            }
        }
        CapabilityCommands::Revoke {
            principal,
            capability,
        } => {
            let response = capability_revoke(&principal, &capability).await?;
            match response {
                ResponsePacket::CapabilityRevoked {
                    capability,
                    message,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "capability": capability,
                                "principal": principal,
                                "message": message,
                            })
                        );
                    } else {
                        println!("✅ {message}");
                    }
                }
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                other => anyhow::bail!(crate::ipc::unexpected_response(&other)),
            }
        }
        CapabilityCommands::List { principal } => {
            let response = capability_list(&principal).await?;
            match response {
                ResponsePacket::CapabilityList {
                    granted,
                    detected,
                    active,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "principal": principal,
                                "granted": granted,
                                "detected": detected,
                                "active": active,
                            })
                        );
                    } else {
                        println!("Capabilities for '{principal}':");
                        println!("\nGranted:");
                        if granted.is_empty() {
                            println!("  (none)");
                        } else {
                            for cap in &granted {
                                println!("  {cap}");
                            }
                        }
                        println!("\nDetected (not granted):");
                        let not_granted: Vec<_> = detected
                            .iter()
                            .filter(|c| !granted.contains(c))
                            .cloned()
                            .collect();
                        if not_granted.is_empty() {
                            println!("  (none)");
                        } else {
                            for cap in &not_granted {
                                println!("  {cap}");
                            }
                        }
                        println!("\nActive now:");
                        if active.is_empty() {
                            println!("  (none)");
                        } else {
                            for cap in &active {
                                println!("  {cap}");
                            }
                        }
                    }
                }
                ResponsePacket::Error { message, .. } => anyhow::bail!(message),
                other => anyhow::bail!(crate::ipc::unexpected_response(&other)),
            }
        }
    }

    Ok(())
}

/// Grant a capability to a Principal via the daemon.
pub async fn capability_grant(
    principal: impl Into<String>,
    capability: impl Into<String>,
) -> Result<ResponsePacket> {
    let client = DaemonClient::connect().await?;
    client.capability_grant(principal, capability).await
}

/// Revoke a capability from a Principal via the daemon.
pub async fn capability_revoke(
    principal: impl Into<String>,
    capability: impl Into<String>,
) -> Result<ResponsePacket> {
    let client = DaemonClient::connect().await?;
    client.capability_revoke(principal, capability).await
}

/// List capabilities granted to a Principal via the daemon.
pub async fn capability_list(principal: impl Into<String>) -> Result<ResponsePacket> {
    let client = DaemonClient::connect().await?;
    client.capability_list(principal).await
}
