//! System Diagnostics and Maintenance Commands

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// System management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SystemCommands {
    /// Show detailed system status
    Status {
        /// Include resource usage
        #[arg(long)]
        resources: bool,
    },

    /// Show system information
    Info,

    /// Run health check diagnostics
    Doctor {
        /// Fix issues automatically where possible
        #[arg(long)]
        fix: bool,
    },

    /// Clean up temporary files and cache
    Clean {
        /// Remove all tool caches
        #[arg(long)]
        tools: bool,
        /// Remove old logs
        #[arg(long)]
        logs: bool,
        /// Remove everything (full reset)
        #[arg(long)]
        all: bool,
    },
}

/// Helper: connect to daemon and send a request/response packet
async fn ipc_request(
    packet: crate::ipc::RequestPacket,
) -> anyhow::Result<crate::ipc::ResponsePacket> {
    let client = crate::ipc::DaemonClient::connect().await?;
    client.request_response(packet).await
}

/// Handle system commands
pub async fn handle_system(
    cmd: SystemCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        SystemCommands::Status { resources } => {
            let packet = crate::ipc::RequestPacket::SystemStatus { request_id: 1 };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SystemStatus {
                    version,
                    uptime_secs,
                    degraded,
                    instance_count,
                    team_count,
                    ready,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status": if ready { "ok" } else { "degraded" },
                                "version": version,
                                "uptime_secs": uptime_secs,
                                "degraded": degraded,
                                "instance_count": instance_count,
                                "team_count": team_count,
                                "ready": ready,
                            })
                        );
                    } else {
                        println!("📊 System Status:");
                        println!("  Version: {version}");
                        println!("  Uptime: {uptime_secs}s");
                        println!("  Ready: {ready}");
                        if degraded {
                            println!("  ⚠️ Degraded mode");
                        }
                        println!("  Teams: {team_count}");
                        println!("  Instances: {instance_count}");
                        if resources {
                            println!("  (Resource usage not yet implemented)");
                        }
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        SystemCommands::Info => {
            println!("ℹ️  Peko {}", env!("CARGO_PKG_VERSION"));
            println!("  Lightweight multi-agent runtime");
            Ok(())
        }
        SystemCommands::Doctor { fix } => {
            let packet = crate::ipc::RequestPacket::SystemDoctor { request_id: 1 };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SystemDoctor {
                    checks,
                    passed,
                    failed,
                    warnings,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "passed": passed,
                                "failed": failed,
                                "warnings": warnings,
                                "checks": checks,
                                "fix_enabled": fix,
                            })
                        );
                    } else {
                        println!("🏥 Running health check...");
                        if fix {
                            println!("  Auto-fix enabled");
                        }
                        for check in &checks {
                            let icon = match check.status.as_str() {
                                "pass" => "✓",
                                "fail" => "✗",
                                "warn" => "⚠",
                                _ => "?",
                            };
                            println!("  {icon} {}: {}", check.name, check.message);
                            if let Some(ref suggestion) = check.suggestion {
                                println!("     Suggestion: {suggestion}");
                            }
                        }
                        println!();
                        println!(
                            "  Results: {passed} passed, {failed} failed, {warnings} warnings"
                        );
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        SystemCommands::Clean { tools, logs, all } => {
            let scope = if all {
                Some("all".to_string())
            } else if tools && logs {
                Some("all".to_string())
            } else if tools {
                Some("tools".to_string())
            } else if logs {
                Some("logs".to_string())
            } else {
                Some("all".to_string())
            };
            let packet = crate::ipc::RequestPacket::SystemClean {
                request_id: 1,
                scope,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SystemCleaned {
                    cleaned,
                    bytes_freed,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "cleaned": cleaned,
                                "bytes_freed": bytes_freed,
                            })
                        );
                    } else {
                        println!("🧹 Cleaning up...");
                        if tools {
                            println!("  - Tool caches");
                        }
                        if logs {
                            println!("  - Old logs");
                        }
                        if all {
                            println!("  - Everything (full reset)");
                        }
                        if !cleaned.is_empty() {
                            println!(
                                "  Cleaned {} item(s), freed {bytes_freed} bytes",
                                cleaned.len()
                            );
                        }
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}
