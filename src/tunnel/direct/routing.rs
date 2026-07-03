//! Transport selection logic for cross-runtime A2A messaging.

use crate::tunnel::known_runtimes::{KnownRuntimes, TransportPreference, TrustLevel};

/// Chosen transport for an outbound A2A request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportChoice {
    /// Send over the existing PekoHub tunnel.
    Tunnel,
    /// Send over a direct connection to the given endpoint.
    Direct { endpoint: String },
    /// Direct was explicitly requested but is unavailable.
    Unavailable { reason: String },
}

/// Select the transport for a peer runtime based on the **callee's**
/// preference and advertised endpoint from the directory, plus the local
/// `KnownRuntimes` registry for trust/TLS overrides.
///
/// The callee owns the connection-method preference; the caller respects
/// it. The local registry is now only a trust store and a place for the
/// operator to override the endpoint or TLS config — it does not decide
/// whether direct is used. Unknown peers default to the tunnel unless the
/// callee explicitly requested direct, in which case the call fails.
pub fn select_transport(
    runtime_id: &str,
    directory_direct_endpoint: Option<&str>,
    callee_transport_preference: TransportPreference,
    known_runtimes: &KnownRuntimes,
) -> TransportChoice {
    let peer = known_runtimes.find(runtime_id);

    // The operator can override the endpoint in known_runtimes.toml.
    // Otherwise, fall back to the endpoint advertised by the directory.
    let configured_endpoint = peer
        .and_then(|p| p.direct_endpoint.as_deref())
        .or(directory_direct_endpoint);

    // Direct connections require explicit authorization in the local
    // known-runtimes registry.
    let authorized = peer
        .map(|p| p.trust_level == TrustLevel::Authorized)
        .unwrap_or(false);

    match callee_transport_preference {
        TransportPreference::Tunnel => TransportChoice::Tunnel,
        TransportPreference::Direct => {
            if authorized {
                if let Some(endpoint) = configured_endpoint {
                    return TransportChoice::Direct {
                        endpoint: endpoint.to_string(),
                    };
                }
            }
            TransportChoice::Unavailable {
                reason: format!(
                    "direct transport requested for {runtime_id} but no authorized direct endpoint is available"
                ),
            }
        }
        TransportPreference::Auto => {
            if authorized {
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

    fn make_registry(
        runtime_id: &str,
        preference: TransportPreference,
        trust: TrustLevel,
        endpoint: Option<String>,
    ) -> KnownRuntimes {
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
            select_transport(
                "did:key:zUnknown",
                Some("wss://host:1"),
                TransportPreference::Auto,
                &registry
            ),
            TransportChoice::Tunnel
        );
    }

    #[test]
    fn select_unavailable_when_direct_requested_but_peer_unknown() {
        let registry = KnownRuntimes::new();
        assert!(
            matches!(
                select_transport(
                    "did:key:zUnknown",
                    Some("wss://host:1"),
                    TransportPreference::Direct,
                    &registry
                ),
                TransportChoice::Unavailable { .. }
            ),
            "expected Unavailable when direct is requested but peer is not known/authorized"
        );
    }

    #[test]
    fn select_direct_when_callee_prefers_direct_and_authorized() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Direct,
            TrustLevel::Authorized,
            Some("wss://host:11436".to_string()),
        );
        assert_eq!(
            select_transport(
                "did:key:zPeer",
                None,
                TransportPreference::Direct,
                &registry
            ),
            TransportChoice::Direct {
                endpoint: "wss://host:11436".to_string()
            }
        );
    }

    #[test]
    fn select_unavailable_when_direct_callee_not_authorized() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Untrusted,
            Some("wss://host:11436".to_string()),
        );
        assert!(
            matches!(
                select_transport(
                    "did:key:zPeer",
                    None,
                    TransportPreference::Direct,
                    &registry
                ),
                TransportChoice::Unavailable { .. }
            ),
            "expected Unavailable when callee wants direct but peer is not authorized"
        );
    }

    #[test]
    fn select_auto_prefers_direct_when_authorized_endpoint_present() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            Some("wss://host:11436".to_string()),
        );
        assert_eq!(
            select_transport("did:key:zPeer", None, TransportPreference::Auto, &registry),
            TransportChoice::Direct {
                endpoint: "wss://host:11436".to_string()
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
            select_transport(
                "did:key:zPeer",
                Some("wss://dir:11436"),
                TransportPreference::Auto,
                &registry
            ),
            TransportChoice::Direct {
                endpoint: "wss://dir:11436".to_string()
            }
        );
    }

    #[test]
    fn select_tunnel_when_preference_is_tunnel() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            Some("wss://host:11436".to_string()),
        );
        assert_eq!(
            select_transport(
                "did:key:zPeer",
                None,
                TransportPreference::Tunnel,
                &registry
            ),
            TransportChoice::Tunnel
        );
    }

    #[test]
    fn directory_endpoint_used_when_no_local_override() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            None,
        );
        assert_eq!(
            select_transport(
                "did:key:zPeer",
                Some("wss://advertised:11436"),
                TransportPreference::Auto,
                &registry
            ),
            TransportChoice::Direct {
                endpoint: "wss://advertised:11436".to_string()
            }
        );
    }

    #[test]
    fn local_endpoint_overrides_directory_endpoint() {
        let registry = make_registry(
            "did:key:zPeer",
            TransportPreference::Auto,
            TrustLevel::Authorized,
            Some("wss://operator-override:11436".to_string()),
        );
        assert_eq!(
            select_transport(
                "did:key:zPeer",
                Some("wss://advertised:11436"),
                TransportPreference::Auto,
                &registry
            ),
            TransportChoice::Direct {
                endpoint: "wss://operator-override:11436".to_string()
            }
        );
    }
}
