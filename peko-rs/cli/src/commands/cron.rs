//! Cron Job Management Commands
//!
//! All operations are delegated to the daemon via IPC.
//! The daemon owns the cron database and the execution engine.
//! Every cron job targets a Principal and runs with that Principal's
//! owner permissions.

use crate::commands::GlobalPaths;
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Subcommand;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_cron::tools::parse_duration_ms;
use peko_cron::{CronJob, CronJobAction, DeliveryMode, ScheduleKind};
use peko_subject::PrincipalId;
use std::str::FromStr;
use uuid::Uuid;

/// Cron management subcommands
///
/// Schedule jobs that target a Principal. Jobs are persisted and
/// survive daemon restarts.
///
/// Examples:
///   # List all cron jobs
///   peko cron list
///
///   # Add a daily job (9 AM) for a Principal
///   peko cron add --name "daily-report" --principal my-principal \\
///     --schedule "0 9 * * *" --message "Generate daily summary"
///
///   # Add a one-time job
///   peko cron at --name "reminder" --principal my-principal \\
///     --at "2026-03-20T14:00:00Z" --message "Meeting in 1 hour"
///
///   # Remove a job
///   peko cron remove <JOB_ID>
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum CronCommands {
    /// List cron jobs
    List {
        /// Show all jobs including disabled
        #[arg(long)]
        all: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Filter to a specific Principal
        #[arg(short, long)]
        principal: Option<String>,
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
        /// Principal to run the job as
        #[arg(short, long)]
        principal: String,
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
        /// RFC3339 timestamp (e.g., "2026-02-25T14:00:00Z") or a
        /// relative delay like "in 10m" / "in 90s" / "in 2h"
        #[arg(long)]
        at: String,
        /// Principal to run the job as
        #[arg(short, long)]
        principal: String,
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
        #[arg(short, long, conflicts_with = "interval")]
        interval_ms: Option<u64>,
        /// Interval as a human duration ("30s", "5m", "1h", "1d");
        /// alternative to --interval-ms
        #[arg(long)]
        interval: Option<String>,
        /// Principal to run the job as
        #[arg(short, long)]
        principal: String,
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
        job_id: Option<String>,
        /// Job name (exact match; alternative to the positional job ID)
        #[arg(long)]
        name: Option<String>,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Run a job immediately (manual execution)
    Run {
        /// Job ID
        job_id: Option<String>,
        /// Job name (exact match; alternative to the positional job ID)
        #[arg(long)]
        name: Option<String>,
    },

    /// Show job run history
    History {
        /// Job ID
        job_id: Option<String>,
        /// Job name (exact match against live jobs; alternative to the
        /// positional job ID). Fired one-shots no longer appear in the
        /// job list — use their job ID for those.
        #[arg(long)]
        name: Option<String>,
        /// Limit results
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },

    /// Add an idle-triggered job (runs when the target Principal is idle)
    AddIdle {
        /// Job name
        #[arg(short, long)]
        name: String,
        /// Idle threshold in minutes
        #[arg(short = 't', long)]
        minutes: u64,
        /// Principal to run as and monitor
        #[arg(short, long)]
        principal: String,
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
        /// Principal to run the job as
        #[arg(short, long)]
        principal: String,
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
pub async fn handle_cron(cmd: CronCommands, paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        CronCommands::List {
            all,
            json: cmd_json,
            principal,
        } => {
            let client = connect_daemon().await?;
            let use_json = cmd_json || json;
            match client.cron_list(all, principal.clone()).await? {
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
                            let action_kind = job.action.kind_label();
                            // Mask the 100-year "never" sentinel parked on
                            // one-shot jobs after they fire — a raw 2126
                            // timestamp reads as nonsense to users
                            // (2026-08-07 field test, U1).
                            let next = if job.next_run
                                > Utc::now() + chrono::Duration::days(365 * 10)
                            {
                                "—".to_string()
                            } else {
                                job.next_run.to_rfc3339()
                            };
                            println!(
                                "  {} {} | {} | {} | principal: {} | next: {}",
                                status,
                                job.id,
                                schedule,
                                action_kind,
                                job.principal_id.0,
                                next
                            );
                            println!("     └─ {}", job.task_description());
                        }
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to list jobs: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::Add {
            name,
            schedule,
            timezone,
            principal,
            message,
            announce,
            delete_after_run,
        } => {
            let client = connect_daemon().await?;

            // Validate cron expression (normalize 5-field to 7-field for the cron crate)
            let normalized = peko_cron::normalize_cron_expr(&schedule);
            let _ = cron::Schedule::from_str(&normalized)
                .map_err(|e| anyhow::anyhow!("Invalid cron expression: {e}"))?;

            let schedule_kind = ScheduleKind::Cron {
                expr: schedule.clone(),
                tz: timezone.clone(),
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
            let next_run = peko_cron::calculate_next_run(&schedule_kind, Utc::now())?;

            // **Phase B.** Resolve the principal name to its stable DID
            // so `CronJob::principal_id` carries the identity the daemon
            // will key its in-memory scheduler hash by. The CLI takes a
            // name from `--principal` because that's what users type, but
            // the wire shape uses the DID for rename-survivability.
            let principal_id: PrincipalId = paths
                .principal_id_for(&principal)
                .ok_or_else(|| anyhow::anyhow!("Principal '{principal}' not found"))?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                principal_id,
                schedule: schedule_kind,
                action: CronJobAction::Send { message },
                delivery,
                delete_after_run,
                enabled: true,
                created_at: Utc::now(),
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
                consecutive_failures: 0,
                max_retries: None,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added cron job {job_id} with schedule '{schedule}'");
                    println!("   Principal: {principal}");
                    if let Some(tz) = timezone {
                        println!("   Timezone: {tz}");
                    }
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::At {
            name,
            at,
            principal,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let at_time = parse_at_time(&at)?;

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let principal_id: PrincipalId = paths
                .principal_id_for(&principal)
                .ok_or_else(|| anyhow::anyhow!("Principal '{principal}' not found"))?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                principal_id,
                schedule: ScheduleKind::At {
                    at: at_time.to_rfc3339(),
                },
                action: CronJobAction::Send { message },
                delivery,
                delete_after_run: true,
                enabled: true,
                created_at: Utc::now(),
                next_run: at_time.with_timezone(&Utc),
                last_run: None,
                last_status: None,
                run_count: 0,
                consecutive_failures: 0,
                max_retries: None,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    // Print the resolved UTC timestamp, not the raw
                    // `--at` input (which may be a relative "in 10m").
                    println!("✅ Added one-shot job {job_id} at {}", at_time.to_rfc3339());
                    println!("   Principal: {principal}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::Every {
            name,
            interval_ms,
            interval,
            principal,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let interval_ms = match (interval_ms, interval) {
                (Some(ms), None) => ms,
                (None, Some(dur)) => parse_duration_ms(&dur)?,
                (None, None) => {
                    anyhow::bail!("either --interval-ms or --interval is required")
                }
                (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
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

            let schedule_kind = ScheduleKind::Every {
                every_ms: interval_ms,
            };
            let next_run = peko_cron::calculate_next_run(&schedule_kind, Utc::now())?;

            let principal_id: PrincipalId = paths
                .principal_id_for(&principal)
                .ok_or_else(|| anyhow::anyhow!("Principal '{principal}' not found"))?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                principal_id,
                schedule: schedule_kind,
                action: CronJobAction::Send { message },
                delivery,
                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run,
                last_run: None,
                last_status: None,
                run_count: 0,
                consecutive_failures: 0,
                max_retries: None,
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
                    println!("   Principal: {principal}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::Remove {
            job_id,
            name,
            force,
        } => {
            let client = connect_daemon().await?;
            let job_id = resolve_job_id(&client, job_id, name).await?;
            if !force {
                println!("🗑️  Removing job '{job_id}'... (use --force to skip confirmation)");
            }
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
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::Run { job_id, name } => {
            let client = connect_daemon().await?;
            let job_id = resolve_job_id(&client, job_id, name).await?;
            match client.cron_run(&job_id).await? {
                ResponsePacket::CronRunStarted {
                    job_id: run_job_id,
                    run_id,
                    ..
                } => {
                    println!("▶️  Triggered job '{run_job_id}' (run_id: {run_id})");
                    println!("   The daemon is firing it now.");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to run job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::History {
            job_id,
            name,
            limit,
        } => {
            let client = connect_daemon().await?;
            let job_id = resolve_job_id(&client, job_id, name).await?;
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
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::AddIdle {
            name,
            minutes,
            principal,
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

            let principal_id: PrincipalId = paths
                .principal_id_for(&principal)
                .ok_or_else(|| anyhow::anyhow!("Principal '{principal}' not found"))?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                principal_id,
                schedule: ScheduleKind::Idle { minutes },
                action: CronJobAction::Send { message },
                delivery,
                delete_after_run: false,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now() + chrono::Duration::days(365 * 100),
                last_run: None,
                last_status: None,
                run_count: 0,
                consecutive_failures: 0,
                max_retries: None,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added idle-triggered job {job_id}");
                    println!("   Principal: {principal}");
                    println!("   Idle threshold: {minutes} minutes");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }

        CronCommands::AddEvent {
            name,
            event_type,
            filter,
            once,
            principal,
            message,
            announce,
        } => {
            let client = connect_daemon().await?;

            let filter_val = filter.and_then(|f| serde_json::from_str(&f).ok());

            let delivery = if announce {
                DeliveryMode::Announce {
                    channel: None,
                    to: None,
                    best_effort: true,
                }
            } else {
                DeliveryMode::None
            };

            let principal_id: PrincipalId = paths
                .principal_id_for(&principal)
                .ok_or_else(|| anyhow::anyhow!("Principal '{principal}' not found"))?;

            let job = CronJob {
                id: format!("cron_{}", Uuid::new_v4().simple()),
                name,
                principal_id,
                schedule: ScheduleKind::Event {
                    event_type,
                    filter: filter_val,
                    once,
                },
                action: CronJobAction::Send { message },
                delivery,
                delete_after_run: once,
                enabled: true,
                created_at: Utc::now(),
                next_run: Utc::now() + chrono::Duration::days(365 * 100),
                last_run: None,
                last_status: None,
                run_count: 0,
                consecutive_failures: 0,
                max_retries: None,
            };

            match client.cron_add(job).await? {
                ResponsePacket::CronAdded { job_id, .. } => {
                    println!("✅ Added event-triggered job {job_id}");
                    println!("   Principal: {principal}");
                    Ok(())
                }
                ResponsePacket::Error { message, .. } => {
                    Err(anyhow::anyhow!("Failed to add job: {message}"))
                }
                other => Err(peko_core::ipc::unexpected_response(&other)),
            }
        }
    }
}

/// Parse `--at`: an RFC3339 timestamp, or a relative delay like
/// "in 10m" / "in 90s" resolved against the local clock (stored UTC).
fn parse_at_time(input: &str) -> Result<chrono::DateTime<Utc>> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("in ") {
        let ms = parse_duration_ms(rest)?;
        return Ok(Utc::now() + chrono::Duration::milliseconds(ms as i64));
    }
    chrono::DateTime::parse_from_rfc3339(input)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow::anyhow!("Invalid timestamp (use RFC3339 or 'in 10m'): {e}"))
}

/// Resolve the target job id from either the positional job ID or
/// `--name` (exact match over live jobs, resolved client-side — mirrors
/// the LLM-facing `CronDelete` tool's `resolve_id_by_label`). Errors on
/// zero or multiple matches.
async fn resolve_job_id(
    client: &DaemonClient,
    job_id: Option<String>,
    name: Option<String>,
) -> Result<String> {
    match (job_id, name) {
        (Some(id), None) => Ok(id),
        (None, Some(name)) => {
            let jobs = match client.cron_list(true, None).await? {
                ResponsePacket::CronList { jobs, .. } => jobs,
                ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("Failed to list jobs: {message}")
                }
                other => return Err(peko_core::ipc::unexpected_response(&other)),
            };
            let matches: Vec<_> = jobs.into_iter().filter(|j| j.name == name).collect();
            match matches.len() {
                0 => anyhow::bail!("No cron job named '{name}'"),
                1 => Ok(matches[0].id.clone()),
                _ => {
                    let ids: Vec<_> = matches.iter().map(|j| j.id.as_str()).collect();
                    anyhow::bail!(
                        "Multiple cron jobs named '{name}' ({}); use the job ID",
                        ids.join(", ")
                    )
                }
            }
        }
        (None, None) => anyhow::bail!("provide a job ID or --name"),
        (Some(_), Some(_)) => anyhow::bail!("provide either a job ID or --name, not both"),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_at_time;
    use peko_cron::tools::parse_duration_ms;
    use chrono::Utc;

    #[test]
    fn duration_bare_number_is_ms() {
        assert_eq!(parse_duration_ms("60000").unwrap(), 60_000);
    }

    #[test]
    fn duration_suffixes() {
        assert_eq!(parse_duration_ms("30s").unwrap(), 30_000);
        assert_eq!(parse_duration_ms("5m").unwrap(), 300_000);
        assert_eq!(parse_duration_ms("1h").unwrap(), 3_600_000);
        assert_eq!(parse_duration_ms("1d").unwrap(), 86_400_000);
    }

    #[test]
    fn duration_rejects_garbage() {
        assert!(parse_duration_ms("soon").is_err());
        assert!(parse_duration_ms("").is_err());
        assert!(parse_duration_ms("10x").is_err());
    }

    #[test]
    fn at_time_rfc3339_passthrough() {
        let dt = parse_at_time("2026-02-25T14:00:00Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-02-25T14:00:00+00:00");
    }

    #[test]
    fn at_time_relative() {
        let before = Utc::now();
        let dt = parse_at_time("in 10m").unwrap();
        let after = Utc::now();
        assert!(dt >= before + chrono::Duration::minutes(10));
        assert!(dt <= after + chrono::Duration::minutes(10));
    }

    #[test]
    fn at_time_rejects_garbage() {
        assert!(parse_at_time("next tuesday").is_err());
    }
}
