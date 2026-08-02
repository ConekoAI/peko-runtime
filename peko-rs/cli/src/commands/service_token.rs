//! Service-token CLI (ADR-045 PR #5 step 2).
//!
//! `peko service-token {create,list,revoke}` is the user-facing
//! terminal for the daemon's named, persistent service-token store.
//! Mirrors the shape of `peko auth {submit,status}` and
//! `peko pending {list,decide}`:
//!
//! - **`list`** — roundtrips via IPC to enumerate every registered
//!   token's metadata (never the raw secret).
//! - **`create`** — generates a fresh 32-byte token, prints the
//!   raw secret **exactly once**, and gives the caller the
//!   instructions to hand it to their long-lived process.
//! - **`revoke`** — deletes the on-disk artifacts and clears the
//!   in-memory cache.
//!
//! Security constraints (preserved per ADR-045):
//! - All three subcommands reach the daemon via `DaemonClient::connect()`
//!   which auto-attaches the per-SID `SessionToken` from PR #2 step 4.
//!   The daemon's strict SID+token gate ensures only an authenticated,
//!   same-session terminal can manage tokens.
//! - The raw token is shown once at create time and never persisted by
//!   the CLI. The on-disk store at `<data>/runtime/service.tokens/<name>/token`
//!   holds it for the caller's reference, separate from the
//!   mode-0600 `meta.json`.
//! - Token names are validated server-side; the CLI never constructs
//!   a name that would survive `ServiceTokenStore::validate_name`.

use crate::commands::GlobalPaths;
use anyhow::{bail, Result};
use clap::Subcommand;

/// Service-token CRUD subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ServiceTokenCommands {
    /// Create a new named, persistent service token.
    ///
    /// The raw token is printed **exactly once** on success. Pass
    /// the token to your long-lived process (runtime, persistent
    /// agent, external script) via
    /// `DaemonClient::connect_with_service_token_v2`.
    ///
    /// `--caps` is a comma-separated list (e.g. `fs:read,tool:Bash`).
    /// Per ADR-045 the capability set is **immutable** at creation —
    /// to change caps, revoke + recreate.
    ///
    /// `--expires-in` is an optional relative TTL in seconds.
    /// `None` means no expiry.
    Create {
        /// Unique token name (CLI identifier; cannot contain `/`,
        /// `.`, `..`, or NUL; ≤ 64 chars).
        #[arg(long)]
        name: String,
        /// Comma-separated capability list (e.g. `fs:read,tool:Bash`).
        #[arg(long, value_delimiter = ',')]
        caps: Vec<String>,
        /// Optional relative TTL in seconds.
        #[arg(long)]
        expires_in: Option<u64>,
    },

    /// List every registered service token's metadata.
    ///
    /// Output never includes the raw secret. Use `--json` for a
    /// machine-readable envelope.
    List {},

    /// Revoke a named service token.
    ///
    /// Removes the on-disk directory and clears the in-memory cache
    /// entry. Idempotent: revoking a non-existent token is a no-op
    /// success.
    Revoke {
        /// Token name to revoke.
        #[arg(long)]
        name: String,
    },
}

/// Top-level dispatcher for `peko service-token {create,list,revoke}`.
pub async fn handle_service_token(
    cmd: ServiceTokenCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    match cmd {
        ServiceTokenCommands::Create {
            name,
            caps,
            expires_in,
        } => handle_create(name, caps, expires_in, json).await,
        ServiceTokenCommands::List {} => handle_list(json).await,
        ServiceTokenCommands::Revoke { name } => handle_revoke(name, json).await,
    }
}

// ── create ──────────────────────────────────────────────────────────────

async fn handle_create(
    name: String,
    caps: Vec<String>,
    expires_in: Option<u64>,
    json: bool,
) -> Result<()> {
    if caps.is_empty() {
        bail!("--caps must contain at least one capability");
    }

    let client = peko_core::ipc::DaemonClient::connect().await?;
    let response = client
        .service_token_create(&name, caps.clone(), expires_in)
        .await?;

    match response {
        peko_core::ipc::ResponsePacket::ServiceTokenCreated {
            name: resp_name,
            token,
            caps: resp_caps,
            expires_at_secs,
            ..
        } => {
            if json {
                let payload = serde_json::json!({
                    "created": true,
                    "name": resp_name,
                    "token": token,
                    "caps": resp_caps,
                    "expires_at_secs": expires_at_secs,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("✓ Service token created");
                println!("  name:               {resp_name}");
                println!("  capabilities:       {}", resp_caps.join(", "));
                match expires_at_secs {
                    Some(secs) => println!("  expires_at_secs:    {secs}"),
                    None => println!("  expires_at_secs:    (none — never expires)"),
                }
                println!();
                // ADR-045: shown ONCE. Print on its own line so it
                // can be copy-pasted cleanly.
                println!("  token (shown once): {token}");
                println!();
                println!(
                    "  Store this token somewhere safe. To use it from a long-lived\n\
                     \x20 process, attach via:\n\
                     \x20   DaemonClient::connect_with_service_token_v2(\"{token}\")\n\
                     \x20 Persisted at:\n\
                     \x20   <data_dir>/runtime/service.tokens/{resp_name}/{{meta.json,token}}"
                );
            }
            Ok(())
        }
        peko_core::ipc::ResponsePacket::ServiceTokenError { message, .. } => {
            bail!(message)
        }
        other => Err(anyhow::anyhow!(
            "unexpected response to ServiceTokenCreate: {other:?}"
        )),
    }
}

// ── list ────────────────────────────────────────────────────────────────

async fn handle_list(json: bool) -> Result<()> {
    let client = peko_core::ipc::DaemonClient::connect().await?;
    let response = client.service_token_list().await?;

    let tokens = match response {
        peko_core::ipc::ResponsePacket::ServiceTokenListed { tokens, .. } => tokens,
        peko_core::ipc::ResponsePacket::ServiceTokenError { message, .. } => {
            bail!(message)
        }
        other => {
            return Err(anyhow::anyhow!(
                "unexpected response to ServiceTokenList: {other:?}"
            ))
        }
    };

    if json {
        let payload = serde_json::json!({
            "count": tokens.len(),
            "tokens": tokens.iter().map(|t| serde_json::json!({
                "name": t.name,
                "caps": t.caps,
                "created_at_secs": t.created_at_secs,
                "expires_at_secs": t.expires_at_secs,
                "last_used_at_secs": t.last_used_at_secs,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if tokens.is_empty() {
        println!("No service tokens registered.");
        println!("  Use `peko service-token create --name <name> --caps <csv>` to add one.");
        return Ok(());
    }

    println!("Registered service tokens ({}):", tokens.len());
    for t in &tokens {
        let expires = match t.expires_at_secs {
            Some(secs) => format!("{secs}"),
            None => "(none)".to_string(),
        };
        let last_used = match t.last_used_at_secs {
            Some(secs) => format!("{secs}"),
            None => "(never)".to_string(),
        };
        println!(
            "  - {}  caps={}  created={}  expires={}  last_used={}",
            t.name,
            t.caps.join(","),
            t.created_at_secs,
            expires,
            last_used,
        );
    }
    Ok(())
}

// ── revoke ──────────────────────────────────────────────────────────────

async fn handle_revoke(name: String, json: bool) -> Result<()> {
    let client = peko_core::ipc::DaemonClient::connect().await?;
    let response = client.service_token_revoke(&name).await?;

    match response {
        peko_core::ipc::ResponsePacket::ServiceTokenRevoked {
            name: resp_name, ..
        } => {
            if json {
                let payload = serde_json::json!({
                    "revoked": true,
                    "name": resp_name,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("✓ Service token revoked: {resp_name}");
            }
            Ok(())
        }
        peko_core::ipc::ResponsePacket::ServiceTokenError { message, .. } => {
            bail!(message)
        }
        other => Err(anyhow::anyhow!(
            "unexpected response to ServiceTokenRevoke: {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        cmd: ServiceTokenCommands,
    }

    #[test]
    fn create_parses_minimal_args() {
        let w = Wrapper::try_parse_from([
            "test",
            "create",
            "--name",
            "runtime",
            "--caps",
            "fs:read,tool:Bash",
        ])
        .unwrap();
        match w.cmd {
            ServiceTokenCommands::Create {
                name,
                caps,
                expires_in,
            } => {
                assert_eq!(name, "runtime");
                assert_eq!(caps, vec!["fs:read", "tool:Bash"]);
                assert!(expires_in.is_none());
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn create_parses_with_expires_in() {
        let w = Wrapper::try_parse_from([
            "test",
            "create",
            "--name",
            "rt",
            "--caps",
            "fs:read",
            "--expires-in",
            "3600",
        ])
        .unwrap();
        match w.cmd {
            ServiceTokenCommands::Create {
                name,
                caps,
                expires_in,
            } => {
                assert_eq!(name, "rt");
                assert_eq!(caps, vec!["fs:read"]);
                assert_eq!(expires_in, Some(3600));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn list_parses() {
        let w = Wrapper::try_parse_from(["test", "list"]).unwrap();
        assert!(matches!(w.cmd, ServiceTokenCommands::List {}));
    }

    #[test]
    fn revoke_parses() {
        let w = Wrapper::try_parse_from(["test", "revoke", "--name", "rt"]).unwrap();
        match w.cmd {
            ServiceTokenCommands::Revoke { name } => assert_eq!(name, "rt"),
            _ => panic!("expected Revoke"),
        }
    }

    #[test]
    fn create_requires_name() {
        let r = Wrapper::try_parse_from(["test", "create", "--caps", "fs:read"]);
        assert!(r.is_err(), "--name is required");
    }

    #[test]
    fn create_with_empty_caps_is_rejected_at_runtime() {
        // clap accepts `--caps` (zero values) — the empty-caps
        // rejection is enforced by `handle_create` so the error
        // carries a clear message rather than a clap-derive error.
        let w = Wrapper::try_parse_from(["test", "create", "--name", "rt"]).unwrap();
        match w.cmd {
            ServiceTokenCommands::Create { caps, .. } => assert!(caps.is_empty()),
            _ => panic!("expected Create"),
        }
    }
}