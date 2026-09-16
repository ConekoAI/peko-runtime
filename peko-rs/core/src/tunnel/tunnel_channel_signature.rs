//! Cross-runtime channel-envelope signatures — ADR-058 D2/D3 (v2).
//!
//! Signing and verification for `TunnelMessage::TunnelChannelEvent` and
//! `TunnelMessage::TunnelChannelInvite`. The v2 wire format replaces
//! the v1 bespoke length-prefixed pre-image with **JWS compact
//! serialization** (RFC 7515, EdDSA / RFC 8037, embedded payload):
//!
//! - **D3** — every signature is a compact JWS over a JSON payload
//!   carrying `iat` / `exp`, so the signing input
//!   (`b64u(header) || "." || b64u(payload)`) is canonical by
//!   construction even though the hub `JSON.parse`→`stringify`
//!   round-trips every relayed frame. `iat`/`exp` close the replay
//!   window the bounded FIFO dedupe cache used to cover alone.
//! - **D2** — each envelope carries TWO JWS over the SAME payload
//!   segment: the source *runtime* counter-signature (verified against
//!   the `did:key`-derived `source_runtime_id` key, as v1) and the
//!   *author* signature produced by the authoring principal's own key
//!   (ADR-058 D1; verified against the key embedded in
//!   `source_principal_did` when it is a `did:key`). The receiver
//!   additionally requires the author's payload segment to be
//!   byte-identical to the runtime's, binding the two signatures to
//!   one payload.
//!
//! Interop with the hub's TypeScript `jose` implementation is pinned
//! by the ADR-058 verification spike: header
//! `{"alg":"EdDSA","typ":"JWS"}`, base64url-no-pad everywhere, signing
//! input `ASCII(b64u(header) + "." + b64u(payload))`.
//!
//! ## Why a separate module (not reuse `a2a_signature`)
//!
//! The a2a request/response envelopes use their own domain-tagged
//! pre-image and are untouched by ADR-058. Keeping the channel v2
//! format self-contained makes the call sites self-documenting and
//! keeps the two envelope families free to evolve independently.

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// JWS header used for every v2 signature. Hardcoded (not reserialized)
/// so the base64url header segment is byte-identical to the hub's
/// `jose` output — a canonical pre-image is mandatory because the hub
/// re-encodes relayed frames (ADR-058 D3).
const JWS_HEADER_JSON: &str = r#"{"alg":"EdDSA","typ":"JWS"}"#;

/// Payload version tag for `ChannelEventPayload`. Bumped from v1 (the
/// bespoke pre-image) to `/2` for the JWS format; the verifier
/// rejects any other value so a v1 signature can never be replayed
/// into the v2 path.
pub const CHANNEL_EVENT_PAYLOAD_VERSION: &str = "peko-channel-event/2";

/// Payload version tag for `ChannelInvitePayload`. See
/// [`CHANNEL_EVENT_PAYLOAD_VERSION`].
pub const CHANNEL_INVITE_PAYLOAD_VERSION: &str = "peko-channel-invite/2";

/// Clock-skew leeway (seconds) applied to `exp`: an envelope is
/// rejected when `now > exp + EXP_LEEWAY_SECS`.
const EXP_LEEWAY_SECS: i64 = 30;

/// Clock-skew allowance (seconds) applied to `iat`: an envelope is
/// rejected when `iat > now + IAT_FUTURE_SECS`.
const IAT_FUTURE_SECS: i64 = 60;

/// Sign `payload` and return the JWS compact serialization with the
/// payload embedded (header `{"alg":"EdDSA","typ":"JWS"}`).
#[must_use]
pub fn jws_sign(signing_key: &SigningKey, payload: &[u8]) -> String {
    let header_seg = URL_SAFE_NO_PAD.encode(JWS_HEADER_JSON.as_bytes());
    let payload_seg = URL_SAFE_NO_PAD.encode(payload);
    let signing_input = format!("{header_seg}.{payload_seg}");
    let sig: Signature = signing_key.sign(signing_input.as_bytes());
    format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

/// Verify a compact JWS with an embedded payload. Returns the payload
/// segment (base64url, for cross-signature binding checks) and the
/// decoded payload bytes.
///
/// # Errors
///
/// - The compact form does not have exactly 3 segments.
/// - The header is not valid base64url / JSON, or its `alg` is not
///   `"EdDSA"`.
/// - The signature segment is not valid base64url or not 64 bytes.
/// - The signature does not verify (`verify_strict`).
pub fn jws_verify(verifying_key: &VerifyingKey, compact: &str) -> Result<(String, Vec<u8>)> {
    let segments: Vec<&str> = compact.split('.').collect();
    if segments.len() != 3 {
        return Err(anyhow!(
            "compact JWS must have exactly 3 segments; got {}",
            segments.len()
        ));
    }
    let (header_seg, payload_seg, sig_seg) = (segments[0], segments[1], segments[2]);

    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_seg)
        .map_err(|e| anyhow!("JWS header is not valid base64url-no-pad: {e}"))?;
    let header: serde_json::Value = serde_json::from_slice(&header_bytes)
        .map_err(|e| anyhow!("JWS header is not valid JSON: {e}"))?;
    let alg = header
        .get("alg")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("JWS header is missing the `alg` field"))?;
    if alg != "EdDSA" {
        return Err(anyhow!(
            "JWS header alg must be \"EdDSA\" (algorithm-confusion guard); got: {alg:?}"
        ));
    }

    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_seg)
        .map_err(|e| anyhow!("JWS signature is not valid base64url-no-pad: {e}"))?;
    if sig_bytes.len() != 64 {
        return Err(anyhow!(
            "JWS signature is {} bytes; expected 64 (ed25519)",
            sig_bytes.len()
        ));
    }
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("JWS signature length is {} bytes; expected 64", v.len()))?;
    let sig = Signature::from_bytes(&sig_arr);

    let signing_input = format!("{header_seg}.{payload_seg}");
    verifying_key
        .verify_strict(signing_input.as_bytes(), &sig)
        .context("JWS signature did not verify against the signing input")?;

    let payload = URL_SAFE_NO_PAD
        .decode(payload_seg)
        .map_err(|e| anyhow!("JWS payload is not valid base64url-no-pad: {e}"))?;
    Ok((payload_seg.to_string(), payload))
}

/// Reject payloads outside their freshness window (ADR-058 D3):
/// `now > exp + leeway` or `iat > now + allowance`.
fn check_freshness(iat: i64, exp: i64) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    if now > exp + EXP_LEEWAY_SECS {
        return Err(anyhow!(
            "signed payload is expired (exp={exp}, now={now}, leeway={EXP_LEEWAY_SECS}s)"
        ));
    }
    if iat > now + IAT_FUTURE_SECS {
        return Err(anyhow!(
            "signed payload's iat is in the future (iat={iat}, now={now}, allowance={IAT_FUTURE_SECS}s)"
        ));
    }
    Ok(())
}

/// Borrowed view of the fields that go into a `ChannelEventPayload`.
/// Holding references means callers can build the view from the
/// `TunnelMessage` variant without cloning every field.
///
/// `event_bytes` is the **pre-serialized** JSON form of the
/// `ChannelEvent` payload (the caller serializes once via
/// `serde_json::to_vec(&event)`); it rides the payload as
/// `event_b64u` (base64url-no-pad) so the bytes that get signed are
/// the bytes that get verified, byte-for-byte.
#[derive(Debug, Clone, Copy)]
pub struct ChannelSignedFields<'a> {
    pub request_id: &'a str,
    pub source_runtime_id: &'a str,
    /// Recipient runtime the hub will route the envelope to. Signed
    /// so a compromised hub cannot silently redirect an event to a
    /// different runtime than the source intended.
    pub recipient_runtime_id: &'a str,
    pub source_principal_did: &'a str,
    pub channel_id: &'a str,
    /// Pre-serialized bytes of the channel event. Caller is
    /// responsible for serializing via `serde_json::to_vec(&event)`
    /// once and passing the same bytes to both sign and verify
    /// paths.
    pub event_bytes: &'a [u8],
}

/// The signed payload of a v2 `TunnelChannelEvent` envelope (ADR-058
/// D2/D3). Serialized to JSON and embedded as the JWS payload of BOTH
/// the runtime counter-signature and the author signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEventPayload {
    /// Payload version tag — must equal
    /// [`CHANNEL_EVENT_PAYLOAD_VERSION`].
    pub v: String,
    pub request_id: String,
    pub source_runtime_id: String,
    pub recipient_runtime_id: String,
    pub source_principal_did: String,
    pub channel_id: String,
    /// base64url-no-pad of the serialized `ChannelEvent` bytes.
    pub event_b64u: String,
    /// Issued-at (Unix seconds, UTC).
    pub iat: i64,
    /// Expiry (Unix seconds, UTC).
    pub exp: i64,
}

/// Sign `fields` into a v2 payload and produce BOTH signatures over
/// the same embedded payload segment: the runtime counter-signature
/// and — when `author_key` is `Some` — the author signature (ADR-058
/// D2). Returns `(runtime_compact_jws, author_compact_jws)`; the
/// author signature is the empty string when `author_key` is `None`
/// (legacy runtime-vouched path).
///
/// `iat` / `exp` are Unix seconds (UTC); the sender uses
/// `iat = now`, `exp = now + 300`.
#[must_use]
pub fn sign_channel_event(
    runtime_key: &SigningKey,
    author_key: Option<&SigningKey>,
    fields: ChannelSignedFields<'_>,
    iat: i64,
    exp: i64,
) -> (String, String) {
    let payload = ChannelEventPayload {
        v: CHANNEL_EVENT_PAYLOAD_VERSION.to_string(),
        request_id: fields.request_id.to_string(),
        source_runtime_id: fields.source_runtime_id.to_string(),
        recipient_runtime_id: fields.recipient_runtime_id.to_string(),
        source_principal_did: fields.source_principal_did.to_string(),
        channel_id: fields.channel_id.to_string(),
        event_b64u: URL_SAFE_NO_PAD.encode(fields.event_bytes),
        iat,
        exp,
    };
    // serde_json cannot fail on a plain data struct; both JWS embed
    // the SAME payload bytes so their payload segments are
    // byte-identical (the D2 binding).
    let payload_bytes = serde_json::to_vec(&payload).expect("ChannelEventPayload serializes");
    let runtime_jws = jws_sign(runtime_key, &payload_bytes);
    let author_jws = author_key.map_or_else(String::new, |k| jws_sign(k, &payload_bytes));
    (runtime_jws, author_jws)
}

/// Verify the runtime counter-signature on a v2
/// `TunnelChannelEvent`. Returns the decoded payload and the payload
/// segment (the caller passes the segment to
/// [`verify_author_signature`] for the D2 author binding).
///
/// # Errors
///
/// - The JWS is malformed or does not verify against `runtime_key`.
/// - The payload version tag is not [`CHANNEL_EVENT_PAYLOAD_VERSION`].
/// - The payload is expired or issued too far in the future.
pub fn verify_channel_event(
    runtime_key: &VerifyingKey,
    compact: &str,
) -> Result<(ChannelEventPayload, String)> {
    let (payload_segment, payload_bytes) = jws_verify(runtime_key, compact)?;
    let payload: ChannelEventPayload = serde_json::from_slice(&payload_bytes)
        .map_err(|e| anyhow!("channel event payload is not valid JSON: {e}"))?;
    if payload.v != CHANNEL_EVENT_PAYLOAD_VERSION {
        return Err(anyhow!(
            "channel event payload version must be {:?}; got: {:?}",
            CHANNEL_EVENT_PAYLOAD_VERSION,
            payload.v
        ));
    }
    check_freshness(payload.iat, payload.exp)?;
    Ok((payload, payload_segment))
}

/// Verify the author signature (ADR-058 D2): derive the verifying
/// key from `author_did` (self-certifying `did:key`), verify the JWS,
/// and require the author's payload segment to be **byte-identical**
/// to the runtime signature's — the property that binds both
/// signatures to the same payload.
///
/// # Errors
///
/// - `author_did` is not a valid `did:key` ed25519 DID.
/// - The JWS is malformed or does not verify.
/// - The payload segments differ.
pub fn verify_author_signature(
    author_compact: &str,
    expected_payload_segment: &str,
    author_did: &str,
) -> Result<()> {
    let author_key = crate::tunnel::did_key::did_key_to_verifying_key(author_did)
        .with_context(|| format!("author DID is not a verifiable did:key: {author_did}"))?;
    let (author_segment, _payload) = jws_verify(&author_key, author_compact)?;
    if author_segment != expected_payload_segment {
        return Err(anyhow!(
            "author signature payload segment does not match the runtime signature's \
             (the two JWS must embed the same payload)"
        ));
    }
    Ok(())
}

// ===========================================================================
// Channel-invite helpers — same v2 treatment as the channel event:
// one shared payload, two JWS (runtime + author), `iat`/`exp`
// freshness. `passive_binding` signs as the empty string when the
// envelope carries `None`.
// ===========================================================================

/// Borrowed view of the fields that go into a `ChannelInvitePayload`.
///
/// `initial_members_bytes` is the **pre-serialized** JSON form of the
/// `initial_members` list; it rides the payload as
/// `initial_members_b64u` (base64url-no-pad) so the bytes that get
/// signed are the bytes that get verified, byte-for-byte.
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
    /// The creator principal's stable DID (ADR-058 D1: a `did:key`
    /// for vault-backed principals) — the receiver names its peer
    /// child for the creator from this and verifies the author
    /// signature against it.
    pub creator_did: &'a str,
    /// Human-readable channel name (`team`, `general`, etc.).
    pub name: &'a str,
    /// DM marker: the source channel's `passive_binding`, or the
    /// EMPTY STRING when the channel is unbound. Only the
    /// presence/absence is meaningful to the receiver (it derives its
    /// own binding); the value is signed so a hub cannot strip or
    /// forge the DM-ness of an invite.
    pub passive_binding: &'a str,
    /// Pre-serialized bytes of the `initial_members` list. Caller is
    /// responsible for serializing via `serde_json::to_vec(&members)`
    /// once and passing the same bytes to both sign and verify paths.
    pub initial_members_bytes: &'a [u8],
}

/// The signed payload of a v2 `TunnelChannelInvite` envelope
/// (ADR-058 D2/D3). Mirrors [`ChannelEventPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInvitePayload {
    /// Payload version tag — must equal
    /// [`CHANNEL_INVITE_PAYLOAD_VERSION`].
    pub v: String,
    pub request_id: String,
    pub source_runtime_id: String,
    pub recipient_runtime_id: String,
    pub source_principal_did: String,
    pub channel_id: String,
    pub creator: String,
    pub creator_did: String,
    pub name: String,
    /// DM marker; the empty string when the channel is unbound.
    pub passive_binding: String,
    /// base64url-no-pad of the serialized `initial_members` bytes.
    pub initial_members_b64u: String,
    /// Issued-at (Unix seconds, UTC).
    pub iat: i64,
    /// Expiry (Unix seconds, UTC).
    pub exp: i64,
}

/// Sign `fields` into a v2 invite payload and produce both
/// signatures, exactly as [`sign_channel_event`] does for events.
#[must_use]
pub fn sign_channel_invite(
    runtime_key: &SigningKey,
    author_key: Option<&SigningKey>,
    fields: ChannelInviteSignedFields<'_>,
    iat: i64,
    exp: i64,
) -> (String, String) {
    let payload = ChannelInvitePayload {
        v: CHANNEL_INVITE_PAYLOAD_VERSION.to_string(),
        request_id: fields.request_id.to_string(),
        source_runtime_id: fields.source_runtime_id.to_string(),
        recipient_runtime_id: fields.recipient_runtime_id.to_string(),
        source_principal_did: fields.source_principal_did.to_string(),
        channel_id: fields.channel_id.to_string(),
        creator: fields.creator.to_string(),
        creator_did: fields.creator_did.to_string(),
        name: fields.name.to_string(),
        passive_binding: fields.passive_binding.to_string(),
        initial_members_b64u: URL_SAFE_NO_PAD.encode(fields.initial_members_bytes),
        iat,
        exp,
    };
    let payload_bytes = serde_json::to_vec(&payload).expect("ChannelInvitePayload serializes");
    let runtime_jws = jws_sign(runtime_key, &payload_bytes);
    let author_jws = author_key.map_or_else(String::new, |k| jws_sign(k, &payload_bytes));
    (runtime_jws, author_jws)
}

/// Verify the runtime counter-signature on a v2
/// `TunnelChannelInvite`. Returns the decoded payload and the payload
/// segment (for [`verify_author_signature`]).
///
/// # Errors
///
/// Same conditions as [`verify_channel_event`].
pub fn verify_channel_invite(
    runtime_key: &VerifyingKey,
    compact: &str,
) -> Result<(ChannelInvitePayload, String)> {
    let (payload_segment, payload_bytes) = jws_verify(runtime_key, compact)?;
    let payload: ChannelInvitePayload = serde_json::from_slice(&payload_bytes)
        .map_err(|e| anyhow!("channel invite payload is not valid JSON: {e}"))?;
    if payload.v != CHANNEL_INVITE_PAYLOAD_VERSION {
        return Err(anyhow!(
            "channel invite payload version must be {:?}; got: {:?}",
            CHANNEL_INVITE_PAYLOAD_VERSION,
            payload.v
        ));
    }
    check_freshness(payload.iat, payload.exp)?;
    Ok((payload, payload_segment))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_identity::keys::KeyPair;
    use peko_protocol::channel::InitialMember;

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn sample_event_fields(event_bytes: &[u8]) -> ChannelSignedFields<'_> {
        ChannelSignedFields {
            request_id: "chan-evt-1",
            source_runtime_id: "did:key:zRuntimeA",
            recipient_runtime_id: "did:key:zRuntimeB",
            source_principal_did: "prin_alice",
            channel_id: "chan_abcdefgh",
            event_bytes,
        }
    }

    /// Round-trip: runtime + author signatures over the same payload
    /// both verify, and the payload segment binding holds.
    #[test]
    fn test_sign_then_verify_round_trip() {
        let kp = KeyPair::generate();
        let author = KeyPair::generate();
        let author_did = crate::tunnel::verifying_key_to_did_key(&author.verifying_key);
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let fields = sample_event_fields(event_bytes);

        let (sig, author_sig) = sign_channel_event(
            &kp.signing_key,
            Some(&author.signing_key),
            fields,
            now(),
            now() + 300,
        );
        assert!(!author_sig.is_empty(), "author key present → author JWS");

        let (payload, segment) = verify_channel_event(&kp.verifying_key, &sig)
            .expect("freshly signed event must verify");
        assert_eq!(payload.v, CHANNEL_EVENT_PAYLOAD_VERSION);
        assert_eq!(payload.request_id, "chan-evt-1");
        assert_eq!(payload.source_runtime_id, "did:key:zRuntimeA");
        assert_eq!(payload.recipient_runtime_id, "did:key:zRuntimeB");
        assert_eq!(payload.source_principal_did, "prin_alice");
        assert_eq!(payload.channel_id, "chan_abcdefgh");
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&payload.event_b64u).unwrap(),
            event_bytes
        );

        verify_author_signature(&author_sig, &segment, &author_did)
            .expect("author signature must verify against the same payload segment");
    }

    /// No author key → empty author signature (legacy runtime-vouched
    /// path); the runtime JWS still verifies on its own.
    #[test]
    fn test_sign_without_author_key_produces_empty_author_signature() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let (sig, author_sig) = sign_channel_event(
            &kp.signing_key,
            None,
            sample_event_fields(event_bytes),
            now(),
            now() + 300,
        );
        assert!(author_sig.is_empty());
        verify_channel_event(&kp.verifying_key, &sig).expect("runtime JWS must verify");
    }

    /// A tampered payload segment fails verification — a hub that
    /// rewrites the signed payload breaks the JWS.
    #[test]
    fn test_tampered_payload_fails_verification() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let (sig, _) = sign_channel_event(
            &kp.signing_key,
            None,
            sample_event_fields(event_bytes),
            now(),
            now() + 300,
        );

        // Replace the payload segment with a different (valid b64url,
        // valid JSON) payload and keep the other segments.
        let segments: Vec<&str> = sig.split('.').collect();
        let tampered_payload = URL_SAFE_NO_PAD.encode(br#"{"v":"peko-channel-event/2"}"#);
        let tampered = format!("{}.{}.{}", segments[0], tampered_payload, segments[2]);
        let result = verify_channel_event(&kp.verifying_key, &tampered);
        assert!(result.is_err(), "tampered payload must not verify");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("did not verify"),
            "error must name the condition; got: {msg}"
        );
    }

    /// Wrong runtime verifying key fails verification.
    #[test]
    fn test_wrong_runtime_key_fails_verification() {
        let kp_signer = KeyPair::generate();
        let kp_verifier = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let (sig, _) = sign_channel_event(
            &kp_signer.signing_key,
            None,
            sample_event_fields(event_bytes),
            now(),
            now() + 300,
        );
        let result = verify_channel_event(&kp_verifier.verifying_key, &sig);
        assert!(result.is_err(), "wrong verifying key must fail");
    }

    /// An author signature produced by a key OTHER than the one
    /// embedded in `author_did` fails verification.
    #[test]
    fn test_wrong_author_key_fails_verification() {
        let kp = KeyPair::generate();
        let author = KeyPair::generate();
        let impostor = KeyPair::generate();
        let impostor_did = crate::tunnel::verifying_key_to_did_key(&impostor.verifying_key);
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;

        let (sig, author_sig) = sign_channel_event(
            &kp.signing_key,
            Some(&author.signing_key),
            sample_event_fields(event_bytes),
            now(),
            now() + 300,
        );
        let (_, segment) = verify_channel_event(&kp.verifying_key, &sig).unwrap();

        // The claimed author DID embeds the impostor's key, but the
        // JWS was signed by the real author — must fail.
        let result = verify_author_signature(&author_sig, &segment, &impostor_did);
        assert!(result.is_err(), "wrong author key must fail");
    }

    /// The author JWS must embed the SAME payload segment as the
    /// runtime JWS — a signature over a different payload (even by
    /// the right key) fails the binding check.
    #[test]
    fn test_author_payload_segment_mismatch_fails() {
        let kp = KeyPair::generate();
        let author = KeyPair::generate();
        let author_did = crate::tunnel::verifying_key_to_did_key(&author.verifying_key);
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;

        let (sig, _) = sign_channel_event(
            &kp.signing_key,
            None,
            sample_event_fields(event_bytes),
            now(),
            now() + 300,
        );
        let (_, segment) = verify_channel_event(&kp.verifying_key, &sig).unwrap();

        // Author signs a DIFFERENT payload (different request_id).
        let other = ChannelSignedFields {
            request_id: "chan-evt-OTHER",
            ..sample_event_fields(event_bytes)
        };
        let (_, mismatched_author_sig) = sign_channel_event(
            &kp.signing_key,
            Some(&author.signing_key),
            other,
            now(),
            now() + 300,
        );
        let result = verify_author_signature(&mismatched_author_sig, &segment, &author_did);
        let err = result.expect_err("payload-segment mismatch must fail");
        assert!(
            err.to_string().contains("payload segment"),
            "error must name the binding condition; got: {err}"
        );
    }

    /// An expired payload is rejected (with the leeway already
    /// exhausted).
    #[test]
    fn test_expired_payload_is_rejected() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let past = now() - 3600;
        let (sig, _) = sign_channel_event(
            &kp.signing_key,
            None,
            sample_event_fields(event_bytes),
            past,
            past + 300,
        );
        let err = verify_channel_event(&kp.verifying_key, &sig)
            .expect_err("expired payload must be rejected");
        assert!(
            err.to_string().contains("expired"),
            "error must name expiry; got: {err}"
        );
    }

    /// A payload issued too far in the future is rejected.
    #[test]
    fn test_future_iat_is_rejected() {
        let kp = KeyPair::generate();
        let event_bytes = br#"{"kind":"posted","text":"hello"}"#;
        let future = now() + 3600;
        let (sig, _) = sign_channel_event(
            &kp.signing_key,
            None,
            sample_event_fields(event_bytes),
            future,
            future + 300,
        );
        let err = verify_channel_event(&kp.verifying_key, &sig)
            .expect_err("future-iat payload must be rejected");
        assert!(
            err.to_string().contains("future"),
            "error must name the iat condition; got: {err}"
        );
    }

    /// Malformed JWS shapes surface structured errors, never panics.
    #[test]
    fn test_malformed_jws_errors_loudly() {
        let kp = KeyPair::generate();

        // Wrong segment count.
        let err = verify_channel_event(&kp.verifying_key, "one.two")
            .expect_err("2-segment JWS must fail");
        assert!(err.to_string().contains("3 segments"), "got: {err}");

        // Bad base64url in the header.
        let err = verify_channel_event(&kp.verifying_key, "!!!.e30.c2ln")
            .expect_err("bad header b64 must fail");
        assert!(err.to_string().contains("base64url"), "got: {err}");

        // Wrong alg (algorithm-confusion guard). header {"alg":"none"}.
        let none_header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let compact = format!("{none_header}.e30.c2ln");
        let err =
            verify_channel_event(&kp.verifying_key, &compact).expect_err("alg:none must fail");
        assert!(err.to_string().contains("EdDSA"), "got: {err}");

        // Signature of the wrong length (32 bytes, not 64).
        let short_sig = URL_SAFE_NO_PAD.encode([0u8; 32]);
        let eddsa_header = URL_SAFE_NO_PAD.encode(JWS_HEADER_JSON.as_bytes());
        let compact = format!("{eddsa_header}.e30.{short_sig}");
        let err = verify_channel_event(&kp.verifying_key, &compact)
            .expect_err("32-byte signature must fail");
        assert!(err.to_string().contains("expected 64"), "got: {err}");
    }

    // =====================================================================
    // Channel-invite tests — mirror the event suite: round-trip (both
    // signatures), tampered members, wrong key.
    // =====================================================================

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

    fn sample_invite_fields(members_bytes: &[u8]) -> ChannelInviteSignedFields<'_> {
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
            initial_members_bytes: members_bytes,
        }
    }

    /// Round-trip: runtime + author signatures over the invite
    /// payload both verify.
    #[test]
    fn test_sign_then_verify_invite_round_trip() {
        let kp = KeyPair::generate();
        let author = KeyPair::generate();
        let author_did = crate::tunnel::verifying_key_to_did_key(&author.verifying_key);
        let members_bytes = sample_initial_members_bytes();

        let (sig, author_sig) = sign_channel_invite(
            &kp.signing_key,
            Some(&author.signing_key),
            sample_invite_fields(&members_bytes),
            now(),
            now() + 300,
        );
        let (payload, segment) = verify_channel_invite(&kp.verifying_key, &sig)
            .expect("freshly signed invite must verify");
        assert_eq!(payload.v, CHANNEL_INVITE_PAYLOAD_VERSION);
        assert_eq!(payload.creator_did, "did:peko:principal:alice");
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(&payload.initial_members_b64u)
                .unwrap(),
            members_bytes
        );
        verify_author_signature(&author_sig, &segment, &author_did)
            .expect("author signature must verify");
    }

    /// A tampered `initial_members_b64u` fails verification — a hub
    /// cannot rewrite the membership snapshot without breaking the
    /// JWS.
    #[test]
    fn test_tampered_members_fails_verification() {
        let kp = KeyPair::generate();
        let members_bytes = sample_initial_members_bytes();
        let (sig, _) = sign_channel_invite(
            &kp.signing_key,
            None,
            sample_invite_fields(&members_bytes),
            now(),
            now() + 300,
        );

        let tampered_members = serde_json::to_vec(&vec![InitialMember {
            principal_did: "prin_alice".to_string(),
            runtime_id: Some("did:key:zRuntimeAttacker".to_string()),
        }])
        .unwrap();
        let tampered_payload = ChannelInvitePayload {
            initial_members_b64u: URL_SAFE_NO_PAD.encode(&tampered_members),
            ..serde_json::from_slice(
                &URL_SAFE_NO_PAD
                    .decode(sig.split('.').nth(1).unwrap())
                    .unwrap(),
            )
            .unwrap()
        };
        let segments: Vec<&str> = sig.split('.').collect();
        let tampered = format!(
            "{}.{}.{}",
            segments[0],
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered_payload).unwrap()),
            segments[2]
        );
        let result = verify_channel_invite(&kp.verifying_key, &tampered);
        assert!(result.is_err(), "tampered initial_members must not verify");
    }

    /// Wrong runtime verifying key fails invite verification.
    #[test]
    fn test_wrong_verifying_key_fails_invite_verification() {
        let kp_signer = KeyPair::generate();
        let kp_verifier = KeyPair::generate();
        let members_bytes = sample_initial_members_bytes();
        let (sig, _) = sign_channel_invite(
            &kp_signer.signing_key,
            None,
            sample_invite_fields(&members_bytes),
            now(),
            now() + 300,
        );
        let result = verify_channel_invite(&kp_verifier.verifying_key, &sig);
        assert!(result.is_err(), "wrong verifying key must fail");
    }
}
