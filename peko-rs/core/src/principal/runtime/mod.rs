//! Runtime contracts and shared data types used by the principal layer.
//!
//! Phase 14.c.2a introduced two runtime types here:
//!
//! * [`OutputFormat`] — the human/JSON preference flag used by slash
//!   commands and IPC responses. **Retired post-slash-removal**: the
//!   only consumer was `/help` and the IPC `no_slash` / `output_format`
//!   fields that paired with it.
//! * [`builtin_tools`] — the canonical list of built-in tool names
//!   (global + agent-specific). Restored to
//!   `crate::extensions::framework::adapters::builtin_tools` (its
//!   pre-Phase-14.c.2a home) to keep the framework free of
//!   principal imports; re-exported here for
//!   [`super::catalog::PrincipalCatalog::build`] and other
//!   principal-layer callers.

pub use crate::extensions::framework::adapters::builtin_tools;
