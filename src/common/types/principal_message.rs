//! Principal-level message request/response types and the
//! `PrincipalMessageService` trait (Phase 8 commit 2 re-export shim).
//!
//! The trait and its data types moved to
//! `peko_extension_host::principal_message::*` in Phase 8 commit 2
//! (host crate). This file is now a thin re-export shim that keeps
//! every `crate::common::types::principal_message::*` path
//! compiling until Phase 10 deletes it.
//!
//! See `peko_extension_host::principal_message` for the moved-in
//! definitions and the long-form docs.

pub use peko_extension_host::principal_message::*;
