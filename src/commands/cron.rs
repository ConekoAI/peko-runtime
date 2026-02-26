//! Cron Job Management Commands

use clap::Subcommand;
use crate::commands::GlobalPaths;

/// Cron management subcommands
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
        /// Timezone (e.g., "`America/Los_Angeles`")
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
        /// Interval (e.g., "5m", "1h", "30s")
        #[arg(short, long)]
        interval: String,
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
}

/// Handle cron commands
pub async fn handle_cron(
    cmd: CronCommands,
    _paths: &GlobalPaths,
    _json: bool,
) -> anyhow::Result<()> {
    match cmd {
        CronCommands::List { all, json } => {
            if json {
                println!("{{\"jobs\": []}}");
            } else {
                println!("🕒 Cron Jobs:");
                if all {
                    println!("  (Including disabled)");
                }
            }
            Ok(())
        }
        CronCommands::Add { name, schedule, timezone, agent, execution, message, announce, delete_after_run } => {
            println!("➕ Adding job '{name}' with schedule '{schedule}'");
            if let Some(tz) = timezone {
                println!("  Timezone: {tz}");
            }
            if let Some(a) = agent {
                println!("  Agent: {a}");
            }
            println!("  Execution: {execution}");
            println!("  Message: {message}");
            if announce {
                println!("  Announce: enabled");
            }
            if delete_after_run {
                println!("  One-shot: yes");
            }
            Ok(())
        }
        CronCommands::At { name, at, agent, message, announce } => {
            println!("➕ Adding one-shot job '{name}' at {at}");
            if let Some(a) = agent {
                println!("  Agent: {a}");
            }
            println!("  Message: {message}");
            if announce {
                println!("  Announce: enabled");
            }
            Ok(())
        }
        CronCommands::Every { name, interval, agent, message, announce } => {
            println!("➕ Adding recurring job '{name}' every {interval}");
            if let Some(a) = agent {
                println!("  Agent: {a}");
            }
            println!("  Message: {message}");
            if announce {
                println!("  Announce: enabled");
            }
            Ok(())
        }
        CronCommands::Remove { job_id, force } => {
            if force {
                println!("🗑️  Removing job '{job_id}'...");
            } else {
                println!("🗑️  Removing job '{job_id}' (use --force to skip confirmation)...");
            }
            Ok(())
        }
        CronCommands::Run { job_id } => {
            println!("▶️  Running job '{job_id}'...");
            Ok(())
        }
        CronCommands::History { job_id, limit } => {
            println!("📜 History for job '{job_id}' (limit: {limit})");
            Ok(())
        }
    }
}
