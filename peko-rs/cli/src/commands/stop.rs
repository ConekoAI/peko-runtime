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

/// Stop what a peko is doing in a conversation.
#[derive(Args, Clone, Debug)]
#[command(disable_version_flag = true)]
pub struct StopArgs {
    /// Peko name
    #[arg(value_name = "PEKO")]
    pub principal: String,

    /// Stop it in this person's or peko's conversation instead of yours
    /// (owner only): `user:local`, `user:hub:<id>`, `peko:local:<id>` or
    /// `peko:hub:<id>`.
    #[arg(long, value_name = "SUBJECT", conflicts_with = "main")]
    pub peer: Option<String>,

    /// Stop what it is doing on its own instead of in a conversation
    /// (owner only): setting itself up on its first turn, or scheduled
    /// work. It stops after the step it is on. A first turn you stop is
    /// not run again.
    #[arg(long)]
    pub main: bool,
}

/// Handle the stop command.
pub async fn handle_stop(args: StopArgs, _paths: &GlobalPaths) -> Result<()> {
    // Group channels never trigger agent runs, so there is nothing
    // to stop.
    if let Recipient::Group(_) = parse_recipient(&args.principal) {
        bail!("groups have no bound run; stop a peko instead");
    }
    let peer = args.peer.as_deref().map(parse_subject).transpose()?;
    let thread = args.peer.clone().unwrap_or_else(|| "owner".to_string());

    let client = DaemonClient::connect().await?;
    if args.main {
        return match client.principal_stop_main(args.principal.clone()).await? {
            ResponsePacket::Done { success: true, .. } => {
                println!(
                    "Stopped what peko '{}' was doing on its own. It stops after the step it is on.",
                    args.principal
                );
                Ok(())
            }
            ResponsePacket::Done { error, .. }
                if error
                    .as_deref()
                    .is_some_and(|e| e.starts_with("no running turn")) =>
            {
                println!("Peko '{}' is not doing anything on its own", args.principal);
                Ok(())
            }
            ResponsePacket::Done { error, .. } => {
                bail!(error.unwrap_or_else(|| "stop failed".into()))
            }
            ResponsePacket::Error { message, .. } => bail!(message),
            other => bail!("unexpected response from daemon: {other:?}"),
        };
    }
    let resp = client.principal_stop(args.principal.clone(), peer).await?;

    match resp {
        ResponsePacket::Done { success, error, .. } => {
            if success {
                println!(
                    "Stopped run on thread '{thread}' with peko '{}'",
                    args.principal
                );
            } else if error
                .as_deref()
                .is_some_and(|e| e.starts_with("no running turn"))
            {
                // Idempotent stop: nothing in flight is a notice, not
                // a failure (scripting-friendly exit 0).
                println!(
                    "No running turn on thread '{thread}' with peko '{}'",
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

/// Parse a `--peer` value into a `ActorRef`. Accepts the wire format
/// `user:<id>`, `principal:<did>`, or `public`.
fn parse_subject(value: &str) -> Result<peko_auth::ActorRef> {
    peko_auth::ActorRef::from_str(value)
        .map_err(|e| anyhow::anyhow!("invalid --peer value '{value}': {e}"))
}

#[cfg(test)]
mod tests {
    use crate::commands::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn stop_parses_principal() {
        let cli =
            Cli::try_parse_from(["peko", "stop", "scout"]).expect("should parse stop command");

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
        let err = super::handle_stop(args, &paths)
            .await
            .expect_err("group stop must be refused before IPC");
        assert!(
            format!("{err:#}").contains("groups have no bound run"),
            "got: {err:#}"
        );
    }
}
