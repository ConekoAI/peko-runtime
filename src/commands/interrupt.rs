//! Interrupt Command - soft-cancel or steer a running `peko send --stream`
//!
//! `peko interrupt <request-id>` sends a `PrincipalSendControl { Interrupt }`
//! to the daemon, which sets the cancel token on the in-flight run. The
//! agentic loop observes the token at the next iteration boundary, emits
//! `Lifecycle::Interrupted`, and exits cleanly.
//!
//! `peko interrupt <request-id> --steer "new message"` sends a
//! `PrincipalSendControl { Steer { text } }` instead, which pushes the
//! text into the run's session inbox. The agentic loop's existing
//! inbox-drain picks it up at the next iteration and folds it in as a
//! new user turn.
//!
//! The `request_id` is the integer printed to stderr by
//! `peko send --stream` at start (and shown in `peko log` events).

use crate::commands::GlobalPaths;
use crate::ipc::packet::PrincipalSendControlMode;
use crate::ipc::{DaemonClient, ResponsePacket};
use anyhow::{bail, Result};
use clap::Args;

/// Soft-interrupt or steer a running `peko send --stream` run.
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct InterruptArgs {
    /// The `request_id` of the running `peko send --stream` (printed
    /// to stderr when the stream starts; visible in `peko log`).
    pub request_id: u64,

    /// Inject `TEXT` as a new user turn into the running run's session
    /// inbox instead of interrupting it. The agentic loop picks it up
    /// at the next iteration and continues.
    #[arg(long, value_name = "TEXT")]
    pub steer: Option<String>,
}

/// Handle the interrupt command.
pub async fn handle_interrupt(
    args: InterruptArgs,
    _paths: &GlobalPaths,
    _json: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let mode = match args.steer {
        Some(text) => PrincipalSendControlMode::Steer { text },
        None => PrincipalSendControlMode::Interrupt,
    };

    let resp = client
        .principal_send_control(args.request_id, mode.clone())
        .await?;

    match resp {
        ResponsePacket::Done { success, error, .. } => {
            if success {
                match mode {
                    PrincipalSendControlMode::Interrupt => {
                        println!("Sent interrupt to run {}", args.request_id);
                    }
                    PrincipalSendControlMode::Steer { .. } => {
                        println!("Sent steering message to run {}", args.request_id);
                    }
                }
                Ok(())
            } else {
                bail!(error.unwrap_or_else(|| "interrupt failed".into()));
            }
        }
        ResponsePacket::Error { message, .. } => bail!(message),
        other => bail!("unexpected response from daemon: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn interrupt_parses_request_id() {
        let cli = Cli::try_parse_from(["peko", "interrupt", "42"])
            .expect("should parse interrupt command");

        match cli.command {
            Commands::Interrupt(args) => {
                assert_eq!(args.request_id, 42);
                assert!(args.steer.is_none());
            }
            _other => panic!("expected Interrupt command"),
        }
    }

    #[test]
    fn interrupt_parses_steer_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "interrupt",
            "42",
            "--steer",
            "actually do X instead",
        ])
        .expect("should parse interrupt command with --steer");

        match cli.command {
            Commands::Interrupt(args) => {
                assert_eq!(args.request_id, 42);
                assert_eq!(args.steer.as_deref(), Some("actually do X instead"));
            }
            _other => panic!("expected Interrupt command"),
        }
    }
}
