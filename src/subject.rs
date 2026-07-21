//! Compatibility re-exports for the neutral `peko-subject` crate.
//!
//! ADR-041's canonical actor types (`Subject`, `SubjectKind`,
//! `PrincipalId`, `PrincipalDID`, `SubjectParseError`,
//! `subject_from_string_with_default_user`) form a pure value/type
//! layer with no inbound edge from principal, agents, engine, daemon,
//! providers, or extensions. They live in their own crate so future
//! workspace members can depend on the actor layer without re-entering
//! the root crate's internal services.
//!
//! Internal consumers keep the historical `peko::subject::*` import
//! paths through this shim; downstream crates that grow out of the
//! workspace migration will depend on `peko-subject` directly.

pub use peko_subject::*;
