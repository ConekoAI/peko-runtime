//! Compatibility re-exports for the neutral `peko-events` crate.
//!
//! The event contract is shared by the agentic loop and legacy provider
//! streaming APIs, so it lives outside the engine implementation crate.

pub use peko_events::*;
