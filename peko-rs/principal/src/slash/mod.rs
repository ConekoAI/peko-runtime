//! Principal slash dispatcher (declarations only — Phase 14.c.2b lifts the impl).
//!
//! Phase 14.c.2a introduces the [`PrincipalExtensionRow`] re-type that
//! `slash/help.rs` will construct when it lifts in Phase 14.c.2b. The
//! actual `SlashDispatcher` + `handle_help` implementations stay in
//! `src/principal/slash/` until that PR.

pub mod extension_row;

pub use extension_row::PrincipalExtensionRow;
