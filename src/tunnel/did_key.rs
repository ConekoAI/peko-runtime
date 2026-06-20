//! `did:key:` (W3C) parser for the inbound `AgentToAgentRequest`
//! signature verifier. Issue #29 Slice C.
//!
//! The did:key method for Ed25519 is self-certifying: the
//! `did:key:z6Mk...` form encodes the verifying public key in the
//! DID itself, no network resolution needed. The encoding is:
//!
//! ```text
//!   did:key:<multibase>
//!   multibase prefix `z` = base58btc
//!   payload = ed25519 multicodec header (0xed 0x01) || 32-byte pubkey
//!   full did = "did:key:z" + base58btc(0xed 0x01 || pubkey)
//! ```
//!
//! See <https://w3c-ccg.github.io/did-method-key/> for the spec.
//! This is the same encoding `PekoHubCredential::runtime_id` uses
//! today (the tunnel handshake's `caller_runtime_id` arrives as
//! exactly this form, and the hub's allowlist at handshake time
//! checks that the runtime_id's derived public key is the one
//! that signed `RuntimeHello`).
//!
//! For inbound `AgentToAgentRequest`, the same check applies: the
//! target runtime derives the verifying key from the
//! `caller_runtime_id` and verifies the signature. This is the
//! defense-in-depth layer pekohub#16's issue body calls out — the
//! hub's source-allowlist check is the primary gate, but the
//! target-side re-verification catches a hub bug or a stale
//! forwarder that mishandles the envelope.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::VerifyingKey;

/// The `did:key` method name. Run-time assertions on the input.
const DID_KEY_PREFIX: &str = "did:key:";

/// Multibase prefix for base58btc-encoded payloads. W3C did:key
/// uses this exclusively for the z6Mk... family of DIDs.
const MULTIBASE_BASE58BTC: &str = "z";

/// The two-byte multicodec header that prefixes an Ed25519 public
/// key in the base58btc payload. See
/// <https://github.com/multiformats/multicodec/blob/master/table.csv>
/// (table: `ed25519-pub = 0xed 0x01`).
const ED25519_MULTICODEC: [u8; 2] = [0xed, 0x01];

/// Parse a `did:key:z6Mk...` string into an Ed25519 `VerifyingKey`.
///
/// # Errors
///
/// - The input does not start with `did:key:`.
/// - The multibase prefix is not `z` (base58btc). Other encodings
///   (multibase `u` for base64url, etc.) are intentionally not
///   supported because the runtime's own `runtime_id` only emits
///   `z6Mk...`; a caller sending a different encoding is a sign of
///   cross-tenant identity confusion that should be loud, not
///   silently tolerated.
/// - The base58btc payload is the wrong length (must be 2 multicodec
///   header bytes + 32 raw pubkey bytes = 34 bytes total).
/// - The multicodec header is not `[0xed, 0x01]` (so the key is not
///   an Ed25519 public key).
/// - The 32-byte tail is not a valid Ed25519 verifying key (e.g.,
///   point on the curve check fails).
pub fn did_key_to_verifying_key(did: &str) -> Result<VerifyingKey> {
    let suffix = did
        .strip_prefix(DID_KEY_PREFIX)
        .ok_or_else(|| anyhow!("DID does not start with `{DID_KEY_PREFIX}`: {did}"))?;

    if !suffix.starts_with(MULTIBASE_BASE58BTC) {
        return Err(anyhow!(
            "DID multibase prefix must be `{MULTIBASE_BASE58BTC}` (base58btc); got: {suffix}"
        ));
    }

    let raw = bs58::decode(&suffix[1..])
        .into_vec()
        .with_context(|| format!("DID base58btc payload failed to decode: {did}"))?;

    if raw.len() != ED25519_MULTICODEC.len() + 32 {
        return Err(anyhow!(
            "DID payload is {} bytes; expected {} (ed25519 multicodec + 32-byte pubkey)",
            raw.len(),
            ED25519_MULTICODEC.len() + 32
        ));
    }

    if raw[..2] != ED25519_MULTICODEC {
        return Err(anyhow!(
            "DID multicodec header is {:?}; expected {:?} (ed25519-pub)",
            &raw[..2],
            ED25519_MULTICODEC
        ));
    }

    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&raw[2..]);
    VerifyingKey::from_bytes(&key_array)
        .with_context(|| format!("DID public key is not a valid Ed25519 point: {did}"))
}

/// Round-trip helper for tests: encode a `VerifyingKey` back to
/// the `did:key:z...` form. Not used in production code today
/// (the runtime's `runtime_id` is set by the credential writer,
/// not synthesized at verify time), but exported so the unit tests
/// can construct a known-good DID from a generated keypair.
pub fn verifying_key_to_did_key(key: &VerifyingKey) -> String {
    let mut payload = ED25519_MULTICODEC.to_vec();
    payload.extend_from_slice(&key.to_bytes());
    format!("did:key:{}{}", MULTIBASE_BASE58BTC, bs58::encode(payload).into_string())
}

/// Public helper for tests / error message formatting — base64
/// the pubkey bytes for log entries. Not used in production code
/// (logs are `Display`-formatted elsewhere) but kept here for
/// the test that pins the round-trip to the well-known format.
#[allow(dead_code)]
pub fn pubkey_to_base64(key: &VerifyingKey) -> String {
    BASE64.encode(key.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::KeyPair;

    /// Round-trip: a generated keypair encodes to a `did:key:z...`
    /// that decodes back to the same `VerifyingKey`.
    #[test]
    fn test_did_key_roundtrip() {
        let kp = KeyPair::generate();
        let did = verifying_key_to_did_key(&kp.verifying_key);
        let parsed = did_key_to_verifying_key(&did).expect("freshly generated DID must parse");
        assert_eq!(
            parsed.to_bytes(),
            kp.verifying_key.to_bytes(),
            "round-trip must preserve the verifying key"
        );
        // The DID form must be the standard `z6Mk...` shape.
        assert!(
            did.starts_with("did:key:z6Mk"),
            "expected z6Mk multibase prefix; got {did}"
        );
    }

    /// The parser refuses a non-`did:key:` prefix with a
    /// descriptive error rather than a panic.
    #[test]
    fn test_did_key_rejects_wrong_prefix() {
        let err = did_key_to_verifying_key("did:peko:agent:abc")
            .expect_err("non-key DID must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("does not start with `did:key:`"),
            "error must name the condition; got: {msg}"
        );
    }

    /// Wrong multibase prefix is rejected (e.g. `did:key:u6M...`
    /// for base64url).
    #[test]
    fn test_did_key_rejects_wrong_multibase() {
        let err = did_key_to_verifying_key("did:key:uABC")
            .expect_err("non-base58btc multibase must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("multibase prefix"),
            "error must name the condition; got: {msg}"
        );
    }

    /// A base58btc payload of the wrong length is rejected.
    #[test]
    fn test_did_key_rejects_wrong_length_payload() {
        // `z` + too-short payload (just "z").
        let err = did_key_to_verifying_key("did:key:z")
            .expect_err("too-short payload must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("payload is"),
            "error must mention the payload length; got: {msg}"
        );
    }

    /// A non-ed25519 multicodec header is rejected. `0x12 0x20`
    /// is the sha256 hash multicodec, not ed25519.
    #[test]
    fn test_did_key_rejects_non_ed25519_multicodec() {
        // Build a synthetic DID with the wrong multicodec header.
        let mut wrong_payload = vec![0x12, 0x20];
        wrong_payload.extend_from_slice(&[0xab; 32]);
        let wrong_did = format!(
            "did:key:z{}",
            bs58::encode(&wrong_payload).into_string()
        );
        let err = did_key_to_verifying_key(&wrong_did)
            .expect_err("non-ed25519 multicodec must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("multicodec"),
            "error must mention multicodec; got: {msg}"
        );
    }
}
