//! Gateway IPC Protocol
//!
//! Out-of-process gateways communicate with the daemon via **stdio-line JSON**
//! (one JSON object per line, newline-delimited). This is the same framing used
//! by MCP stdio transport but with gateway-specific message types.
//!
//! See ADR-025 Section 9 for the full protocol specification.

use serde::{Deserialize, Serialize};

// Re-export the canonical GatewayRoutingConfig from the router module
pub use crate::extensions::gateway::runtime::router::GatewayRoutingConfig;

// =============================================================================
// Daemon → Gateway (stdin)
// =============================================================================

/// A packet sent from the daemon to a gateway process via stdin
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayPacket {
    /// Send routing configuration on startup
    #[serde(rename = "config")]
    Config {
        gateway_id: String,
        routing: GatewayRoutingConfig,
    },

    /// Deliver an agent response to a channel
    #[serde(rename = "deliver")]
    Deliver {
        request_id: u64,
        channel_id: String,
        message: String,
        session_id: String,
    },

    /// Health check ping
    #[serde(rename = "ping")]
    Ping { request_id: u64 },

    /// Request graceful shutdown
    #[serde(rename = "shutdown")]
    Shutdown { request_id: u64 },
}

// =============================================================================
// Gateway → Daemon (stdout)
// =============================================================================

/// A response sent from a gateway process to the daemon via stdout
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GatewayResponse {
    /// Incoming message from a user
    #[serde(rename = "receive")]
    Receive {
        request_id: u64,
        channel_id: String,
        user_id: String,
        message: String,
        #[serde(default)]
        metadata: serde_json::Value,
    },

    /// Ping response
    #[serde(rename = "pong")]
    Pong { request_id: u64 },

    /// Delivery acknowledgement
    #[serde(rename = "delivered")]
    Delivered {
        request_id: u64,
        message_id: Option<String>,
    },

    /// Error report
    #[serde(rename = "error")]
    Error {
        request_id: u64,
        message: String,
    },
}

// =============================================================================
// Helpers
// =============================================================================

/// Serialize a packet to a newline-delimited JSON string
pub fn encode_packet<T: Serialize>(packet: &T) -> anyhow::Result<String> {
    let json = serde_json::to_string(packet)?;
    Ok(format!("{}\n", json))
}

/// Deserialize a response from a JSON string
pub fn decode_response(line: &str) -> anyhow::Result<GatewayResponse> {
    let response = serde_json::from_str(line.trim())?;
    Ok(response)
}

/// Deserialize a packet from a JSON string (for gateway-side parsing)
pub fn decode_packet(line: &str) -> anyhow::Result<GatewayPacket> {
    let packet = serde_json::from_str(line.trim())?;
    Ok(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_packet() {
        let packet = GatewayPacket::Ping { request_id: 42 };
        let encoded = encode_packet(&packet).unwrap();
        assert!(encoded.ends_with('\n'));

        // Gateway side would decode as packet
        let decoded: GatewayPacket = decode_packet(&encoded.trim()).unwrap();
        match decoded {
            GatewayPacket::Ping { request_id } => assert_eq!(request_id, 42),
            _ => panic!("Expected Ping"),
        }
    }

    #[test]
    fn test_encode_decode_response() {
        let response = GatewayResponse::Pong { request_id: 42 };
        let encoded = encode_packet(&response).unwrap();

        let decoded = decode_response(&encoded.trim()).unwrap();
        match decoded {
            GatewayResponse::Pong { request_id } => assert_eq!(request_id, 42),
            _ => panic!("Expected Pong"),
        }
    }

    #[test]
    fn test_receive_response() {
        let json = r##"{"type":"receive","request_id":1,"channel_id":"#general","user_id":"u123","message":"hello","metadata":{}}"##;
        let decoded = decode_response(json).unwrap();
        match decoded {
            GatewayResponse::Receive {
                request_id,
                channel_id,
                user_id,
                message,
                ..
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(channel_id, "#general");
                assert_eq!(user_id, "u123");
                assert_eq!(message, "hello");
            }
            _ => panic!("Expected Receive"),
        }
    }

    #[test]
    fn test_config_packet() {
        let mut channel_map = std::collections::HashMap::new();
        channel_map.insert("#general".to_string(), "assistant".to_string());

        let packet = GatewayPacket::Config {
            gateway_id: "discord".to_string(),
            routing: GatewayRoutingConfig {
                default_agent: "assistant".to_string(),
                channel_map,
                dm_agents: std::collections::HashMap::new(),
            },
        };

        let encoded = encode_packet(&packet).unwrap();
        assert!(encoded.contains("\"type\":\"config\""));
        assert!(encoded.contains("\"gateway_id\":\"discord\""));
    }

    #[test]
    fn test_error_response() {
        let response = GatewayResponse::Error {
            request_id: 5,
            message: "Channel not found".to_string(),
        };

        let encoded = encode_packet(&response).unwrap();
        let decoded = decode_response(&encoded.trim()).unwrap();
        match decoded {
            GatewayResponse::Error { request_id, message } => {
                assert_eq!(request_id, 5);
                assert_eq!(message, "Channel not found");
            }
            _ => panic!("Expected Error"),
        }
    }
}
