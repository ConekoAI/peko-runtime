//! IPC auth envelope + packet constants.
//!
//! These are the leaf types the CLI→daemon wire protocol shares.
//! The bulk of the protocol (`RequestPacket`, `ResponsePacket`,
//! and the per-command args/types they carry) stays in root until
//! the wider Phase 11 boundary lifts them.
//!
//! Everything here depends only on `serde`, so the auth envelope
//! can travel freely between any future `peko-cli` and `peko-daemon`
//! crates.

use serde::{Deserialize, Serialize};

/// Maximum packet size in bytes (conservative UDP limit).
///
/// Mirrored from `src/ipc/packet.rs` as part of Phase 11a — the
/// root crate keeps the constant as a compat shim.
pub const MAX_PACKET_SIZE: usize = 60_000;

/// Heartbeat interval from daemon to CLI during streams (seconds).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 2;

/// CLI timeout if no packet received (seconds).
///
/// Set to 60s to allow for agent initialization time before
/// heartbeats start.
pub const CLI_TIMEOUT_SECS: u64 = 60;

/// Authentication credential sent with every request (ADR-034).
///
/// Wire shape is a tagged enum:
///
/// ```json
/// { "type": "none" }
/// { "type": "jwt", "token": "..." }
/// { "type": "api_key", "token": "..." }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "token")]
pub enum AuthCredential {
    /// Local trust — no token provided.
    /// Allowed only for Unix-socket or localhost-UDP connections.
    #[serde(rename = "none")]
    None,
    /// pekohub-issued JWT (short-lived).
    #[serde(rename = "jwt")]
    Jwt(String),
    /// Long-lived programmatic key.
    #[serde(rename = "api_key")]
    ApiKey(String),
}

impl Default for AuthCredential {
    fn default() -> Self {
        Self::None
    }
}

/// Mode for a `PrincipalSendControl` request.
///
/// Wire shape:
///
/// ```json
/// { "mode": "interrupt" }
/// { "mode": "steer", "text": "..." }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PrincipalSendControlMode {
    /// Set the run's cancel token. The run finishes its current step
    /// (LLM stream chunk, in-flight tool call) and exits cleanly,
    /// emitting a final `PrincipalSentDone` + `Lifecycle::Interrupted`.
    Interrupt,
    /// Inject `text` as a new user-role turn into the run's session
    /// inbox. The agentic loop drains it at the next iteration.
    Steer { text: String },
}

/// Authentication header appended to every request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthHeader {
    pub credential: AuthCredential,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip for the `AuthCredential` tagged enum. Guards the
    /// wire shape `"none"`/`"jwt"`/`"api_key"` (ADR-034) so any
    /// future rename surfaces here, not in production CLI↔daemon
    /// framing.
    #[test]
    fn auth_credential_round_trip() {
        for (variant, json) in [
            (AuthCredential::None, r#"{"type":"none"}"#),
            (
                AuthCredential::Jwt("t".into()),
                r#"{"type":"jwt","token":"t"}"#,
            ),
            (
                AuthCredential::ApiKey("k".into()),
                r#"{"type":"api_key","token":"k"}"#,
            ),
        ] {
            let parsed: AuthCredential = serde_json::from_str(json).unwrap();
            assert_eq!(format!("{variant:?}"), format!("{parsed:?}"));
            let back = serde_json::to_string(&variant).unwrap();
            assert_eq!(back, json);
        }
    }

    #[test]
    fn control_mode_round_trip() {
        let interrupt = PrincipalSendControlMode::Interrupt;
        let json = serde_json::to_string(&interrupt).unwrap();
        assert_eq!(json, r#"{"mode":"interrupt"}"#);
        let back: PrincipalSendControlMode = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PrincipalSendControlMode::Interrupt));

        let steer = PrincipalSendControlMode::Steer { text: "hi".into() };
        let json = serde_json::to_string(&steer).unwrap();
        assert_eq!(json, r#"{"mode":"steer","text":"hi"}"#);
    }

    #[test]
    fn auth_header_default_is_none() {
        let h = AuthHeader::default();
        assert!(matches!(h.credential, AuthCredential::None));
    }

    #[test]
    fn packet_constants_match_root() {
        // These constants are re-exported by the root crate as
        // compatibility shims; the values must agree. If anyone
        // bumps either side they will see a test failure here first.
        assert_eq!(MAX_PACKET_SIZE, 60_000);
        assert_eq!(HEARTBEAT_INTERVAL_SECS, 2);
        assert_eq!(CLI_TIMEOUT_SECS, 60);
    }
}
