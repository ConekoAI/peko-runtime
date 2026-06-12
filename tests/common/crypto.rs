//! Ed25519 identity + nonce-signing helpers for tunnel authentication.

#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;

/// Generate a fresh runtime identity: returns `(did:key:…, signing_key)`.
///
/// The DID encodes the Ed25519 public key with the multicodec prefix
/// `0xed 0x01` and base58btc, matching what `pekobot::identity` produces.
pub fn generate_runtime_identity() -> (String, SigningKey) {
    let mut rng = rand::thread_rng();
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    let signing_key = SigningKey::from_bytes(&secret);
    let public_key = signing_key.verifying_key();

    let multicodec = [0xed, 0x01];
    let mut prefixed = Vec::with_capacity(2 + 32);
    prefixed.extend_from_slice(&multicodec);
    prefixed.extend_from_slice(public_key.as_bytes());
    let encoded = bs58::encode(&prefixed).into_string();
    let did = format!("did:key:z{encoded}");

    (did, signing_key)
}

/// Sign a tunnel-handshake nonce; returns the base64-encoded signature.
pub fn sign_nonce(signing_key: &SigningKey, nonce: &str) -> String {
    let signature = signing_key.sign(nonce.as_bytes());
    BASE64.encode(signature.to_bytes())
}
