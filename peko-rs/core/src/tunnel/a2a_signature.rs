//! Length-prefixed signature pre-image primitives shared by every
//! signed tunnel envelope kind.
//!
//! Sprint 3 Phase 12b note: this module used to also carry the
//! principal-to-principal A2A signer/verifier (`SignedFields`,
//! `sign_request`, `verify_request`). That stack retired when
//! principal-to-principal DM moved onto channels (the
//! `TunnelChannelEvent` / `TunnelChannelInvite` envelopes in
//! [`crate::tunnel::tunnel_channel_signature`]); what remains here is
//! the shared pre-image builder + the base64url-no-pad sign/verify
//! wrappers, consumed by `tunnel_channel_signature` and
//! [`crate::tunnel::invite_token`].
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

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Domain-separation tag of the retired v1 a2a pre-image
/// (`principal_to_principal_request`, removed in sprint 3 Phase 12b).
/// Retained so no future envelope kind reuses the tag — a signature
/// over one envelope kind must never validate against another.
pub const A2A_SIGNATURE_DOMAIN: &str = "a2a:v1";

/// Generic length-prefixed pre-image builder shared by every signed
/// envelope kind (tunnel channel events/invites, and PR #11 invite
/// tokens).
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

/// Verify an arbitrary pre-image. Counterpart of [`sign_pre_image`];
/// the resulting `Result` is prefixed with the inner message; the
/// `context` is added by the caller (the channel-signature wrapper
/// adds its own "did not verify" prefix; invite tokens add theirs).
///
/// # Errors
///
/// - The signature string is not valid base64url-no-pad.
/// - The decoded signature is not 64 bytes (ed25519 signatures are 64
///   bytes by definition).
/// - The signature does not verify against the pre-image with this
///   verifying key.
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

    fn sample_pre_image() -> Vec<u8> {
        build_pre_image(
            "test:v1",
            &[
                ("request_id", b"req-abc-123"),
                ("message", b"review this"),
            ],
        )
    }

    /// `build_pre_image` is byte-stable and starts with the
    /// length-prefixed domain tag — the foundational property every
    /// envelope kind built on it relies on.
    #[test]
    fn test_build_pre_image_is_deterministic_and_domain_tagged() {
        let a = sample_pre_image();
        let b = sample_pre_image();
        assert_eq!(a, b);
        let domain = "test:v1";
        assert_eq!(&a[0..4], &(domain.len() as u32).to_be_bytes());
        assert_eq!(&a[4..4 + domain.len()], domain.as_bytes());
    }

    /// Round-trip over the keeper primitives: `sign_pre_image` output
    /// verifies under `verify_pre_image` with the matching key, and
    /// fails against a tampered pre-image or an unrelated key.
    #[test]
    fn test_sign_verify_pre_image_roundtrip() {
        let kp = KeyPair::generate();
        let pre_image = sample_pre_image();
        let sig = sign_pre_image(&kp.signing_key, &pre_image);
        verify_pre_image(&kp.verifying_key, &pre_image, &sig).unwrap();

        let tampered = build_pre_image("test:v1", &[("request_id", b"req-evil")]);
        verify_pre_image(&kp.verifying_key, &tampered, &sig)
            .expect_err("tampered pre-image must not verify");

        let other = KeyPair::generate();
        verify_pre_image(&other.verifying_key, &pre_image, &sig)
            .expect_err("signature must not verify against an unrelated key");
    }

    /// Malformed signatures (non-base64url, wrong byte length) return
    /// structured errors rather than panic. The verifier sits on
    /// inbound dispatcher paths, so a hostile peer sending garbage
    /// MUST produce a clean `Err`, never a panic.
    #[test]
    fn test_verify_pre_image_rejects_malformed_signature() {
        let kp = KeyPair::generate();
        let pre_image = sample_pre_image();

        let err = verify_pre_image(&kp.verifying_key, &pre_image, "not%%base64!!")
            .expect_err("non-base64url signature must error");
        assert!(
            err.chain().any(|e| e.to_string().contains("base64url")),
            "error chain must mention base64url; got: {err}"
        );

        let short_sig = URL_SAFE_NO_PAD.encode([0xab; 16]);
        let err = verify_pre_image(&kp.verifying_key, &pre_image, &short_sig)
            .expect_err("wrong-length signature must error");
        assert!(
            err.chain().any(|e| e.to_string().contains("length")),
            "error chain must mention length; got: {err}"
        );
    }
}
