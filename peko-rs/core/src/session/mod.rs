//! Root-side session adapters.
//!
//! Phase 7 lifted all session persistence + types into `peko-session`.
//! What remains here is the **composition-layer glue** that wires the
//! workspace crates into root's `SessionManager` / `TodoStorage` so
//! the lifted built-in tools can speak to a port trait instead of
//! importing root internals.
//!
//! # Crate boundary
//!
//! This module is intentionally tiny. It does NOT re-export anything
//! from `peko_session` — callers in root should import directly from
//! `peko_session::*`. The only items that live here are the runtime
//! adapters the lifted tools need:
//!
//! - [`session_runtime_impl`] — `SessionManagerRuntime` impl of the
//!   `SessionRuntime` port trait (consumed by `peko_tools_builtin::session`).
//! - [`todo_runtime_impl`] — `TodoStorageRuntime` impl of the
//!   `TodoRuntime` port trait (consumed by `peko_tools_builtin::tasks`).
//! - [`ownership`] — caller classification + guard refusals for
//!   agent-owned session management (plan D4/D5), shared by the
//!   session-tool adapter and the Agent-tool spawn path.

pub mod ownership;
pub mod session_runtime_impl;
pub mod todo_runtime_impl;
