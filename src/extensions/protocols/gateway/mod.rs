//! Gateway protocol module
//!
//! Contains the gateway IPC protocol types and codec.

pub mod protocol;

pub use protocol::{
    decode_packet, decode_response, encode_packet, GatewayPacket, GatewayResponse,
};
