//! Per-principal token quota CLI (F18).
//!
//! All operations are delegated to the daemon via IPC — the daemon
//! owns the live [`QuotaMeter`](crate::quota::QuotaMeter) and the
//! persisted `quota_state.json` per principal.
//!
//! Subcommands:
//!
//! - `peko quota status <name>` — read the live counters + window.
//! - `peko quota set <name> --input N --output N --requests N
//!   --cycle hourly|daily|weekly|monthly` — replace the limits.
//! - `peko quota reset <name>` — force a fresh window.

use crate::commands::GlobalPaths;
use crate::ipc::{DaemonClient, ResponsePacket};
use crate::quota::{QuotaConfig, QuotaCycle, QuotaState};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use std::str::FromStr;

/// Quota management subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum QuotaCommands {
    /// Show a principal's live quota counters and window.
    ///
    /// F20: pass `--peer` to query a peer's quota instead of a
    /// principal's. The `name` is then interpreted as a peer id.
    Status {
        /// Principal name (or peer id when `--peer` is set)
        name: String,
        /// Query a peer's quota instead of a principal's
        #[arg(long)]
        peer: bool,
    },

    /// Replace a principal's quota configuration.
    ///
    /// All four flags are optional; any flag you omit keeps its
    /// current value. Pass `--clear` to remove the quota entirely
    /// (the principal becomes unlimited).
    ///
    /// F20: pass `--peer` to set a peer's quota. The `name` is then
    /// interpreted as a peer id and the config is persisted to
    /// `<config_dir>/peers/<peer_id>/peer.toml`.
    Set {
        /// Principal name (or peer id when `--peer` is set)
        name: String,
        /// Input token limit per window (omit to keep current)
        #[arg(long)]
        input: Option<u64>,
        /// Output token limit per window (omit to keep current)
        #[arg(long)]
        output: Option<u64>,
        /// Request count limit per window (omit to keep current)
        #[arg(long)]
        requests: Option<u64>,
        /// Reset cycle: hourly | daily | weekly | monthly
        #[arg(long, value_parser = parse_cycle)]
        cycle: Option<QuotaCycle>,
        /// Remove the quota (principal becomes unlimited)
        #[arg(long)]
        clear: bool,
        /// Set a peer's quota instead of a principal's
        #[arg(long)]
        peer: bool,
    },

    /// Force a fresh quota window (counters → 0).
    ///
    /// F20: pass `--peer` to reset a peer's quota.
    Reset {
        /// Principal name (or peer id when `--peer` is set)
        name: String,
        /// Reset a peer's quota instead of a principal's
        #[arg(long)]
        peer: bool,
    },
}

/// Parse a `QuotaCycle` from a CLI string. Delegates to the
/// canonical `FromStr` impl that lives in `peko-quota` next to the
/// type itself (the orphan rule forbids defining it here, because
/// `QuotaCycle` now belongs to the `peko-quota` crate).
fn parse_cycle(s: &str) -> Result<QuotaCycle, String> {
    QuotaCycle::from_str(s).map_err(|e: &'static str| e.to_string())
}

async fn connect_daemon() -> Result<DaemonClient> {
    DaemonClient::connect()
        .await
        .context("Daemon is not running. Start it with: peko daemon start")
}

/// Top-level dispatcher for `peko quota <sub>`.
pub async fn handle_quota(cmd: QuotaCommands, _paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        QuotaCommands::Status { name, peer } => status(name, peer, json).await,
        QuotaCommands::Set {
            name,
            input,
            output,
            requests,
            cycle,
            clear,
            peer,
        } => set(name, input, output, requests, cycle, clear, peer, json).await,
        QuotaCommands::Reset { name, peer } => reset(name, peer, json).await,
    }
}

async fn status(name: String, is_peer: bool, json: bool) -> Result<()> {
    let client = connect_daemon().await?;
    match client.quota_get(name.clone(), is_peer).await? {
        ResponsePacket::QuotaStatus { state, config, .. } => {
            render_status(&name, &config, &state, json);
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            Err(anyhow::anyhow!("quota status failed: {message}"))
        }
        other => Err(crate::ipc::unexpected_response(&other)),
    }
}

async fn set(
    name: String,
    input: Option<u64>,
    output: Option<u64>,
    requests: Option<u64>,
    cycle: Option<QuotaCycle>,
    clear: bool,
    is_peer: bool,
    json: bool,
) -> Result<()> {
    let client = connect_daemon().await?;

    // Fetch the existing config so omitted flags preserve their
    // current values. `--clear` shortcuts this — the new config is
    // the empty default (unlimited).
    let existing: QuotaConfig = match client.quota_get(name.clone(), is_peer).await? {
        ResponsePacket::QuotaStatus { config, .. } => config,
        ResponsePacket::Error { message, .. } => {
            return Err(anyhow::anyhow!("quota status failed: {message}"));
        }
        other => return Err(crate::ipc::unexpected_response(&other)),
    };

    let new_config = if clear {
        // `--clear` semantics: remove all limits, keep cycle as
        // default. A principal with `quota = None` has no meter
        // wiring at all — but the daemon's `QuotaSet` path is
        // designed to accept any `QuotaConfig`, including the
        // empty one. The `[quota]` block becomes `input_tokens =
        // None, ...` which serialises to absence.
        QuotaConfig::default()
    } else {
        QuotaConfig {
            input_tokens: input.or(existing.input_tokens),
            output_tokens: output.or(existing.output_tokens),
            request_count: requests.or(existing.request_count),
            cycle: cycle.unwrap_or(existing.cycle),
        }
    };

    match client.quota_set(name.clone(), new_config, is_peer).await? {
        ResponsePacket::QuotaStatus { state, config, .. } => {
            if json {
                let snapshot = serde_json::json!({
                    "name": name,
                    "is_peer": is_peer,
                    "config": config,
                    "state": state,
                });
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else if !config.has_any_limit() {
                let target = if is_peer { "peer" } else { "principal" };
                println!("✅ Quota cleared for '{name}' — {target} is now unlimited.");
            } else {
                let target = if is_peer { "peer" } else { "principal" };
                println!("✅ Quota updated for {target} '{name}':");
                render_status(&name, &config, &state, false);
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            Err(anyhow::anyhow!("quota set failed: {message}"))
        }
        other => Err(crate::ipc::unexpected_response(&other)),
    }
}

async fn reset(name: String, is_peer: bool, json: bool) -> Result<()> {
    let client = connect_daemon().await?;
    match client.quota_reset(name.clone(), is_peer).await? {
        ResponsePacket::QuotaStatus { state, config, .. } => {
            if json {
                let snapshot = serde_json::json!({
                    "name": name,
                    "is_peer": is_peer,
                    "config": config,
                    "state": state,
                    "reset": true,
                });
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                let target = if is_peer { "peer" } else { "principal" };
                println!(
                    "✅ Quota reset for {target} '{name}' — fresh window started at {}.",
                    state.window_start.to_rfc3339()
                );
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            Err(anyhow::anyhow!("quota reset failed: {message}"))
        }
        other => Err(crate::ipc::unexpected_response(&other)),
    }
}

/// Pretty-print a single principal's quota status. When `json` is
/// true, callers handle their own JSON output (so we don't double-
/// print), and this function is a no-op.
fn render_status(name: &str, config: &QuotaConfig, state: &QuotaState, json: bool) {
    if json {
        return;
    }
    println!("📊 Quota for '{name}':");
    println!("  cycle:      {}", config.cycle);

    let fmt_limit = |label: &str, limit: Option<u64>, used: u64| match limit {
        Some(n) => println!("  {label:<11} {used:>10} / {n:<10}"),
        None => println!("  {label:<11} {used:>10} / {:<10} (unlimited)", "∞"),
    };
    fmt_limit("input:", config.input_tokens, state.input_tokens);
    fmt_limit("output:", config.output_tokens, state.output_tokens);
    fmt_limit("requests:", config.request_count, state.request_count);

    let end: DateTime<Utc> = state.window_end;
    let start: DateTime<Utc> = state.window_start;
    println!(
        "  window:     {} → {}",
        start.to_rfc3339(),
        end.to_rfc3339()
    );
}

// Re-export the quota state struct for `render_status`'s signature
// — keeps callers from needing a deep path.

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_cycle_accepts_canonical_lowercase_forms() {
        for s in ["hourly", "daily", "weekly", "monthly"] {
            assert!(QuotaCycle::from_str(s).is_ok(), "{s} should parse");
        }
    }

    #[test]
    fn parse_cycle_is_case_insensitive() {
        assert!(matches!(
            QuotaCycle::from_str("DAILY"),
            Ok(QuotaCycle::Daily)
        ));
        assert!(matches!(
            QuotaCycle::from_str("Hourly"),
            Ok(QuotaCycle::Hourly)
        ));
    }

    #[test]
    fn parse_cycle_rejects_unknown_forms() {
        let err = QuotaCycle::from_str("yearly").unwrap_err();
        assert!(
            err.contains("yearly"),
            "error should mention the bad input: {err}"
        );
        assert!(
            err.contains("hourly"),
            "error should list valid options: {err}"
        );
    }

    #[test]
    fn parse_cycle_does_not_match_partial_strings() {
        assert!(QuotaCycle::from_str("day").is_err());
        assert!(QuotaCycle::from_str("dail").is_err());
    }

    // F20: CLI `--peer` flag parses on all three subcommands. We don't
    // exercise the IPC round-trip here (that needs a live daemon) —
    // just verify the clap wiring accepts the flag and flips `peer`
    // to `true`. The `is_peer` field is added by `#[derive(Subcommand)]`
    // — clap's `bool` flag default is `false`.

    fn parse_status(args: &[&str]) -> QuotaCommands {
        let mut full = vec!["peko", "quota"];
        full.extend_from_slice(args);
        let cli = crate::commands::Cli::try_parse_from(full).expect("status should parse");
        let crate::commands::Commands::Quota(cmd) = cli.command else {
            panic!("expected Quota variant");
        };
        cmd
    }

    fn parse_set(args: &[&str]) -> QuotaCommands {
        let mut full = vec!["peko", "quota"];
        full.extend_from_slice(args);
        let cli = crate::commands::Cli::try_parse_from(full).expect("set should parse");
        let crate::commands::Commands::Quota(cmd) = cli.command else {
            panic!("expected Quota variant");
        };
        cmd
    }

    fn parse_reset(args: &[&str]) -> QuotaCommands {
        let mut full = vec!["peko", "quota"];
        full.extend_from_slice(args);
        let cli = crate::commands::Cli::try_parse_from(full).expect("reset should parse");
        let crate::commands::Commands::Quota(cmd) = cli.command else {
            panic!("expected Quota variant");
        };
        cmd
    }

    #[test]
    fn cli_status_default_is_principal() {
        match parse_status(&["status", "alice"]) {
            QuotaCommands::Status { name, peer } => {
                assert_eq!(name, "alice");
                assert!(!peer, "default `peer` must be false");
            }
            _ => panic!("expected Status variant"),
        }
    }

    #[test]
    fn cli_status_with_peer_flag() {
        match parse_status(&["status", "pekohub-user-bob", "--peer"]) {
            QuotaCommands::Status { name, peer } => {
                assert_eq!(name, "pekohub-user-bob");
                assert!(peer, "--peer must set `peer` to true");
            }
            _ => panic!("expected Status variant"),
        }
    }

    #[test]
    fn cli_set_with_peer_flag_emits_is_peer_true_in_packet() {
        // We can't construct a QuotaSet packet from the CLI directly,
        // but we can verify the `peer` field is wired. The actual
        // `is_peer` boolean on the IPC packet is set by the
        // `quota_set` client method (commands/quota.rs handler),
        // not by clap. This test pins the CLI side: when --peer is
        // present, the `peer` field on the parsed subcommand is true.
        match parse_set(&["set", "carol", "--input", "1000", "--peer"]) {
            QuotaCommands::Set {
                name, input, peer, ..
            } => {
                assert_eq!(name, "carol");
                assert_eq!(input, Some(1000));
                assert!(peer);
            }
            _ => panic!("expected Set variant"),
        }
    }

    #[test]
    fn cli_set_default_peer_is_false() {
        match parse_set(&["set", "carol", "--input", "1000"]) {
            QuotaCommands::Set {
                name, input, peer, ..
            } => {
                assert_eq!(name, "carol");
                assert_eq!(input, Some(1000));
                assert!(!peer);
            }
            _ => panic!("expected Set variant"),
        }
    }

    #[test]
    fn cli_reset_with_peer_flag() {
        match parse_reset(&["reset", "pekohub-user-bob", "--peer"]) {
            QuotaCommands::Reset { name, peer } => {
                assert_eq!(name, "pekohub-user-bob");
                assert!(peer);
            }
            _ => panic!("expected Reset variant"),
        }
    }
}
