//! Persistent storage primitives (ADR-045 PR #5).
//!
//! The `storage` module owns on-disk artifacts that are user-created
//! (vs. daemon-internal state, which lives under `daemon/`). The
//! first inhabitant is [`service_token_store`] — named, persistent
//! service tokens for long-lived daemon clients (runtime, cron,
//! persistent agents, external scripts).
//!
//! ## Why a new module
//!
//! `daemon::approval_queue` is the closest analog (temp+rename,
//! mode 0600, per-uuid file) but lives under `daemon/` because it is
//! runtime-internal state. Service tokens are **user-created** via
//! `peko service-token create` and persist across daemon restarts —
//! they belong alongside other user-managed data files. Splitting
//! them out keeps `daemon/` focused on the runtime's own bookkeeping.
//!
//! ## Conventions
//!
//! - All file writes use the **temp+rename** pattern (atomic on the
//!   same filesystem; never leave a torn file).
//! - All artifact files are written mode `0600`; the bucket directory
//!   itself is mode `0700` (mirrors `pending_requests_dir`).
//! - The bucket lives under `{data_dir}/runtime/<plural>/`, set up by
//!   `PathResolver::ensure_dirs` so daemon startup is a no-op when
//!   the bucket is fresh.

pub mod service_token_store;

pub use service_token_store::{ServiceTokenInfo, ServiceTokenStore};