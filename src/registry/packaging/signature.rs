//! Manifest signature verification for `.agent` packages.
//!
//! ## Why this exists (issue #14)
//!
//! `Packager::sign_manifest` ([`crate::registry::packaging::packager::Packager::sign_manifest`])
//! signs the manifest with the agent's ed25519 DID key on write. Until the
//! unpacker started verifying the signature, that signature was write-only — a
//! tampered `.agent` pulled from a registry or a mirror would import
//! successfully and the runtime's per-author trust assumption would be silently
//! broken. The `verify_signature` helper in [`crate::identity::resolver`] was
//! also implemented but unwired (the resolver looks the DID up in *local*
//! storage, which on first import has no entry for the incoming agent).
//!
//! This module verifies the manifest signature using the public key embedded in
//! the package's own `identity/did.json` (which the unpacker has on hand and
//! which is part of the package's per-layer content-addressed storage, so it
//! is already tamper-evident up to the manifest swap problem this module
//! closes).
//!
//! ## Canonicalization
//!
//! The packager reconstructs the signed bytes by cloning the manifest with
//! `signatures.manifest = ""`, then re-serializing via `AgentManifest::to_toml`.
//! The verifier MUST do the same — `toml::to_string_pretty` is deterministic
//! for a given input value, so the reconstructed bytes match as long as we
//! match the packager's reconstruction. We pin the algorithm field to
//! `"ed25519"` to match the on-disk value (the packager leaves it untouched
//! during reconstruction; setting it explicitly keeps the verifier robust if
//! that ever changes).
//!
//! ## Failure modes
//!
//! Verification can fail for distinct reasons. The variants of
//! [`SignatureError`] preserve the reason so the CLI can show a useful
//! message. The user-visible "stable error code" the issue asks for is
//! always `signature_verification_failed` — the human-readable reason is
//! carried in the message and `SignatureError` variant.
//!
//! ## Trust model
//!
//! Signature verification is a *necessary* condition for trusting a `.agent`,
//! not a sufficient one. The remaining check (after this module returns Ok)
//! is that the per-file SHA-256 checksums in `manifest.packaging.checksums`
//! match the actual file bytes — the manifest binds the file list and
//! checksums together via the signature, so a successful verification means
//! a tampered file is detectable by the checksum path that follows.

use crate::identity::DIDDocument;
use crate::registry::packaging::manifest::AgentManifest;
use thiserror::Error;

/// Stable error code for the CLI (issue #14 acceptance criteria).
///
/// The CLI surfaces this as the "code" component of an `InvalidSignature`
/// error so downstream tooling can match on a single string regardless of
/// the underlying reason. The reason itself is in the human message.
pub const SIGNATURE_VERIFICATION_FAILED_CODE: &str = "signature_verification_failed";

/// Reasons verification can fail.
///
/// All variants map to `SIGNATURE_VERIFICATION_FAILED_CODE` for CLI matching;
/// the variant is preserved in error messages and test assertions.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// Manifest is not signed (no `signatures.manifest` value).
    #[error("manifest is not signed (signatures.manifest is empty)")]
    Unsigned,

    /// Signature uses an unsupported algorithm.
    #[error("unsupported signature algorithm: {0}")]
    UnsupportedAlgorithm(String),

    /// Signature is not valid base64url-no-pad.
    #[error("signature is not valid base64url: {0}")]
    InvalidBase64(String),

    /// Signature decodes to the wrong byte length (must be 64 for ed25519).
    #[error("signature has wrong length: expected 64, got {0}")]
    WrongLength(usize),

    /// `identity/did.json` is missing from the package.
    #[error("identity/did.json is missing from the package")]
    MissingDidDocument,

    /// `identity/did.json` cannot be parsed.
    #[error("identity/did.json is malformed: {0}")]
    MalformedDidDocument(String),

    /// DID document has no verification methods.
    #[error("DID document has no verification methods")]
    NoVerificationMethod,

    /// Public key is not encoded as a multibase `z…` (base58btc) string.
    #[error("public key is not multibase z-base58: {0}")]
    InvalidMultibase(String),

    /// Public key does not decode to 32 bytes (ed25519).
    #[error("public key has wrong length: expected 32, got {0}")]
    InvalidPublicKeyLength(usize),

    /// ed25519 signature verification failed (signature does not match
    /// message under the claimed public key).
    #[error("ed25519 signature verification failed: {0}")]
    VerificationFailed(String),

    /// Recomputing the canonical manifest bytes failed (TOML error).
    #[error("failed to reconstruct signed manifest bytes: {0}")]
    CanonicalizationFailed(String),
}

/// Outcome of a signature check.
///
/// Distinguishes "the package is signed and the signature is good" from
/// "the package is unsigned but the caller asked to allow that". The
/// unpacker uses this to decide whether to hard-fail or warn-and-continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    /// Signature is present and verified.
    Verified,
    /// Signature is missing; caller permitted it (logged as a warning).
    AllowedUnsigned,
}

/// Verify the signature on a `.agent` manifest.
///
/// # Arguments
///
/// * `manifest_bytes` — the raw bytes of `manifest.toml` as stored in the
///   package. Used to detect tampering in transit (the packager never
///   re-serializes on read, so any byte change here means the bytes are
///   not what the author wrote).
/// * `did_doc_bytes` — the raw bytes of `identity/did.json` as stored in
///   the package. The DID document carries the public key we verify
///   against.
/// * `allow_unsigned` — if `true`, a package with `signatures.manifest == ""`
///   is permitted (returns `Ok(SignatureStatus::AllowedUnsigned)`); if
///   `false`, an unsigned package is a hard error.
///
/// # Returns
///
/// * `Ok(SignatureStatus::Verified)` — the manifest is signed and the
///   signature is good.
/// * `Ok(SignatureStatus::AllowedUnsigned)` — the manifest is unsigned
///   and the caller opted in.
/// * `Err(SignatureError)` — verification failed.
///
/// # Signing byte reconstruction
///
/// The packager signs the byte sequence produced by:
///
/// ```ignore
/// AgentManifest { signatures: Signatures { manifest: "".into(), algorithm: "ed25519".into() }, ..clone() }.to_toml()?
/// ```
///
/// We mirror that exactly. `toml::to_string_pretty` is deterministic for
/// a given value, so the reconstructed bytes match.
pub fn verify_manifest_signature(
    manifest_bytes: &[u8],
    did_doc_bytes: &[u8],
    allow_unsigned: bool,
) -> Result<SignatureStatus, SignatureError> {
    // Parse the manifest to extract the signature/algorithm fields. We
    // re-parse (rather than reusing a pre-parsed manifest) so the caller
    // is forced to hand us the on-disk bytes — this makes tamper detection
    // automatic.
    let manifest_str = std::str::from_utf8(manifest_bytes).map_err(|e| {
        SignatureError::CanonicalizationFailed(format!("manifest is not utf-8: {e}"))
    })?;
    let manifest = AgentManifest::from_toml(manifest_str)
        .map_err(|e| SignatureError::CanonicalizationFailed(e.to_string()))?;

    let signature_b64 = manifest.signatures.manifest.trim();
    if signature_b64.is_empty() {
        if allow_unsigned {
            tracing::warn!(
                "agent package '{}' is not signed; importing anyway because allow_unsigned is set",
                manifest.agent.name
            );
            return Ok(SignatureStatus::AllowedUnsigned);
        }
        return Err(SignatureError::Unsigned);
    }

    if manifest.signatures.algorithm != "ed25519" {
        return Err(SignatureError::UnsupportedAlgorithm(
            manifest.signatures.algorithm.clone(),
        ));
    }

    // Reconstruct the canonical signed bytes. Pin algorithm to "ed25519"
    // even though the packager leaves it alone — keeps the verifier robust
    // if the packager ever changes.
    let manifest_for_verification = AgentManifest {
        signatures: crate::registry::packaging::manifest::Signatures {
            manifest: String::new(),
            algorithm: "ed25519".to_string(),
        },
        ..manifest.clone()
    };
    let signed_bytes = manifest_for_verification
        .to_toml()
        .map_err(|e| SignatureError::CanonicalizationFailed(e.to_string()))?
        .into_bytes();

    // Decode signature.
    use base64::Engine;
    let signature_vec = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature_b64.as_bytes())
        .map_err(|e| SignatureError::InvalidBase64(e.to_string()))?;
    if signature_vec.len() != 64 {
        return Err(SignatureError::WrongLength(signature_vec.len()));
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&signature_vec);

    // Parse DID document.
    let did_doc: DIDDocument = serde_json::from_slice(did_doc_bytes)
        .map_err(|e| SignatureError::MalformedDidDocument(e.to_string()))?;
    let vm = did_doc
        .verification_method
        .first()
        .ok_or(SignatureError::NoVerificationMethod)?;
    let multibase = &vm.public_key_multibase;
    if !multibase.starts_with('z') {
        return Err(SignatureError::InvalidMultibase(multibase.clone()));
    }
    let public_key = bs58::decode(&multibase[1..])
        .into_vec()
        .map_err(|e| SignatureError::InvalidMultibase(e.to_string()))?;
    if public_key.len() != 32 {
        return Err(SignatureError::InvalidPublicKeyLength(public_key.len()));
    }
    let mut public_key_arr = [0u8; 32];
    public_key_arr.copy_from_slice(&public_key);

    // Verify.
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let verifying_key = VerifyingKey::from_bytes(&public_key_arr)
        .map_err(|e| SignatureError::VerificationFailed(e.to_string()))?;
    let sig = Signature::from_bytes(&signature);
    verifying_key
        .verify(&signed_bytes, &sig)
        .map_err(|e| SignatureError::VerificationFailed(e.to_string()))?;

    Ok(SignatureStatus::Verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::did::DIDScope;
    use crate::identity::keys::KeyPair;
    use crate::identity::{DIDDocument, Identity, VerificationMethod};

    fn make_signed_manifest(keypair: &KeyPair, did: &str) -> Vec<u8> {
        // Mirror Packager::sign_manifest: clone, zero signatures, to_toml, sign.
        let mut manifest = AgentManifest::new("test-agent", "1.0.0", did);
        manifest.packaging.files.push("manifest.toml".into());
        let manifest_for_signing = AgentManifest {
            signatures: crate::registry::packaging::manifest::Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
            ..manifest.clone()
        };
        let signed_bytes = manifest_for_signing.to_toml().unwrap().into_bytes();
        let signature = keypair.sign(&signed_bytes);
        use base64::Engine;
        manifest.signatures.manifest =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
        manifest.signatures.algorithm = "ed25519".to_string();
        manifest.to_toml().unwrap().into_bytes()
    }

    fn did_doc_for(keypair: &KeyPair, did: &str) -> Vec<u8> {
        let pk_bytes = keypair.verifying_key.as_bytes();
        let multibase = format!("z{}", bs58::encode(pk_bytes).into_string());
        let doc = DIDDocument {
            context: vec!["https://www.w3.org/ns/did/v1".into()],
            id: did.into(),
            verification_method: vec![VerificationMethod {
                id: format!("{did}#keys-1"),
                key_type: "Ed25519VerificationKey2020".into(),
                controller: did.into(),
                public_key_multibase: multibase,
            }],
            authentication: vec![format!("{did}#keys-1")],
            assertion_method: vec![format!("{did}#keys-1")],
            service: vec![],
            created: "2026-01-01T00:00:00Z".into(),
            updated: "2026-01-01T00:00:00Z".into(),
        };
        serde_json::to_vec_pretty(&doc).unwrap()
    }

    #[test]
    fn verifies_a_correctly_signed_manifest() {
        let keypair = KeyPair::generate();
        let pk_bytes = keypair.verifying_key.as_bytes();
        let did = format!(
            "did:peko:local:{}",
            &blake3::hash(pk_bytes).to_hex().to_string()[..16]
        );

        let manifest_bytes = make_signed_manifest(&keypair, &did);
        let did_bytes = did_doc_for(&keypair, &did);

        let status = verify_manifest_signature(&manifest_bytes, &did_bytes, false)
            .expect("signature should verify");
        assert_eq!(status, SignatureStatus::Verified);
    }

    #[test]
    fn rejects_unsigned_when_not_allowed() {
        let identity = Identity::generate(DIDScope::Local, Some("ns")).unwrap();
        let manifest = AgentManifest::new("u", "1.0.0", &identity.did);
        // signatures.manifest is empty by default.
        let manifest_bytes = manifest.to_toml().unwrap().into_bytes();
        let did_bytes = serde_json::to_vec(&identity.document).unwrap();

        let err = verify_manifest_signature(&manifest_bytes, &did_bytes, false).unwrap_err();
        assert_eq!(err, SignatureError::Unsigned);
    }

    #[test]
    fn permits_unsigned_when_allowed() {
        let identity = Identity::generate(DIDScope::Local, Some("ns")).unwrap();
        let manifest = AgentManifest::new("u", "1.0.0", &identity.did);
        let manifest_bytes = manifest.to_toml().unwrap().into_bytes();
        let did_bytes = serde_json::to_vec(&identity.document).unwrap();

        let status = verify_manifest_signature(&manifest_bytes, &did_bytes, true)
            .expect("unsigned should be allowed when opted in");
        assert_eq!(status, SignatureStatus::AllowedUnsigned);
    }

    #[test]
    fn rejects_tampered_manifest_bytes() {
        let keypair = KeyPair::generate();
        let pk_bytes = keypair.verifying_key.as_bytes();
        let did = format!(
            "did:peko:local:{}",
            &blake3::hash(pk_bytes).to_hex().to_string()[..16]
        );

        let mut manifest_bytes = make_signed_manifest(&keypair, &did);
        // Tamper: flip one byte well inside the body.
        let last = manifest_bytes.len() - 1;
        manifest_bytes[last] ^= 0x01;
        let did_bytes = did_doc_for(&keypair, &did);

        let err = verify_manifest_signature(&manifest_bytes, &did_bytes, false).unwrap_err();
        assert!(matches!(
            err,
            SignatureError::VerificationFailed(_) | SignatureError::CanonicalizationFailed(_)
        ));
    }

    #[test]
    fn rejects_signature_from_different_key() {
        let signer = KeyPair::generate();
        let imposter = KeyPair::generate();

        let pk_bytes = signer.verifying_key.as_bytes();
        let did = format!(
            "did:peko:local:{}",
            &blake3::hash(pk_bytes).to_hex().to_string()[..16]
        );

        // Manifest is signed by `imposter` but DID doc claims `signer`'s key.
        let manifest_bytes = make_signed_manifest(&imposter, &did);
        let did_bytes = did_doc_for(&signer, &did);

        let err = verify_manifest_signature(&manifest_bytes, &did_bytes, false).unwrap_err();
        assert!(
            matches!(err, SignatureError::VerificationFailed(_)),
            "expected VerificationFailed, got {err:?}"
        );
    }
}
