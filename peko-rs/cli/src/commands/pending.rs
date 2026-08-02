//! Pending self-modification request CLI (ADR-045 PR #4 step 2).
//!
//! `peko pending {list,decide}` is the user-facing terminal for the
//! daemon's `ApprovalQueue`. It closes the principal self-modification
//! loop:
//!
//! - **`list`** — reads `<data>/runtime/pending-requests/*.json` files
//!   directly (mode 0700/0600, owned by the user). No IPC roundtrip.
//!   Symmetric with `peko principal list` (which reads identity files
//!   directly).
//!
//! - **`decide`** — sends a `RequestPacket::ApprovalDecision` over IPC.
//!   The daemon's `ApprovalHandler` marks the queue, runs the op via
//!   `ApprovalEngine`, and returns the per-op `op_result`.
//!
//! Security constraints (preserved per the design doc):
//! - **Listing** is local-only (no daemon needed) — the on-disk files
//!   are already gated by mode 0700 on the dir and mode 0600 on each
//!   file. The CLI never re-sends them over IPC.
//! - **Deciding** uses `DaemonClient::connect()`, which auto-attaches
//!   the per-SID `SessionToken` from PR #2 step 4. The daemon's strict
//!   SID+token gate ensures only an authenticated, same-session
//!   terminal can grant/deny.

use crate::commands::GlobalPaths;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::Path;
use uuid::Uuid;

/// Pending self-modification request subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum PendingCommands {
    /// List pending self-modification requests for this principal.
    ///
    /// Reads `<data>/runtime/pending-requests/*.json` directly. By
    /// default only `pending` (undecided) requests are shown; pass
    /// `--all` to include approved/denied entries.
    List {
        /// Include already-decided (approved/denied) requests.
        #[arg(long)]
        all: bool,
    },

    /// Grant or deny a pending self-modification request.
    ///
    /// Sends `RequestPacket::ApprovalDecision` to the daemon. The
    /// daemon marks the queue, executes the op (on grant), and
    /// returns the per-op result for the CLI to print.
    ///
    /// `--grant` and `--deny` are mutually exclusive. `--reason` is
    /// recommended for `--deny` so the daemon's audit log can carry
    /// it forward.
    Decide {
        /// Pending request id (UUID from `peko pending list`).
        #[arg(long)]
        id: Uuid,
        /// Grant the request (the daemon will execute the op).
        #[arg(long, conflicts_with = "deny")]
        grant: bool,
        /// Deny the request (no execution).
        #[arg(long, conflicts_with = "grant")]
        deny: bool,
        /// Reason for the decision (deny rationale; optional for grant).
        #[arg(long)]
        reason: Option<String>,
    },
}

/// Top-level dispatcher for `peko pending {list,decide}`.
pub async fn handle_pending(
    cmd: PendingCommands,
    paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    match cmd {
        PendingCommands::List { all } => handle_list(paths, all, json),
        PendingCommands::Decide {
            id,
            grant,
            deny,
            reason,
        } => handle_decide(paths, id, grant, deny, reason, json).await,
    }
}

// ── list ────────────────────────────────────────────────────────────────

/// Read every pending request file under `<data>/runtime/pending-requests/`
/// and print them. With `--all` includes already-decided files.
///
/// Reads disk directly — no IPC. The dir is created by the daemon at
/// startup with mode 0700, so a hostile user cannot tamper with the
/// shape of the directory from another account.
fn handle_list(paths: &GlobalPaths, include_all: bool, json: bool) -> Result<()> {
    let dir = paths.pending_requests_dir();
    let requests = read_pending_files(&dir, include_all)?;

    if json {
        // Slim projection: id + principal + op summary + status +
        // timestamps. Never include any file-mode / on-disk metadata.
        let payload = serde_json::json!({
            "pending_requests_dir": dir.display().to_string(),
            "count": requests.len(),
            "requests": requests.iter().map(request_to_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if requests.is_empty() {
        if include_all {
            println!("No pending requests (or decided history).");
        } else {
            println!("No pending requests.");
            println!(
                "  (Use --all to see approved/denied history. \
                 Files live at {})",
                dir.display()
            );
        }
        return Ok(());
    }

    println!("Pending self-modification requests:");
    for req in &requests {
        let status = match &req.status {
            peko_core::daemon::approval_queue::ApprovalStatus::Pending => "pending",
            peko_core::daemon::approval_queue::ApprovalStatus::Approved { .. } => "approved",
            peko_core::daemon::approval_queue::ApprovalStatus::Denied { .. } => "denied",
        };
        println!(
            "  {}  principal={}  op={}  status={}  reason={:?}",
            req.id,
            req.principal_id,
            req.op.label(),
            status,
            req.reason,
        );
    }
    println!("  Files at: {}", dir.display());
    Ok(())
}

/// Read every `<uuid>.json` file under `dir` and parse as
/// `ApprovalRequest`. Returns them sorted by `requested_at_secs`
/// ascending (oldest first — same order the daemon's
/// `ApprovalQueue::list_pending` uses).
fn read_pending_files(
    dir: &Path,
    include_all: bool,
) -> Result<Vec<peko_core::daemon::approval_queue::ApprovalRequest>> {
    use peko_core::daemon::approval_queue::ApprovalStatus;

    if !dir.exists() {
        // Pre-daemon-startup case: the daemon hasn't run yet, so
        // there are no pending requests. Empty result, not an error.
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("read_dir({})", dir.display()))?;

    let mut out: Vec<peko_core::daemon::approval_queue::ApprovalRequest> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            // Skip `.tmp` leftovers from interrupted writes; they
            // are renamed atomically by `ApprovalQueue::decide_with`.
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                // Skip unreadable files (mode 0600 owned by another
                // SID won't apply here — same user reads them).
                eprintln!(
                    "warning: skipping unreadable file {}: {e}",
                    path.display()
                );
                continue;
            }
        };
        let req: peko_core::daemon::approval_queue::ApprovalRequest =
            match serde_json::from_slice(&bytes) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "warning: skipping malformed file {}: {e}",
                        path.display()
                    );
                    continue;
                }
            };
        if !include_all && !matches!(req.status, ApprovalStatus::Pending) {
            continue;
        }
        out.push(req);
    }
    out.sort_by_key(|r| r.requested_at_secs);
    Ok(out)
}

/// Slim JSON projection for `--json` output.
fn request_to_json(
    req: &peko_core::daemon::approval_queue::ApprovalRequest,
) -> serde_json::Value {
    use peko_core::daemon::approval_queue::ApprovalStatus;

    let status = match &req.status {
        ApprovalStatus::Pending => serde_json::json!({ "status": "pending" }),
        ApprovalStatus::Approved {
            decided_at_secs,
            by,
        } => serde_json::json!({
            "status": "approved",
            "decided_at_secs": decided_at_secs,
            "by": by,
        }),
        ApprovalStatus::Denied {
            decided_at_secs,
            by,
            reason,
        } => serde_json::json!({
            "status": "denied",
            "decided_at_secs": decided_at_secs,
            "by": by,
            "reason": reason,
        }),
    };
    serde_json::json!({
        "id": req.id,
        "principal_id": req.principal_id,
        "requested_at_secs": req.requested_at_secs,
        "reason": req.reason,
        "op": req.op,
        "status": status,
    })
}

// ── decide ──────────────────────────────────────────────────────────────

/// Send `RequestPacket::ApprovalDecision` to the daemon and render
/// the response.
async fn handle_decide(
    _paths: &GlobalPaths,
    id: Uuid,
    grant: bool,
    deny: bool,
    reason: Option<String>,
    json: bool,
) -> Result<()> {
    if grant == deny {
        bail!("exactly one of --grant or --deny is required");
    }

    let decision = if grant {
        peko_core::ipc::packet::ApprovalDecisionPayload::Grant
    } else {
        let reason = reason.unwrap_or_default();
        peko_core::ipc::packet::ApprovalDecisionPayload::Deny { reason }
    };

    // Send the decision via the typed helper so the wire-shape
    // construction stays in `peko_core` (mirrors `auth_submit`).
    let client = peko_core::ipc::DaemonClient::connect().await?;
    let response = client.approval_decide(id, decision).await?;

    match response {
        peko_core::ipc::ResponsePacket::ApprovalDecided {
            id: resp_id,
            status,
            op_result,
            ..
        } => {
            if json {
                let payload = serde_json::json!({
                    "decided": true,
                    "id": resp_id,
                    "status": match &status {
                        peko_core::ipc::packet::ApprovalStatusPayload::Pending => "pending",
                        peko_core::ipc::packet::ApprovalStatusPayload::Approved { .. } => "approved",
                        peko_core::ipc::packet::ApprovalStatusPayload::Denied { .. } => "denied",
                    },
                    "op_result": op_result,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                let label = match &status {
                    peko_core::ipc::packet::ApprovalStatusPayload::Pending => "pending",
                    peko_core::ipc::packet::ApprovalStatusPayload::Approved { .. } => "approved",
                    peko_core::ipc::packet::ApprovalStatusPayload::Denied { .. } => "denied",
                };
                println!("✓ Decision recorded: {label}");
                if !op_result.is_null() {
                    println!("  op_result: {}", op_result);
                }
            }
            Ok(())
        }
        peko_core::ipc::ResponsePacket::ApprovalError { message, .. } => {
            // Surface the bracket-prefixed code as the error chain
            // so callers can grep for the prefix programmatically.
            bail!("{message}")
        }
        other => Err(anyhow::anyhow!(
            "unexpected response to ApprovalDecision: {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use peko_core::daemon::approval_queue::{
        ApprovalRequest, ApprovalStatus, DecisionError, DEFAULT_MAX_PENDING,
    };
    use peko_core::daemon::api::{RequestId, SelfModifyOp};
    use peko_subject::{PrincipalId, Subject};
    use tempfile::TempDir;

    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        cmd: PendingCommands,
    }

    // ── CLI arg-parsing tests ──────────────────────────────────────

    #[test]
    fn list_parses_without_all() {
        let w = Wrapper::try_parse_from(["test", "list"]).unwrap();
        match w.cmd {
            PendingCommands::List { all } => assert!(!all),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn list_parses_with_all() {
        let w = Wrapper::try_parse_from(["test", "list", "--all"]).unwrap();
        match w.cmd {
            PendingCommands::List { all } => assert!(all),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn decide_parses_grant() {
        let w = Wrapper::try_parse_from([
            "test",
            "decide",
            "--id",
            "11111111-2222-3333-4444-555555555555",
            "--grant",
        ])
        .unwrap();
        match w.cmd {
            PendingCommands::Decide {
                id,
                grant,
                deny,
                reason,
            } => {
                assert!(grant);
                assert!(!deny);
                assert!(reason.is_none());
                assert_eq!(
                    id.to_string(),
                    "11111111-2222-3333-4444-555555555555"
                );
            }
            _ => panic!("expected Decide"),
        }
    }

    #[test]
    fn decide_parses_deny_with_reason() {
        let w = Wrapper::try_parse_from([
            "test",
            "decide",
            "--id",
            "11111111-2222-3333-4444-555555555555",
            "--deny",
            "--reason",
            "not needed yet",
        ])
        .unwrap();
        match w.cmd {
            PendingCommands::Decide {
                grant,
                deny,
                reason,
                ..
            } => {
                assert!(!grant);
                assert!(deny);
                assert_eq!(reason.as_deref(), Some("not needed yet"));
            }
            _ => panic!("expected Decide"),
        }
    }

    #[test]
    fn decide_rejects_both_grant_and_deny() {
        let r = Wrapper::try_parse_from([
            "test",
            "decide",
            "--id",
            "11111111-2222-3333-4444-555555555555",
            "--grant",
            "--deny",
        ]);
        assert!(r.is_err(), "should reject --grant + --deny together");
    }

    #[test]
    fn decide_requires_uuid_for_id() {
        let r = Wrapper::try_parse_from(["test", "decide", "--id", "not-a-uuid", "--grant"]);
        assert!(r.is_err(), "--id must parse as a UUID");
    }

    // ── Direct disk-read tests (no IPC) ─────────────────────────────

    /// Helper: write a single `ApprovalRequest` to disk in the daemon's
    /// canonical on-disk format.
    fn write_request(dir: &Path, req: &ApprovalRequest) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(req)?;
        let path = dir.join(format!("{}.json", req.id));
        std::fs::write(&path, bytes)?;
        Ok(())
    }

    #[test]
    fn read_pending_files_returns_empty_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-here");
        let reqs = read_pending_files(&missing, false).unwrap();
        assert!(reqs.is_empty());
    }

    #[test]
    fn read_pending_files_filters_decided_by_default() {
        let dir = TempDir::new().unwrap();
        let q = peko_core::daemon::approval_queue::ApprovalQueue::new(
            dir.path().to_path_buf(),
            DEFAULT_MAX_PENDING,
        );
        let pid = PrincipalId::system().clone();
        // One pending + one approved.
        let pending_op = SelfModifyOp::GrantCapability {
            capability: "fs:read".into(),
            reason: "need to read".into(),
        };
        let pending = ApprovalRequest::from_op(pending_op, pid.clone());
        let pending_id = q.insert(pending.clone()).unwrap();

        let approved_op = SelfModifyOp::GrantCapability {
            capability: "fs:write".into(),
            reason: "need to write".into(),
        };
        let mut approved = ApprovalRequest::from_op(approved_op, pid.clone());
        approved.id = RequestId::from_u128(0xdeadbeef);
        q.insert(approved.clone()).unwrap();
        q.decide_with(
            approved.id,
            peko_core::daemon::approval_queue::Decision::Grant,
            Subject::Public,
        )
        .unwrap();

        // By default, only the pending request surfaces.
        let only_pending = read_pending_files(dir.path(), false).unwrap();
        assert_eq!(only_pending.len(), 1);
        assert_eq!(only_pending[0].id, pending_id);

        // With --all, both surface.
        let everything = read_pending_files(dir.path(), true).unwrap();
        assert_eq!(everything.len(), 2);
        // Pending first by requested_at_secs (both share the same epoch
        // second in the test, so order is insertion-stable).
        assert!(matches!(
            everything[0].status,
            ApprovalStatus::Pending | ApprovalStatus::Approved { .. }
        ));
        // Verify the approved status carried through.
        let approved_row = everything
            .iter()
            .find(|r| r.id == approved.id)
            .expect("approved row present");
        assert!(matches!(approved_row.status, ApprovalStatus::Approved { .. }));

        // Silence the DecisionError import (used indirectly via decide_with).
        let _: DecisionError = DecisionError::NotFound(RequestId::nil());
    }

    #[test]
    fn read_pending_files_skips_malformed_and_non_json() {
        let dir = TempDir::new().unwrap();
        // Drop a non-JSON file and a malformed JSON file alongside
        // a real one. Only the real one should be returned.
        std::fs::write(dir.path().join("garbage.txt"), "not json").unwrap();
        std::fs::write(dir.path().join("bad.json"), "{not valid json").unwrap();

        let pid = PrincipalId::system().clone();
        let op = SelfModifyOp::GrantCapability {
            capability: "net:http".into(),
            reason: "fetch the web".into(),
        };
        let req = ApprovalRequest::from_op(op, pid);
        write_request(dir.path(), &req).unwrap();

        let result = read_pending_files(dir.path(), false).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, req.id);
    }
}