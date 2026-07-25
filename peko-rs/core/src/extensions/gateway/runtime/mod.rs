//! Gateway Runtime Module
//!
//! Bridges gateway extensions to the daemon's background runtime infrastructure.

pub mod adapter;
pub mod router;
pub mod starter;

pub use adapter::{GatewayFlavor, GatewayRuntimeAdapter};
pub use router::{GatewayRouter, GatewayRoutingConfig, QueuedMessage};
pub use starter::GatewayRuntimeStarter;
