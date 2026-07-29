//! Cross-runtime A2A Signature — Slice B of issue #29.
//!
//! Slice A landed the `TunnelMessage::AgentToAgentRequest` wire shape with
//! a `signature: String` (base64url) field and a rustdoc-only spec of the
//! canonical pre-image. This module is the concrete implementation: a
//! deterministic encoding of the request fields, plus thin signer /
//! verifier wrappers over `ed25519_dalek`.
//!
//! ## Why length-prefixed concatenation, not JSON
//!
//! The receiver MUST reconstruct the same bytes the caller signed. JSON
//! is the wrong choice — `serde_json` does not promise key ordering or
//! whitespace stability across Rust versions, and the wire-form JSON
//! that goes over the tunnel may be re-serialized by the hub (pekohub#16
//! forwarding is verbatim today, but no one wants the signature to break
//! the moment a hub edition re-pretty-prints the envelope).
//!
//! Length-prefixed concatenation is the standard pattern: each field is
//! preceded by its big-endian `u32` byte length, so unambiguous parsing
//! is guaranteed even if a field contains arbitrary bytes (including
//! null and `\n`).
//!
//! ## Field order
//!
//! Pre-image bytes are, in order:
//!
//! ```text
//!   [4 bytes BE u32 = len("a2a:v1")] || "a2a:v1"
//!   [4 bytes BE u32 = len(request_id)] || request_id
//!   [4 bytes BE u32 = len(caller_runtime_id)] || caller_runtime_id
//!   [4 bytes BE u32 = len(caller_principal_did)] || caller_principal_did
//!   [4 bytes BE u32 = len(target_principal_did)] || target_principal_did
//!   [4 bytes BE u32 = len(message)] || message
//! ```
//!
//! The leading `"a2a:v1"` domain-separation tag makes it impossible for
//! a signature over an `a2a` request to also validate against a future
//! envelope kind (or against `RuntimeHello.signature` etc.) — even if
//! someone manages to construct a colliding suffix.
//!
//! Slice C's verifier reads the same shape. Slice E's E2E test signs
//! end-to-end with no shortcuts.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Domain-separation tag for the v1 a2a pre-image. Embedded in every
/// signed pre-image as the first length-prefixed field so a signature
/// over an a2a request cannot collide with a signature over any other
/// kind of envelope (existing or future).
pub const A2A_SIGNATURE_DOMAIN: &str = "a2a:v1";

/// Borrowed view of the fields that are signed in an
/// `AgentToAgentRequest`. Holding references means callers can build
/// the view from the `TunnelMessage` variant without cloning every
/// field; the verifier on the inbound path constructs the same view
/// from the deserialized message.
///
/// Keep this list **byte-for-byte identical** to the
/// `canonical_pre_image` field order — adding a field here without
/// updating the pre-image (or vice versa) silently breaks every
/// signature.
#[derive(Debug, Clone, Copy)]
pub struct SignedFields<'a> {
    pub request_id: &'a str,
    pub caller_runtime_id: &'a str,
    pub caller_principal_did: &'a str,
    pub target_principal_did: &'a str,
    pub message: &'a str,
}

/// Build the deterministic pre-image bytes that get signed / verified
/// for an a2a request. See module docs for the byte layout.
#[must_use]
pub fn canonical_pre_image(fields: SignedFields<'_>) -> Vec<u8> {
    build_pre_image(
        A2A_SIGNATURE_DOMAIN,
        &[
            ("request_id", fields.request_id.as_bytes()),
            (
                "caller_runtime_id",
                fields.caller_runtime_id.as_bytes(),
            ),
            (
                "caller_principal_did",
                fields.caller_principal_did.as_bytes(),
            ),
            (
                "target_principal_did",
                fields.target_principal_did.as_bytes(),
            ),
            ("message", fields.message.as_bytes()),
        ],
    )
}

/// Generic length-prefixed pre-image builder shared by every signed
/// envelope kind (issue #29 a2a, and PR #11 invite tokens).
///
/// Each field is preceded by its big-endian `u32` byte length, so
/// unambiguous parsing is guaranteed even if a field contains
/// arbitrary bytes (including null and `\n`). The leading `domain`
/// field is the domain-separation tag — embedding it as the first
/// length-prefixed entry makes it impossible for a signature over
/// one envelope kind to also validate against another kind (or
/// against a future envelope kind) even if someone manages to
/// construct a colliding suffix.
///
/// Field order is **part of the contract**. The signer and the
/// verifier must build the `fields` slice in the same order; the
/// resulting bytes are sensitive to permutation. Embedded in the
/// hub-side spec — change it and you break every token the runtime
/// has ever issued.
#[must_use]
pub fn build_pre_image(domain: &str, fields: &[(&str, &[u8])]) -> Vec<u8> {
    let mut estimated = domain.len() + 4; // domain + its length prefix
    for (k, v) in fields {
        estimated += k.len() + v.len() + 8; // name + value + 2 length prefixes
    }
    let mut out = Vec::with_capacity(estimated);

    push_lp(&mut out, domain);
    for (k, v) in fields {
        push_lp(&mut out, k);
        push_lp_bytes(&mut out, v);
    }
    out
}

/// Append a length-prefixed string. Length is big-endian `u32`. Strings
/// over 4 GiB are not signable — that's fine; an a2a payload that
/// large should have been rejected upstream long before reaching the
/// signer.
fn push_lp(out: &mut Vec<u8>, s: &str) {
    // u32 cast is checked: a `&str` longer than `u32::MAX` is
    // unrepresentable on any platform we target, but be explicit so a
    // future 128-bit platform doesn't silently truncate.
    let len =
        u32::try_from(s.len()).expect("signed field exceeds u32::MAX bytes; rejected upstream");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Append a length-prefixed byte slice. Mirrors [`push_lp`] but for
/// arbitrary bytes (invite tokens carry a JSON body that may include
/// non-UTF-8 sequences after base64-decoding some fields).
fn push_lp_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len())
        .expect("signed field exceeds u32::MAX bytes; rejected upstream");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Sign the canonical pre-image of `fields` with `signing_key` and
/// return the base64url-no-pad encoding suitable for the wire-form
/// `signature: String` field on `TunnelMessage::AgentToAgentRequest`.
///
/// Base64url-no-pad matches the encoding used for `RuntimeHello.signature`
/// and `TunnelChallengeAck.signature` already shipping over the tunnel
/// (see [crate::tunnel::client] auth path).
#[must_use]
pub fn sign_request(signing_key: &SigningKey, fields: SignedFields<'_>) -> String {
    sign_pre_image(signing_key, &canonical_pre_image(fields))
}

/// Sign an arbitrary pre-image (the inner format is the caller's
/// concern — see [`build_pre_image`]). Used by
/// [`crate::tunnel::invite_token::mint_token`] and any future
/// signed-envelope kind that wants to share the same base64url-no-pad
/// wire encoding.
#[must_use]
pub fn sign_pre_image(signing_key: &SigningKey, pre_image: &[u8]) -> String {
    let sig: Signature = signing_key.sign(pre_image);
    URL_SAFE_NO_PAD.encode(sig.to_bytes())
}

/// Verify a base64url-no-pad signature against the canonical pre-image
/// of `fields`. Returns `Ok(())` on a verified signature and a
/// structured error otherwise.
///
/// # Errors
///
/// - The signature string is not valid base64url-no-pad.
/// - The decoded signature is not 64 bytes (ed25519 signatures are 64
///   bytes by definition).
/// - The signature does not verify against the pre-image with this
///   verifying key.
pub fn verify_request(
    verifying_key: &VerifyingKey,
    fields: SignedFields<'_>,
    signature_b64url: &str,
) -> Result<()> {
    verify_pre_image(verifying_key, &canonical_pre_image(fields), signature_b64url)
        .context("a2a signature did not verify against the canonical pre-image")
}

/// Verify an arbitrary pre-image. Counterpart of [`sign_pre_image`];
/// the resulting `Result` is prefixed with the inner message; the
/// `context` is added by the caller (the a2a wrapper adds the
/// "a2a signature did not verify..." prefix; invite tokens add their
/// own).
pub fn verify_pre_image(
    verifying_key: &VerifyingKey,
    pre_image: &[u8],
    signature_b64url: &str,
) -> Result<()> {
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64url)
        .context("signature is not valid base64url-no-pad")?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("signature length is {} bytes; expected 64", v.len()))?;
    let signature = Signature::from_bytes(&sig_arr);
    verifying_key.verify(pre_image, &signature).map_err(|e| e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_identity::keys::KeyPair;

    fn sample_fields() -> SignedFields<'static> {
        SignedFields {
            request_id: "req-abc-123",
            caller_runtime_id: "did:key:zCaller",
            caller_principal_did: "did:peko:agent:caller-hash",
            target_principal_did: "did:peko:agent:target-hash",
            message: "review this",
        }
    }

    /// `canonical_pre_image` is byte-stable: two calls with the same
    /// inputs produce the same output. This is the foundational
    /// property the whole signature scheme relies on — if this ever
    /// regresses, every cross-runtime signature breaks.
    #[test]
    fn test_canonical_pre_image_is_deterministic() {
        let a = canonical_pre_image(sample_fields());
        let b = canonical_pre_image(sample_fields());
        assert_eq!(a, b);
    }

    /// The pre-image starts with the length-prefixed domain tag. The
    /// first 4 bytes are the BE length of `"a2a:v1"` (6 bytes); the
    /// next 6 bytes are the tag itself.
    #[test]
    fn test_canonical_pre_image_starts_with_domain_tag() {
        let bytes = canonical_pre_image(sample_fields());
        assert_eq!(
            &bytes[0..4],
            &(A2A_SIGNATURE_DOMAIN.len() as u32).to_be_bytes()
        );
        assert_eq!(
            &bytes[4..4 + A2A_SIGNATURE_DOMAIN.len()],
            A2A_SIGNATURE_DOMAIN.as_bytes()
        );
    }

    /// Changing any single field in `SignedFields` produces a
    /// different pre-image. Test each field individually so a future
    /// refactor that accidentally drops a field from the pre-image
    /// gets caught immediately.
    #[test]
    fn test_canonical_pre_image_mutates_with_each_field() {
        let base = canonical_pre_image(sample_fields());
        let cases: Vec<(&str, SignedFields<'_>)> = vec![
            (
                "request_id",
                SignedFields {
                    request_id: "different-req",
                    ..sample_fields()
                },
            ),
            (
                "caller_runtime_id",
                SignedFields {
                    caller_runtime_id: "did:key:zOther",
                    ..sample_fields()
                },
            ),
            (
                "caller_principal_did",
                SignedFields {
                    caller_principal_did: "did:peko:agent:other-caller",
                    ..sample_fields()
                },
            ),
            (
                "target_principal_did",
                SignedFields {
                    target_principal_did: "did:peko:agent:other-target",
                    ..sample_fields()
                },
            ),
            (
                "message",
                SignedFields {
                    message: "different message body",
                    ..sample_fields()
                },
            ),
        ];
        for (name, fields) in cases {
            assert_ne!(
                base,
                canonical_pre_image(fields),
                "changing {name} must change the pre-image"
            );
        }
    }

    /// Round-trip: a signature produced by `sign_request` verifies
    /// against the same fields with the matching public key.
    #[test]
    fn test_sign_verify_roundtrip() {
        let kp = KeyPair::generate();
        let sig = sign_request(&kp.signing_key, sample_fields());
        verify_request(&kp.verifying_key, sample_fields(), &sig).unwrap();
    }

    /// A signature does NOT verify if even one field changes. Trying
    /// each field individually pins the contract that the verifier
    /// catches tampering across the whole envelope, not just the
    /// trailing bytes.
    #[test]
    fn test_verify_rejects_field_tampering() {
        let kp = KeyPair::generate();
        let sig = sign_request(&kp.signing_key, sample_fields());

        let tampers: Vec<(&str, SignedFields<'_>)> = vec![
            (
                "request_id",
                SignedFields {
                    request_id: "evil-req",
                    ..sample_fields()
                },
            ),
            (
                "caller_runtime_id",
                SignedFields {
                    caller_runtime_id: "did:key:zEvil",
                    ..sample_fields()
                },
            ),
            (
                "caller_principal_did",
                SignedFields {
                    caller_principal_did: "did:peko:agent:evil-caller",
                    ..sample_fields()
                },
            ),
            (
                "target_principal_did",
                SignedFields {
                    target_principal_did: "did:peko:agent:evil-target",
                    ..sample_fields()
                },
            ),
            (
                "message",
                SignedFields {
                    message: "evil rewritten body",
                    ..sample_fields()
                },
            ),
        ];
        for (name, tampered) in tampers {
            verify_request(&kp.verifying_key, tampered, &sig).expect_err(&format!(
                "tampering with {name} must invalidate the signature"
            ));
        }
    }

    /// A signature produced with one key does NOT verify against
    /// another key. Together with the field-tampering test, this
    /// gives us the two halves of "signature binds caller+fields".
    #[test]
    fn test_verify_rejects_wrong_key() {
        let kp_caller = KeyPair::generate();
        let kp_other = KeyPair::generate();

        let sig = sign_request(&kp_caller.signing_key, sample_fields());

        verify_request(&kp_other.verifying_key, sample_fields(), &sig)
            .expect_err("signature must not verify against an unrelated key");
    }

    /// Malformed signatures (non-base64url, wrong byte length) return
    /// structured errors rather than panic. The verifier sits on the
    /// inbound dispatcher path, so a hostile peer sending garbage
    /// MUST produce a clean `Err`, never a panic that crashes the
    /// tunnel handler.
    #[test]
    fn test_verify_rejects_malformed_signature() {
        let kp = KeyPair::generate();

        // Not base64url at all.
        let err = verify_request(&kp.verifying_key, sample_fields(), "not%%base64!!")
            .expect_err("non-base64url signature must error");
        // Walk the error chain — the inner context lives one
        // level down (inside `verify_pre_image`) and is
        // shadowed by the outer a2a wrapper context.
        assert!(
            err.chain().any(|e| e.to_string().contains("base64url")),
            "error chain must mention base64url; got: {err}"
        );

        // Valid base64url but wrong length (e.g. 16 bytes instead of 64).
        let short_sig = URL_SAFE_NO_PAD.encode([0xab; 16]);
        let err = verify_request(&kp.verifying_key, sample_fields(), &short_sig)
            .expect_err("wrong-length signature must error");
        assert!(
            err.chain().any(|e| e.to_string().contains("length")),
            "error chain must mention length; got: {err}"
        );
    }
}
