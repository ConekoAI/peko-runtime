//! Secret types and structures

use serde::{Deserialize, Serialize};
use std::fmt;

/// Secret types supported by the secret manager
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretType {
    /// API key (e.g., OpenAI, Stripe)
    ApiKey,
    /// OAuth token or access token
    Token,
    /// SSH private key
    SshKey,
    /// TLS certificate or private key
    Certificate,
    /// Generic password
    Password,
    /// Other secret type
    Other,
}

impl fmt::Display for SecretType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretType::ApiKey => write!(f, "api_key"),
            SecretType::Token => write!(f, "token"),
            SecretType::SshKey => write!(f, "ssh_key"),
            SecretType::Certificate => write!(f, "certificate"),
            SecretType::Password => write!(f, "password"),
            SecretType::Other => write!(f, "other"),
        }
    }
}

/// Secret scope — global or per-agent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretScope {
    /// Global secret accessible to all agents (with permissions)
    Global,
    /// Agent-specific secret
    Agent { did: String },
}

impl SecretScope {
    /// Create a global scope
    #[must_use]
    pub fn global() -> Self {
        Self::Global
    }

    /// Create an agent scope
    #[must_use]
    pub fn agent(did: impl Into<String>) -> Self {
        Self::Agent { did: did.into() }
    }

    /// Get the scope as a string for storage
    pub fn as_str(&self) -> String {
        match self {
            SecretScope::Global => "global".to_string(),
            SecretScope::Agent { did } => format!("agent:{did}"),
        }
    }
}

/// Metadata for a secret
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretMetadata {
    /// Human-readable description
    pub description: Option<String>,
    /// URL or hint for where to obtain this secret
    pub source_hint: Option<String>,
    /// Expiration date (if applicable)
    pub expires_at: Option<String>,
    /// Custom tags
    pub tags: Vec<String>,
}

impl Default for SecretMetadata {
    fn default() -> Self {
        Self {
            description: None,
            source_hint: None,
            expires_at: None,
            tags: Vec::new(),
        }
    }
}

/// A secret entry (without the actual value)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    /// Unique ID
    pub id: String,
    /// Secret name (unique per scope)
    pub name: String,
    /// Secret scope
    pub scope: SecretScope,
    /// Secret type
    pub secret_type: SecretType,
    /// Metadata
    pub metadata: SecretMetadata,
    /// Version number (for rotation tracking)
    pub version: u32,
    /// Creation timestamp
    pub created_at: String,
    /// Last update timestamp
    pub updated_at: String,
}

/// Secret with decrypted value (for internal use)
#[derive(Debug, Clone)]
pub struct Secret {
    pub entry: SecretEntry,
    pub value: secrecy::SecretString,
}

/// Permission levels for secret access
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretPermission {
    /// No access
    None,
    /// Read-only access
    Read,
    /// Read and write access
    Write,
}

/// Access control entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretAccessControl {
    /// Secret ID
    pub secret_id: String,
    /// Agent DID (None = applies to all agents for global secrets)
    pub agent_did: Option<String>,
    /// Permission level
    pub permission: SecretPermission,
}

/// Audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Entry ID
    pub id: String,
    /// Timestamp
    pub timestamp: String,
    /// Event type
    pub event: AuditEvent,
    /// Secret name (for readability)
    pub secret_name: String,
    /// Secret scope
    pub secret_scope: String,
    /// Agent DID (if applicable)
    pub agent_did: Option<String>,
    /// Whether the action succeeded
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Audit statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditStats {
    /// Total number of audit events
    pub total: usize,
    /// Number of successful operations
    pub successful: usize,
    /// Number of failed operations
    pub failed: usize,
    /// Number of access denied events
    pub access_denied: usize,
}

impl AuditStats {
    /// Calculate success rate as percentage
    #[must_use]
    pub fn success_rate(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            (self.successful as f64 / self.total as f64) * 100.0
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    /// Secret created
    SecretCreated,
    /// Secret accessed (read)
    SecretAccessed,
    /// Secret updated
    SecretUpdated,
    /// Secret deleted
    SecretDeleted,
    /// Permission granted
    PermissionGranted,
    /// Permission revoked
    PermissionRevoked,
    /// Master password unlock
    StoreUnlocked,
    /// Master password lock
    StoreLocked,
    /// Master password changed
    PasswordChanged,
    /// Failed access attempt
    AccessDenied,
}

impl fmt::Display for AuditEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditEvent::SecretCreated => write!(f, "SECRET_CREATED"),
            AuditEvent::SecretAccessed => write!(f, "SECRET_ACCESSED"),
            AuditEvent::SecretUpdated => write!(f, "SECRET_UPDATED"),
            AuditEvent::SecretDeleted => write!(f, "SECRET_DELETED"),
            AuditEvent::PermissionGranted => write!(f, "PERMISSION_GRANTED"),
            AuditEvent::PermissionRevoked => write!(f, "PERMISSION_REVOKED"),
            AuditEvent::StoreUnlocked => write!(f, "STORE_UNLOCKED"),
            AuditEvent::StoreLocked => write!(f, "STORE_LOCKED"),
            AuditEvent::PasswordChanged => write!(f, "PASSWORD_CHANGED"),
            AuditEvent::AccessDenied => write!(f, "ACCESS_DENIED"),
        }
    }
}
