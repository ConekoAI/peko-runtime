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
//!   [4 bytes BE u32 = len(caller_agent_did)] || caller_agent_did
//!   [4 bytes BE u32 = len(target_agent_did)] || target_agent_did
//!   [4 bytes BE u32 = len(message)] || message
//!   [1 byte presence]
//!     if 0x01:  [4 bytes BE u32 = len(session_id)] || session_id
//!     if 0x00:  (nothing further)
//!   [1 byte presence]
//!     if 0x01:  [4 bytes BE u32 = len(team)] || team
//!     if 0x00:  (nothing further)
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
    pub caller_agent_did: &'a str,
    pub target_agent_did: &'a str,
    pub message: &'a str,
    pub session_id: Option<&'a str>,
    pub team: Option<&'a str>,
}

/// Build the deterministic pre-image bytes that get signed / verified
/// for an a2a request. See module docs for the byte layout.
#[must_use]
pub fn canonical_pre_image(fields: SignedFields<'_>) -> Vec<u8> {
    // Tight capacity hint: 1 domain + 4 required + up to 2 optional,
    // each with a 4-byte length prefix and a presence byte on the
    // optionals. Slightly over-allocates rather than reallocates.
    let estimated = A2A_SIGNATURE_DOMAIN.len()
        + fields.request_id.len()
        + fields.caller_runtime_id.len()
        + fields.caller_agent_did.len()
        + fields.target_agent_did.len()
        + fields.message.len()
        + fields.session_id.map_or(0, str::len)
        + fields.team.map_or(0, str::len)
        + (4 * 7) // length prefixes (domain + 4 required + 2 optional fields)
        + 2; // presence bytes
    let mut out = Vec::with_capacity(estimated);

    push_lp(&mut out, A2A_SIGNATURE_DOMAIN);
    push_lp(&mut out, fields.request_id);
    push_lp(&mut out, fields.caller_runtime_id);
    push_lp(&mut out, fields.caller_agent_did);
    push_lp(&mut out, fields.target_agent_did);
    push_lp(&mut out, fields.message);
    push_opt_lp(&mut out, fields.session_id);
    push_opt_lp(&mut out, fields.team);
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
    let len = u32::try_from(s.len())
        .expect("a2a signed field exceeds u32::MAX bytes; rejected upstream");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Append an optional length-prefixed string. The presence byte is 0x01
/// for `Some` (followed by a length-prefixed string) or 0x00 for `None`
/// (no following bytes). Critically, `Some("")` and `None` produce
/// *different* pre-images — the absence of a field is not the same as
/// the empty string for audit / replay purposes.
fn push_opt_lp(out: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(v) => {
            out.push(0x01);
            push_lp(out, v);
        }
        None => out.push(0x00),
    }
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
    let pre_image = canonical_pre_image(fields);
    let sig: Signature = signing_key.sign(&pre_image);
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
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64url)
        .context("a2a signature is not valid base64url-no-pad")?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("a2a signature length is {} bytes; expected 64", v.len()))?;
    let signature = Signature::from_bytes(&sig_arr);
    let pre_image = canonical_pre_image(fields);
    verifying_key
        .verify(&pre_image, &signature)
        .context("a2a signature did not verify against the canonical pre-image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::KeyPair;

    fn sample_fields() -> SignedFields<'static> {
        SignedFields {
            request_id: "req-abc-123",
            caller_runtime_id: "did:key:zCaller",
            caller_agent_did: "did:peko:agent:caller-hash",
            target_agent_did: "did:peko:agent:target-hash",
            message: "review this",
            session_id: Some("sess-xyz"),
            team: Some("default"),
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

    /// `Some("")` and `None` MUST produce different pre-images. This
    /// is the property that prevents a replay where a caller signs an
    /// `a2a` with `session_id = None` and an attacker re-routes it as
    /// `session_id = Some("")` (or vice versa), getting the receiver
    /// to attribute the call to the wrong session.
    #[test]
    fn test_canonical_pre_image_distinguishes_none_from_empty() {
        let none_session = SignedFields {
            session_id: None,
            ..sample_fields()
        };
        let empty_session = SignedFields {
            session_id: Some(""),
            ..sample_fields()
        };
        assert_ne!(
            canonical_pre_image(none_session),
            canonical_pre_image(empty_session),
            "Some(\"\") and None must NOT produce the same pre-image \
             (replay protection across the optional fields)"
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
                "caller_agent_did",
                SignedFields {
                    caller_agent_did: "did:peko:agent:other-caller",
                    ..sample_fields()
                },
            ),
            (
                "target_agent_did",
                SignedFields {
                    target_agent_did: "did:peko:agent:other-target",
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
            (
                "session_id",
                SignedFields {
                    session_id: Some("different-sess"),
                    ..sample_fields()
                },
            ),
            (
                "team",
                SignedFields {
                    team: Some("different-team"),
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
                "caller_agent_did",
                SignedFields {
                    caller_agent_did: "did:peko:agent:evil-caller",
                    ..sample_fields()
                },
            ),
            (
                "target_agent_did",
                SignedFields {
                    target_agent_did: "did:peko:agent:evil-target",
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
            (
                "session_id",
                SignedFields {
                    session_id: None,
                    ..sample_fields()
                },
            ),
            (
                "team",
                SignedFields {
                    team: None,
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
        assert!(
            err.to_string().contains("base64url"),
            "error must mention base64url; got: {err}"
        );

        // Valid base64url but wrong length (e.g. 16 bytes instead of 64).
        let short_sig = URL_SAFE_NO_PAD.encode([0xab; 16]);
        let err = verify_request(&kp.verifying_key, sample_fields(), &short_sig)
            .expect_err("wrong-length signature must error");
        assert!(
            err.to_string().contains("length"),
            "error must mention length; got: {err}"
        );
    }

    /// `Some("")` and `None` produce signatures that DO NOT
    /// cross-verify — the wire-level replay-distinction test from
    /// `test_canonical_pre_image_distinguishes_none_from_empty`
    /// elevated to the signature layer, where it actually matters.
    #[test]
    fn test_signature_distinguishes_none_from_empty_optional_fields() {
        let kp = KeyPair::generate();
        let with_none = SignedFields {
            session_id: None,
            team: None,
            ..sample_fields()
        };
        let with_empty = SignedFields {
            session_id: Some(""),
            team: Some(""),
            ..sample_fields()
        };

        let sig_none = sign_request(&kp.signing_key, with_none);
        let sig_empty = sign_request(&kp.signing_key, with_empty);

        // The signatures themselves differ (different pre-images), and
        // neither verifies against the other's fields.
        assert_ne!(sig_none, sig_empty);
        verify_request(&kp.verifying_key, with_empty, &sig_none)
            .expect_err("None signature must not verify against Some(\"\")");
        verify_request(&kp.verifying_key, with_none, &sig_empty)
            .expect_err("Some(\"\") signature must not verify against None");
    }
}
