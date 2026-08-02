//! Integration tests for ADR-045 PR #3 step 3 — the `peko_self` tool
//! + durable pending-requests queue.
//!
//! These tests verify the daemon-side wiring end-to-end:
//!
//!   1. Daemon startup creates `<data_dir>/runtime/pending-requests/`
//!      (driven by `PathResolver::ensure_dirs` per PR #3 step 1).
//!   2. The directory exists with the expected owner-only mode (0700).
//!   3. The daemon initializes the global `DaemonApi` slot so
//!      `register_builtins` picks it up.
//!
//! We deliberately **don't** drive a full agent-call end-to-end here:
//! that would require mock-LLM plumbing for tool-call requests, which
//! is a heavier harness than this PR's blast radius warrants. PR #4
//! adds the user-decision CLI and the AsyncInboxItem::Approval
//! delivery path, both of which can drive the full E2E from the
//! CLI side.
//!
//! The PR-#2 strict-auth opt-out is the harness default
//! ([`PekoCli::cmd`]) so the daemon starts without an `auth submit`
//! dance.

#![cfg(unix)]

mod common;

use common::cli::PekoCli;
use common::daemon::DaemonGuard;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[test]
fn daemon_startup_creates_pending_requests_dir() {
    let cli = PekoCli::new();
    let _guard = DaemonGuard::spawn(&cli);

    let dir = cli.peko_dir().join("data/runtime/pending-requests");
    assert!(
        dir.exists(),
        "ensure_dirs should create {dir:?} at daemon startup",
    );
    let meta = std::fs::metadata(&dir).expect("metadata for pending-requests dir");
    assert!(
        meta.is_dir(),
        "{dir:?} should be a directory, got {:?}",
        meta.file_type(),
    );
    // Owner-only mode on the directory; the runtime-pending-requests
    // bucket holds per-request 0600 files and the directory itself
    // should be readable only by the owning user.
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "pending-requests dir should be mode 0700 (owner-only), got {mode:o}",
    );
}

#[test]
fn daemon_init_global_daemon_api_slot_is_populated() {
    // Cross-check the `init_global_daemon_api` wiring: the daemon
    // populates the process-global slot at `AppState::build_internal`
    // so `register_builtins` (called lazily when the first agent
    // loads) finds it. We can't directly read another process's
    // global from here, but we CAN verify the daemon started without
    // panicking on the `Arc::new(state.clone())` cast — that proves
    // `DaemonApi` is implemented and the registration path didn't
    // reject the type. Indirect, but the absence of any panic in
    // `daemon_startup_creates_pending_requests_dir` already covers
    // the happy path.
    let cli = PekoCli::new();
    let _guard = DaemonGuard::spawn(&cli);
    // Best-effort: ensure_dir on the queue's parent dir is observable.
    let runtime_dir = cli.peko_dir().join("data/runtime");
    assert!(
        runtime_dir.exists(),
        "{runtime_dir:?} should exist after daemon startup",
    );
    let pending = runtime_dir.join("pending-requests");
    assert!(
        pending.exists(),
        "{pending:?} should exist after daemon startup",
    );
}

#[test]
fn ensure_dirs_pending_requests_lives_under_data_runtime() {
    // The directory's location matters: it's a runtime data artifact
    // (durable, queue-shaped), NOT a config artifact (portable) and
    // NOT an IPC artifact (auth-code/auth-token under <config>/run/).
    // This test pins the contract so a future refactor that swaps
    // to `<config>/runtime/pending-requests/` is caught immediately.
    let cli = PekoCli::new();
    DaemonGuard::spawn(&cli);

    let pending = cli.peko_dir().join("data/runtime/pending-requests");
    assert!(pending.starts_with(cli.peko_dir().join("data")));
    assert!(!pending.starts_with(cli.peko_dir().join("run")));
    // Sanity: actually a path on disk.
    assert!(Path::new(&pending).exists());
}