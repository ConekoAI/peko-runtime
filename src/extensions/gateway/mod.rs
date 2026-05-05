//! Gateway Extension Type Implementation
//!
//! This module contains all Gateway-specific code:
//! - `adapter`: ExtensionTypeAdapter for gateway plugins
//! - `runtime`: Runtime adapters, starters, and router
//! - `protocol`: Gateway IPC Protocol

pub mod adapter;
pub mod protocol;
pub mod runtime;

pub use adapter::{
    discover_gateway_extensions, load_and_register_gateways, register_gateways_with_core,
    DiscoveredGateway, GatewayAdapter, GatewayExtensionConfig, GatewayHookConfig, GatewayToolConfig,
};
pub use protocol::{encode_packet, decode_response, GatewayPacket, GatewayResponse, GatewayRoutingConfig};
pub use runtime::{
    adapter::{GatewayFlavor, GatewayRuntimeAdapter},
    router::{GatewayRouter, QueuedMessage},
    starter::GatewayRuntimeStarter,
};
