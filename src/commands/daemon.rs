//! Daemon Management Commands

use crate::commands::GlobalPaths;
use crate::daemon::{Daemon, DaemonConfig};
use clap::Subcommand;

/// Daemon management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum DaemonCommands {
    /// Start the daemon
    Start {
        /// Run in foreground (don't detach)
        #[arg(short, long)]
        foreground: bool,
        /// Polling interval in seconds
        #[arg(short, long, default_value = "15")]
        interval: u64,
    },

    /// Stop the daemon
    Stop {
        /// Force stop (kill immediately)
        #[arg(short, long)]
        force: bool,
    },

    /// Check daemon status
    Status,

    /// Restart the daemon
    Restart {
        /// Polling interval in seconds
        #[arg(short, long, default_value = "15")]
        interval: u64,
    },

    /// Trigger immediate cron check
    Check,
}

/// Handle daemon commands
pub async fn handle_daemon(
    cmd: DaemonCommands,
    paths: &GlobalPaths,
    _json: bool,
) -> anyhow::Result<()> {
    match cmd {
        DaemonCommands::Start {
            foreground,
            interval,
        } => {
            let config = DaemonConfig {
                cron_db_path: paths.data_dir.join("cron.db"),
                poll_interval: std::time::Duration::from_secs(interval),
                config_dir: paths.config_dir.clone(),
                data_dir: paths.data_dir.clone(),
                enable_isolated_execution: true,
            };

            if foreground {
                println!("🚀 Starting daemon in foreground (interval: {interval}s)...");
                println!("   Config dir: {}", config.config_dir.display());
                println!("   Data dir: {}", config.data_dir.display());

                let (_tx, rx) = tokio::sync::mpsc::channel(10);
                let daemon = Daemon::new(config, rx)?;
                daemon.run().await?;
            } else {
                println!("🚀 Starting daemon (interval: {interval}s)...");
                println!("   (Background daemon mode not yet implemented, use --foreground)");
            }
            Ok(())
        }
        DaemonCommands::Stop { force } => {
            if force {
                println!("💀 Force stopping daemon...");
            } else {
                println!("🛑 Stopping daemon...");
            }
            println!("   (Daemon stop not yet implemented)");
            Ok(())
        }
        DaemonCommands::Status => {
            println!("📊 Daemon Status:");
            println!("  Status: unknown (not implemented)");
            Ok(())
        }
        DaemonCommands::Restart { interval } => {
            println!("🔄 Restarting daemon (interval: {interval}s)...");
            println!("   (Daemon restart not yet implemented)");
            Ok(())
        }
        DaemonCommands::Check => {
            println!("🔍 Triggering cron check...");
            println!("   (Manual cron check not yet implemented)");
            Ok(())
        }
    }
}
