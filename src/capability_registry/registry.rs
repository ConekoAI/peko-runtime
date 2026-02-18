//! Agent Capability Registry
//!
//! Layer 2 foundation for cross-org agent trust and discovery.
//! Agents advertise capabilities, others discover and verify them.

pub mod standard_capabilities;

mod registry;
pub use registry::*;
