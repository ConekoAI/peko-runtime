//! DID (Decentralized Identifier) creation and validation

use crate::keys::KeyPair;
use serde::{Deserialize, Serialize};
use tracing::info;

/// DID method for Peko
pub const DID_METHOD: &str = "peko";

/// DID Identity document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// Self-certifying signing DID (did:key).
    pub did: String,
    /// DID document containing public keys and services
    pub document: DIDDocument,
    /// Private keys (stored securely)
    #[serde(skip)]
    pub keypair: Option<KeyPair>,
}

/// DID Document (public-facing identity)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DIDDocument {
    #[serde(rename = "@context")]
    pub context: Vec<String>,
    pub id: String,
    #[serde(rename = "verificationMethod")]
    pub verification_method: Vec<VerificationMethod>,
    #[serde(rename = "authentication")]
    pub authentication: Vec<String>,
    #[serde(rename = "assertionMethod")]
    pub assertion_method: Vec<String>,
    pub service: Vec<Service>,
    pub created: String,
    pub updated: String,
}

/// Verification method (public key)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationMethod {
    pub id: String,
    #[serde(rename = "type")]
    pub key_type: String,
    pub controller: String,
    #[serde(rename = "publicKeyMultibase")]
    pub public_key_multibase: String,
}

/// Service endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    pub id: String,
    #[serde(rename = "type")]
    pub service_type: String,
    #[serde(rename = "serviceEndpoint")]
    pub service_endpoint: String,
}

/// DID scope
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum DIDScope {
    /// Public - visible to everyone
    #[serde(rename = "public")]
    Public,
    /// Local - visible within tenant/organization
    #[serde(rename = "local")]
    Local,
    /// Private - visible only to owner
    #[serde(rename = "private")]
    Private,
}

impl std::fmt::Display for DIDScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DIDScope::Public => write!(f, "public"),
            DIDScope::Local => write!(f, "local"),
            DIDScope::Private => write!(f, "private"),
        }
    }
}

/// Parsed DID components
#[derive(Debug, Clone)]
pub struct ParsedDID {
    pub method: String,
    pub scope: DIDScope,
    pub tenant: Option<String>,
    pub key_hash: String,
}

impl Identity {
    /// Generate a new identity with keys
    pub fn generate(scope: DIDScope, tenant: Option<&str>) -> anyhow::Result<Self> {
        info!("Generating signing identity");
        let _ = (scope, tenant);
        Ok(Self::from_keypair(KeyPair::generate()))
    }

    /// Build a portable identity from a vault-backed signing key.
    #[must_use]
    pub fn from_keypair(keypair: KeyPair) -> Self {
        let did = crate::runtime::public_key_to_did_key(&keypair.public_key_bytes());
        let document = Self::create_did_document(&did, &keypair, DIDScope::Local);
        Self {
            did,
            document,
            keypair: Some(keypair),
        }
    }

    /// Create DID document
    fn create_did_document(did: &str, keypair: &KeyPair, _scope: DIDScope) -> DIDDocument {
        let key_id = format!("{did}#keys-1");
        let public_key_bytes = keypair.verifying_key.as_bytes();
        let public_key_multibase = format!("z{}", bs58::encode(public_key_bytes).into_string());

        DIDDocument {
            context: vec![
                "https://www.w3.org/ns/did/v1".to_string(),
                "https://w3id.org/security/suites/ed25519-2020/v1".to_string(),
            ],
            id: did.to_string(),
            verification_method: vec![VerificationMethod {
                id: key_id.clone(),
                key_type: "Ed25519VerificationKey2020".to_string(),
                controller: did.to_string(),
                public_key_multibase,
            }],
            authentication: vec![key_id.clone()],
            assertion_method: vec![key_id],
            service: vec![], // Empty initially, can add later
            created: chrono::Utc::now().to_rfc3339(),
            updated: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Parse a DID string
    pub fn parse_did(did: &str) -> anyhow::Result<ParsedDID> {
        if did.starts_with("did:key:") {
            let public_key = crate::runtime::did_key_to_public_key(did)?;
            return Ok(ParsedDID {
                method: "key".into(),
                scope: DIDScope::Local,
                tenant: None,
                key_hash: blake3::hash(&public_key).to_hex().to_string()[..16].into(),
            });
        }
        let parts: Vec<&str> = did.split(':').collect();

        if parts.len() < 3 || parts[0] != "did" || parts[1] != DID_METHOD {
            anyhow::bail!("Invalid DID format");
        }

        let scope = match parts[2] {
            "public" => DIDScope::Public,
            "local" => DIDScope::Local,
            "private" => DIDScope::Private,
            _ => anyhow::bail!("Invalid DID scope"),
        };

        // Format: did:peko:scope:keyhash OR did:peko:scope:tenant:keyhash
        let (tenant, key_hash) = if parts.len() == 4 {
            (None, parts[3].to_string())
        } else if parts.len() == 5 {
            (Some(parts[3].to_string()), parts[4].to_string())
        } else {
            anyhow::bail!("Invalid DID format");
        };

        Ok(ParsedDID {
            method: DID_METHOD.to_string(),
            scope,
            tenant,
            key_hash,
        })
    }

    /// Get the DID string
    #[must_use]
    pub fn did(&self) -> &str {
        &self.did
    }

    /// Serialize DID document to JSON
    pub fn document_to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(&self.document)?)
    }

    /// Resolve verification method by ID
    #[must_use]
    pub fn resolve_verification_method(&self, method_id: &str) -> Option<&VerificationMethod> {
        self.document
            .verification_method
            .iter()
            .find(|vm| vm.id == method_id)
    }

    /// Create an identity from a DID document and keypair (for import)
    pub fn from_did_document_and_key(
        document: DIDDocument,
        key_export: crate::keys::KeyPairExport,
    ) -> anyhow::Result<Self> {
        let keypair = KeyPair::import(&key_export)?;
        let did = document.id.clone();

        Ok(Self {
            did,
            document,
            keypair: Some(keypair),
        })
    }

    /// Generate a new identity asynchronously
    pub async fn new(name: &str, scope: DIDScope) -> anyhow::Result<Self> {
        let name = name.to_string();
        tokio::task::spawn_blocking(move || Self::generate(scope, Some(&name)))
            .await
            .map_err(|e| anyhow::anyhow!("Task failed: {e}"))?
    }

    /// Convert to DID document JSON
    pub fn to_did_document(&self) -> anyhow::Result<DIDDocument> {
        Ok(self.document.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_identity() {
        let identity = Identity::generate(DIDScope::Local, Some("acme")).unwrap();

        assert!(identity.did.starts_with("did:key:"));
        assert_eq!(
            crate::runtime::did_key_to_public_key(&identity.did).unwrap(),
            identity.keypair.as_ref().unwrap().public_key_bytes()
        );
        assert_eq!(identity.document.id, identity.did);
        assert!(!identity.document.verification_method.is_empty());
        assert!(identity.keypair.is_some());
    }

    #[test]
    fn test_parse_did() {
        let did = "did:peko:local:acme:abc123";
        let parsed = Identity::parse_did(did).unwrap();

        assert_eq!(parsed.method, "peko");
        assert_eq!(parsed.scope, DIDScope::Local);
        assert_eq!(parsed.tenant, Some("acme".to_string()));
        assert_eq!(parsed.key_hash, "abc123");
    }

    #[test]
    fn test_parse_public_did() {
        let did = "did:peko:public:xyz789";
        let parsed = Identity::parse_did(did).unwrap();

        assert_eq!(parsed.scope, DIDScope::Public);
        assert_eq!(parsed.tenant, None);
    }

    #[test]
    fn test_document_to_json() {
        let identity = Identity::generate(DIDScope::Public, None).unwrap();
        let json = identity.document_to_json().unwrap();

        assert!(json.contains("@context"));
        assert!(json.contains(&identity.did));
    }
}
