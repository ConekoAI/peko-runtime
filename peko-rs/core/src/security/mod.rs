//! The trust floor and acting-for stamps (ADR-078).
//!
//! - [`stamps`] decides whom each run acts for, and at what trust.
//! - [`floor`] is the fixed floor every call passes: no shell below
//!   trusted, workspace confinement, and owner-only protected files.
//! - [`proposals`] files a non-owner's protected write for the owner.
//! - [`policy`] reads the owner's `POLICY.md`.
//! - [`judge`] decides which calls a separate reviewer sees, and how.
//!
//! The module never imports `principal/`, so `agents/` can depend on it;
//! the principal layer supplies trust through [`stamps::TrustSource`].

pub mod floor;
pub mod judge;
pub mod policy;
pub mod proposals;
pub mod stamps;

pub use stamps::{
    compartment_of, err_out_of_reach, may_reach, trust_for, visible_metas, Driver, Stamps,
    TrustSource,
};

#[cfg(test)]
mod dispatch_tests;
