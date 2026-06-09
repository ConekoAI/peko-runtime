//! JWT validation for pekohub tokens
//!
//! **SECURITY WARNING (v0.1.0):** Cryptographic signature verification is NOT
//! implemented. The `validate()` method performs structural checks (format,
//! expiry, audience, issuer) but accepts ANY well-formed token. This means
//! token forgery is trivial for anyone who can construct a JWT-shaped string.
//!
//! `enable_pekohub_jwt` in `auth_config.toml` should remain `false` until
//! signature verification is wired up. See GitHub issue for tracking.

use super::caller::CallerContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Validated JWT claims extracted from a token
#[derive(Clone, Debug)]
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

// JWKS structures — defined here for future use when full JWKS fetching is implemented.
// For v0.1.0, JWT validation is structural (signature verification stubbed).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwkEntry {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkEntry>,
}

/// JWT validator
///
/// For v0.1.0, this is a stub that does basic structural validation.
/// Full JWKS fetching and signature verification will be implemented
/// when pekohub integration is complete.
#[derive(Clone)]
pub struct JwtValidator {
    trusted_issuers: Vec<String>,
    runtime_did: String,
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
    pub fn new(trusted_issuers: Vec<String>, runtime_did: String) -> Self {
        Self {
            trusted_issuers,
            runtime_did,
        }
    }

    /// Validate a JWT token.
    ///
    /// # Errors
    /// Returns `JwtError` if the token is structurally invalid or expired.
    ///
    /// # Security
    /// **v0.1.0 STUB:** Signature verification is NOT implemented. This method
    /// always returns `Err(JwtError::InvalidSignature)` to prevent accidental
    /// use of unverified JWTs. Re-enable when JWKS fetching + signature
    /// verification is wired up.
    pub fn validate(&self, _token: &str) -> Result<ValidatedJwt, JwtError> {
        // v0.1.0: JWT signature verification is disabled to prevent token forgery.
        // The structural validation code below is preserved for reference but
        // unreachable. Re-enable after implementing:
        //   1. JWKS endpoint fetching with caching
        //   2. RS256 / EdDSA signature verification
        //   3. Key ID (kid) resolution from JWT header
        Err(JwtError::InvalidSignature)
    }

    /// Structural validation logic (preserved for when signatures are enabled).
    #[allow(dead_code)]
    fn validate_structural(&self, token: &str) -> Result<ValidatedJwt, JwtError> {
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
            "RS256" | "EdDSA" => {
                // Supported algorithms — signature verification stubbed for v0.1.0
            }
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

        // NOTE: When re-enabling JWT support, add signature verification here.
        // In production, we would:
        // 1. Parse unverified header to determine `kid`
        // 2. Fetch key from JWKS cache
        // 3. Verify signature with ring or jsonwebtoken crate
        // This requires async HTTP client integration for JWKS refresh.

        Ok(ValidatedJwt {
            sub: claims.sub,
            name: claims.name,
            email: claims.email,
            permissions: claims.permissions.unwrap_or_default(),
        })
    }

    /// Build a CallerContext from a validated JWT.
    ///
    /// This is an associated function (does not need `self`) because
    /// all required data is in `ValidatedJwt`.
    #[must_use]
    pub fn to_caller(validated: ValidatedJwt) -> CallerContext {
        CallerContext::from_jwt(validated.sub)
    }
}

/// Base64URL-decode without padding
fn base64_decode(input: &str) -> Result<String, base64::DecodeError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let bytes = URL_SAFE_NO_PAD.decode(input)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validator() -> JwtValidator {
        JwtValidator::new(
            vec!["pekohub".to_string()],
            "did:key:z6MkTestRuntime".to_string(),
        )
    }

    // Helper: base64url-encode without padding
    fn base64_encode(data: String) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        URL_SAFE_NO_PAD.encode(data.as_bytes())
    }

    #[test]
    fn test_validate_rejects_all_tokens_in_v010() {
        // v0.1.0 security fix: validate() must reject ALL tokens because
        // signature verification is not implemented.
        let validator = test_validator();
        assert_eq!(
            validator.validate("not-a-jwt").unwrap_err(),
            JwtError::InvalidSignature
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
            base64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        assert_eq!(
            validator.validate(&token).unwrap_err(),
            JwtError::InvalidSignature
        );
    }

    #[test]
    fn test_structural_validation_invalid_format() {
        let validator = test_validator();
        assert_eq!(
            validator.validate_structural("not-a-jwt").unwrap_err(),
            JwtError::InvalidFormat
        );
    }

    #[test]
    fn test_structural_validation_expired_token() {
        let validator = test_validator();
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
            base64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        assert_eq!(
            validator.validate_structural(&token).unwrap_err(),
            JwtError::Expired
        );
    }

    #[test]
    fn test_structural_validation_invalid_audience() {
        let validator = test_validator();
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
            base64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        assert_eq!(
            validator.validate_structural(&token).unwrap_err(),
            JwtError::InvalidAudience
        );
    }

    #[test]
    fn test_structural_validation_invalid_issuer() {
        let validator = test_validator();
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
            base64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        assert_eq!(
            validator.validate_structural(&token).unwrap_err(),
            JwtError::InvalidIssuer
        );
    }

    #[test]
    fn test_structural_validation_unsupported_algorithm() {
        let validator = test_validator();
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
            base64_encode(serde_json::json!({"alg":"HS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        assert_eq!(
            validator.validate_structural(&token).unwrap_err(),
            JwtError::UnsupportedAlgorithm
        );
    }

    #[test]
    fn test_structural_validation_valid_token() {
        let validator = test_validator();
        let now = chrono::Utc::now().timestamp() + 3600;
        let claims = serde_json::json!({
            "iss": "pekohub",
            "sub": "user123",
            "aud": "did:key:z6MkTestRuntime",
            "exp": now,
            "name": "Test User",
            "email": "test@example.com",
            "permissions": ["read", "write"],
        })
        .to_string();
        let token = format!(
            "{}.{}.dummy",
            base64_encode(serde_json::json!({"alg":"RS256","typ":"JWT"}).to_string()),
            base64_encode(claims)
        );
        let result = validator.validate_structural(&token).unwrap();
        assert_eq!(result.sub, "user123");
        assert_eq!(result.name, Some("Test User".to_string()));
        assert_eq!(result.permissions, vec!["read", "write"]);
    }
}
