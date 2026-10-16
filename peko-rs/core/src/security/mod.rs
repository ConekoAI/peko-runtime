//! The trust floor and acting-for stamps (ADR-078).
//!
//! - [`stamps`] decides whom each run acts for, and at what trust.
//! - [`floor`] is the fixed floor every call passes: no shell below
//!   trusted, workspace confinement, and owner-only protected files.
//! - [`proposals`] files a non-owner's protected write for the owner.
//! - [`memory_gate`] holds back hot-memory lines a scheduled owner run
//!   cannot source to the owner (ADR-080 D4).
//! - [`policy`] reads the owner's `POLICY.md`.
//! - [`judge`] decides which calls a separate reviewer sees, and how.
//! - [`ledger`] is the per-principal tool ledger (ADR-079).
//!
//! The module never imports `principal/`, so `agents/` can depend on it;
//! the principal layer supplies trust through [`stamps::TrustSource`].

pub mod floor;
pub mod judge;
pub mod ledger;
pub mod memory_gate;
pub mod policy;
pub mod proposals;
pub mod stamps;

pub use stamps::{
    compartment_of, err_out_of_reach, may_reach, trust_for, visible_metas, Driver, Stamps,
    TrustSource,
};

/// This runtime's identity comparison (ADR-081 D1): the actor registry
/// behind `port`, or canonical addresses when there is none.
#[must_use]
pub fn actor_keys(
    port: Option<&dyn peko_channel::ChannelPort>,
) -> Box<dyn peko_subject::ActorKeys + '_> {
    match port {
        Some(port) => Box::new(peko_channel::PortKeys(port)),
        None => Box::new(peko_subject::CanonicalKeys),
    }
}

/// [`actor_keys`] over a shared port.
#[must_use]
pub fn shared_actor_keys(
    port: Option<std::sync::Arc<dyn peko_channel::ChannelPort>>,
) -> Box<dyn peko_subject::ActorKeys> {
    struct SharedPortKeys(std::sync::Arc<dyn peko_channel::ChannelPort>);
    impl peko_subject::ActorKeys for SharedPortKeys {
        fn key_of(&self, subject: &peko_subject::ActorRef) -> peko_subject::ActorKey {
            self.0.key_of(subject)
        }
    }
    match port {
        Some(port) => Box::new(SharedPortKeys(port)),
        None => Box::new(peko_subject::CanonicalKeys),
    }
}

#[cfg(test)]
mod dispatch_tests;
