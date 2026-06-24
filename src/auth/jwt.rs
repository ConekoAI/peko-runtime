//! JWT validation for pekohub tokens (ADR-034)
//!
//! Supports RS256 and EdDSA algorithms with JWKS endpoint fetching and caching.
//!
//! # Architecture
//! - `JwtValidator` holds configuration (trusted issuers, runtime DID, JWKS URL)
//! - `JwksCache` provides thread-safe cached JWKS with TTL-based refresh
//! - `validate()` is async to support on-demand JWKS fetching
//! - Signature verification uses `jsonwebtoken` for RS256 and `ed25519-dalek` for EdDSA

use super::caller::CallerContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Validated JWT claims extracted from a token
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedJwt {
    /// Subject (pekohub user ID)
    pub sub: String,
    /// User name
    pub name: Option<String>,
    /// User email
    pub email: Option<String>,
    /// Permissions granted on this runtime
    pub permissions: Vec<String>,
}

/// JWT header
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwtHeader {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    typ: Option<String>,
}

/// JWT payload claims
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwtClaims {
    iss: String,
    sub: String,
    aud: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    permissions: Option<Vec<String>>,
    exp: i64,
    #[serde(default)]
    iat: Option<i64>,
}

/// JWKS response structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwksResponse {
    pub keys: Vec<JwkEntry>,
}

/// Individual JWK entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkEntry {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Cached JWKS with TTL
#[derive(Clone)]
struct JwksCache {
    /// The cached JWKS response
    jwks: Option<JwksResponse>,
    /// When the cache was last refreshed
    fetched_at: Option<Instant>,
    /// Cache TTL
    ttl: Duration,
    /// The JWKS endpoint URL
    url: String,
}

impl JwksCache {
    fn new(url: String, ttl_secs: u64) -> Self {
        Self {
            jwks: None,
            fetched_at: None,
            ttl: Duration::from_secs(ttl_secs),
            url,
        }
    }

    /// Create a cache with pre-populated JWKS (for testing)
    #[cfg(test)]
    fn with_jwks(url: String, jwks: JwksResponse) -> Self {
        Self {
            jwks: Some(jwks),
            fetched_at: Some(Instant::now()),
            ttl: Duration::from_mins(5),
            url,
        }
    }

    /// Check if the cache is stale or empty
    fn needs_refresh(&self) -> bool {
        match (self.jwks.as_ref(), self.fetched_at) {
            (Some(_), Some(fetched_at)) => fetched_at.elapsed() >= self.ttl,
            _ => true,
        }
    }

    /// Get a JWK entry by key ID
    fn get_key(&self, kid: Option<&str>) -> Option<JwkEntry> {
        let jwks = self.jwks.as_ref()?;
        match kid {
            Some(kid) => jwks
                .keys
                .iter()
                .find(|k| k.kid.as_deref() == Some(kid))
                .cloned(),
            None => jwks.keys.first().cloned(),
        }
    }

    /// Refresh the cache by fetching from the JWKS endpoint
    async fn refresh(&mut self) -> Result<(), JwtError> {
        // Use a fresh client with no proxy to avoid system proxy interference
        // (especially important in test environments)
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|_| JwtError::KeyNotFound)?;
        let response = client.get(&self.url).send().await.map_err(|e| {
            tracing::warn!("Failed to fetch JWKS from {}: {}", self.url, e);
            JwtError::KeyNotFound
        })?;

        if !response.status().is_success() {
            tracing::warn!("JWKS endpoint returned status: {}", response.status());
            return Err(JwtError::KeyNotFound);
        }

        let jwks: JwksResponse = response.json().await.map_err(|e| {
            tracing::warn!("Failed to parse JWKS response: {}", e);
            JwtError::KeyNotFound
        })?;

        self.jwks = Some(jwks);
        self.fetched_at = Some(Instant::now());
        Ok(())
    }
}

/// JWT validator with JWKS caching
#[derive(Clone)]
pub struct JwtValidator {
    trusted_issuers: Vec<String>,
    runtime_did: String,
    cache: Arc<tokio::sync::RwLock<JwksCache>>,
}

/// Errors during JWT validation
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JwtError {
    InvalidFormat,
    InvalidSignature,
    Expired,
    InvalidAudience,
    InvalidIssuer,
    UnsupportedAlgorithm,
    KeyNotFound,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "Invalid JWT format"),
            Self::InvalidSignature => write!(f, "Invalid JWT signature"),
            Self::Expired => write!(f, "JWT has expired"),
            Self::InvalidAudience => write!(f, "Invalid JWT audience"),
            Self::InvalidIssuer => write!(f, "Invalid JWT issuer"),
            Self::UnsupportedAlgorithm => write!(f, "Unsupported JWT algorithm"),
            Self::KeyNotFound => write!(f, "JWKS key not found"),
        }
    }
}

impl std::error::Error for JwtError {}

impl JwtValidator {
    /// Create a new JWT validator
    ///
    /// # Arguments
    /// * `trusted_issuers` — List of trusted JWT issuers (e.g., `["pekohub"]`)
    /// * `runtime_did` — The runtime's DID, used as the expected audience
    /// * `jwks_url` — Optional JWKS endpoint URL. If not provided, derives from issuer.
    pub fn new(
        trusted_issuers: Vec<String>,
        runtime_did: String,
        jwks_url: Option<String>,
    ) -> Self {
        let url = jwks_url.unwrap_or_else(|| {
            // Derive JWKS URL from first trusted issuer
            // e.g., "pekohub" -> "https://pekohub.org/.well-known/jwks.json"
            trusted_issuers
                .first()
                .map(|issuer| format!("https://{}/.well-known/jwks.json", issuer))
                .unwrap_or_default()
        });

        Self {
            trusted_issuers,
            runtime_did,
            cache: Arc::new(tokio::sync::RwLock::new(JwksCache::new(url, 300))), // 5 min TTL
        }
    }

    /// Create a validator with a pre-populated JWKS. Crate-visible
    /// (not public) so integration tests in sibling modules can wire
    /// a real `JwtValidator` against a static JWKS without standing up
    /// a mock HTTP server. See `tunnel::dispatcher::tests` for usage.
    #[cfg(test)]
    pub(crate) fn with_jwks(
        trusted_issuers: Vec<String>,
        runtime_did: String,
        jwks: JwksResponse,
    ) -> Self {
        Self {
            trusted_issuers,
            runtime_did,
            cache: Arc::new(tokio::sync::RwLock::new(JwksCache::with_jwks(
                "http://test/.well-known/jwks.json".to_string(),
                jwks,
            ))),
        }
    }

    /// Validate a JWT token asynchronously.
    ///
    /// This method:
    /// 1. Parses the JWT header and payload
    /// 2. Validates structural claims (expiry, audience, issuer)
    /// 3. Fetches JWKS if cache is stale
    /// 4. Verifies the cryptographic signature
    ///
    /// # Errors
    /// Returns `JwtError` if the token is invalid, expired, or signature verification fails.
    pub async fn validate(&self, token: &str) -> Result<ValidatedJwt, JwtError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(JwtError::InvalidFormat);
        }

        // Decode header
        let header_json = base64_decode(parts[0]).map_err(|_| JwtError::InvalidFormat)?;
        let header: JwtHeader =
            serde_json::from_str(&header_json).map_err(|_| JwtError::InvalidFormat)?;

        // Decode payload
        let payload_json = base64_decode(parts[1]).map_err(|_| JwtError::InvalidFormat)?;
        let claims: JwtClaims =
            serde_json::from_str(&payload_json).map_err(|_| JwtError::InvalidFormat)?;

        // Check algorithm support
        match header.alg.as_str() {
            "RS256" | "EdDSA" => {}
            _ => return Err(JwtError::UnsupportedAlgorithm),
        }

        // Validate expiry (with 30-second clock skew leeway)
        let now = chrono::Utc::now().timestamp();
        if claims.exp < now - 30 {
            return Err(JwtError::Expired);
        }

        // Validate audience against runtime DID
        if claims.aud != self.runtime_did {
            return Err(JwtError::InvalidAudience);
        }

        // Validate issuer
        if !self.trusted_issuers.contains(&claims.iss) {
            return Err(JwtError::InvalidIssuer);
        }

        // Verify signature
        self.verify_signature(token, &header, &claims).await?;

        Ok(ValidatedJwt {
            sub: claims.sub,
            name: claims.name,
            email: claims.email,
            permissions: claims.permissions.unwrap_or_default(),
        })
    }

    /// Verify the JWT signature using the appropriate algorithm
    async fn verify_signature(
        &self,
        token: &str,
        header: &JwtHeader,
        _claims: &JwtClaims,
    ) -> Result<(), JwtError> {
        // Refresh cache if needed
        {
            let cache = self.cache.read().await;
            if cache.needs_refresh() {
                drop(cache);
                let mut cache = self.cache.write().await;
                if cache.needs_refresh() {
                    cache.refresh().await?;
                }
            }
        }

        let cache = self.cache.read().await;
        let jwk = cache
            .get_key(header.kid.as_deref())
            .ok_or(JwtError::KeyNotFound)?;
        drop(cache);

        match header.alg.as_str() {
            "RS256" => Self::verify_rs256(token, &jwk),
            "EdDSA" => Self::verify_eddsa(token, &jwk),
            _ => Err(JwtError::UnsupportedAlgorithm),
        }
    }

    /// Verify RS256 signature using jsonwebtoken
    fn verify_rs256(token: &str, jwk: &JwkEntry) -> Result<(), JwtError> {
        let n = jwk.n.as_ref().ok_or(JwtError::KeyNotFound)?;
        let e = jwk.e.as_ref().ok_or(JwtError::KeyNotFound)?;

        let decoding_key = jsonwebtoken::DecodingKey::from_rsa_components(n, e)
            .map_err(|_| JwtError::KeyNotFound)?;

        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.validate_exp = false; // We already checked expiry
        validation.validate_aud = false; // We already checked audience
        validation.validate_nbf = false;
        validation.required_spec_claims.clear();

        let _ = jsonwebtoken::decode::<serde_json::Value>(token, &decoding_key, &validation)
            .map_err(|e| {
                tracing::debug!("RS256 signature verification failed: {}", e);
                JwtError::InvalidSignature
            })?;

        Ok(())
    }

    /// Verify EdDSA signature using ed25519-dalek
    fn verify_eddsa(token: &str, jwk: &JwkEntry) -> Result<(), JwtError> {
        let x = jwk.x.as_ref().ok_or(JwtError::KeyNotFound)?;
        let public_key_bytes = base64_decode_raw(x).map_err(|_| JwtError::KeyNotFound)?;

        if public_key_bytes.len() != 32 {
            tracing::warn!(
                "EdDSA public key has wrong length: expected 32, got {}",
                public_key_bytes.len()
            );
            return Err(JwtError::KeyNotFound);
        }

        let public_key = ed25519_dalek::VerifyingKey::from_bytes(
            &public_key_bytes
                .try_into()
                .map_err(|_| JwtError::KeyNotFound)?,
        )
        .map_err(|_| JwtError::KeyNotFound)?;

        let parts: Vec<&str> = token.split('.').collect();
        let message = format!("{}.{}", parts[0], parts[1]);
        let signature_bytes = base64_decode_raw(parts[2]).map_err(|_| JwtError::InvalidFormat)?;

        let signature = ed25519_dalek::Signature::from_slice(&signature_bytes)
            .map_err(|_| JwtError::InvalidSignature)?;

        public_key
            .verify_strict(message.as_bytes(), &signature)
            .map_err(|e| {
                tracing::debug!("EdDSA signature verification failed: {}", e);
                JwtError::InvalidSignature
            })
    }

    /// Build a CallerContext from a validated JWT.
    #[must_use]
    pub fn to_caller(validated: ValidatedJwt) -> CallerContext {
        CallerContext::from_jwt(validated.sub)
    }
}

/// Base64URL-decode without padding, returning a String
fn base64_decode(input: &str) -> Result<String, base64::DecodeError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let bytes = URL_SAFE_NO_PAD.decode(input)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

/// Base64URL-decode without padding, returning raw bytes
fn base64_decode_raw(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.decode(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    fn b64_encode(data: impl AsRef<[u8]>) -> String {
        URL_SAFE_NO_PAD.encode(data)
    }

    // ─── RS256 Tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rs256_valid_token() {
        // Generate an RSA key pair using the `rsa` crate
        use rand::rngs::OsRng;
        use rsa::{traits::PublicKeyParts, RsaPrivateKey, RsaPublicKey};

        let private_key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);

        // Encode n and e for JWKS (base64url without padding)
        let n = b64_encode(public_key.n().to_bytes_be());
        let e = b64_encode(public_key.e().to_bytes_be());

        let jwks = JwksResponse {
            keys: vec![JwkEntry {
                kty: "RSA".to_string(),
                kid: Some("test-key-1".to_string()),
                n: Some(n),
                e: Some(e),
                x: None,
                crv: None,
                extra: HashMap::new(),
            }],
        };

        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );

        // Export private key to PKCS#8 PEM for jsonwebtoken
        use rsa::pkcs8::EncodePrivateKey;
        let private_key_pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).unwrap();

        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
            "name": "Test User",
            "email": "test@example.com",
            "permissions": ["read", "write"],
        });

        let token = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key).unwrap();

        let result = validator.validate(&token).await.unwrap();
        assert_eq!(result.sub, "user123");
        assert_eq!(result.name, Some("Test User".to_string()));
        assert_eq!(result.email, Some("test@example.com".to_string()));
        assert_eq!(result.permissions, vec!["read", "write"]);
    }

    #[tokio::test]
    async fn test_rs256_invalid_signature() {
        use rand::rngs::OsRng;
        use rsa::pkcs8::EncodePrivateKey;
        use rsa::{traits::PublicKeyParts, RsaPrivateKey, RsaPublicKey};

        // Generate two different RSA key pairs
        let private_key1 = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public_key1 = RsaPublicKey::from(&private_key1);
        let private_key2 = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();

        let n = b64_encode(public_key1.n().to_bytes_be());
        let e = b64_encode(public_key1.e().to_bytes_be());

        let jwks = JwksResponse {
            keys: vec![JwkEntry {
                kty: "RSA".to_string(),
                kid: Some("test-key-1".to_string()),
                n: Some(n),
                e: Some(e),
                x: None,
                crv: None,
                extra: HashMap::new(),
            }],
        };

        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );

        // Sign with key2, but JWKS has key1's public key
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });

        let private_key_pem2 = private_key2
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let encoding_key = EncodingKey::from_rsa_pem(private_key_pem2.as_bytes()).unwrap();
        let token = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key).unwrap();

        let result = validator.validate(&token).await;
        assert_eq!(result.unwrap_err(), JwtError::InvalidSignature);
    }

    // ─── Structural Validation Tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_invalid_format() {
        let jwks = JwksResponse { keys: vec![] };
        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        assert_eq!(
            validator.validate("not-a-jwt").await.unwrap_err(),
            JwtError::InvalidFormat
        );
    }

    #[tokio::test]
    async fn test_expired_token() {
        let jwks = JwksResponse { keys: vec![] };
        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": 1000,
            "iat": 500,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            b64_encode(claims)
        );
        assert_eq!(
            validator.validate(&token).await.unwrap_err(),
            JwtError::Expired
        );
    }

    #[tokio::test]
    async fn test_invalid_audience() {
        let jwks = JwksResponse { keys: vec![] };
        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        let now = chrono::Utc::now().timestamp() + 3600;
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "wrong-audience",
            "exp": now,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            b64_encode(claims)
        );
        assert_eq!(
            validator.validate(&token).await.unwrap_err(),
            JwtError::InvalidAudience
        );
    }

    #[tokio::test]
    async fn test_invalid_issuer() {
        let jwks = JwksResponse { keys: vec![] };
        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        let now = chrono::Utc::now().timestamp() + 3600;
        let claims = serde_json::json!({
            "iss": "evil-issuer",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": now,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            b64_encode(claims)
        );
        assert_eq!(
            validator.validate(&token).await.unwrap_err(),
            JwtError::InvalidIssuer
        );
    }

    #[tokio::test]
    async fn test_unsupported_algorithm() {
        let jwks = JwksResponse { keys: vec![] };
        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );
        let now = chrono::Utc::now().timestamp() + 3600;
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": now,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(serde_json::json!({"alg":"HS256","typ":"JWT"}).to_string()),
            b64_encode(claims)
        );
        assert_eq!(
            validator.validate(&token).await.unwrap_err(),
            JwtError::UnsupportedAlgorithm
        );
    }

    // ─── EdDSA Tests ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_eddsa_valid_token() {
        use ed25519_dalek::{Signer, SigningKey};

        // Generate an Ed25519 key pair
        let signing_key = SigningKey::from_bytes(&rand::random());
        let verifying_key = signing_key.verifying_key();

        let x = b64_encode(verifying_key.to_bytes());

        let jwks = JwksResponse {
            keys: vec![JwkEntry {
                kty: "OKP".to_string(),
                kid: Some("test-eddsa-key".to_string()),
                n: None,
                e: None,
                x: Some(x),
                crv: Some("Ed25519".to_string()),
                extra: HashMap::new(),
            }],
        };

        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );

        // Create a valid EdDSA token manually
        let header = serde_json::json!({"alg":"EdDSA","typ":"JWT","kid":"test-eddsa-key"});
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user456",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });

        let header_b64 = b64_encode(header.to_string());
        let claims_b64 = b64_encode(claims.to_string());
        let message = format!("{}.{}", header_b64, claims_b64);

        let signature = signing_key.sign(message.as_bytes());
        let sig_b64 = b64_encode(signature.to_bytes());

        let token = format!("{}.{}.{}", header_b64, claims_b64, sig_b64);

        let result = validator.validate(&token).await.unwrap();
        assert_eq!(result.sub, "user456");
    }

    #[tokio::test]
    async fn test_eddsa_invalid_signature() {
        use ed25519_dalek::{Signer, SigningKey};

        // Generate two different Ed25519 key pairs
        let signing_key1 = SigningKey::from_bytes(&rand::random());
        let verifying_key1 = signing_key1.verifying_key();

        let x = b64_encode(verifying_key1.to_bytes());

        let jwks = JwksResponse {
            keys: vec![JwkEntry {
                kty: "OKP".to_string(),
                kid: Some("test-eddsa-key".to_string()),
                n: None,
                e: None,
                x: Some(x),
                crv: Some("Ed25519".to_string()),
                extra: HashMap::new(),
            }],
        };

        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );

        // Create token signed with a different key
        let wrong_signing_key = SigningKey::from_bytes(&rand::random());
        let header = serde_json::json!({"alg":"EdDSA","typ":"JWT","kid":"test-eddsa-key"});
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user456",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });

        let header_b64 = b64_encode(header.to_string());
        let claims_b64 = b64_encode(claims.to_string());
        let message = format!("{}.{}", header_b64, claims_b64);

        let signature = wrong_signing_key.sign(message.as_bytes());
        let sig_b64 = b64_encode(signature.to_bytes());

        let token = format!("{}.{}.{}", header_b64, claims_b64, sig_b64);

        let result = validator.validate(&token).await;
        assert_eq!(result.unwrap_err(), JwtError::InvalidSignature);
    }

    // ─── JWKS Cache Tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_jwks_key_not_found() {
        let jwks = JwksResponse {
            keys: vec![JwkEntry {
                kty: "RSA".to_string(),
                kid: Some("different-key".to_string()),
                n: Some("abc123".to_string()),
                e: Some("AQAB".to_string()),
                x: None,
                crv: None,
                extra: HashMap::new(),
            }],
        };

        let validator = JwtValidator::with_jwks(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            jwks,
        );

        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(
                serde_json::json!({"alg":"RS256","typ":"JWT","kid":"missing-key"}).to_string()
            ),
            b64_encode(claims)
        );

        let result = validator.validate(&token).await;
        assert_eq!(result.unwrap_err(), JwtError::KeyNotFound);
    }

    // ─── JWKS Fetch Integration Test ─────────────────────────────────────────

    /// Simple mock JWKS HTTP server for integration testing
    async fn run_mock_jwks_server(jwks_json: String) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
                let mut reader = tokio::io::BufReader::new(&mut socket);
                // Read headers until empty line
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    jwks_json.len(),
                    jwks_json
                );
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
                let _ = reader.get_mut().flush().await;
            }
        });

        // Give the server a moment to start listening
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        port
    }

    #[tokio::test]
    async fn test_jwks_fetch_from_endpoint() {
        use rand::rngs::OsRng;
        use rsa::pkcs8::EncodePrivateKey;
        use rsa::{traits::PublicKeyParts, RsaPrivateKey, RsaPublicKey};

        let private_key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let public_key = RsaPublicKey::from(&private_key);

        let n = b64_encode(public_key.n().to_bytes_be());
        let e = b64_encode(public_key.e().to_bytes_be());

        let jwks = serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "kid": "fetched-key",
                "n": n,
                "e": e,
                "alg": "RS256",
                "use": "sig"
            }]
        });

        let port = run_mock_jwks_server(jwks.to_string()).await;
        let validator = JwtValidator::new(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            Some(format!("http://127.0.0.1:{}/.well-known/jwks.json", port)),
        );

        let private_key_pem = private_key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes()).unwrap();

        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "fetched-user",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        });

        let token = encode(&Header::new(Algorithm::RS256), &claims, &encoding_key).unwrap();

        let result = validator.validate(&token).await.unwrap();
        assert_eq!(result.sub, "fetched-user");
    }

    #[tokio::test]
    async fn test_jwks_endpoint_failure() {
        // Bind to a port but don't respond — this will cause connection refused
        let validator = JwtValidator::new(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
            Some("http://127.0.0.1:1/.well-known/jwks.json".to_string()),
        );

        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": chrono::Utc::now().timestamp() + 3600,
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            b64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            b64_encode(claims)
        );

        let result = validator.validate(&token).await;
        assert_eq!(result.unwrap_err(), JwtError::KeyNotFound);
    }
}
