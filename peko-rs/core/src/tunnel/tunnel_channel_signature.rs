//! Cross-runtime channel-event signature — peko-channel cross-runtime PR-A.
//!
//! Signing and verification for `TunnelMessage::TunnelChannelEvent`.
//! Mirrors [`crate::tunnel::a2a_signature`] exactly: ed25519 over a
//! deterministic length-prefixed pre-image, base64url-no-pad on the wire.
//!
//! peko-channel cross-runtime PR-3a commit 1 adds the parallel
//! `sign_channel_invite` / `verify_channel_invite` helpers for
//! `TunnelMessage::TunnelChannelInvite` — separate helpers (not
//! sharing code) keep the call sites self-documenting and the
//! domain tags distinct (collisions would be catastrophic).
//!
//! ## Why a separate module (not reuse `a2a_signature`)
//!
//! The two envelopes are semantically distinct (1:1 request/response
//! vs. N-way push fan-out) and use different domain tags. Sharing the
//! signer struct would force the call sites to remember which domain
//! they meant; splitting them makes the call site self-documenting.
//! The generic length-prefixed pre-image builder IS shared
//! (`a2a_signature::build_pre_image`).
//!
//! ## Field order
//!
//! Pre-image bytes are, in order:
//!
//! ```text
//!   [4 bytes BE u32 = len("channel:v1")] || "channel:v1"
//!   [4 bytes BE u32 = len(request_id)] || request_id
//!   [4 bytes BE u32 = len(source_runtime_id)] || source_runtime_id
//!   [4 bytes BE u32 = len(recipient_runtime_id)] || recipient_runtime_id
//!   [4 bytes BE u32 = len(source_principal_did)] || source_principal_did
//!   [4 bytes BE u32 = len(channel_id)] || channel_id
//!   [4 bytes BE u32 = len(event_bytes)] || event_bytes
//! ```
//!
//! `event_bytes` is the **pre-serialized** JSON form of the
//! `ChannelEvent` payload (the caller serializes once via
//! `serde_json::to_vec(&event)` and passes the same bytes to both the
//! outbound `sign_channel_event` and the inbound `verify_channel_event`
//! paths). Using pre-serialized bytes avoids any risk of
//! `serde_json` reordering enum-variant fields differently across
//! versions — the bytes that get signed are the bytes that get
//! verified, byte-for-byte.
//!
//! The leading `"channel:v1"` domain-separation tag makes it
//! impossible for a signature over a channel event to also validate
//! against a future envelope kind (or against an a2a signature with
//! a colliding suffix).
//!
//! Field order is **part of the contract**. The signer and the
//! verifier must build the `fields` slice in the same order; the
//! resulting bytes are sensitive to permutation. Adding a field
//! here without updating the pre-image (or vice versa) silently
//! breaks every signature.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::tunnel::a2a_signature::build_pre_image;

/// Domain-separation tag for the v1 channel-event pre-image. Embedded
/// in every signed pre-image as the first length-prefixed field so a
/// signature over a channel event cannot collide with a signature
/// over any other envelope kind (existing or future).
pub const CHANNEL_SIGNATURE_DOMAIN: &str = "channel:v1";

/// Domain-separation tag for the v1 channel-invite pre-image
/// (peko-channel cross-runtime PR-3a commit 1). Distinct from
/// `CHANNEL_SIGNATURE_DOMAIN` so a signature over a channel event
/// cannot be replayed as a channel invite or vice versa. Embedded
/// in every signed pre-image as the first length-prefixed field.
pub const CHANNEL_INVITE_SIGNATURE_DOMAIN: &str = "channel-invite:v1";

/// Borrowed view of the fields that are signed in a
/// `TunnelChannelEvent`. Holding references means callers can build
/// the view from the `TunnelMessage` variant without cloning every
/// field; the verifier on the inbound path constructs the same view
/// from the deserialized message.
///
/// Keep this list **byte-for-byte identical** to the
/// `canonical_pre_image` field order — adding a field here without
/// updating the pre-image (or vice versa) silently breaks every
/// signature.
#[derive(Debug, Clone, Copy)]
pub struct ChannelSignedFields<'a> {
    pub request_id: &'a str,
    pub source_runtime_id: &'a str,
    /// Recipient runtime the hub will route the envelope to. Signed
    /// so a compromised hub cannot silently redirect an event to a
    /// different runtime than the source intended. Added by
    /// peko-channel cross-runtime PR-B commit 4.
    pub recipient_runtime_id: &'a str,
    pub source_principal_did: &'a str,
    pub channel_id: &'a str,
    /// Pre-serialized bytes of the channel event. Caller is
    /// responsible for serializing via `serde_json::to_vec(&event)`
    /// once and passing the same bytes to both sign and verify
    /// paths.
    pub event_bytes: &'a [u8],
}

/// Build the deterministic pre-image bytes that get signed /
/// verified for a channel event. See module docs for the byte
/// layout.
#[must_use]
pub fn canonical_pre_image(fields: ChannelSignedFields<'_>) -> Vec<u8> {
    build_pre_image(
        CHANNEL_SIGNATURE_DOMAIN,
        &[
            ("request_id", fields.request_id.as_bytes()),
            ("source_runtime_id", fields.source_runtime_id.as_bytes()),
            ("recipient_runtime_id", fields.recipient_runtime_id.as_bytes()),
            (
                "source_principal_did",
                fields.source_principal_did.as_bytes(),
            ),
            ("channel_id", fields.channel_id.as_bytes()),
            ("event", fields.event_bytes),
        ],
    )
}

/// Sign the canonical pre-image of `fields` with `signing_key` and
/// return the base64url-no-pad encoding suitable for the wire-form
/// `signature: String` field on `TunnelMessage::TunnelChannelEvent`.
///
/// The caller is responsible for serializing `event` once via
/// `serde_json::to_vec(&event)` and passing the resulting bytes
/// through `ChannelSignedFields.event_bytes` — the signer does not
/// re-serialize (that would risk key-order drift across serde_json
/// versions).
#[must_use]
pub fn sign_channel_event(
    signing_key: &SigningKey,
    fields: ChannelSignedFields<'_>,
) -> String {
    let pre_image = canonical_pre_image(fields);
    let sig: Signature = signing_key.sign(&pre_image);
    URL_SAFE_NO_PAD.encode(sig.to_bytes())
}

/// Verify a base64url-no-pad signature against the canonical
/// pre-image of `fields`. Returns `Ok(())` on a verified signature
/// and a structured error otherwise.
///
/// The caller MUST pass the same `event_bytes` that were signed
/// (re-serializing on the inbound path risks drift). In practice,
/// the inbound `dispatcher` deserializes the envelope, then re-serializes
/// the `event` field via `serde_json::to_vec(&event)` (serde_json
/// emits struct fields in declaration order, so the round-trip is
/// stable) before calling this function.
///
/// # Errors
///
/// - The signature string is not valid base64url-no-pad.
/// - The decoded signature is not 64 bytes (ed25519 signatures are
///   64 bytes by definition).
/// - The signature does not verify against the pre-image with this
///   verifying key.
pub fn verify_channel_event(
    verifying_key: &VerifyingKey,
    fields: ChannelSignedFields<'_>,
    signature_b64url: &str,
) -> Result<()> {
    let pre_image = canonical_pre_image(fields);
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64url)
        .map_err(|e| anyhow!("signature is not valid base64url-no-pad: {e}"))?;
    if sig_bytes.len() != 64 {
        return Err(anyhow!(
            "signature is {} bytes; expected 64 (ed25519)",
            sig_bytes.len()
        ));
    }
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| {
            anyhow!("signature length is {} bytes; expected 64", v.len())
        })?;
    let sig = Signature::from_bytes(&sig_arr);
    verifying_key
        .verify(&pre_image, &sig)
        .context("channel event signature did not verify against the canonical pre-image")
}

// ===========================================================================
// Channel-invite helpers — peko-channel cross-runtime PR-3a commit 1.
//
// Sign/verify for `TunnelMessage::TunnelChannelInvite`. Pre-image:
// `request_id || source_runtime_id || recipient_runtime_id ||
// source_principal_did || channel_id || creator || creator_did || name ||
// passive_binding || JSON(initial_members)`. (`passive_binding` signs as
// the empty string when the envelope carries `None` — Phase 12a.)
//
// Why a separate module (vs. sharing `sign_channel_event`):
//
// - Different domain tag (collisions would be catastrophic).
// - Different field set (no `event_bytes`, has `creator`/`name`).
// - Keeps call sites self-documenting (`sign_channel_invite` vs.
//   `sign_channel_event`) — the kind of envelope being signed is
//   visible at the use site.
// ===========================================================================

/// Borrowed view of the fields that are signed in a
/// `TunnelChannelInvite`. Holding references means callers can build
/// the view from the `TunnelMessage` variant without cloning every
/// field; the verifier on the inbound path constructs the same view
/// from the deserialized message.
///
/// Keep this list **byte-for-byte identical** to the
/// `invite_canonical_pre_image` field order — adding a field here
/// without updating the pre-image (or vice versa) silently breaks
/// every signature.
#[derive(Debug, Clone, Copy)]
pub struct ChannelInviteSignedFields<'a> {
    pub request_id: &'a str,
    pub source_runtime_id: &'a str,
    /// Recipient runtime the hub will route the envelope to. Signed
    /// so a compromised hub cannot silently redirect an invite to a
    /// different runtime than the source intended.
    pub recipient_runtime_id: &'a str,
    pub source_principal_did: &'a str,
    pub channel_id: &'a str,
    /// The creator's display name (principal DID, e.g. `prin_alice`).
    /// Snapshotted from the source runtime's `meta.json` at invite
    /// time so the receiver can populate its local mirror without a
    /// follow-up `peek` round-trip.
    pub creator: &'a str,
    /// The creator principal's stable DID (Phase 12a) — the receiver
    /// names its peer child for the creator from this. Signed so a
    /// hub cannot silently reattribute the DM to a different peer.
    pub creator_did: &'a str,
    /// Human-readable channel name (`team`, `general`, etc.).
    pub name: &'a str,
    /// DM marker (Phase 12a): the source channel's `passive_binding`,
    /// or the EMPTY STRING when the channel is unbound. Only the
    /// presence/absence is meaningful to the receiver (it derives its
    /// own binding); the value is signed so a hub cannot strip or
    /// forge the DM-ness of an invite.
    pub passive_binding: &'a str,
    /// Pre-serialized bytes of the `initial_members` list. Caller
    /// is responsible for serializing via
    /// `serde_json::to_vec(&initial_members)` once and passing the
    /// same bytes to both sign and verify paths. Using pre-serialized
    /// bytes avoids any risk of `serde_json` reordering array items
    /// differently across versions — the bytes that get signed are
    /// the bytes that get verified, byte-for-byte.
    pub initial_members_bytes: &'a [u8],
}

/// Build the deterministic pre-image bytes that get signed /
/// verified for a channel invite. See module docs for the byte
/// layout.
#[must_use]
pub fn invite_canonical_pre_image(fields: ChannelInviteSignedFields<'_>) -> Vec<u8> {
    build_pre_image(
        CHANNEL_INVITE_SIGNATURE_DOMAIN,
        &[
            ("request_id", fields.request_id.as_bytes()),
            ("source_runtime_id", fields.source_runtime_id.as_bytes()),
            ("recipient_runtime_id", fields.recipient_runtime_id.as_bytes()),
            (
                "source_principal_did",
                fields.source_principal_did.as_bytes(),
            ),
            ("channel_id", fields.channel_id.as_bytes()),
            ("creator", fields.creator.as_bytes()),
            ("creator_did", fields.creator_did.as_bytes()),
            ("name", fields.name.as_bytes()),
            ("passive_binding", fields.passive_binding.as_bytes()),
            ("initial_members", fields.initial_members_bytes),
        ],
    )
}

/// Sign the canonical pre-image of `fields` with `signing_key` and
/// return the base64url-no-pad encoding suitable for the wire-form
/// `signature: String` field on `TunnelMessage::TunnelChannelInvite`.
///
/// The caller is responsible for serializing `initial_members` once
/// via `serde_json::to_vec(&members)` and passing the resulting
/// bytes through `ChannelInviteSignedFields.initial_members_bytes` —
/// the signer does not re-serialize (that would risk key-order drift
/// across serde_json versions).
#[must_use]
pub fn sign_channel_invite(
    signing_key: &SigningKey,
    fields: ChannelInviteSignedFields<'_>,
) -> String {
    let pre_image = invite_canonical_pre_image(fields);
    let sig: Signature = signing_key.sign(&pre_image);
    URL_SAFE_NO_PAD.encode(sig.to_bytes())
}

/// Verify a base64url-no-pad signature against the canonical
/// pre-image of `fields`. Returns `Ok(())` on a verified signature
/// and a structured error otherwise.
///
/// The caller MUST pass the same `initial_members_bytes` that were
/// signed (re-serializing on the inbound path risks drift). In
/// practice, the inbound `dispatcher` deserializes the envelope,
/// then re-serializes the `initial_members` field via
/// `serde_json::to_vec(&members)` (serde_json emits vec items in
/// declaration order, so the round-trip is stable) before calling
/// this function.
///
/// # Errors
///
/// - The signature string is not valid base64url-no-pad.
/// - The decoded signature is not 64 bytes (ed25519 signatures are
///   64 bytes by definition).
/// - The signature does not verify against the pre-image with this
///   verifying key.
pub fn verify_channel_invite(
    verifying_key: &VerifyingKey,
    fields: ChannelInviteSignedFields<'_>,
    signature_b64url: &str,
) -> Result<()> {
    let pre_image = invite_canonical_pre_image(fields);
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64url)
        .map_err(|e| anyhow!("signature is not valid base64url-no-pad: {e}"))?;
    if sig_bytes.len() != 64 {
        return Err(anyhow!(
            "signature is {} bytes; expected 64 (ed25519)",
            sig_bytes.len()
        ));
    }
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| {
            anyhow!("signature length is {} bytes; expected 64", v.len())
        })?;
    let sig = Signature::from_bytes(&sig_arr);
    verifying_key
        .verify(&pre_image, &sig)
        .context("channel invite signature did not verify against the canonical pre-image")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use peko_identity::keys::KeyPair;
    use peko_protocol::channel::InitialMember;

    /// Round-trip: a generated keypair signs a `ChannelSignedFields`
    /// and the matching verifying key verifies it.
    #[test]
    fn test_sign_then_verify_round_trip() {
        let kp = KeyPair::generate();
        let signing_key: &SigningKey = &kp.signing_key;
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let fields = ChannelSignedFields {
            request_id: "chan-evt-1",
            source_runtime_id: "did:key:zRuntimeA",
            recipient_runtime_id: "did:key:zRuntimeB",
            source_principal_did: "prin_alice",
            channel_id: "chan_abcdefgh",
            event_bytes,
        };
        let signature = sign_channel_event(signing_key, fields);
        let verified = verify_channel_event(&kp.verifying_key, fields, &signature);
        assert!(verified.is_ok(), "freshly signed event must verify: {verified:?}");
    }

    /// A tampered `event_bytes` fails verification. This is the
    /// defense-in-depth property: a hub that re-encodes the JSON
    /// differently (different field order, trailing whitespace,
    /// etc.) breaks the signature and the receiver rejects the
    /// event.
    #[test]
    fn test_tampered_event_fails_verification() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let signature = sign_channel_event(
            &kp.signing_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes,
            },
        );

        // Tamper: change the event bytes on the verify path.
        let tampered = br#"{"kind":"posted","text":"goodbye"}"#;
        let result = verify_channel_event(
            &kp.verifying_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes: tampered,
            },
            &signature,
        );
        assert!(
            result.is_err(),
            "tampered event_bytes must not verify; got: {:?}",
            result
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("did not verify"),
            "error must name the condition; got: {msg}"
        );
    }

    /// Wrong verifying key fails verification. (Equivalent to the
    /// tampered-bytes case from the caller's perspective: ed25519
    /// determinism.)
    #[test]
    fn test_wrong_verifying_key_fails_verification() {
        let kp_signer = KeyPair::generate();
        let kp_verifier = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let signature = sign_channel_event(
            &kp_signer.signing_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes,
            },
        );
        let result = verify_channel_event(
            &kp_verifier.verifying_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes,
            },
            &signature,
        );
        assert!(result.is_err(), "wrong verifying key must fail");
    }

    /// Malformed signature (not base64url) surfaces a structured
    /// error rather than panicking.
    #[test]
    fn test_malformed_signature_errors_loudly() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let result = verify_channel_event(
            &kp.verifying_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes,
            },
            "not-base64url-!!!",
        );
        let err = result.expect_err("malformed signature must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("base64url"),
            "error must name the encoding; got: {msg}"
        );
    }

    /// A signature of the wrong length (decoded) is rejected
    /// before the verifier is asked to do any work.
    #[test]
    fn test_wrong_length_signature_is_rejected() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        // base64url-no-pad of 32 zero bytes — exactly half the
        // expected 64-byte ed25519 signature.
        let bogus = URL_SAFE_NO_PAD.encode([0u8; 32]);
        let result = verify_channel_event(
            &kp.verifying_key,
            ChannelSignedFields {
                request_id: "chan-evt-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                event_bytes,
            },
            &bogus,
        );
        let err = result.expect_err("32-byte signature must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("expected 64"),
            "error must name the length expectation; got: {msg}"
        );
    }

    // =====================================================================
    // Channel-invite tests — peko-channel cross-runtime PR-3a commit 1.
    //
    // Mirror the event-signing suite's coverage: round-trip,
    // tampered-members, tampered-channel-id, and a wrong-key check.
    // The tampered-bytes case is the most important: it pins the
    // invariant that a hub cannot rewrite `initial_members` (e.g.
    // demote a remote runtime membership to local) without breaking
    // the signature.
    // =====================================================================

    /// Helper: build a representative `initial_members` snapshot
    /// (one local, one remote) and serialize it once. Tests share
    /// this so they exercise the same byte-for-byte pre-image.
    fn sample_initial_members_bytes() -> Vec<u8> {
        let members = vec![
            InitialMember {
                principal_did: "prin_alice".to_string(),
                runtime_id: None,
            },
            InitialMember {
                principal_did: "prin_bob".to_string(),
                runtime_id: Some("did:key:zRuntimeB".to_string()),
            },
        ];
        serde_json::to_vec(&members).expect("InitialMember round-trips through serde_json")
    }

    /// Round-trip: a generated keypair signs a
    /// `ChannelInviteSignedFields` and the matching verifying key
    /// verifies it.
    #[test]
    fn test_sign_then_verify_invite_round_trip() {
        let kp = KeyPair::generate();
        let signing_key: &SigningKey = &kp.signing_key;
        let members_bytes = sample_initial_members_bytes();
        let fields = ChannelInviteSignedFields {
            request_id: "chan-invite-1",
            source_runtime_id: "did:key:zRuntimeA",
            recipient_runtime_id: "did:key:zRuntimeB",
            source_principal_did: "prin_alice",
            channel_id: "chan_abcdefgh",
            creator: "prin_alice",
            creator_did: "did:peko:principal:alice",
            name: "team-chat",
            passive_binding: "",
            initial_members_bytes: &members_bytes,
        };
        let signature = sign_channel_invite(signing_key, fields);
        let verified = verify_channel_invite(&kp.verifying_key, fields, &signature);
        assert!(verified.is_ok(), "freshly signed invite must verify: {verified:?}");
    }

    /// A tampered `initial_members_bytes` fails verification. Pins
    /// the security invariant: a hub cannot silently rewrite the
    /// membership snapshot (e.g. promote a local principal to
    /// remote by faking a `runtime_id`) without breaking the
    /// signature.
    #[test]
    fn test_tampered_members_fails_verification() {
        let kp = KeyPair::generate();
        let members_bytes = sample_initial_members_bytes();
        let signature = sign_channel_invite(
            &kp.signing_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &members_bytes,
            },
        );

        // Tamper: rewrite the membership so the local-only row now
        // pretends to be on a different host. The pre-image bytes
        // change → the signature must no longer verify.
        let tampered = serde_json::to_vec(&vec![
            InitialMember {
                principal_did: "prin_alice".to_string(),
                runtime_id: Some("did:key:zRuntimeAttacker".to_string()),
            },
            InitialMember {
                principal_did: "prin_bob".to_string(),
                runtime_id: Some("did:key:zRuntimeB".to_string()),
            },
        ])
        .unwrap();
        let result = verify_channel_invite(
            &kp.verifying_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &tampered,
            },
            &signature,
        );
        assert!(
            result.is_err(),
            "tampered initial_members must not verify; got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("did not verify"),
            "error must name the condition; got: {msg}"
        );
    }

    /// A tampered `channel_id` fails verification. Pins the
    /// invariant that an invite is bound to the channel it was
    /// minted for — a hub cannot redirect an invite to bootstrap a
    /// different channel on the receiver.
    #[test]
    fn test_tampered_channel_id_fails_verification() {
        let kp = KeyPair::generate();
        let members_bytes = sample_initial_members_bytes();
        let signature = sign_channel_invite(
            &kp.signing_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &members_bytes,
            },
        );

        let result = verify_channel_invite(
            &kp.verifying_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                // Different channel id → different pre-image.
                channel_id: "chan_zzzzzzzz",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &members_bytes,
            },
            &signature,
        );
        assert!(
            result.is_err(),
            "tampered channel_id must not verify; got: {result:?}"
        );
    }

    /// Wrong verifying key fails verification. Defends against a
    /// compromised hub substituting a different runtime's
    /// verifying key on the inbound path.
    #[test]
    fn test_wrong_verifying_key_fails_invite_verification() {
        let kp_signer = KeyPair::generate();
        let kp_verifier = KeyPair::generate();
        let members_bytes = sample_initial_members_bytes();
        let signature = sign_channel_invite(
            &kp_signer.signing_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &members_bytes,
            },
        );
        let result = verify_channel_invite(
            &kp_verifier.verifying_key,
            ChannelInviteSignedFields {
                request_id: "chan-invite-1",
                source_runtime_id: "did:key:zRuntimeA",
                recipient_runtime_id: "did:key:zRuntimeB",
                source_principal_did: "prin_alice",
                channel_id: "chan_abcdefgh",
                creator: "prin_alice",
                creator_did: "did:peko:principal:alice",
                name: "team-chat",
                passive_binding: "",
                initial_members_bytes: &members_bytes,
            },
            &signature,
        );
        assert!(result.is_err(), "wrong verifying key must fail");
    }
}