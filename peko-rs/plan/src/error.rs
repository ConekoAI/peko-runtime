//! Error type for `peko-plan`.
//!
//! `PlanError` is the boundary error returned by [`PlanStorage`](crate::PlanStorage)
//! methods. The crate does not use `thiserror` to stay consistent with `peko-cron`
//! and `peko-session`, which both propagate via `anyhow::Result` at higher
//! layers and reserve structured enums for the storage/IO boundary.
//!
//! Top-level callers should bubble errors with `?`; callers that want a
//! specific kind should match on the variant.

use std::fmt;

/// The single error type for the storage layer.
#[derive(Debug)]
pub enum PlanError {
    /// Read targeted a plan id that does not exist.
    NotFound,
    /// `close` was called twice on the same plan. `close` is idempotent in
    /// the sense that calling it on an already-closed plan is a no-op for
    /// state, but it is *non-idempotent for return value*: the second
    /// call returns `AlreadyClosed` so callers can detect concurrent close
    /// races.
    AlreadyClosed,
    /// `get_for_principal` / `update` / `close` were called with a
    /// `PrincipalId` that does not match the on-disk record's
    /// `principal_id`. This is a corruption signal, not a permission
    /// signal — the storage layer does not authorize reads.
    PrincipalMismatch {
        expected: String,
        got: String,
    },
    /// A `*.jsonl` file failed to deserialize as a `PlanRecord`. Unlike
    /// `peko-session::TodoStorage` (which silently drops corrupt lines on
    /// an append-only stream), plan storage refuses the read — a single
    /// full-record file is either the record or corruption. Use
    /// [`PlanStorage::list_corrupt`](crate::PlanStorage::list_corrupt)
    /// to enumerate unreadable files without parsing them.
    CorruptRecord {
        plan_id: String,
        source: serde_json::Error,
    },
    /// `NodeId::parse` rejected a string because it didn't match the
    /// `node_<8 base36>` convention.
    InvalidNodeId(String),
    /// Filesystem failure during a read or write. The inner source is
    /// preserved for diagnostics.
    Io(anyhow::Error),
}

/// Result alias for `PlanError`-returning operations.
pub type Result<T> = std::result::Result<T, PlanError>;

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NotFound => write!(f, "plan not found"),
            PlanError::AlreadyClosed => write!(f, "plan is already closed"),
            PlanError::PrincipalMismatch { expected, got } => write!(
                f,
                "plan principal_id mismatch: expected={expected}, got={got}"
            ),
            PlanError::CorruptRecord { plan_id, source } => {
                write!(f, "plan {plan_id} record is corrupt: {source}")
            }
            PlanError::InvalidNodeId(s) => write!(f, "invalid node id: {s}"),
            PlanError::Io(e) => write!(f, "plan storage io error: {e}"),
        }
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PlanError::CorruptRecord { source, .. } => Some(source),
            PlanError::Io(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for PlanError {
    fn from(e: serde_json::Error) -> Self {
        // Construct a CorruptRecord with an empty plan_id; the storage
        // layer overrides this with the real plan_id at the call site.
        PlanError::CorruptRecord {
            plan_id: String::new(),
            source: e,
        }
    }
}

impl From<std::io::Error> for PlanError {
    fn from(e: std::io::Error) -> Self {
        PlanError::Io(anyhow::Error::new(e))
    }
}

impl From<anyhow::Error> for PlanError {
    fn from(e: anyhow::Error) -> Self {
        PlanError::Io(e)
    }
}
