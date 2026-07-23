//! Direct runtime-to-runtime identity handshake.
//!
//! The handshake reuses the `RuntimeHello`, `TunnelChallenge`,
//! `TunnelChallengeAck`, and `TunnelReady` variants from the existing
//! tunnel protocol. It proves that each side controls the private key
//! corresponding to its self-certifying `did:key` runtime identity.
//!
//! Flow:
//!
//! 1. Outbound sends `RuntimeHello { runtime_id, nonce, signature }`.
//! 2. Inbound verifies the signature, sends `TunnelChallenge { nonce }`.
//! 3. Outbound signs the challenge nonce, sends `TunnelChallengeAck`.
//! 4. Inbound verifies the ack, sends `TunnelReady`.
//!
//! The handshake is intentionally simple: per-message authorization is
//! provided by the A2A signature on every `AgentToAgentRequest`.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey, Verifier};
use rand::RngCore;

use crate::tunnel::did_key::did_key_to_verifying_key;
use crate::tunnel::protocol::TunnelMessage;

/// Errors that can occur during the direct handshake.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("Invalid runtime DID: {0}")]
    InvalidDid(String),
    #[error("Signature verification failed: {0}")]
    InvalidSignature(String),
    #[error("Unexpected handshake message: expected {expected}, got {actual}")]
    UnexpectedMessage { expected: String, actual: String },
    #[error("Handshake field missing: {0}")]
    MissingField(String),
    #[error("Signature decode failed: {0}")]
    SignatureDecode(String),
    #[error("Wrong challenge nonce in ack")]
    WrongNonce,
}

/// Build a `RuntimeHello` message signed by the local runtime.
pub fn build_runtime_hello(runtime_id: &str, signing_key: &SigningKey) -> TunnelMessage {
    let mut nonce_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = BASE64.encode(nonce_bytes);
    let signature = BASE64.encode(signing_key.sign(nonce.as_bytes()).to_bytes());

    TunnelMessage::RuntimeHello {
        runtime_id: runtime_id.to_string(),
        nonce,
        signature,
    }
}

/// Verify an inbound `RuntimeHello` and return the claimed runtime DID.
///
/// The signature is verified against the public key derived from the
/// message's `runtime_id`.
pub fn verify_runtime_hello(msg: &TunnelMessage) -> Result<String, HandshakeError> {
    let TunnelMessage::RuntimeHello {
        runtime_id,
        nonce,
        signature,
    } = msg
    else {
        return Err(HandshakeError::UnexpectedMessage {
            expected: "runtime_hello".to_string(),
            actual: format!("{msg:?}"),
        });
    };

    verify_signature(runtime_id, nonce.as_bytes(), signature)?;
    Ok(runtime_id.clone())
}

/// Build a `TunnelChallenge` message.
pub fn build_tunnel_challenge() -> TunnelMessage {
    let mut nonce_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = BASE64.encode(nonce_bytes);

    TunnelMessage::TunnelChallenge { nonce }
}

/// Extract the nonce from a `TunnelChallenge`.
pub fn verify_tunnel_challenge(msg: &TunnelMessage) -> Result<String, HandshakeError> {
    match msg {
        TunnelMessage::TunnelChallenge { nonce } => Ok(nonce.clone()),
        other => Err(HandshakeError::UnexpectedMessage {
            expected: "tunnel_challenge".to_string(),
            actual: format!("{other:?}"),
        }),
    }
}

/// Build a `TunnelChallengeAck` signing the server's challenge nonce.
pub fn build_tunnel_challenge_ack(
    challenge_nonce: &str,
    signing_key: &SigningKey,
) -> TunnelMessage {
    let signature = BASE64.encode(signing_key.sign(challenge_nonce.as_bytes()).to_bytes());

    TunnelMessage::TunnelChallengeAck {
        nonce: challenge_nonce.to_string(),
        signature,
    }
}

/// Verify an inbound `TunnelChallengeAck`.
///
/// `runtime_id` is the peer DID established during `RuntimeHello`.
/// `expected_nonce` is the challenge nonce we issued.
pub fn verify_tunnel_challenge_ack(
    msg: &TunnelMessage,
    runtime_id: &str,
    expected_nonce: &str,
) -> Result<(), HandshakeError> {
    let TunnelMessage::TunnelChallengeAck { nonce, signature } = msg else {
        return Err(HandshakeError::UnexpectedMessage {
            expected: "tunnel_challenge_ack".to_string(),
            actual: format!("{msg:?}"),
        });
    };

    if nonce != expected_nonce {
        return Err(HandshakeError::WrongNonce);
    }

    verify_signature(runtime_id, nonce.as_bytes(), signature)?;
    Ok(())
}

/// Build a `TunnelReady` message.
pub fn build_tunnel_ready() -> TunnelMessage {
    TunnelMessage::TunnelReady {
        heartbeat_interval_secs: 0,
    }
}

/// Verify an inbound `TunnelReady`.
pub fn verify_tunnel_ready(msg: &TunnelMessage) -> Result<(), HandshakeError> {
    match msg {
        TunnelMessage::TunnelReady { .. } => Ok(()),
        other => Err(HandshakeError::UnexpectedMessage {
            expected: "tunnel_ready".to_string(),
            actual: format!("{other:?}"),
        }),
    }
}

/// Verify a base64-encoded Ed25519 signature over `message` using the
/// public key derived from `runtime_id`.
fn verify_signature(
    runtime_id: &str,
    message: &[u8],
    signature_b64: &str,
) -> Result<(), HandshakeError> {
    let verifying_key = did_key_to_verifying_key(runtime_id)
        .map_err(|e| HandshakeError::InvalidDid(e.to_string()))?;

    let signature_bytes = BASE64
        .decode(signature_b64)
        .map_err(|e| HandshakeError::SignatureDecode(e.to_string()))?;
    if signature_bytes.len() != 64 {
        return Err(HandshakeError::InvalidSignature(format!(
            "signature is {} bytes; expected 64",
            signature_bytes.len()
        )));
    }
    let mut sig_array = [0u8; 64];
    sig_array.copy_from_slice(&signature_bytes);
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);

    verifying_key
        .verify(message, &signature)
        .map_err(|e| HandshakeError::InvalidSignature(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::verifying_key_to_did_key;
    use peko_identity::keys::KeyPair;

    fn make_test_keypair() -> (SigningKey, String) {
        let kp = KeyPair::generate();
        let runtime_id = verifying_key_to_did_key(&kp.verifying_key);
        (kp.signing_key, runtime_id)
    }

    #[test]
    fn test_runtime_hello_roundtrip() {
        let (signing_key, runtime_id) = make_test_keypair();
        let hello = build_runtime_hello(&runtime_id, &signing_key);
        let verified_id = verify_runtime_hello(&hello).unwrap();
        assert_eq!(verified_id, runtime_id);
    }

    #[test]
    fn test_runtime_hello_rejects_wrong_signature() {
        let (signing_key, runtime_id) = make_test_keypair();
        let mut hello = build_runtime_hello(&runtime_id, &signing_key);
        // Corrupt the signature.
        if let TunnelMessage::RuntimeHello { signature, .. } = &mut hello {
            *signature = BASE64.encode([0u8; 64]);
        }
        assert!(verify_runtime_hello(&hello).is_err());
    }

    #[test]
    fn test_challenge_ack_roundtrip() {
        let (signing_key, runtime_id) = make_test_keypair();
        let challenge = build_tunnel_challenge();
        let nonce = verify_tunnel_challenge(&challenge).unwrap();

        let ack = build_tunnel_challenge_ack(&nonce, &signing_key);
        assert!(verify_tunnel_challenge_ack(&ack, &runtime_id, &nonce).is_ok());
    }

    #[test]
    fn test_challenge_ack_rejects_wrong_nonce() {
        let (signing_key, runtime_id) = make_test_keypair();
        let ack = build_tunnel_challenge_ack("expected-nonce", &signing_key);
        assert!(verify_tunnel_challenge_ack(&ack, &runtime_id, "different-nonce").is_err());
    }

    #[test]
    fn test_tunnel_ready_verifies() {
        let ready = build_tunnel_ready();
        assert!(verify_tunnel_ready(&ready).is_ok());
        assert!(verify_tunnel_ready(&TunnelMessage::Heartbeat { seq: 1 }).is_err());
    }
}
