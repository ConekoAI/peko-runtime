//! Runtime management commands (ADR-032)

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Runtime management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum RuntimeCommands {
    /// Show this runtime's DID
    Id,
    /// Show runtime metadata
    Info,
    /// Update display name
    Rename {
        /// New display name
        display_name: String,
    },
    /// List known runtimes
    List,
    /// Register a runtime
    Register {
        /// Runtime DID
        runtime_id: String,
        /// Display name
        #[arg(short, long)]
        name: String,
    },
    /// Authorize a runtime
    Trust {
        /// Runtime DID
        runtime_id: String,
    },
    /// Remove a runtime from registry
    Remove {
        /// Runtime DID
        runtime_id: String,
    },
}

/// Handle runtime commands
pub async fn handle_runtime(
    cmd: RuntimeCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        RuntimeCommands::Id => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeId { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::RuntimeId { did, .. } => {
                    if json {
                        println!("{}", serde_json::json!({"did": did}));
                    } else {
                        println!("{}", did);
                    }
                    Ok(())
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Info => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeInfo { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::RuntimeInfo { metadata, .. } => {
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
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Rename { display_name } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeRename {
                request_id: 1,
                display_name,
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("✅ Runtime renamed");
                        Ok(())
                    } else {
                        anyhow::bail!("{}", error.unwrap_or_else(|| "Unknown error".to_string()))
                    }
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::List => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeList { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::RuntimeList { runtimes, .. } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(
                                &serde_json::json!({"runtimes": runtimes})
                            )?
                        );
                    } else if runtimes.is_empty() {
                        println!("No known runtimes.");
                    } else {
                        println!("Known runtimes ({}):", runtimes.len());
                        for rt in runtimes {
                            let icon = match rt.trust_level.as_str() {
                                "selfruntime" => "⭐",
                                "authorized" => "✅",
                                _ => "❓",
                            };
                            println!(
                                "{} {} — {} (trust: {})",
                                icon, rt.runtime_id, rt.display_name, rt.trust_level
                            );
                        }
                    }
                    Ok(())
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Register { runtime_id, name } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeRegister {
                request_id: 1,
                runtime_id: runtime_id.clone(),
                display_name: name,
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("✅ Registered runtime {}", runtime_id);
                        Ok(())
                    } else {
                        anyhow::bail!("{}", error.unwrap_or_else(|| "Unknown error".to_string()))
                    }
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Trust { runtime_id } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeTrust {
                request_id: 1,
                runtime_id: runtime_id.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("✅ Trusted runtime {}", runtime_id);
                        Ok(())
                    } else {
                        anyhow::bail!("{}", error.unwrap_or_else(|| "Unknown error".to_string()))
                    }
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Remove { runtime_id } => {
            let client = crate::ipc::DaemonClient::connect().await?;
            let packet = crate::ipc::RequestPacket::RuntimeRemove {
                request_id: 1,
                runtime_id: runtime_id.clone(),
            };
            let response = client.request_response(packet).await?;
            match response {
                crate::ipc::ResponsePacket::Done { success, error, .. } => {
                    if success {
                        println!("✅ Removed runtime {}", runtime_id);
                        Ok(())
                    } else {
                        anyhow::bail!("{}", error.unwrap_or_else(|| "Unknown error".to_string()))
                    }
                }
                crate::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}
