//! Stop Command - soft-stop the run on a (principal, peer) thread
//!
//! `peko stop <principal>` cancels the in-flight run bound to the
//! caller's thread with the principal: the daemon fires the run's
//! cancel token (the agentic loop exits at the next iteration
//! boundary), posts a `⏹ stopped by user` marker to the thread, and
//! leaves a stop-context note for the next run. With no run in flight
//! it reports "no running turn" and exits 0 — safe to call from
//! scripts.
//!
//! `peko stop <principal> --peer user:alice` (owner only) stops the
//! run on alice's thread.

use crate::commands::{parse_recipient, GlobalPaths, Recipient};
use anyhow::{bail, Result};
use clap::Args;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use std::str::FromStr;

/// Soft-stop the running turn on a thread with a Principal.
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct StopArgs {
    /// Principal name
    pub principal: String,

    /// Stop the run on this peer's thread instead of your own (owner
    /// only). Accepts the wire format `user:<id>` or `principal:<did>`.
    #[arg(long, value_name = "SUBJECT")]
    pub peer: Option<String>,
}

/// Handle the stop command.
pub async fn handle_stop(args: StopArgs, _paths: &GlobalPaths, _json: bool) -> Result<()> {
    // Group channels never trigger agent runs, so there is nothing
    // to stop.
    if let Recipient::Group(_) = parse_recipient(&args.principal) {
        bail!("groups have no bound run; stop a principal instead");
    }
    let peer = args.peer.as_deref().map(parse_subject).transpose()?;
    let thread = args.peer.clone().unwrap_or_else(|| "owner".to_string());

    let client = DaemonClient::connect().await?;
    let resp = client.principal_stop(args.principal.clone(), peer).await?;

    match resp {
        ResponsePacket::Done { success, error, .. } => {
            if success {
                println!(
                    "Stopped run on thread '{thread}' with principal '{}'",
                    args.principal
                );
            } else if error.as_deref().is_some_and(|e| e.starts_with("no running turn")) {
                // Idempotent stop: nothing in flight is a notice, not
                // a failure (scripting-friendly exit 0).
                println!(
                    "No running turn on thread '{thread}' with principal '{}'",
                    args.principal
                );
            } else {
                bail!(error.unwrap_or_else(|| "stop failed".into()));
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => bail!(message),
        other => bail!("unexpected response from daemon: {other:?}"),
    }
}

/// Parse a `--peer` value into a `Subject`. Accepts the wire format
/// `user:<id>`, `principal:<did>`, or `public`.
fn parse_subject(value: &str) -> Result<peko_auth::Subject> {
    peko_auth::Subject::from_str(value)
        .map_err(|e| anyhow::anyhow!("invalid --peer value '{value}': {e}"))
}

#[cfg(test)]
mod tests {
    use crate::commands::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn stop_parses_principal() {
        let cli = Cli::try_parse_from(["peko", "stop", "scout"])
            .expect("should parse stop command");

        match cli.command {
            Commands::Stop(args) => {
                assert_eq!(args.principal, "scout");
                assert!(args.peer.is_none());
            }
            _other => panic!("expected Stop command"),
        }
    }

    #[test]
    fn stop_parses_peer_flag() {
        let cli = Cli::try_parse_from(["peko", "stop", "scout", "--peer", "user:alice"])
            .expect("should parse stop command with --peer");

        match cli.command {
            Commands::Stop(args) => {
                assert_eq!(args.principal, "scout");
                assert_eq!(args.peer.as_deref(), Some("user:alice"));
            }
            _other => panic!("expected Stop command"),
        }
    }

    #[tokio::test]
    async fn stop_group_recipient_refused() {
        use crate::commands::from_cli;
        let cli = Cli::try_parse_from(["peko", "stop", "group:eng-standup"])
            .expect("should parse group recipient");
        let paths = from_cli(&cli);
        let args = match cli.command {
            Commands::Stop(args) => args,
            _ => panic!("expected Stop"),
        };
        let err = super::handle_stop(args, &paths, false)
            .await
            .expect_err("group stop must be refused before IPC");
        assert!(
            format!("{err:#}").contains("groups have no bound run"),
            "got: {err:#}"
        );
    }
}
