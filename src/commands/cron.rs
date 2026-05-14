//! Cron Job Management Commands
//!
//! All operations are delegated to the daemon via IPC.
//! The daemon owns the SQLite cron database and the execution engine.

use crate::commands::GlobalPaths;
use crate::cron::{CronJob, DeliveryMode, ExecutionTarget, ScheduleKind};
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Subcommand;
use std::str::FromStr;
use uuid::Uuid;

/// Cron management subcommands
///
/// Schedule agents to run at specific times or intervals.
/// Jobs are persisted and survive daemon restarts.
///
/// Examples:
///   # List all cron jobs
///   peko cron list
///
///   # Add a daily job (9 AM)
///   peko cron add --name "daily-report" --schedule "0 9 * * *" --message "Generate daily summary"
///
///   # Add a one-time job
///   peko cron at --name "reminder" --at "2026-03-20T14:00:00Z" --message "Meeting in 1 hour"
///
///   # Remove a job
///   peko cron remove daily-report
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum CronCommands {
    /// List all cron jobs
    List {
        /// Show all jobs including disabled
        #[arg(long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Add a new cron job
    Add {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Schedule expression (cron format: "0 9 * * *")
        #[arg(short, long)]
        schedule: String,
        /// Timezone (e.g., "America/Los_Angeles")
        #[arg(short, long)]
        timezone: Option<String>,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Execution mode: main or isolated
        #[arg(short, long, default_value = "main")]
        execution: String,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
        /// Delete after successful run (one-shot)
        #[arg(long)]
        delete_after_run: bool,
    },

    /// Add a one-shot job at specific time
    At {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// ISO timestamp (e.g., "2026-02-25T14:00:00Z")
        #[arg(long)]
        at: String,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },

    /// Add a recurring interval job
    Every {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Interval in milliseconds
        #[arg(short, long)]
        interval_ms: u64,
        /// Agent to run as
        #[arg(short, long)]
        agent: Option<String>,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },

    /// Remove a cron job
    Remove {
        /// Job ID
        job_id: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Run a job immediately (manual execution)
    Run {
        /// Job ID
        job_id: String,
    },

    /// Show job run history
    History {
        /// Job ID
        job_id: String,
        /// Limit results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },

    /// Add an idle-triggered job (runs when agent is idle)
    AddIdle {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Idle threshold in minutes
        #[arg(short = 't', long)]
        minutes: u64,
        /// Specific agent to monitor (omit for any agent)
        #[arg(short, long)]
        agent: Option<String>,
        /// Message/prompt to execute
        #[arg(short = 'm', long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },

    /// Add an event-triggered job (runs on system event)
    AddEvent {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Event type to listen for (file, webhook, internal, timer)
        #[arg(short, long)]
        event_type: String,
        /// JSON filter expression (e.g., '{"source": "github"}')
        #[arg(short, long)]
        filter: Option<String>,
        /// Run only once then disable
        #[arg(long)]
        once: bool,
        /// Message/prompt to execute
        #[arg(short, long)]
        message: String,
        /// Announce results
        #[arg(long)]
        announce: bool,
    },
}

/// Connect to daemon or return a clear error
async fn connect_daemon() -> Result<DaemonClient> {
    DaemonClient::connect()
        .await
        .context("Daemon is not running. Start it with: peko daemon start")
}

/// Handle cron commands
pub async fn handle_cron(cmd: CronCommands, _paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        CronCommands::List {
            all,
            json: cmd_json,
        } => {
            let client = connect_daemon().await?;
            let use_json = cmd_json || json;
            match client.cron_list(all).await? {
                ResponsePacket::CronList { jobs, .. } => {
                    if use_json {
                        println!("{}", serde_json::to_string_pretty(&jobs)?);
                    } else if jobs.is_empty() {
                        println!("🕒 No cron jobs found.");
                    } else {
                        println!("🕒 Cron Jobs:");
                        for job in jobs {
                            let status = if job.enabled { "✅" } else { "⏸️" };
                            let schedule = job.schedule.display();
                            let agent = job.agent_id.as_deref().unwrap_or("default");
                            println!(
                                "  {} {} | {} | agent: {} | next: {}",
                                status,
                                job.id,
                                schedule,
                                agent,
                                job.next_run.to_rfc3339()
                            );
                            println!("     └─ {}", job.message);
                        }
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to list jobs: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::Add {
            name,
            schedule,
            timezone,
            agent,
            execution,
            message,
            announce,
            delete_after_run,
        } => {
            let client = connect_daemon().await?;

            // Validate cron expression (normalize 5-field to 7-field for the cron crate)
            let normalized = crate::cron::normalize_cron_expr(&schedule);
            let _ = cron::Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

            let schedule_kind = ScheduleKind::Cron {
                expr: schedule.clone(),
                tz: timezone.clone(),
            };

            let target = match execution.as_str() {
                "isolated" => ExecutionTarget::Isolated,
                _ => ExecutionTarget::Main,
            };

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            // Compute next run
            let next_run = crate::cron::calculate_next_run(&schedule_kind, Utc::now())?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                schedule: schedule_kind,
                target,
                agent_id: agent,
                message,
                delivery,
                delete_after_run,
                enabled: true,
                created_at: Utc::now(),
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added cron job {job_id} with schedule '{schedule}'");
                    if let Some(tz) = timezone {
                        println!("   Timezone: {tz}");
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::At {
            name,
            at,
            agent,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let at_time = chrono::DateTime::parse_from_rfc3339(&at)
                .map_err(|e| anyhow::anyhow!("Invalid timestamp (use RFC3339): {e}"))?;

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                schedule: ScheduleKind::At {
                    at: at_time.to_rfc3339(),
                },
                target: ExecutionTarget::Main,
                agent_id: agent,
                message,
                delivery,
                delete_after_run: true,
                enabled: true,
                created_at: Utc::now(),
                next_run: at_time.with_timezone(&Utc),
                last_run: None,
                last_status: None,
                run_count: 0,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added one-shot job {job_id} at {at}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::Every {
            name,
            interval_ms,
            agent,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let schedule_kind = ScheduleKind::Every {
                every_ms: interval_ms,
            };
            let next_run = crate::cron::calculate_next_run(&schedule_kind, Utc::now())?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                schedule: schedule_kind,
                target: ExecutionTarget::Main,
                agent_id: agent,
                message,
                delivery,
                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    let secs = interval_ms / 1000;
                    let interval_str = if secs < 60 {
                        format!("{secs}s")
                    } else if secs < 3600 {
                        format!("{}m", secs / 60)
                    } else {
                        format!("{}h", secs / 3600)
                    };
                    println!("✅ Added recurring job {job_id} every {interval_str}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::Remove { job_id, force } => {
            if !force {
                println!("🗑️  Removing job '{job_id}'... (use --force to skip confirmation)");
            }
            let client = connect_daemon().await?;
            match client.cron_remove(&job_id).await? {
                ResponsePacket::CronRemoved {
                    job_id: removed_id, ..
                } => {
                    println!("✅ Removed job '{removed_id}'");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to remove job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::Run { job_id } => {
            let client = connect_daemon().await?;
            match client.cron_run(&job_id).await? {
                ResponsePacket::CronRunStarted {
                    job_id: run_job_id,
                    run_id,
                    ..
                } => {
                    println!("▶️  Triggered job '{run_job_id}' (run_id: {run_id})");
                    println!("   The daemon will execute it on the next poll cycle.");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to run job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::History { job_id, limit } => {
            let client = connect_daemon().await?;
            match client.cron_history(&job_id, limit).await? {
                ResponsePacket::CronHistory { runs, .. } => {
                    if runs.is_empty() {
                        println!("📜 No history for job '{job_id}'");
                    } else {
                        println!("📜 History for job '{job_id}':");
                        for run in runs {
                            let status_icon = match run.status.as_str() {
                                "success" => "✅",
                                "failed" => "❌",
                                "running" => "🔄",
                                _ => "⏸️",
                            };
                            println!(
                                "  {} {} | started: {} | status: {}",
                                status_icon,
                                run.id,
                                run.started_at.to_rfc3339(),
                                run.status
                            );
                            if let Some(ref err) = run.error {
                                println!("     └─ Error: {err}");
                            }
                        }
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to get history: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::AddIdle {
            name,
            minutes,
            agent,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                schedule: ScheduleKind::Idle {
                    minutes,
                    agent_id: agent.clone(),
                },
                target: ExecutionTarget::Main,
                agent_id: agent.clone(),
                message,
                delivery,
                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now() + chrono::Duration::days(365 * 100),
                last_run: None,
                last_status: None,
                run_count: 0,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added idle-triggered job {job_id}");
                    println!("   Idle threshold: {minutes} minutes");
                    if let Some(a) = &agent {
                        println!("   Monitor agent: {a}");
                    } else {
                        println!("   Monitor: any agent");
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }

        CronCommands::AddEvent {
            name,
            event_type,
            filter,
            once,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let filter_val = filter.map(|f| serde_json::from_str(&f).ok()).flatten();

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                schedule: ScheduleKind::Event {
                    event_type,
                    filter: filter_val,
                    once,
                },
                target: ExecutionTarget::Main,
                agent_id: None,
                message,
                delivery,
                delete_after_run: once,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now() + chrono::Duration::days(365 * 100),
                last_run: None,
                last_status: None,
                run_count: 0,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added event-triggered job {job_id}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(anyhow::anyhow!("Unexpected response: {other:?}")),
            }
        }
    }
}
