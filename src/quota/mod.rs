//! Compatibility re-exports for the `peko-quota` workspace crate.
//!
//! The full quota domain (F18–F20) — `QuotaConfig`, `QuotaCycle`,
//! `QuotaState`, `QuotaMeter`, `QuotaScope`, `QuotaError` — lives in
//! the `peko-quota` crate as one coherent unit. Internal consumers
//! keep the historical `peko::quota::*` import paths through these
//! shim modules.

// Re-export each submodule so existing `peko::quota::config::*`,
// `peko::quota::meter::*`, etc. paths keep resolving. The actual
// implementations live in the `peko_quota` crate.
pub use peko_quota::config;
pub use peko_quota::error;
pub use peko_quota::meter;
pub use peko_quota::scope;
pub use peko_quota::state;

// Re-export the top-level types so `peko::quota::QuotaMeter` and the
// fully-qualified submodule paths both resolve.
pub use peko_quota::{QuotaConfig, QuotaCycle, QuotaError, QuotaMeter, QuotaScope, QuotaState};
