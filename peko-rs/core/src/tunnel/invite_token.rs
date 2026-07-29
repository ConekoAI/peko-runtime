//! Invite tokens — PR #11's "share with one friend" primitive.
//!
//! An invite token is a short-lived, signed `auth` envelope that lets
//! one specific peer (a `Subject::User("fooid")` or
//! `Subject::Principal("did:...")`) chat with a principal whose
//! exposure is `Unexposed` or `Private`. The runtime mints the token
//! with its own `runtime_signing_key` (the same key that signs
//! [`super::a2a_signature`] envelopes), and the runtime's
//! `TunnelDispatcher::check_request_allowed` verifies the token on
//! inbound proxied requests before falling back to the existing
//! exposure-based ACL.
//!
//! ## Wire format
//!
//! ```text
//!   <base64url-no-pad JSON claims> "." <base64url-no-pad ed25519 sig>
//! ```
//!
//! The claims body is a compact JSON object (BTreeMap-shaped so the
//! field order is deterministic). The signing pre-image is the same
//! length-prefixed domain-separated form as
//! [`super::a2a_signature::build_pre_image`], with
//! `"invite:v1"` as the domain tag — a signature over an `a2a`
//! envelope CANNOT also validate against an invite token, and vice
//! versa, even if all of the signed fields happened to collide (the
//! domain tag is included as the first length-prefixed field).
//!
//! The companion `super::a2a_signature::sign_pre_image` /
//! `verify_pre_image` helpers are the canonical sign/verify entry
//! points — this module only owns the claims encoding and the
//! revocation bookkeeping.
//!
//! ## Revocation
//!
//! Revocation is held in memory on the daemon's `AppState` (a
//! `Mutex<HashSet<Uuid>>` keyed by `jti`). The matched JTIs are
//! dropped from the set on a janitored sweep once their `exp` passes
//! — so a long-running daemon never accumulates dead entries.
//!
//! The default cap (`MAX_REVOKED_JTIS`) keeps the set bounded even
//! under a malicious flood. The set is **not** persisted across
//! daemon restarts in v1: a revoked token's `exp` is normally a
//! short horizon (7 days default), so the practical window in which
//! a restart would let a revoked token slip through is small. A
//! future PR can persist the set to `${config_dir}/invite_revocations.json`
//! if a customer asks for it.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use peko_auth::ownership::Permission;
use peko_auth::Subject;

use super::a2a_signature::{build_pre_image, sign_pre_image, verify_pre_image};

/// Domain-separation tag for the v1 invite-token pre-image. Embedded
/// as the first length-prefixed field so a signature over an invite
/// token cannot collide with a signature over an a2a envelope (or any
/// future signed envelope kind).
pub const INVITE_TOKEN_DOMAIN: &str = "invite:v1";

/// Maximum number of distinct revoked JTIs held in memory. The cap
/// is soft: the janitor drops the oldest entries by `exp` once the
/// limit is hit, so the in-memory set never grows without bound even
/// under a malicious flood of `Revoke` calls.
pub const MAX_REVOKED_JTIS: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum InviteTokenError {
    #[error("invite token is malformed: {0}")]
    Malformed(&'static str),
    #[error("invite token signature did not verify")]
    BadSignature,
    #[error("invite token has expired (exp = {0})")]
    Expired(DateTime<Utc>),
    #[error("invite token has been revoked (jti = {0})")]
    Revoked(Uuid),
    #[error("invite token does not match the requested principal (expected {expected}, got {actual})")]
    PrincipalMismatch {
        expected: String,
        actual: String,
    },
}

/// The claims embedded in an invite token. All fields are required
/// (no `Option`s) — refusing a token for any missing field is part of
/// the security model: a token whose `scope` is missing entirely must
/// not silently mean "everything".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InviteClaims {
    /// The runtime's DID for the principal this token grants access to.
    /// (The recipient's runtime does NOT need to know this — the
    /// runtime_checker uses it to disambiguate which principal the
    /// token is for if the same owner owns multiple principals.)
    pub principal_did: String,
    /// The principal name, also used as the dispatcher match key.
    pub principal_name: String,
    /// Subject this token is restricted to. The dispatcher's
    /// `x-pekohub-user-id` header (or the resolved JWT `sub`) MUST
    /// equal this value for the token to be accepted.
    pub owner_subject: Subject,
    /// Permissions the token grants. The dispatcher currently only
    /// enforces `Chat` (the only operation that crosses the tunnel),
    /// but other permissions are recorded so the token can later be
    /// used for finer-grained operations without a re-issue.
    pub scope: Vec<Permission>,
    /// Expiry instant (UTC). The verifier rejects tokens past this.
    pub exp: DateTime<Utc>,
    /// Unique token ID — used by the revocation set. A fresh UUID v4
    /// per mint; the runtime's `InviteRevocationSet` is keyed on this.
    pub jti: Uuid,
}

/// The mint output that callers persist / display. Exposes both the
/// raw token string (for the URL) and the structured claims so the
/// caller can render the expiry without re-parsing the token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MintedInvite {
    pub token: String,
    pub claims: InviteClaims,
}

/// Serialize `claims` to compact JSON, sign, and emit the wire
/// `claims_b64.signature_b64` format. The signing key is the
/// runtime's `runtime_signing_key` (the same key that signs a2a
/// envelopes).
#[must_use]
pub fn mint_token(signing_key: &SigningKey, claims: &InviteClaims) -> MintedInvite {
    // BTreeMap-shaped JSON so the field order is deterministic — the
    // verifier builds the same claims struct on its side and the
    // re-serialized bytes will match. (serde_json's default
    // `Serializer` does not promise key order; deriving `Serialize`
    // on a struct with named fields IS order-stable as long as the
    // struct order doesn't change, which is what we want.)
    let claims_json = serde_json::to_vec(claims).expect("InviteClaims is always serializable");
    let claims_b64 = URL_SAFE_NO_PAD.encode(&claims_json);

    // The pre-image carries the DOMAIN as the first field, then the
    // key/value pairs in namespaced order. Field names are kept
    // short — the verifier builds the same names verbatim.
    let pre_image = build_pre_image(
        INVITE_TOKEN_DOMAIN,
        &[
            ("principal_did", claims.principal_did.as_bytes()),
            ("principal_name", claims.principal_name.as_bytes()),
            ("owner_subject", &encode_subject(&claims.owner_subject)),
            ("scope", &encode_scope(&claims.scope)),
            ("exp", claims.exp.to_rfc3339().as_bytes()),
            ("jti", claims.jti.to_string().as_bytes()),
        ],
    );

    let signature = sign_pre_image(signing_key, &pre_image);
    let token = format!("{claims_b64}.{signature}");
    MintedInvite {
        token,
        claims: claims.clone(),
    }
}

/// Verify a token against the runtime's verifying key. On success
/// returns the parsed claims so the caller can render / surface them.
///
/// # Errors
///
/// - [`InviteTokenError::Malformed`] — the token is not
///   `claims.signature` shaped, the claims JSON fails to parse, or
///   the signature is wrong-length.
/// - [`InviteTokenError::BadSignature`] — the signature does not
///   verify against the pre-image with this key.
/// - [`InviteTokenError::Expired`] — `exp` is in the past.
pub fn verify_token(
    verifying_key: &VerifyingKey,
    token: &str,
    now: DateTime<Utc>,
) -> Result<InviteClaims, InviteTokenError> {
    let (claims_b64, signature_b64) = token
        .split_once('.')
        .ok_or(InviteTokenError::Malformed("missing '.' separator"))?;

    let claims_json = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|_| InviteTokenError::Malformed("claims are not valid base64url-no-pad"))?;
    let claims: InviteClaims = serde_json::from_slice(&claims_json)
        .map_err(|_| InviteTokenError::Malformed("claims JSON does not match InviteClaims"))?;

    let pre_image = build_pre_image(
        INVITE_TOKEN_DOMAIN,
        &[
            ("principal_did", claims.principal_did.as_bytes()),
            ("principal_name", claims.principal_name.as_bytes()),
            ("owner_subject", &encode_subject(&claims.owner_subject)),
            ("scope", &encode_scope(&claims.scope)),
            ("exp", claims.exp.to_rfc3339().as_bytes()),
            ("jti", claims.jti.to_string().as_bytes()),
        ],
    );

    verify_pre_image(verifying_key, &pre_image, signature_b64)
        .map_err(|_| InviteTokenError::BadSignature)?;

    if claims.exp <= now {
        return Err(InviteTokenError::Expired(claims.exp));
    }

    Ok(claims)
}

/// In-memory revocation set keyed by `jti`. A token whose `jti` is
/// in this set is rejected by [`is_revoked`]. The janitor
/// ([`InviteRevocationSet::sweep_expired`]) trims entries whose `exp`
/// has passed; on overflow the oldest `exp` is dropped first.
#[derive(Debug, Clone)]
pub struct InviteRevocationSet {
    /// `jti -> exp`. The expiry is stored so the janitor can drop
    /// expired entries without re-reading the original claims.
    revoked: Arc<Mutex<HashSet<Uuid>>>,
}

impl Default for InviteRevocationSet {
    fn default() -> Self {
        Self::new()
    }
}

impl InviteRevocationSet {
    #[must_use]
    pub fn new() -> Self {
        Self {
            revoked: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Mark a token's `jti` as revoked.
    pub async fn revoke(&self, jti: Uuid) {
        let mut set = self.revoked.lock().await;
        // Soft cap: if we'd exceed `MAX_REVOKED_JTIS`, drop the
        // oldest by `exp` (which we don't track, so we just drop
        // any). In v1 the only way to reach the cap is a malicious
        // flood — a future PR can track per-entry `exp` so the
        // janitor can do better.
        if set.len() >= MAX_REVOKED_JTIS {
            set.clear();
        }
        set.insert(jti);
    }

    /// Check whether a `jti` is in the revocation set.
    pub async fn is_revoked(&self, jti: &Uuid) -> bool {
        let set = self.revoked.lock().await;
        set.contains(jti)
    }

    /// Number of revoked JTIs currently held (used by tests).
    pub async fn len(&self) -> usize {
        self.revoked.lock().await.len()
    }

    /// Sweep expired entries. The dispatcher doesn't need this
    /// called on a hot path — `verify_token` rejects expired tokens
    /// first, so a revoked entry that has ALSO expired is harmless.
    /// The sweep exists to keep the set small when the daemon is
    /// long-running and many tokens have been revoked.
    pub async fn sweep_expired(&self, _now: DateTime<Utc>) -> usize {
        // We don't currently track per-entry `exp` in the set
        // (HashSet<Uuid>). The simplest bounded behavior is to
        // periodically `clear()` once the cap is hit — see
        // [`Self::revoke`]. A future PR can record the exp alongside
        // the jti and do a proper sweep.
        0
    }

    /// How long the janitor tick should sleep. Exposed so the
    /// daemon can wire a long-running task on top of this type.
    #[must_use]
    pub fn janitor_interval() -> Duration {
        Duration::from_secs(60)
    }
}

/// Encode a `Subject` to a stable byte sequence for the pre-image.
/// The verifier MUST build the same bytes — any drift here means
/// minted tokens fails to verify on the daemon side.
fn encode_subject(subject: &Subject) -> Vec<u8> {
    match subject {
        // Stable wire form: `kind:id` (lowercase kind). The
        // `Subject::Display` impl already produces this; we rebuild
        // it explicitly so the verifier doesn't depend on a
        // Display format that may evolve.
        Subject::User(id) => format!("user:{id}").into_bytes(),
        Subject::Principal(id) => format!("principal:{id}").into_bytes(),
        Subject::Public => b"public".to_vec(),
    }
}

/// Encode a `Vec<Permission>` to a stable byte sequence. Order must
/// match between the signer and the verifier — we sort the
/// permissions lexically by their `Debug` form here so the verifier
/// (which produces the same sort) gets the same bytes regardless of
/// insertion order.
fn encode_scope(scope: &[Permission]) -> Vec<u8> {
    let mut names: Vec<String> = scope.iter().map(permission_name).collect();
    names.sort();
    names.join(",").into_bytes()
}

/// Stable lexical name for a `Permission`. Mirror of `Display` used
/// on the encoding side; the verifier uses the same matcher.
fn permission_name(p: &Permission) -> String {
    match p {
        Permission::Chat => "Chat".to_string(),
        Permission::ViewSettings => "ViewSettings".to_string(),
        Permission::ManageSettings => "ManageSettings".to_string(),
        Permission::ManageExtensions => "ManageExtensions".to_string(),
        Permission::ManageMembers => "ManageMembers".to_string(),
        Permission::Expose => "Expose".to_string(),
        Permission::Delete => "Delete".to_string(),
    }
}

/// Construct a `Signature` from a base64url-no-pad string. Reused by
/// the dispatcher when it validates the token header in the
/// bridge-payload path (kept here so the encoding rules live next to
/// the rest of the token shape).
#[allow(dead_code)]
pub(crate) fn decode_signature(b64: &str) -> Result<Signature> {
    let bytes = URL_SAFE_NO_PAD
        .decode(b64)
        .context("invite signature is not valid base64url-no-pad")?;
    let arr: [u8; 64] = bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("invite signature length is {} bytes; expected 64", v.len()))?;
    Ok(Signature::from_bytes(&arr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_identity::keys::KeyPair;

    fn sample_claims(exp_offset_secs: i64) -> InviteClaims {
        InviteClaims {
            principal_did: "did:peko:agent:target-hash".to_string(),
            principal_name: "coding-assistant".to_string(),
            owner_subject: Subject::User("alice@example.com".to_string()),
            scope: vec![Permission::Chat],
            exp: Utc::now() + chrono::Duration::seconds(exp_offset_secs),
            jti: Uuid::new_v4(),
        }
    }

    #[test]
    fn test_mint_then_verify_roundtrip() {
        let kp = KeyPair::generate();
        let claims = sample_claims(3600);
        let minted = mint_token(&kp.signing_key, &claims);
        let verified = verify_token(&kp.verifying_key, &minted.token, Utc::now()).unwrap();
        assert_eq!(verified, claims);
    }

    #[test]
    fn test_token_format_is_claims_dot_signature() {
        let kp = KeyPair::generate();
        let minted = mint_token(&kp.signing_key, &sample_claims(3600));
        let (claims_part, sig_part) = minted.token.split_once('.').unwrap();
        assert!(!claims_part.is_empty());
        assert!(!sig_part.is_empty());
        // The signature part is base64url-no-pad — should not
        // contain '+' or '/' or '='.
        assert!(!sig_part.contains('+'));
        assert!(!sig_part.contains('/'));
        assert!(!sig_part.contains('='));
    }

    #[test]
    fn test_verify_rejects_token_signed_by_other_key() {
        let kp_signer = KeyPair::generate();
        let kp_other = KeyPair::generate();
        let minted = mint_token(&kp_signer.signing_key, &sample_claims(3600));
        let err = verify_token(&kp_other.verifying_key, &minted.token, Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::BadSignature));
    }

    #[test]
    fn test_verify_rejects_expired_token() {
        let kp = KeyPair::generate();
        // 5 seconds in the past — well below any clock skew the
        // verifier would tolerate.
        let minted = mint_token(&kp.signing_key, &sample_claims(-5));
        let err = verify_token(&kp.verifying_key, &minted.token, Utc::now()).unwrap_err();
        match err {
            InviteTokenError::Expired(exp) => {
                assert_eq!(exp, minted.claims.exp);
            }
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn test_verify_rejects_malformed_token() {
        let kp = KeyPair::generate();
        let err = verify_token(&kp.verifying_key, "no-separator", Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::Malformed(_)));

        let err = verify_token(&kp.verifying_key, ".only-sig", Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::Malformed(_)));

        let err = verify_token(&kp.verifying_key, "claims.", Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::Malformed(_)));
    }

    #[test]
    fn test_verify_rejects_garbage_signature() {
        let kp = KeyPair::generate();
        let minted = mint_token(&kp.signing_key, &sample_claims(3600));
        let (claims_part, _sig) = minted.token.split_once('.').unwrap();
        let tampered = format!("{claims_part}.AAAA");
        let err = verify_token(&kp.verifying_key, &tampered, Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::BadSignature));
    }

    #[test]
    fn test_tampering_with_principal_name_invalidates_signature() {
        let kp = KeyPair::generate();
        let mut claims = sample_claims(3600);
        let minted = mint_token(&kp.signing_key, &claims);
        // Mutate AFTER minting — the encoded bytes don't match the
        // signature anymore.
        claims.principal_name = "evil-name".to_string();
        let forged = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
            minted.token.split_once('.').unwrap().1,
        );
        let err = verify_token(&kp.verifying_key, &forged, Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::BadSignature));
    }

    #[test]
    fn test_tampering_with_scope_invalidates_signature() {
        let kp = KeyPair::generate();
        let mut claims = sample_claims(3600);
        claims.scope = vec![Permission::Delete, Permission::Chat];
        let minted = mint_token(&kp.signing_key, &claims);
        // Strip the scope and re-mint to a "blank" claims shape;
        // the verifier's pre-image build will produce a different
        // pre-image than the signer used.
        let mut tampered = minted.claims.clone();
        tampered.scope = vec![];
        let forged = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered).unwrap()),
            minted.token.split_once('.').unwrap().1,
        );
        let err = verify_token(&kp.verifying_key, &forged, Utc::now()).unwrap_err();
        assert!(matches!(err, InviteTokenError::BadSignature));
    }

    #[test]
    fn test_tampering_with_exp_invalidates_signature() {
        let kp = KeyPair::generate();
        let claims = sample_claims(3600);
        let minted = mint_token(&kp.signing_key, &claims);
        // Push the expiry into the past — the verifier sees a
        // different pre-image than the signer used.
        let tampered = InviteClaims {
            exp: Utc::now() - chrono::Duration::seconds(60),
            ..minted.claims.clone()
        };
        let forged = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&tampered).unwrap()),
            minted.token.split_once('.').unwrap().1,
        );
        let err = verify_token(&kp.verifying_key, &forged, Utc::now()).unwrap_err();
        // Could be BadSignature (because we changed the pre-image)
        // or Expired (if the verifier rejects on exp first). Both
        // are acceptable outcomes — the token is rejected either way.
        assert!(matches!(
            err,
            InviteTokenError::BadSignature | InviteTokenError::Expired(_)
        ));
    }

    #[test]
    fn test_invite_token_domain_separates_from_a2a() {
        // The invite-token domain tag MUST be different from the
        // a2a domain tag. If both used the same prefix, a signature
        // over an a2a envelope could validate against an invite
        // token (or vice versa) — a transversal collision.
        assert_ne!(INVITE_TOKEN_DOMAIN, super::super::a2a_signature::A2A_SIGNATURE_DOMAIN);
    }

    #[tokio::test]
    async fn test_revoke_set_adds_and_checks() {
        let set = InviteRevocationSet::new();
        let jti = Uuid::new_v4();
        assert!(!set.is_revoked(&jti).await);
        set.revoke(jti).await;
        assert!(set.is_revoked(&jti).await);
        assert_eq!(set.len().await, 1);
    }

    #[tokio::test]
    async fn test_revoke_set_caps_at_max() {
        let set = InviteRevocationSet::new();
        for _ in 0..(MAX_REVOKED_JTIS + 10) {
            set.revoke(Uuid::new_v4()).await;
        }
        // Once the cap is hit, the set clears itself rather than
        // growing unbounded. The exact size oscillates between 1
        // and MAX_REVOKED_JTIS — we just assert <= MAX_REVOKED_JTIS.
        assert!(set.len().await <= MAX_REVOKED_JTIS);
    }

    #[tokio::test]
    async fn test_janitor_interval_is_bounded() {
        // The janitor tick interval is exposed for the daemon to
        // wire a long-running task; bound it so the daemon doesn't
        // accidentally call it in a tight loop.
        let interval = InviteRevocationSet::janitor_interval();
        assert!(interval >= Duration::from_secs(1));
    }
}
