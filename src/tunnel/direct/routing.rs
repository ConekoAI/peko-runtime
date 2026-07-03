//! Transport selection logic for cross-runtime A2A messaging.

use crate::tunnel::known_runtimes::{KnownRuntimes, TransportPreference, TrustLevel};

/// Chosen transport for an outbound A2A request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportChoice {
    /// Send over the existing PekoHub tunnel.
    Tunnel,
    /// Send over a direct connection to the given endpoint.
    Direct {
        endpoint: String,
    },
    /// Direct was explicitly requested but is unavailable.
    Unavailable {
        reason: String,
    },
}

/// Select the transport for a peer runtime based on directory resolution
/// and the local known-runtimes registry.
pub fn select_transport(
    runtime_id: &str,
    directory_direct_endpoint: Option<&str>,
    known_runtimes: &KnownRuntimes,
) -> TransportChoice {
    let Some(peer) = known_runtimes.find(runtime_id) else {
        return TransportChoice::Tunnel;
    };

    // The peer's own config takes precedence over the directory hint.
    let configured_endpoint = peer.direct_endpoint.as_deref().or(directory_direct_endpoint);

    match peer.transport_preference {
        TransportPreference::Tunnel => TransportChoice::Tunnel,
        TransportPreference::Direct => {
            if peer.trust_level == TrustLevel::Authorized {
                if let Some(endpoint) = configured_endpoint {
                    return TransportChoice::Direct {
                        endpoint: endpoint.to_string(),
                    };
                }
            }
            TransportChoice::Unavailable {
                reason: format!(
                    "direct transport requested for {runtime_id} but no authorized direct endpoint is configured"
                ),
            }
        }
        TransportPreference::Auto => {
            if peer.trust_level == TrustLevel::Authorized {
                if let Some(endpoint) = configured_endpoint {
                    return TransportChoice::Direct {
                        endpoint: endpoint.to_string(),
                    };
                }
            }
            TransportChoice::Tunnel
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry(runtime_id: &str, preference: TransportPreference, trust: TrustLevel, endpoint: Option<String>) -> KnownRuntimes {
        let mut registry = KnownRuntimes::new();
        registry.register_with_direct(
            runtime_id,
            "Peer Runtime",
            None,
            endpoint,
            preference,
            None,
            trust,
        );
        registry
    }

    #[test]
    fn select_tunnel_when_peer_unknown() {
        let registry = KnownRuntimes::new();
        assert_eq!(
            select_transport("did:key:zUnknown", Some("tls://host:1"), &registry),
            TransportChoice::Tunnel
        );
    }

    #[test]
    fn select_direct_when_preference_is_direct_and_authorized() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Direct,
            TrustLevel::Authorized,
            Some("tls://host:11436".to_string()),
        );
        assert_eq!(
            select_transport("did:key:zPeer", None, &registry),
            TransportChoice::Direct {
                endpoint: "tls://host:11436".to_string()
            }
        );
    }

    #[test]
    fn select_unavailable_when_direct_requested_but_not_authorized() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Direct,
            TrustLevel::Untrusted,
            Some("tls://host:11436".to_string()),
        );
        assert!(
            matches!(
                select_transport("did:key:zPeer", None, &registry),
                TransportChoice::Unavailable { .. }
            ),
            "expected Unavailable when direct is requested but peer is not authorized"
        );
    }

    #[test]
    fn select_auto_prefers_direct_when_authorized_endpoint_present() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            Some("tls://host:11436".to_string()),
        );
        assert_eq!(
            select_transport("did:key:zPeer", None, &registry),
            TransportChoice::Direct {
                endpoint: "tls://host:11436".to_string()
            }
        );
    }

    #[test]
    fn select_auto_falls_back_to_tunnel_when_no_direct_endpoint() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            None,
        );
        assert_eq!(
            select_transport("did:key:zPeer", None, &registry),
            TransportChoice::Tunnel
        );
    }

    #[test]
    fn select_tunnel_when_preference_is_tunnel() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Tunnel,
            TrustLevel::Authorized,
            Some("tls://host:11436".to_string()),
        );
        assert_eq!(
            select_transport("did:key:zPeer", None, &registry),
            TransportChoice::Tunnel
        );
    }
}
