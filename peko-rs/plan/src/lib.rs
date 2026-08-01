//! `peko-plan` — file-backed Plan DAG primitive for the principal harness.
//!
//! v1 scope: durable on-disk schema ([`PlanRecord`], [`PlanNode`],
//! [`PlanNodeStatus`]) + atomic JSONL-per-plan storage ([`PlanStorage`])
//! with [`peko_fs_persistence::FileLock`]-coordinated writes.
//!
//! ## Crate boundary
//!
//! What lives here:
//! - The serializable record types ([`schema::PlanRecord`], [`schema::PlanNode`]).
//! - A file-per-record storage layer ([`storage::PlanStorage`]).
//! - The boundary error type ([`error::PlanError`]).
//! - One typed id newtype ([`schema::NodeId`]) for in-plan references.
//!
//! What does not live here (deferred to follow-on PRs):
//! - `ContextInjectionKind::Plan` extension in `peko-core` (PR #3).
//! - Resume-on-session-start re-injection (PR #4).
//!
//! What lives here as of PR #2 (shipped 2026-07-31):
//! - The 7 `Plan*` tool handlers are NOT in this crate — they live
//!   in `peko-rs/core/src/tools/builtin/plan/` and reach the storage
//!   via the [`PlanPort`] trait below. This crate stays pure data
//!   + storage; the tool surface is root-side.
//!
//! The [`PlanPort`] trait + impl-on-`PlanStorage` lives here now (PR #1 of
//! the wiring sequence). The trait is the boundary `peko-rs/core` consumes
//! via `Arc<dyn PlanPort>` held on the `Principal` struct.
//!
//! ## Forbidden deps
//!
//! This crate must not depend on:
//! - `peko-engine` — would invert the storage/contract direction.
//! - `peko-session` — would couple plan storage to session storage;
//!   the on-disk shape is intentionally separate.
//! - `peko-core` — the root crate is a thin composition layer; leaf
//!   crates don't reach into it.
//! - `peko-protocol` — wire-only contract; storage is local.
//! - `peko-tools-core` — tool API is separate from storage.
//! - Any `peko-extension-*` crate.
//!
//! Allowed deps:
//! - `peko-fs-persistence` — for [`FileLock`]; storage mechanism only.
//! - `peko-subject` — for [`peko_subject::PrincipalId`] identity.
//!
//! ## On-disk layout
//!
//! ```text
//! <plans_dir>/
//!   <plan_id>.jsonl         # exactly one record, terminated `\n`
//!   <plan_id>.lock          # sibling; created/destroyed by FileLock
//!   <plan_id>.jsonl.<pid>.tmp   # transient during write
//! ```
//!
//! `<plans_dir>` is supplied by callers — the principal harness resolves
//! `<principal_root>/plans/` and hands the path to [`PlanStorage::new`].
//! This crate does not own the directory's lifecycle.

pub mod error;
pub mod plan_port;
pub mod schema;
pub mod storage;

pub use error::{PlanError, Result};
pub use plan_port::PlanPort;
pub use schema::{
    ClosedState, NodeEvidence, NodeId, PlanNode, PlanNodeStatus, PlanRecord,
};
pub use storage::{PlanStorage, PLAN_LOCK_TIMEOUT_MS};

// `PLAN_SCHEMA_VERSION` re-exported alongside the storage const for
// ergonomic imports.
pub use schema::PLAN_SCHEMA_VERSION;

// `peko_subject::PrincipalId` is the canonical principal identity type
// across the workspace. Re-export it under our crate root so callers
// don't need to reach into `peko_subject` just to construct a plan.
pub use peko_subject::PrincipalId;
