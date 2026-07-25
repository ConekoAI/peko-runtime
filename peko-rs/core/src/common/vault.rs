//! Unified encrypted vault for runtime secrets.
//!
//! The vault stores all reversible runtime secrets in a single encrypted file
//! at `{config_dir}/vault.enc` (by default `~/.peko/vault.enc`).
//!
//! # Encryption
//!
//! The vault is encrypted with AES-256-GCM. The data-encryption key (DEK) is
//! obtained using one of two methods:
//!
//! 1. **OS keychain (default)** — a random 32-byte DEK is generated on first
//!    use and stored in the OS keychain under service `peko`, account
//!    `vault-key`.
//! 2. **Master passphrase fallback** — when the OS keychain is unavailable
//!    (headless/CI), or when the user has set `PEKO_MASTER_PASSPHRASE` and
//!    migrated with `peko vault migrate --to passphrase`, the DEK is
//!    derived from `PEKO_MASTER_PASSPHRASE` using Argon2id. A vault created
//!    this way stores a salt in its envelope and can only be unlocked with
//!    the same passphrase.
//!
//! # Switching modes
//!
//! The on-disk mode is determined by whether the envelope has a `salt`
//! field. To switch, run `peko vault migrate --to <passphrase|keychain>`.
//! The subcommand re-encrypts the vault under a new DEK and updates the
//! keychain entry as needed. It refuses to run while a peko daemon is
//! reachable over IPC, because the daemon holds a long-lived `Arc<Vault>`
//! whose unlock method is set at construction time.
//!
//! The `PEKO_UNLOCK_METHOD` env var is an *assertion* of the expected mode
//! for the current process (`auto` / `passphrase` / `keychain`). A
//! mismatch with the on-disk envelope is a hard error pointing at
//! `peko vault migrate`. The env var never mutates the envelope on disk.
//!
//! # Contents
//!
//! The plaintext inside the envelope is a `VaultFile`: a versioned map of
//! typed secret entries. Entries include provider API keys, registry tokens,
//! identity private keys, and tunnel private keys.
//!
//! # Thread safety
//!
//! `Vault` uses interior mutability (`std::sync::RwLock`) so it can be shared
//! across async tasks and implements the `SecretStore` trait used by
//! `LlmResolver`. Mutating methods automatically persist the vault.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use anyhow::{Context, Result};
use argon2::Argon2;
use chrono::{DateTime, Utc};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

/// On-disk vault filename.
pub const VAULT_FILE_NAME: &str = "vault.enc";

/// OS keychain service name for the vault DEK.
pub const KEYCHAIN_SERVICE: &str = "peko";

/// OS keychain account name for the vault DEK.
pub const KEYCHAIN_ACCOUNT: &str = "vault-key";

/// Environment variable used for passphrase-based vault unlock.
pub const MASTER_PASSPHRASE_ENV: &str = "PEKO_MASTER_PASSPHRASE";

/// Environment variable asserting which unlock method the current process
/// expects for the on-disk vault.
///
/// Values: `auto` (default — trust the on-disk envelope), `passphrase`,
/// `keychain`. A mismatch with the on-disk envelope is a hard error
/// pointing the user at `peko vault migrate`. The env var never mutates
/// the envelope on disk; the explicit subcommand does that.
pub const UNLOCK_METHOD_ENV: &str = "PEKO_UNLOCK_METHOD";

/// Current vault file format version.
pub const VAULT_VERSION: u32 = 1;

/// Per-process assertion of which unlock method the caller expects.
///
/// `Auto` is the historical default and trusts the on-disk envelope.
/// The other variants cause `Vault::load` to error if the envelope's
/// stored mode does not match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMethodOverride {
    /// Trust the on-disk envelope (current behavior).
    Auto,
    /// Require passphrase mode.
    Passphrase,
    /// Require keychain mode.
    Keychain,
}

impl UnlockMethodOverride {
    /// Parse the env var. Missing or empty string returns `Auto`.
    pub fn from_env() -> Self {
        match std::env::var(UNLOCK_METHOD_ENV) {
            Ok(s) if !s.is_empty() => match s.to_ascii_lowercase().as_str() {
                "auto" => Self::Auto,
                "passphrase" => Self::Passphrase,
                "keychain" => Self::Keychain,
                other => {
                    // Surface the bad value rather than silently falling
                    // back to Auto — a typo here would otherwise be invisible.
                    tracing::warn!(
                        "{}={other:?} is not a valid unlock method; expected auto|passphrase|keychain; falling back to auto",
                        UNLOCK_METHOD_ENV,
                    );
                    Self::Auto
                }
            },
            _ => Self::Auto,
        }
    }

    /// Convert into the corresponding `UnlockMethod`, if concrete.
    ///
    /// `Auto` has no concrete value and returns `None` — callers fall
    /// through to the on-disk envelope.
    #[must_use]
    pub fn as_unlock_method(self) -> Option<UnlockMethod> {
        match self {
            Self::Auto => None,
            Self::Passphrase => Some(UnlockMethod::Passphrase),
            Self::Keychain => Some(UnlockMethod::Keychain),
        }
    }
}

/// AES-GCM nonce length in bytes.
const NONCE_LENGTH: usize = 12;

/// AES-256 key length in bytes.
const KEY_LENGTH: usize = 32;

/// Test-only fallback passphrase used when the OS keychain is unavailable and
/// `PEKO_MASTER_PASSPHRASE` is not set. This is only compiled into test builds
/// so that unit tests are self-contained in headless environments.
#[cfg(test)]
const TEST_MASTER_PASSPHRASE: &str = "peko-unit-test-passphrase-do-not-use";

/// Argon2id default parameters for passphrase derivation.
const ARGON2_MEMORY_COST: u32 = 65536; // 64 MB
const ARGON2_TIME_COST: u32 = 3;
const ARGON2_PARALLELISM: u32 = 4;

/// Errors specific to vault operations.
#[derive(Debug, Error)]
pub enum VaultError {
    #[error("vault is locked: {0}")]
    Locked(String),

    #[error("vault backend error: {0}")]
    Backend(String),

    #[error("no master passphrase available; set {MASTER_PASSPHRASE_ENV} or use an OS keychain")]
    NoPassphrase,

    #[error("invalid secret entry type for key '{0}'")]
    InvalidEntryType(String),

    #[error("credential '{0}' is runtime-owned; use the runtime-specific command instead")]
    SystemCredential(String),
}

/// Encrypted envelope written to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEnvelope {
    pub version: u32,
    /// `None` when the DEK is stored in the OS keychain (raw key mode).
    /// `Some(salt)` when the DEK is derived from a passphrase.
    pub salt: Option<Vec<u8>>,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// Plaintext vault contents.
///
/// `version == 2` is the generic-credential schema (RP3A). The
/// envelope version (`VaultEnvelope::version`) stays at 1 because
/// only the plaintext structure changed, not the encryption
/// envelope.
///
/// A `VaultFileV1` (`{ version: 1, entries: HashMap<String, VaultEntry> }`)
/// is deserialized only during migration. After v1→v2 conversion,
/// `legacy_entries` is empty and the data lives under
/// `credentials`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    pub version: u32,
    /// Generic credential records keyed by `Credential::id`.
    #[serde(default)]
    pub credentials: BTreeMap<String, Credential>,
    /// Rotation bindings keyed by `{namespace}:{name}`.
    #[serde(default)]
    pub rotation_bindings: BTreeMap<String, RotationBinding>,
    /// Legacy v1 entries preserved for backward-compat decoding.
    /// Empty after migration; serialization uses a custom impl that
    /// skips empty maps so v2-on-disk files never write this field.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub legacy_entries: HashMap<String, VaultEntry>,
}

impl Default for VaultFile {
    fn default() -> Self {
        Self {
            version: 2,
            credentials: BTreeMap::new(),
            rotation_bindings: BTreeMap::new(),
            legacy_entries: HashMap::new(),
        }
    }
}

/// On-disk shape of a v1 vault file, kept only for one-time migration
/// when an existing user's vault predates the generic-credential
/// schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultFileV1 {
    pub version: u32,
    pub entries: HashMap<String, VaultEntry>,
}

impl VaultFileV1 {
    /// Convert a v1 file to v2 in memory: walk `entries`, build a
    /// `Credential` for each via the per-variant `from_legacy_*`
    /// constructors, and stash the originals in `legacy_entries` for
    /// any consumer that still wants to read them back. After this
    /// returns, the resulting `VaultFile` is a valid v2 file.
    fn into_v2(self) -> VaultFile {
        let mut credentials = BTreeMap::new();
        for (legacy_key, entry) in &self.entries {
            if let Some(c) = Self::credential_from_legacy(legacy_key, entry) {
                credentials.insert(c.id.clone(), c);
            }
        }
        VaultFile {
            version: 2,
            credentials,
            rotation_bindings: BTreeMap::new(),
            legacy_entries: self.entries,
        }
    }

    fn credential_from_legacy(legacy_key: &str, entry: &VaultEntry) -> Option<Credential> {
        match entry {
            VaultEntry::ProviderApiKey { provider, key } => Some(
                Credential::from_legacy_provider_key(provider, key, legacy_key),
            ),
            VaultEntry::RegistryToken {
                host,
                token,
                namespace,
            } => Some(Credential::from_legacy_registry_token(
                host,
                token,
                namespace.as_deref(),
                legacy_key,
            )),
            VaultEntry::IdentityPrivateKey {
                key_id,
                algorithm,
                key,
            } => Some(Credential::from_legacy_identity_key(
                key_id, algorithm, key, legacy_key,
            )),
            VaultEntry::TunnelPrivateKey { runtime_id, key } => Some(
                Credential::from_legacy_tunnel_key(runtime_id, key, legacy_key),
            ),
            VaultEntry::OAuthToken {
                server,
                access_token,
                refresh_token,
                expires_at,
            } => Some(Credential::from_legacy_oauth_token(
                server,
                access_token,
                refresh_token.as_deref(),
                *expires_at,
                legacy_key,
            )),
            VaultEntry::Secret { value } => Some(Credential::from_legacy_secret(value, legacy_key)),
        }
    }
}

/// A typed secret entry in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VaultEntry {
    /// LLM provider API key.
    ProviderApiKey { provider: String, key: String },
    /// PekoHub registry token.
    RegistryToken {
        host: String,
        token: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },
    /// Runtime identity private signing key.
    IdentityPrivateKey {
        key_id: String,
        algorithm: String,
        key: String,
    },
    /// PekoHub tunnel private key.
    TunnelPrivateKey { runtime_id: String, key: String },
    /// OAuth token for an MCP (or other) remote server.
    OAuthToken {
        server: String,
        access_token: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        refresh_token: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
    },
    /// Generic fallback secret.
    Secret { value: String },
}

/// A stored OAuth token for a remote server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokenEntry {
    /// Server identifier (matches the MCP server name).
    pub server: String,
    /// Current access token.
    pub access_token: String,
    /// Optional refresh token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Optional Unix timestamp when the access token expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

/// Generic credential kind — discriminates how a credential's material
/// is to be interpreted by consumers.
///
/// `Copy` so it can be used in filter structs and passed by value
/// without ceremony. `#[serde(rename_all = "snake_case")]` matches the
/// CLI's `--kind` argument spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// Static API key (`sk-...`, `sk-ant-...`, etc.) for an LLM
    /// provider or HTTP API.
    ApiKey,
    /// Bearer token for HTTP `Authorization: Bearer <material>`.
    BearerToken,
    /// OAuth access token (with optional `refresh_token` /
    /// `expires_at` in the metadata blob).
    OAuthToken,
    /// HTTP basic auth (`username:password` joined with a colon;
    /// the username lives in `metadata.username`).
    BasicAuth,
    /// Cryptographic private key (identity, tunnel, signing).
    PrivateKey,
    /// Generic fallback for secrets that don't fit any of the
    /// above kinds.
    GenericSecret,
}

impl CredentialKind {
    /// Stable lowercase wire form (matches the serde rename).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::BearerToken => "bearer_token",
            Self::OAuthToken => "oauth_token",
            Self::BasicAuth => "basic_auth",
            Self::PrivateKey => "private_key",
            Self::GenericSecret => "generic_secret",
        }
    }
}

/// A single stored credential in the generic vault.
///
/// One credential = one (namespace, name) slot holding a piece of
/// secret material. Multiple credentials can share a `(namespace,
/// name)` pair — they're picked up by a [`RotationBinding`] and
/// tried in order on 401 (RP3B wires the swap; RP3A wires the
/// storage).
///
/// `id` is a UUID v4 generated on first write. When a legacy
/// v1 entry is migrated, the id is a UUID v5 derived from the
/// legacy entry key for stability across reloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// Stable UUID identifying this credential. Used as the key in
    /// `VaultFile::credentials` and as the member of
    /// `RotationBinding::ordered_credential_ids`.
    pub id: String,
    /// Namespace, e.g. `provider:openai`, `mcp:analytics`,
    /// `registry:pekohub.ai`, `oauth:myremote`, `identity`,
    /// `tunnel`, `secret`.
    pub namespace: String,
    /// Slot name within the namespace. Most credentials are
    /// `default`; rotation scenarios add `alt-1`, `alt-2`, etc.
    pub name: String,
    /// Discriminator for how the material is to be consumed.
    pub kind: CredentialKind,
    /// Free-form per-kind metadata (OAuth `refresh_token` /
    /// `expires_at`, BasicAuth `username`, PrivateKey `algorithm`,
    /// etc.). The schema is enforced at the validator / set
    /// sites, not the type system.
    #[serde(default = "serde_json::Value::default")]
    pub metadata: serde_json::Value,
    /// The secret itself. Stored in memory as a [`SecretString`] so
    /// it doesn't leak into Debug / Display / log lines. Serialized
    /// to plain JSON via a custom helper that unwraps the secret
    /// during (de)serialization only — never log this field.
    #[serde(
        serialize_with = "serialize_secret_string",
        deserialize_with = "deserialize_secret_string"
    )]
    pub material: SecretString,
    /// When this credential was first written.
    pub created_at: DateTime<Utc>,
    /// When the material was last overwritten.
    pub updated_at: DateTime<Utc>,
    /// When `peko credential test <id>` (or its desktop equivalent)
    /// last verified this credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    /// Result of the last test (`true` = ok, `false` = validation
    /// / network failure). `None` until the first test runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_ok: Option<bool>,
    /// Whether this credential is runtime-owned and should be hidden
    /// from user-facing surfaces and protected from generic mutation.
    /// Defaults to `false` for backward compatibility with older v2
    /// vault files; the reserved namespaces `identity` and `tunnel`
    /// are also treated as system-owned regardless of this flag.
    #[serde(default)]
    pub system_owned: bool,
}

/// Reserved namespaces that hold runtime-owned credentials.
fn is_system_namespace(namespace: &str) -> bool {
    matches!(namespace, "identity" | "tunnel")
}
/// the secret on the way in and out so the on-disk format is
/// identical to a plain `String`. The on-disk vault is itself
/// encrypted, so this isn't a security regression vs. the old
/// `VaultEntry::ProviderApiKey { key: String }` layout.
fn serialize_secret_string<S>(secret: &SecretString, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(secret.expose_secret())
}

fn deserialize_secret_string<'de, D>(deserializer: D) -> Result<SecretString, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(SecretString::new(s.into()))
}

impl Credential {
    /// True if this credential is runtime-owned, either by explicit
    /// flag or by living in a reserved system namespace.
    #[must_use]
    pub fn is_system_owned(&self) -> bool {
        self.system_owned || is_system_namespace(&self.namespace)
    }

    /// Generate a fresh UUID v4 for a new credential.
    pub fn generate_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// Build a credential for the current moment.
    pub fn now(
        namespace: impl Into<String>,
        name: impl Into<String>,
        kind: CredentialKind,
        material: SecretString,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Self::generate_id(),
            namespace: namespace.into(),
            name: name.into(),
            kind,
            metadata: serde_json::Value::Null,
            material,
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: false,
        }
    }

    /// Deterministic UUID v5 from a legacy entry key. Used during
    /// v1→v2 migration so the same legacy entry always produces the
    /// same credential id across reloads.
    fn legacy_id(legacy_key: &str) -> String {
        Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!("peko-vault-legacy:{legacy_key}").as_bytes(),
        )
        .to_string()
    }

    /// Migrate a legacy `ProviderApiKey` entry to a Credential.
    fn from_legacy_provider_key(provider: &str, key: &str, legacy_key: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: format!("provider:{provider}"),
            name: "default".to_string(),
            kind: CredentialKind::ApiKey,
            metadata: serde_json::Value::Null,
            material: SecretString::new(key.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: false,
        }
    }

    /// Migrate a legacy `RegistryToken` entry.
    fn from_legacy_registry_token(
        host: &str,
        token: &str,
        namespace: Option<&str>,
        legacy_key: &str,
    ) -> Self {
        let now = Utc::now();
        let metadata = namespace
            .map(|ns| serde_json::json!({ "namespace": ns }))
            .unwrap_or(serde_json::Value::Null);
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: format!("registry:{host}"),
            name: "default".to_string(),
            kind: CredentialKind::BearerToken,
            metadata,
            material: SecretString::new(token.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: false,
        }
    }

    /// Migrate a legacy `IdentityPrivateKey` entry.
    fn from_legacy_identity_key(
        key_id: &str,
        algorithm: &str,
        key: &str,
        legacy_key: &str,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: "identity".to_string(),
            name: key_id.to_string(),
            kind: CredentialKind::PrivateKey,
            metadata: serde_json::json!({ "algorithm": algorithm }),
            material: SecretString::new(key.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: true,
        }
    }

    /// Migrate a legacy `TunnelPrivateKey` entry.
    fn from_legacy_tunnel_key(runtime_id: &str, key: &str, legacy_key: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: "tunnel".to_string(),
            name: runtime_id.to_string(),
            kind: CredentialKind::PrivateKey,
            metadata: serde_json::Value::Null,
            material: SecretString::new(key.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: true,
        }
    }

    /// Migrate a legacy `OAuthToken` entry.
    fn from_legacy_oauth_token(
        server: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        expires_at: Option<i64>,
        legacy_key: &str,
    ) -> Self {
        let now = Utc::now();
        let mut metadata = serde_json::Map::new();
        if let Some(rt) = refresh_token {
            metadata.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(rt.to_string()),
            );
        }
        if let Some(exp) = expires_at {
            metadata.insert(
                "expires_at".to_string(),
                serde_json::Value::Number(exp.into()),
            );
        }
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: format!("oauth:{server}"),
            name: "default".to_string(),
            kind: CredentialKind::OAuthToken,
            metadata: serde_json::Value::Object(metadata),
            material: SecretString::new(access_token.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: false,
        }
    }

    /// Migrate a legacy generic `Secret` entry. The legacy
    /// storage lost the original key (only `value` was kept), so
    /// the legacy entry key from the surrounding `HashMap` is used
    /// as the credential `name` to preserve whatever grouping the
    /// caller originally intended.
    fn from_legacy_secret(value: &str, legacy_key: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Self::legacy_id(legacy_key),
            namespace: "secret".to_string(),
            name: legacy_key.to_string(),
            kind: CredentialKind::GenericSecret,
            metadata: serde_json::Value::Null,
            material: SecretString::new(value.to_string().into()),
            created_at: now,
            updated_at: now,
            last_tested_at: None,
            last_tested_ok: None,
            system_owned: false,
        }
    }

    /// Strip the material field — used to produce a [`CredentialSummary`]
    /// for list endpoints that should never round-trip the secret to
    /// the wire.
    #[must_use]
    pub fn to_summary(&self) -> CredentialSummary {
        CredentialSummary {
            id: self.id.clone(),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            kind: self.kind,
            has_key: true,
            last_tested_at: self.last_tested_at,
            last_tested_ok: self.last_tested_ok,
            system_owned: self.is_system_owned(),
        }
    }
}

/// Redacted view of a [`Credential`] for list endpoints. Drops
/// `material` so a network sniff of the desktop's IPC stream can't
/// capture secrets; the full record is fetched via a separate
/// `CredentialGet` IPC (which also still hides material — the only
/// path that returns material is `CredentialGetMaterial`, which is
/// explicitly audit-logged).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialSummary {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub kind: CredentialKind,
    pub has_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_ok: Option<bool>,
    /// Whether this credential is runtime-owned. User-facing lists
    /// exclude system credentials by default.
    #[serde(default)]
    pub system_owned: bool,
}

/// Filter for [`Vault::list_credentials`]. Any `None` field matches
/// all values. System-owned credentials are excluded unless
/// `include_system` is set to `true`.
#[derive(Debug, Clone, Default)]
pub struct CredentialFilter {
    pub namespace: Option<String>,
    pub kind: Option<CredentialKind>,
    pub include_system: bool,
}

impl CredentialFilter {
    #[must_use]
    pub fn matches(&self, c: &Credential) -> bool {
        if let Some(ns) = &self.namespace {
            if &c.namespace != ns {
                return false;
            }
        }
        if let Some(k) = self.kind {
            if c.kind != k {
                return false;
            }
        }
        if c.is_system_owned() && !self.include_system {
            return false;
        }
        true
    }
}

/// Strategy used to walk through a rotation binding's credential
/// list on auth failure.
///
/// Only `RoundRobin` is honored by the resolver today. The other
/// variants deserialize from disk (so a v2 file written by an
/// older version with `last_resort` / `random` still loads), but
/// the resolver rejects them with a clear "unsupported rotation
/// strategy" error rather than silently picking the wrong key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationStrategy {
    /// Try each credential in order; on 401, advance to the next.
    /// After the last credential fails, surface the last error.
    RoundRobin,
    /// Reserved for future use. The resolver rejects this with a
    /// clear "unsupported" error if encountered.
    LastResort,
    /// Reserved for future use. Rejected by the resolver for now.
    Random,
}

impl RotationStrategy {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::LastResort => "last_resort",
            Self::Random => "random",
        }
    }
}

/// A rotation binding associates an ordered list of credential ids
/// with a `(namespace, name)` slot. On 401 from an LLM call, the
/// resolver advances to the next credential in
/// `ordered_credential_ids` and retries (RP3B wires the swap).
///
/// The `key` is the binding slot identifier (`{namespace}:{name}`)
/// so the binding map is keyed the same way as credentials for
/// ergonomic lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationBinding {
    pub strategy: RotationStrategy,
    pub ordered_credential_ids: Vec<String>,
}

impl RotationBinding {
    /// Build the binding-slot key from a namespace + name pair.
    #[must_use]
    pub fn slot_key(namespace: &str, name: &str) -> String {
        format!("{namespace}:{name}")
    }
}

/// How the vault DEK was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMethod {
    Keychain,
    Passphrase,
}

/// In-memory vault state holding the decrypted DEK and, for passphrase-backed
/// vaults, the salt used to derive it.
struct VaultState {
    file: VaultFile,
    dek: Vec<u8>,
    salt: Option<Vec<u8>>,
}

/// Unified encrypted secret vault.
pub struct Vault {
    path: PathBuf,
    inner: std::sync::RwLock<VaultState>,
    unlock_method: UnlockMethod,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("unlock_method", &self.unlock_method)
            .finish()
    }
}

impl Vault {
    /// Load an existing vault or create a new one at the given path.
    ///
    /// Preferentially uses the OS keychain. If the keychain is unavailable
    /// and the vault does not yet exist, falls back to
    /// `PEKO_MASTER_PASSPHRASE`. The caller can override the on-disk
    /// mode decision via `PEKO_UNLOCK_METHOD`; a mismatch with the
    /// envelope is a hard error.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_with_override(path, UnlockMethodOverride::from_env())
    }

    /// Like [`Self::load`], but with an explicit override.
    ///
    /// `UnlockMethodOverride::Auto` is the historical default — trust
    /// whatever the on-disk envelope says. `Passphrase` and `Keychain`
    /// assert a specific mode and error if it doesn't match the
    /// envelope's salt field.
    ///
    /// The `peko vault migrate` subcommand uses this with `Auto` so the
    /// migration can proceed regardless of what the user has set in
    /// `PEKO_UNLOCK_METHOD`.
    pub fn load_with_override(
        path: impl AsRef<Path>,
        method_override: UnlockMethodOverride,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if path.exists() {
            return Self::load_existing_with_override(path, method_override);
        }

        Self::create_new_with_override(path, method_override)
    }

    /// Load an existing passphrase-protected vault using the provided
    /// passphrase, bypassing environment-variable lookup.
    ///
    /// Returns an error if the vault was created in keychain mode.
    pub fn load_with_passphrase(path: impl AsRef<Path>, passphrase: &SecretString) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read vault: {}", path.display()))?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).with_context(|| "failed to parse vault envelope")?;

        if envelope.version != VAULT_VERSION {
            anyhow::bail!(
                "unsupported vault version: {} (expected {})",
                envelope.version,
                VAULT_VERSION
            );
        }

        let salt = envelope
            .salt
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("vault is not passphrase-protected"))?;
        let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), salt)?;
        let plaintext = Self::decrypt(&envelope, &dek)?;
        let file = Self::parse_vault_file(&plaintext)?;

        Ok(Self {
            path,
            inner: std::sync::RwLock::new(VaultState {
                file,
                dek,
                salt: Some(salt.to_vec()),
            }),
            unlock_method: UnlockMethod::Passphrase,
        })
    }

    /// Create a vault in the given directory with the provided master passphrase.
    ///
    /// This is useful for headless/CI environments where the OS keychain is
    /// not available. The passphrase is used directly to derive the DEK.
    pub fn with_passphrase(path: impl AsRef<Path>, passphrase: &SecretString) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let (file, dek, salt) = Self::new_file_with_passphrase(passphrase)?;
        let state = VaultState {
            file,
            dek,
            salt: Some(salt.clone()),
        };
        let vault = Self {
            path,
            inner: std::sync::RwLock::new(state),
            unlock_method: UnlockMethod::Passphrase,
        };
        vault.save_envelope(Some(&salt))?;
        info!(
            "Created new passphrase-protected vault at {}",
            vault.path.display()
        );
        Ok(vault)
    }

    /// Create a test vault using a temporary directory and a known passphrase.
    ///
    /// The vault file is created inside the provided directory.
    #[must_use]
    pub fn for_test(dir: &Path, passphrase: &str) -> Self {
        let path = dir.join(VAULT_FILE_NAME);
        Self::with_passphrase(&path, &SecretString::new(passphrase.into()))
            .expect("test vault creation should succeed")
    }

    /// Return the path to the vault file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the vault file from disk and swap the in-memory state.
    /// Used by the daemon after a CLI mutation (`peko credential set`,
    /// etc.) so the long-running process sees new keys without being
    /// restarted.
    ///
    /// The same `unlock_method` (keychain or passphrase) is reused — if
    /// the user has switched methods they'd need a full daemon
    /// restart, which is acceptable. On failure we keep the prior
    /// in-memory state so a transient fs hiccup doesn't blank the
    /// daemon. Returns the entry count after reload.
    pub fn reload(&self) -> Result<usize> {
        let bytes = std::fs::read(&self.path)
            .with_context(|| format!("failed to read vault: {}", self.path.display()))?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).with_context(|| "failed to parse vault envelope")?;
        if envelope.version != VAULT_VERSION {
            anyhow::bail!(
                "unsupported vault version: {} (expected {})",
                envelope.version,
                VAULT_VERSION
            );
        }
        let dek = match self.unlock_method {
            UnlockMethod::Passphrase => {
                let passphrase =
                    Self::passphrase_from_env_or_test_fallback().ok_or(VaultError::NoPassphrase)?;
                let salt = envelope.salt.as_deref().ok_or_else(|| {
                    VaultError::Backend("passphrase-mode vault missing salt".into())
                })?;
                Self::derive_key_from_passphrase(passphrase.expose_secret(), salt)?
            }
            UnlockMethod::Keychain => Self::retrieve_dek_from_keychain()
                .with_context(|| format!("while reloading vault at {}", self.path.display()))?,
        };
        let plaintext = Self::decrypt(&envelope, &dek)?;
        let file = Self::parse_vault_file(&plaintext)?;

        let count = file.credentials.len() + file.legacy_entries.len();
        let mut guard = self
            .inner
            .write()
            .map_err(|e| anyhow::anyhow!("vault reload: failed to acquire write lock: {e}"))?;
        guard.file = file;
        guard.dek = dek;
        Ok(count)
    }

    /// Return how the vault was unlocked.
    #[must_use]
    pub fn unlock_method(&self) -> UnlockMethod {
        self.unlock_method
    }

    // ------------------------------------------------------------------
    // Entry key namespacing (legacy only — kept so callers that still
    // poke at the legacy `entries` map can find entries by their old
    // key. New code should use `get_material_for` / `set_credential`.)
    // ------------------------------------------------------------------

    #[allow(dead_code)]
    fn provider_key(provider: &str) -> String {
        format!("provider:{provider}")
    }

    #[allow(dead_code)]
    fn registry_key(host: &str) -> String {
        format!("registry:{host}")
    }

    #[allow(dead_code)]
    fn identity_key(key_id: &str) -> String {
        format!("identity:{key_id}")
    }

    #[allow(dead_code)]
    fn tunnel_key(runtime_id: &str) -> String {
        format!("tunnel:{runtime_id}")
    }

    #[allow(dead_code)]
    fn oauth_token_key(server: &str) -> String {
        format!("oauth:{server}")
    }

    /// Return the credential id for a `(namespace, name)` slot if one
    /// already exists in the v2 `credentials` map. Used by the typed
    /// adapters below to preserve the id (and thus `created_at`)
    /// across overwrites.
    fn credential_id_for_slot(&self, namespace: &str, name: &str) -> Option<String> {
        let inner = self.inner.read().ok()?;
        inner
            .file
            .credentials
            .values()
            .find(|c| c.namespace == namespace && c.name == name)
            .map(|c| c.id.clone())
    }

    /// Return the credential ids for a `(namespace, name)` slot.
    /// Always returns a vec because future rotation flows allow
    /// multiple credentials at the same slot.
    fn credential_ids_for_slot(&self, namespace: &str, name: &str) -> Vec<String> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        inner
            .file
            .credentials
            .values()
            .filter(|c| c.namespace == namespace && c.name == name)
            .map(|c| c.id.clone())
            .collect()
    }

    // ------------------------------------------------------------------
    // Provider API keys (typed adapters over the generic API)
    // ------------------------------------------------------------------

    /// Get a provider API key.
    pub fn get_provider_key(&self, provider: &str) -> Option<SecretString> {
        self.get_material_for(&format!("provider:{provider}"), "default")
            .ok()
            .flatten()
    }

    /// Store or overwrite a provider API key.
    pub fn set_provider_key(&self, provider: &str, key: &SecretString) -> Result<()> {
        let namespace = format!("provider:{provider}");
        let mut c = Credential::now(
            namespace.clone(),
            "default",
            CredentialKind::ApiKey,
            key.clone(),
        );
        if let Some(id) = self.credential_id_for_slot(&namespace, "default") {
            c.id = id;
        }
        self.set_credential(&c)
    }

    /// Remove a provider API key.
    pub fn delete_provider_key(&self, provider: &str) -> Result<bool> {
        let namespace = format!("provider:{provider}");
        let ids = self.credential_ids_for_slot(&namespace, "default");
        let mut any = false;
        for id in ids {
            if self.delete_credential(&id)? {
                any = true;
            }
        }
        Ok(any)
    }

    /// Return all provider ids that have a stored API key.
    #[must_use]
    pub fn list_providers(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut providers: Vec<String> = inner
            .file
            .credentials
            .values()
            .filter(|c| c.namespace.starts_with("provider:") && c.name == "default")
            .map(|c| c.namespace.trim_start_matches("provider:").to_string())
            .collect();
        providers.sort();
        providers.dedup();
        providers
    }

    /// Cheap format check for a provider key.
    pub fn test_provider_key(&self, provider: &str) -> Option<bool> {
        let key = self.get_provider_key(provider)?;
        let s = key.expose_secret();
        let ok = match provider {
            "openai" | "azure-openai" | "azure" | "openrouter" | "together" | "fireworks"
            | "groq" | "deepseek" | "xai" | "grok" | "moonshot" | "kimi" => {
                s.starts_with("sk-") || s.len() > 10
            }
            "anthropic" => s.starts_with("sk-ant-") || s.len() > 10,
            "ollama" => true,
            _ => s.len() > 4 && !s.trim().is_empty(),
        };
        Some(ok)
    }

    // ------------------------------------------------------------------
    // Registry token (typed adapters)
    // ------------------------------------------------------------------

    /// Get the stored registry token, if any.
    ///
    /// Returns the first registry token found. Callers that know the host can
    /// use [`Self::get_registry_token_for_host`].
    pub fn get_registry_token(&self) -> Option<RegistryToken> {
        let inner = self.inner.read().ok()?;
        let cred = inner
            .file
            .credentials
            .values()
            .find(|c| c.namespace.starts_with("registry:") && c.name == "default")?;
        let host = cred.namespace.trim_start_matches("registry:").to_string();
        let namespace = cred
            .metadata
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from);
        Some(RegistryToken {
            host,
            token: cred.material.expose_secret().to_string(),
            namespace,
        })
    }

    /// Get the registry token for a specific host.
    pub fn get_registry_token_for_host(&self, host: &str) -> Option<RegistryToken> {
        let inner = self.inner.read().ok()?;
        let cred = inner
            .file
            .credentials
            .values()
            .find(|c| c.namespace == format!("registry:{host}") && c.name == "default")?;
        let namespace = cred
            .metadata
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from);
        Some(RegistryToken {
            host: host.to_string(),
            token: cred.material.expose_secret().to_string(),
            namespace,
        })
    }

    /// Store or overwrite the registry token for a host.
    pub fn set_registry_token(
        &self,
        host: &str,
        token: &str,
        namespace: Option<&str>,
    ) -> Result<()> {
        let ns = format!("registry:{host}");
        let mut c = Credential::now(
            ns.clone(),
            "default",
            CredentialKind::BearerToken,
            SecretString::new(token.to_string().into()),
        );
        if let Some(id) = self.credential_id_for_slot(&ns, "default") {
            c.id = id;
        }
        if let Some(n) = namespace {
            c.metadata = serde_json::json!({ "namespace": n });
        }
        self.set_credential(&c)
    }

    /// Clear the registry token for a host.
    pub fn clear_registry_token(&self, host: &str) -> Result<bool> {
        let ns = format!("registry:{host}");
        let ids = self.credential_ids_for_slot(&ns, "default");
        let mut any = false;
        for id in ids {
            if self.delete_credential(&id)? {
                any = true;
            }
        }
        Ok(any)
    }

    // ------------------------------------------------------------------
    // Identity private key (typed adapters)
    // ------------------------------------------------------------------

    /// Store a runtime identity private key.
    pub fn set_identity_private_key(&self, key_id: &str, algorithm: &str, key: &str) -> Result<()> {
        let mut c = Credential::now(
            "identity",
            key_id,
            CredentialKind::PrivateKey,
            SecretString::new(key.to_string().into()),
        );
        if let Some(id) = self.credential_id_for_slot("identity", key_id) {
            c.id = id;
        }
        c.metadata = serde_json::json!({ "algorithm": algorithm });
        c.system_owned = true;
        self.set_credential_internal(&c)
    }

    /// Get a runtime identity private key by key id.
    pub fn get_identity_private_key(&self, key_id: &str) -> Option<SecretString> {
        self.get_material_for("identity", key_id).ok().flatten()
    }

    /// Remove a runtime identity private key.
    pub fn delete_identity_private_key(&self, key_id: &str) -> Result<bool> {
        let ids = self.credential_ids_for_slot("identity", key_id);
        let mut any = false;
        for id in ids {
            if self.delete_credential_internal(&id)? {
                any = true;
            }
        }
        Ok(any)
    }

    // ------------------------------------------------------------------
    // Tunnel private key (typed adapters)
    // ------------------------------------------------------------------

    /// Store a PekoHub tunnel private key.
    pub fn set_tunnel_private_key(&self, runtime_id: &str, key: &str) -> Result<()> {
        let mut c = Credential::now(
            "tunnel",
            runtime_id,
            CredentialKind::PrivateKey,
            SecretString::new(key.to_string().into()),
        );
        if let Some(id) = self.credential_id_for_slot("tunnel", runtime_id) {
            c.id = id;
        }
        c.system_owned = true;
        self.set_credential_internal(&c)
    }

    /// Get a PekoHub tunnel private key by runtime id.
    pub fn get_tunnel_private_key(&self, runtime_id: &str) -> Option<SecretString> {
        self.get_material_for("tunnel", runtime_id).ok().flatten()
    }

    /// Remove a PekoHub tunnel private key.
    pub fn delete_tunnel_private_key(&self, runtime_id: &str) -> Result<bool> {
        let ids = self.credential_ids_for_slot("tunnel", runtime_id);
        let mut any = false;
        for id in ids {
            if self.delete_credential_internal(&id)? {
                any = true;
            }
        }
        Ok(any)
    }

    // ------------------------------------------------------------------
    // OAuth tokens (typed adapters)
    // ------------------------------------------------------------------

    /// Get an OAuth token entry for a remote server.
    #[must_use]
    pub fn get_oauth_token(&self, server: &str) -> Option<OAuthTokenEntry> {
        let inner = self.inner.read().ok()?;
        let cred = inner
            .file
            .credentials
            .values()
            .find(|c| c.namespace == format!("oauth:{server}") && c.name == "default")?;
        let refresh_token = cred
            .metadata
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(String::from);
        let expires_at = cred
            .metadata
            .get("expires_at")
            .and_then(serde_json::Value::as_i64);
        Some(OAuthTokenEntry {
            server: server.to_string(),
            access_token: cred.material.expose_secret().to_string(),
            refresh_token,
            expires_at,
        })
    }

    /// Store or overwrite an OAuth token entry for a remote server.
    pub fn set_oauth_token(&self, server: &str, entry: &OAuthTokenEntry) -> Result<()> {
        let ns = format!("oauth:{server}");
        let mut c = Credential::now(
            ns.clone(),
            "default",
            CredentialKind::OAuthToken,
            SecretString::new(entry.access_token.clone().into()),
        );
        if let Some(id) = self.credential_id_for_slot(&ns, "default") {
            c.id = id;
        }
        let mut metadata = serde_json::Map::new();
        if let Some(rt) = entry.refresh_token.as_ref() {
            metadata.insert(
                "refresh_token".to_string(),
                serde_json::Value::String(rt.clone()),
            );
        }
        if let Some(exp) = entry.expires_at {
            metadata.insert(
                "expires_at".to_string(),
                serde_json::Value::Number(exp.into()),
            );
        }
        c.metadata = serde_json::Value::Object(metadata);
        self.set_credential(&c)
    }

    /// Remove an OAuth token entry for a remote server.
    pub fn delete_oauth_token(&self, server: &str) -> Result<bool> {
        let ns = format!("oauth:{server}");
        let ids = self.credential_ids_for_slot(&ns, "default");
        let mut any = false;
        for id in ids {
            if self.delete_credential(&id)? {
                any = true;
            }
        }
        Ok(any)
    }

    // ------------------------------------------------------------------
    // Generic credential API (RP3A)
    // ------------------------------------------------------------------

    /// List credentials matching `filter`. Returns redacted summaries
    /// (no material). Stable order: sorted by `id` (UUID lexicographic,
    /// which approximates insertion-time ordering for v4 UUIDs).
    #[must_use]
    pub fn list_credentials(&self, filter: &CredentialFilter) -> Vec<CredentialSummary> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut summaries: Vec<CredentialSummary> = inner
            .file
            .credentials
            .values()
            .filter(|c| filter.matches(c))
            .map(Credential::to_summary)
            .collect();
        summaries.sort_by(|a, b| a.id.cmp(&b.id));
        summaries
    }

    /// Fetch the full record for `id` (including `material`). The
    /// caller is responsible for not serializing the material to a
    /// log line or a non-audit wire endpoint.
    #[must_use]
    pub fn get_credential(&self, id: &str) -> Option<Credential> {
        let inner = self.inner.read().ok()?;
        inner.file.credentials.get(id).cloned()
    }

    /// Insert or overwrite a credential by `id`. `updated_at` is
    /// bumped to "now" on overwrite; `created_at` is preserved.
    /// Rejects runtime-owned credentials; use the typed system
    /// adapters (`set_identity_private_key`, `set_tunnel_private_key`)
    /// for those.
    pub fn set_credential(&self, c: &Credential) -> Result<()> {
        if c.is_system_owned() {
            return Err(VaultError::SystemCredential(c.id.clone()).into());
        }
        self.set_credential_internal(c)
    }

    fn set_credential_internal(&self, c: &Credential) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            let mut to_store = c.clone();
            if let Some(existing) = inner.file.credentials.get(&c.id) {
                to_store.created_at = existing.created_at;
            }
            to_store.updated_at = Utc::now();
            inner.file.credentials.insert(c.id.clone(), to_store);
        }
        self.save()
    }

    /// Delete the credential with this `id`. Returns `true` if a
    /// credential was removed. Rejects deletion of runtime-owned
    /// credentials; use the typed system adapters for those.
    pub fn delete_credential(&self, id: &str) -> Result<bool> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        if let Some(c) = inner.file.credentials.get(id) {
            if c.is_system_owned() {
                return Err(VaultError::SystemCredential(id.to_string()).into());
            }
        }
        drop(inner);
        self.delete_credential_internal(id)
    }

    fn delete_credential_internal(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.credentials.remove(id).is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Look up the material for a `(namespace, name)` slot. Used by
    /// the resolver when there's no rotation binding (the common
    /// case). Returns `Ok(None)` when no credential exists at the
    /// slot; callers treat that as "no key configured".
    pub fn get_material_for(&self, namespace: &str, name: &str) -> Result<Option<SecretString>> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        // Prefer the first credential at (namespace, name); fall
        // through to legacy_entries for pre-v2 data.
        if let Some(c) = inner
            .file
            .credentials
            .values()
            .find(|c| c.namespace == namespace && c.name == name)
        {
            return Ok(Some(c.material.clone()));
        }
        if let Some(legacy_key) = Self::legacy_slot_key(namespace, name) {
            if let Some(entry) = inner.file.legacy_entries.get(&legacy_key) {
                return Ok(Self::legacy_entry_material(entry));
            }
        }
        Ok(None)
    }

    /// Look up the ordered list of materials for a `(namespace, name)`
    /// slot, walking the rotation binding if one exists. Returns the
    /// primary material in position 0 when no binding is configured
    /// (i.e. it falls back to [`Self::get_material_for`] and wraps
    /// the result in a single-element vec).
    ///
    /// The resolver (RP3B) uses this to pick the next credential on
    /// 401. Today (RP3A) only position 0 is consumed; positions 1..N
    /// are silently preserved so RP3B can wire the swap without a
    /// second data migration.
    pub fn get_material_with_rotation(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Vec<SecretString>> {
        let slot_key = RotationBinding::slot_key(namespace, name);
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;

        if let Some(binding) = inner.file.rotation_bindings.get(&slot_key) {
            let mut materials = Vec::with_capacity(binding.ordered_credential_ids.len());
            for id in &binding.ordered_credential_ids {
                if let Some(c) = inner.file.credentials.get(id) {
                    materials.push(c.material.clone());
                }
            }
            if !materials.is_empty() {
                return Ok(materials);
            }
        }

        // No binding (or binding had missing ids). Fall through to
        // the single-slot lookup.
        drop(inner);
        Ok(self
            .get_material_for(namespace, name)?
            .into_iter()
            .collect())
    }

    /// Look up a single rotation binding by slot key.
    #[must_use]
    pub fn get_binding(&self, namespace: &str, name: &str) -> Option<RotationBinding> {
        let inner = self.inner.read().ok()?;
        let slot_key = RotationBinding::slot_key(namespace, name);
        inner.file.rotation_bindings.get(&slot_key).cloned()
    }

    /// Return the ordered `(credential_id, material)` pairs for a
    /// rotation binding. If no binding exists, falls back to the single
    /// credential at `(namespace, name)` (with an empty id, since the
    /// non-binding path does not need per-credential test recording).
    pub fn get_rotation_credentials(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Vec<(String, SecretString)>> {
        let slot_key = RotationBinding::slot_key(namespace, name);
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;

        if let Some(binding) = inner.file.rotation_bindings.get(&slot_key) {
            let mut out = Vec::with_capacity(binding.ordered_credential_ids.len());
            for id in &binding.ordered_credential_ids {
                if let Some(c) = inner.file.credentials.get(id) {
                    out.push((id.clone(), c.material.clone()));
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }

        // No binding (or binding referenced only missing ids). Fall back
        // to any single credential at the slot; the id is empty because
        // this path is not used for rotation-aware test recording.
        drop(inner);
        if let Some(m) = self.get_material_for(namespace, name)? {
            return Ok(vec![(String::new(), m)]);
        }
        Ok(Vec::new())
    }

    /// List every rotation binding currently configured.
    #[must_use]
    pub fn list_bindings(&self) -> Vec<(String, RotationBinding)> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        inner
            .file
            .rotation_bindings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Store (or overwrite) a rotation binding for the given
    /// `{namespace}:{name}` slot key.
    pub fn set_binding(&self, slot_key: &str, binding: &RotationBinding) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner
                .file
                .rotation_bindings
                .insert(slot_key.to_string(), binding.clone());
        }
        self.save()
    }

    /// Delete a rotation binding by slot key. Returns `true` if a
    /// binding was removed.
    pub fn delete_binding(&self, slot_key: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.rotation_bindings.remove(slot_key).is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Record the outcome of `peko credential test <id>` against this
    /// credential so subsequent listings can surface "last tested"
    /// metadata without re-running the network check.
    pub fn record_test(&self, id: &str, ok: bool) -> Result<()> {
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            if let Some(c) = inner.file.credentials.get_mut(id) {
                c.last_tested_at = Some(Utc::now());
                c.last_tested_ok = Some(ok);
            } else {
                return Err(VaultError::Backend(format!(
                    "record_test: no credential with id {id:?}"
                ))
                .into());
            }
        }
        self.save()
    }

    /// Derive the legacy `entries` map key for a `(namespace, name)`
    /// pair. Returns `None` for namespaces that don't have a legacy
    /// encoding (e.g. `mcp:*`, `secret:*`).
    fn legacy_slot_key(namespace: &str, name: &str) -> Option<String> {
        // The provider / oauth / registry namespaces all stored at
        // a single slot under the legacy key, so the lookup uses
        // the bare legacy key (no `name` suffix). Other namespaces
        // didn't exist in v1 and have no legacy encoding.
        match namespace {
            ns if ns.starts_with("provider:") => Some(format!("provider:{}", &ns[9..])),
            ns if ns.starts_with("oauth:") => Some(format!("oauth:{}", &ns[6..])),
            ns if ns.starts_with("registry:") => Some(format!("registry:{}", &ns[9..])),
            "identity" => Some(format!("identity:{name}")),
            "tunnel" => Some(format!("tunnel:{name}")),
            _ => None,
        }
    }

    fn legacy_entry_material(entry: &VaultEntry) -> Option<SecretString> {
        match entry {
            VaultEntry::ProviderApiKey { key, .. } => Some(SecretString::new(key.clone().into())),
            VaultEntry::RegistryToken { token, .. } => {
                Some(SecretString::new(token.clone().into()))
            }
            VaultEntry::IdentityPrivateKey { key, .. } => {
                Some(SecretString::new(key.clone().into()))
            }
            VaultEntry::TunnelPrivateKey { key, .. } => Some(SecretString::new(key.clone().into())),
            VaultEntry::OAuthToken { access_token, .. } => {
                Some(SecretString::new(access_token.clone().into()))
            }
            VaultEntry::Secret { value } => Some(SecretString::new(value.clone().into())),
        }
    }

    // ------------------------------------------------------------------
    // Generic entry access (legacy)
    // ------------------------------------------------------------------

    /// Return a reference to a raw vault entry. Looks up the legacy
    /// v1 `entries` map; for new (post-migration) credentials, use
    /// [`Self::get_credential`].
    pub fn get_entry(&self, key: &str) -> Option<VaultEntry> {
        let inner = self.inner.read().ok()?;
        inner.file.legacy_entries.get(key).cloned()
    }

    /// Remove an arbitrary legacy entry by key.
    pub fn delete_entry(&self, key: &str) -> Result<bool> {
        let removed = {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            inner.file.legacy_entries.remove(key).is_some()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Return all entry keys in the vault (legacy entries only).
    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let inner = match self.inner.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut keys: Vec<String> = inner.file.legacy_entries.keys().cloned().collect();
        keys.sort();
        keys
    }

    // ------------------------------------------------------------------
    // Persistence
    // ------------------------------------------------------------------

    /// Persist the vault to disk.
    pub fn save(&self) -> Result<()> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        let salt = inner.salt.clone();
        Self::write_envelope(&self.path, &inner.dek, salt.as_deref(), &inner.file)
    }

    /// Rotate the DEK and re-encrypt the vault.
    ///
    /// Only supported for keychain-backed vaults.
    pub fn rotate_key(&self) -> Result<()> {
        if self.unlock_method != UnlockMethod::Keychain {
            anyhow::bail!("key rotation is only supported for keychain-backed vaults");
        }

        let new_dek = Self::generate_dek();
        {
            let mut inner = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            Self::store_dek_in_keychain(&new_dek)?;
            inner.dek = new_dek;
        }
        self.save()?;
        info!("Rotated vault DEK and re-encrypted {}", self.path.display());
        Ok(())
    }

    /// Re-encrypt the vault under a different unlock mode.
    ///
    /// This is the on-disk format switch: it rewrites the envelope under a
    /// new DEK (passphrase-derived or freshly generated) and updates the
    /// keychain entry as needed. It is the only path that mutates the
    /// envelope's unlock mode; the `PEKO_UNLOCK_METHOD` env var only
    /// *asserts* the mode and never rewrites.
    ///
    /// When `target` is the same as the current mode, this method still
    /// re-encrypts: passing `target = Passphrase` with a new passphrase
    /// rotates the passphrase. The "no-op when already in target mode"
    /// check is a *policy* decision that lives in the `peko vault migrate`
    /// CLI subcommand, not in this primitive.
    ///
    /// The migration is mostly atomic: the new envelope is written via the
    /// existing temp-file-then-rename helper before the old keychain entry
    /// is touched. If the process dies between the rename and the keychain
    /// cleanup, the on-disk state is already consistent and the orphaned
    /// keychain entry is harmless.
    ///
    /// Callers (the `peko vault migrate` subcommand) are responsible for
    /// refusing to run while a peko daemon is reachable over IPC, since
    /// the daemon holds a long-lived `Arc<Vault>` whose `unlock_method`
    /// field is not mutable via `reload()`.
    pub fn migrate(
        &mut self,
        target: UnlockMethod,
        passphrase: Option<&SecretString>,
    ) -> Result<UnlockMethod> {
        // Step 1: build the new DEK.
        let (new_dek, new_salt): (Vec<u8>, Option<Vec<u8>>) = match target {
            UnlockMethod::Keychain => {
                let dek = Self::generate_dek();
                Self::store_dek_in_keychain(&dek)?;
                (dek, None)
            }
            UnlockMethod::Passphrase => {
                let pw = passphrase.ok_or(VaultError::NoPassphrase)?;
                let mut salt = vec![0u8; 32];
                OsRng.fill_bytes(&mut salt);
                let dek = Self::derive_key_from_passphrase(pw.expose_secret(), &salt)?;
                (dek, Some(salt))
            }
        };

        // Step 2: snapshot the current plaintext. This is what we'll
        // re-encrypt under the new DEK.
        let file = {
            let guard = self
                .inner
                .read()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            guard.file.clone()
        };

        // Step 3: write the new envelope atomically. After this point
        // the on-disk state matches `target` and a process crash here
        // is recoverable: the old keychain DEK (if any) is still
        // available, but the envelope no longer accepts it. Recovery
        // is to run `peko vault migrate --to keychain` from the same
        // passphrase, which will regenerate a keychain DEK and re-write
        // the (already-passphrase) envelope — a no-op for the
        // plaintext but a no-op-correctness fix.
        Self::write_envelope(&self.path, &new_dek, new_salt.as_deref(), &file)?;

        // Step 4: clean up the old keychain entry when leaving keychain
        // mode. Best-effort — a leftover keychain entry is harmless
        // (the new envelope doesn't use it) but we surface the failure
        // so the operator knows to clean up manually if needed.
        if self.unlock_method == UnlockMethod::Keychain && target == UnlockMethod::Passphrase {
            if let Err(e) = Self::delete_dek_from_keychain() {
                tracing::warn!(
                    "failed to delete old vault DEK from keychain: {e}; \
                     remove it manually with Keychain Access (service '{KEYCHAIN_SERVICE}', \
                     account '{KEYCHAIN_ACCOUNT}') for a fully clean state"
                );
            }
        }

        // Step 5: update in-memory state.
        {
            let mut guard = self
                .inner
                .write()
                .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
            guard.dek = new_dek;
            guard.salt = new_salt.clone();
            // `file` is unchanged.
        }
        self.unlock_method = target;

        info!(
            "Migrated vault at {} from {:?} to {:?} mode",
            self.path.display(),
            // self.unlock_method was just reassigned, so capture the
            // *previous* mode for the log line.
            if target == UnlockMethod::Keychain {
                UnlockMethod::Passphrase
            } else {
                UnlockMethod::Keychain
            },
            target
        );
        Ok(target)
    }

    // ------------------------------------------------------------------
    // SecretStore trait integration
    // ------------------------------------------------------------------

    fn validate_account(
        account: &str,
    ) -> Result<(), peko_providers::secret_store::SecretStoreError> {
        if account.is_empty() {
            return Err(
                peko_providers::secret_store::SecretStoreError::InvalidAccount(
                    "empty account name".to_string(),
                ),
            );
        }
        if account.len() > 128 {
            return Err(
                peko_providers::secret_store::SecretStoreError::InvalidAccount(format!(
                    "account name too long ({} > 128 chars)",
                    account.len()
                )),
            );
        }
        if !account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        {
            return Err(
                peko_providers::secret_store::SecretStoreError::InvalidAccount(format!(
                    "account name '{account}' contains disallowed characters"
                )),
            );
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Parse decrypted plaintext into a v2 `VaultFile`, transparently
    /// migrating a v1 file (legacy `entries: HashMap<String, VaultEntry>`)
    /// into the new generic-credential schema.
    ///
    /// Branching on the JSON `version` discriminator (rather than a
    /// serde tagged enum) lets us preserve the `VaultFile` struct as
    /// the canonical in-memory shape; v1 is a transient migration
    /// type. The cost is one extra `serde_json::Value` parse — a
    /// negligible one-time tax on `Vault::load` / `Vault::reload`.
    fn parse_vault_file(plaintext: &[u8]) -> Result<VaultFile> {
        let value: serde_json::Value = serde_json::from_slice(plaintext)
            .with_context(|| "failed to parse vault contents as JSON")?;
        let version = value
            .get("version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("vault file missing 'version' field"))?;
        match version {
            2 => serde_json::from_value(value)
                .with_context(|| "failed to parse vault contents as v2 VaultFile"),
            1 => {
                let v1: VaultFileV1 = serde_json::from_value(value)
                    .with_context(|| "failed to parse vault contents as v1 VaultFile")?;
                Ok(v1.into_v2())
            }
            other => anyhow::bail!("unsupported vault file version: {other} (expected 1 or 2)"),
        }
    }

    fn load_existing_with_override(
        path: PathBuf,
        method_override: UnlockMethodOverride,
    ) -> Result<Self> {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read vault: {}", path.display()))?;
        let envelope: VaultEnvelope =
            serde_json::from_slice(&bytes).with_context(|| "failed to parse vault envelope")?;

        if envelope.version != VAULT_VERSION {
            anyhow::bail!(
                "unsupported vault version: {} (expected {})",
                envelope.version,
                VAULT_VERSION
            );
        }

        // The on-disk envelope determines which mode the vault unlocks in.
        // `PEKO_UNLOCK_METHOD` is an *assertion* by the caller, not a switch —
        // a mismatch is a hard error pointing at `peko vault migrate`.
        let on_disk_mode = if envelope.salt.is_some() {
            UnlockMethod::Passphrase
        } else {
            UnlockMethod::Keychain
        };
        let override_method = method_override.as_unlock_method();
        if let Some(requested) = override_method {
            if requested != on_disk_mode {
                anyhow::bail!(
                    "{UNLOCK_METHOD_ENV}={requested:?} does not match the vault's current mode ({on_disk_mode:?}); \
                     run `peko vault migrate --to {requested:?}` to switch, \
                     or unset {UNLOCK_METHOD_ENV} to use the existing mode"
                );
            }
        }

        let (dek, unlock_method, salt) = if envelope.salt.is_some() {
            // Passphrase mode.
            let passphrase =
                Self::passphrase_from_env_or_test_fallback().ok_or(VaultError::NoPassphrase)?;
            let salt = envelope.salt.as_deref().expect("checked above");
            let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), salt)?;
            (dek, UnlockMethod::Passphrase, Some(salt.to_vec()))
        } else {
            // Keychain mode.
            let dek = Self::retrieve_dek_from_keychain()
                .with_context(|| format!("while unlocking vault at {}", path.display()))?;
            (dek, UnlockMethod::Keychain, None)
        };

        let plaintext = Self::decrypt(&envelope, &dek)?;
        let file = Self::parse_vault_file(&plaintext)?;

        Ok(Self {
            path,
            inner: std::sync::RwLock::new(VaultState { file, dek, salt }),
            unlock_method,
        })
    }

    fn create_new_with_override(
        path: PathBuf,
        method_override: UnlockMethodOverride,
    ) -> Result<Self> {
        // In test builds, never probe or use the OS keychain. Tests run in
        // parallel and may be executed headless, so always derive the DEK from
        // PEKO_MASTER_PASSPHRASE (if set) or the test fallback. This avoids
        // keychain permission dialogs during local `cargo test` runs and keeps
        // CI deterministic.
        #[cfg(test)]
        {
            let _ = method_override; // unused in test build
            let passphrase = Self::passphrase_from_env_or_test_fallback()
                .expect("test passphrase fallback is always available");
            Self::with_passphrase(&path, &passphrase)
        }

        #[cfg(not(test))]
        {
            let keychain = peko_identity::keychain::KeychainStorage::with_service(
                KEYCHAIN_SERVICE.to_string(),
            );
            let (file, dek, salt, unlock_method) = match method_override.as_unlock_method() {
                // Caller asserted passphrase: skip the keychain probe and
                // derive the DEK from `PEKO_MASTER_PASSPHRASE`. This is the
                // path that lets a developer on macOS avoid keychain ACL
                // prompts even on a fresh install.
                Some(UnlockMethod::Passphrase) => {
                    let passphrase = Self::passphrase_from_env_or_test_fallback()
                        .ok_or(VaultError::NoPassphrase)?;
                    let (file, dek, salt) = Self::new_file_with_passphrase(&passphrase)?;
                    (file, dek, Some(salt), UnlockMethod::Passphrase)
                }
                // Caller asserted keychain: require it to actually be
                // available rather than silently downgrading.
                Some(UnlockMethod::Keychain) => {
                    if !keychain.is_available() {
                        anyhow::bail!(
                            "{UNLOCK_METHOD_ENV}=keychain but the OS keychain is unavailable; \
                             remove the override to allow passphrase fallback"
                        );
                    }
                    let dek = match Self::try_retrieve_dek_from_keychain() {
                        Ok(Some(dek)) => dek,
                        Ok(None) => {
                            let dek = Self::generate_dek();
                            Self::store_dek_in_keychain(&dek)?;
                            dek
                        }
                        Err(e) => return Err(e),
                    };
                    (VaultFile::default(), dek, None, UnlockMethod::Keychain)
                }
                // Default: prefer keychain when available, fall back to
                // passphrase (unchanged from pre-override behavior).
                None => {
                    if keychain.is_available() {
                        // If a DEK already exists in the keychain, reuse it so that a
                        // deleted vault file can be recreated without destroying the
                        // key needed to decrypt any backups of the old file.
                        let dek = match Self::try_retrieve_dek_from_keychain() {
                            Ok(Some(dek)) => dek,
                            Ok(None) => {
                                let dek = Self::generate_dek();
                                Self::store_dek_in_keychain(&dek)?;
                                dek
                            }
                            Err(e) => return Err(e),
                        };
                        (VaultFile::default(), dek, None, UnlockMethod::Keychain)
                    } else {
                        let passphrase = Self::passphrase_from_env_or_test_fallback()
                            .ok_or(VaultError::NoPassphrase)?;
                        let (file, dek, salt) = Self::new_file_with_passphrase(&passphrase)?;
                        (file, dek, Some(salt), UnlockMethod::Passphrase)
                    }
                }
            };

            let vault = Self {
                path,
                inner: std::sync::RwLock::new(VaultState {
                    file,
                    dek,
                    salt: salt.clone(),
                }),
                unlock_method,
            };
            vault.save_envelope(salt.as_deref())?;
            info!("Created new vault at {}", vault.path.display());
            Ok(vault)
        }
    }

    /// Return the configured master passphrase, if any.
    ///
    /// In test builds, falls back to a hardcoded test passphrase so that unit
    /// tests do not require an OS keychain or environment variable to create a
    /// vault. Production builds only use `PEKO_MASTER_PASSPHRASE`.
    fn passphrase_from_env_or_test_fallback() -> Option<SecretString> {
        std::env::var(MASTER_PASSPHRASE_ENV)
            .ok()
            .filter(|s| !s.is_empty())
            .map(|s| SecretString::new(s.into()))
            .or_else(|| {
                #[cfg(test)]
                {
                    Some(SecretString::new(TEST_MASTER_PASSPHRASE.into()))
                }
                #[cfg(not(test))]
                {
                    None
                }
            })
    }

    fn new_file_with_passphrase(
        passphrase: &SecretString,
    ) -> Result<(VaultFile, Vec<u8>, Vec<u8>)> {
        let mut salt = vec![0u8; 32];
        OsRng.fill_bytes(&mut salt);
        let dek = Self::derive_key_from_passphrase(passphrase.expose_secret(), &salt)?;
        Ok((VaultFile::default(), dek, salt))
    }

    fn save_envelope(&self, salt: Option<&[u8]>) -> Result<()> {
        let inner = self
            .inner
            .read()
            .map_err(|e| VaultError::Backend(format!("vault lock poisoned: {e}")))?;
        Self::write_envelope(&self.path, &inner.dek, salt, &inner.file)
    }

    fn write_envelope(
        path: &Path,
        dek: &[u8],
        salt: Option<&[u8]>,
        file: &VaultFile,
    ) -> Result<()> {
        let plaintext =
            serde_json::to_vec(file).with_context(|| "failed to serialize vault contents")?;
        let mut nonce = vec![0u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut nonce);

        let key = Key::<Aes256Gcm>::from_slice(dek);
        let cipher = Aes256Gcm::new(key);
        let nonce_slice = Nonce::from_slice(&nonce);
        let ciphertext = cipher
            .encrypt(nonce_slice, plaintext.as_ref())
            .map_err(|e| anyhow::anyhow!("failed to encrypt vault: {e:?}"))?;

        let envelope = VaultEnvelope {
            version: VAULT_VERSION,
            salt: salt.map(|s| s.to_vec()),
            nonce,
            ciphertext,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create vault directory: {parent:?}"))?;
        }

        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&envelope)?)
            .with_context(|| format!("failed to write vault temp file: {tmp:?}"))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("failed to finalize vault file: {path:?}"))?;

        #[cfg(unix)]
        {
            let permissions = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, permissions)
                .with_context(|| "failed to set vault file permissions")?;
        }

        Ok(())
    }

    fn decrypt(envelope: &VaultEnvelope, dek: &[u8]) -> Result<Vec<u8>> {
        let key = Key::<Aes256Gcm>::from_slice(dek);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&envelope.nonce);
        cipher
            .decrypt(nonce, envelope.ciphertext.as_ref())
            .map_err(|e| anyhow::anyhow!("failed to decrypt vault (wrong key?): {e:?}").into())
    }

    fn generate_dek() -> Vec<u8> {
        let mut dek = vec![0u8; KEY_LENGTH];
        OsRng.fill_bytes(&mut dek);
        dek
    }

    fn store_dek_in_keychain(dek: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .with_context(|| "failed to create keychain entry for vault DEK")?;
        let dek_hex = hex::encode(dek);
        entry
            .set_password(&dek_hex)
            .with_context(|| "failed to store vault DEK in OS keychain")?;
        Ok(())
    }

    /// Try to retrieve an existing DEK from the OS keychain.
    ///
    /// Returns `Ok(None)` if no entry exists, `Ok(Some(dek))` if a valid DEK
    /// is found, and propagates any other keychain error.
    fn try_retrieve_dek_from_keychain() -> Result<Option<Vec<u8>>> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .with_context(|| "failed to create keychain entry for vault DEK")?;
        match entry.get_password() {
            Ok(dek_hex) => {
                let dek = hex::decode(&dek_hex)
                    .with_context(|| "vault DEK in keychain is not valid hex")?;
                Ok(Some(dek))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e)
                .context("failed to retrieve vault DEK from OS keychain")
                .into()),
        }
    }

    fn retrieve_dek_from_keychain() -> Result<Vec<u8>> {
        Self::try_retrieve_dek_from_keychain()?
            .ok_or_else(|| anyhow::anyhow!(Self::missing_keychain_dek_message()))
    }

    /// Actionable diagnostic for the case where a keychain-mode
    /// vault's DEK has been removed from the OS keychain.
    ///
    /// When the on-disk envelope (no salt, i.e. mode = Keychain) is
    /// still present but `try_retrieve_dek_from_keychain` returns
    /// `Ok(None)`, the vault is unrecoverable without the DEK. A bare
    /// "not found" string leaves non-technical testers stuck; this
    /// names the service+account (so they can audit it themselves),
    /// lists the typical causes (macOS aging out unsigned-binary
    /// entries, manual `security delete-generic-password`, cross-
    /// machine copies), and tells them how to start fresh.
    ///
    /// The resulting string is long but information-dense; the chain
    /// only surfaces it to the user via `{:#}` Display (and `{:?}` in
    /// debug builds), so the verbosity is justified.
    fn missing_keychain_dek_message() -> String {
        format!(
            "no vault DEK found in OS keychain (service '{KEYCHAIN_SERVICE}', \
             account '{KEYCHAIN_ACCOUNT}'). The on-disk envelope is no \
             longer backed by a usable keychain entry — typically \
             because macOS cleaned up the entry (unsigned binaries are \
             especially prone to this), or because you ran a \
             `security delete-generic-password -s {KEYCHAIN_SERVICE} \
             -a {KEYCHAIN_ACCOUNT}`, or because the vault file was \
             copied to this machine without the matching keychain \
             item. The vault file is unrecoverable without the DEK; \
             to start fresh, move the file aside (e.g. \
             `mv ~/.peko/vault.enc ~/.peko/vault.enc.broken`) and \
             re-run — a fresh vault will be created from scratch."
        )
    }

    /// Delete the vault DEK from the OS keychain. Treats "no such entry"
    /// as success — the goal is a clean state, and the entry may already
    /// be gone.
    fn delete_dek_from_keychain() -> Result<()> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .with_context(|| "failed to create keychain entry for vault DEK deletion")?;
        match entry.delete_password() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!(e)
                .context("failed to delete vault DEK from OS keychain")
                .into()),
        }
    }

    fn derive_key_from_passphrase(passphrase: &str, salt: &[u8]) -> Result<Vec<u8>> {
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(
                ARGON2_MEMORY_COST,
                ARGON2_TIME_COST,
                ARGON2_PARALLELISM,
                None,
            )
            .map_err(|e| anyhow::anyhow!("invalid Argon2 params: {e}"))?,
        );
        let mut key = vec![0u8; KEY_LENGTH];
        argon2
            .hash_password_into(passphrase.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow::anyhow!("Argon2 derivation failed: {e:?}"))?;
        Ok(key)
    }
}

// =============================================================================
// `VaultAccess` impl — narrow cross-boundary view of `Vault` used by the
// extension framework's `services::reserved_params` module. The
// `peko-extension-host` crate owns the trait (host is a leaf, root is
// the facade); root's concrete `Vault` impls it via single delegation.
// The method shape mirrors `Vault::get_material_for` exactly so the
// impl is a one-liner.
// =============================================================================

impl peko_extension_host::vault::VaultAccess for Vault {
    fn get_material_for(
        &self,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<Option<secrecy::SecretString>> {
        Vault::get_material_for(self, namespace, name)
    }
}

/// Owned registry token entry.
#[derive(Debug, Clone)]
pub struct RegistryToken {
    pub host: String,
    pub token: String,
    pub namespace: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use secrecy::SecretString;
    use tempfile::TempDir;

    #[test]
    fn test_passphrase_vault_roundtrip() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        assert_eq!(vault.unlock_method(), UnlockMethod::Passphrase);

        vault
            .set_provider_key("openai", &SecretString::new("sk-test".into()))
            .unwrap();
        let key = vault.get_provider_key("openai").unwrap();
        assert_eq!(key.expose_secret(), "sk-test");

        // Reload from disk using the explicit passphrase.
        let reloaded =
            Vault::load_with_passphrase(vault.path(), &SecretString::new("test-passphrase".into()))
                .unwrap();
        let reloaded_key = reloaded.get_provider_key("openai").unwrap();
        assert_eq!(reloaded_key.expose_secret(), "sk-test");
    }

    #[test]
    fn test_provider_list_and_delete() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_provider_key("openai", &SecretString::new("sk-a".into()))
            .unwrap();
        vault
            .set_provider_key("anthropic", &SecretString::new("sk-ant-b".into()))
            .unwrap();

        let mut providers = vault.list_providers();
        providers.sort();
        assert_eq!(providers, vec!["anthropic", "openai"]);

        assert!(vault.delete_provider_key("openai").unwrap());
        assert!(vault.get_provider_key("openai").is_none());
        assert!(!vault.delete_provider_key("openai").unwrap());
    }

    /// `reload()` re-reads the on-disk file so a separate process
    /// that wrote to the vault (e.g. `peko credential set`) becomes
    /// visible to the long-running daemon that holds this Vault
    /// instance. Mirrors `ProviderCatalog::reload`.
    #[test]
    #[serial_test::serial]
    fn reload_picks_up_keys_written_by_another_holder() {
        use std::sync::Arc;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        // Holder 1: daemon-side. Loads the empty vault, keeps it
        // open. It does not see the keys we'll add below.
        std::env::set_var(MASTER_PASSPHRASE_ENV, "test-reload");
        let holder1 = Arc::new(Vault::for_test(dir.path(), "test-reload"));
        assert_eq!(holder1.list_providers().len(), 0);

        // Holder 2: simulates `peko credential set`. Writes keys via
        // its own Vault instance, then closes.
        let holder2 = Vault::for_test(dir.path(), "test-reload");
        holder2
            .set_provider_key("anthropic", &SecretString::new("sk-ant-reload".into()))
            .unwrap();
        holder2
            .set_provider_key("openai", &SecretString::new("sk-openai-reload".into()))
            .unwrap();
        assert!(path.exists(), "vault file should be persisted");

        // Holder 1 still has zero keys (no reload yet).
        assert_eq!(holder1.list_providers().len(), 0);

        // Reload → holder1 sees both keys, decrypted correctly.
        let count = holder1.reload().unwrap();
        assert_eq!(count, 2);
        let mut keys: Vec<String> = holder1.list_providers();
        keys.sort();
        assert_eq!(keys, vec!["anthropic", "openai"]);

        let stored = holder1
            .get_provider_key("anthropic")
            .expect("anthropic key should be readable after reload");
        assert_eq!(stored.expose_secret(), "sk-ant-reload");

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
    }

    #[test]
    fn test_registry_token() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_registry_token("pekohub.ai", "ph_abc", Some("acme"))
            .unwrap();
        let token = vault.get_registry_token().unwrap();
        assert_eq!(token.host, "pekohub.ai");
        assert_eq!(token.token, "ph_abc");
        assert_eq!(token.namespace, Some("acme".to_string()));

        assert!(vault.clear_registry_token("pekohub.ai").unwrap());
        assert!(vault.get_registry_token().is_none());
    }

    #[test]
    fn test_oauth_token_roundtrip() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        let entry = OAuthTokenEntry {
            server: "myremote".to_string(),
            access_token: "access-123".to_string(),
            refresh_token: Some("refresh-456".to_string()),
            expires_at: Some(1_700_000_000),
        };

        vault.set_oauth_token("myremote", &entry).unwrap();
        let stored = vault.get_oauth_token("myremote").unwrap();
        assert_eq!(stored.server, "myremote");
        assert_eq!(stored.access_token, "access-123");
        assert_eq!(stored.refresh_token, Some("refresh-456".to_string()));
        assert_eq!(stored.expires_at, Some(1_700_000_000));

        // Reload from disk and confirm persistence.
        let reloaded =
            Vault::load_with_passphrase(vault.path(), &SecretString::new("test-passphrase".into()))
                .unwrap();
        let reloaded_token = reloaded.get_oauth_token("myremote").unwrap();
        assert_eq!(reloaded_token.access_token, "access-123");

        assert!(vault.delete_oauth_token("myremote").unwrap());
        assert!(vault.get_oauth_token("myremote").is_none());
        assert!(!vault.delete_oauth_token("myremote").unwrap());
    }

    #[test]
    fn test_identity_key_storage() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_identity_private_key("did:key:z6MkTest#keys-1", "ed25519-raw-base64", "dGVzdA==")
            .unwrap();
        let key = vault
            .get_identity_private_key("did:key:z6MkTest#keys-1")
            .unwrap();
        assert_eq!(key.expose_secret(), "dGVzdA==");
    }

    #[test]
    fn test_tunnel_key_storage() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");

        vault
            .set_tunnel_private_key("did:key:z6MkTunnel", "dHVubmVsLWtleQ==")
            .unwrap();
        let key = vault.get_tunnel_private_key("did:key:z6MkTunnel").unwrap();
        assert_eq!(key.expose_secret(), "dHVubmVsLWtleQ==");
    }

    #[test]
    fn test_secret_store_trait() {
        // The trait impl moved to `VaultSecretStore` (root composition
        // layer adapter) in Phase 6 because the orphan rule forbids
        // `impl ForeignTrait for ForeignType` once `SecretStore` lives
        // in `peko-providers`. We exercise the trait surface through
        // the adapter here.
        use peko_providers::secret_store::SecretStore;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let vault = Arc::new(Vault::for_test(dir.path(), "test-passphrase"));
        let store = crate::common::vault_secret_store::VaultSecretStore::new(vault.clone());

        // Seed via Vault's own typed write API. The credential id is a
        // UUID; `VaultSecretStore::get(&str)` looks up by credential
        // id (matches the original `impl SecretStore for Vault` body).
        let cred = Credential::now(
            "openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-trait".into()),
        );
        vault.set_credential(&cred).unwrap();
        let cred_id = cred.id.clone();

        let got = store.get(&cred_id).unwrap().unwrap();
        assert_eq!(got.expose_secret(), "sk-trait");

        // The SecretStore trait adapter doesn't enumerate accounts.
        let accounts = store.list_accounts().unwrap();
        assert!(accounts.is_empty());

        // Trait write paths are deliberately rejected.
        assert!(store.delete(&cred_id).is_err());
    }

    // ------------------------------------------------------------------
    // migrate() + UnlockMethodOverride
    // ------------------------------------------------------------------

    /// Migrating a passphrase vault to a *different* passphrase
    /// re-encrypts the envelope under a fresh salt + new DEK. The
    /// old passphrase can no longer unlock it.
    ///
    /// Note: the underlying `migrate()` always re-encrypts even when
    /// `target == self.unlock_method()`. The "no-op when already in
    /// target mode" check is a policy decision that lives in the CLI
    /// subcommand, not in this primitive, so calling code that *wants*
    /// to rotate the passphrase without leaving passphrase mode can
    /// do so via this method.
    #[test]
    #[serial_test::serial]
    fn migrate_passphrase_to_passphrase_with_new_pw() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        // Create and seed the original passphrase vault.
        let old_pw = SecretString::new("old-passphrase".into());
        std::env::set_var(MASTER_PASSPHRASE_ENV, "old-passphrase");
        let mut vault = Vault::with_passphrase(&path, &old_pw).unwrap();
        vault
            .set_provider_key("openai", &SecretString::new("sk-keep".into()))
            .unwrap();
        let path_buf = path.clone();

        // Migrate to a new passphrase.
        let new_pw = SecretString::new("new-passphrase".into());
        let result = vault
            .migrate(UnlockMethod::Passphrase, Some(&new_pw))
            .expect("migrate should succeed");
        assert_eq!(result, UnlockMethod::Passphrase);

        // Old passphrase must NOT unlock the new envelope.
        std::env::set_var(MASTER_PASSPHRASE_ENV, "old-passphrase");
        let err = Vault::load_with_override(&path_buf, UnlockMethodOverride::Auto)
            .expect_err("old passphrase should be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("decrypt") || msg.contains("wrong key"),
            "unexpected error message: {msg}"
        );

        // New passphrase unlocks and the entry survives.
        std::env::set_var(MASTER_PASSPHRASE_ENV, "new-passphrase");
        let reloaded = Vault::load(&path_buf).expect("new passphrase should unlock");
        let key = reloaded
            .get_provider_key("openai")
            .expect("entry should survive the migration");
        assert_eq!(key.expose_secret(), "sk-keep");

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
    }

    /// `PEKO_UNLOCK_METHOD=passphrase` is a no-op when the envelope is
    /// already in passphrase mode — the env var is just an assertion.
    #[test]
    #[serial_test::serial]
    fn override_accepts_matching_envelope() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        let pw = SecretString::new("test-passphrase".into());
        std::env::set_var(MASTER_PASSPHRASE_ENV, "test-passphrase");
        let seed = Vault::with_passphrase(&path, &pw).unwrap();
        seed.set_provider_key("anthropic", &SecretString::new("sk-ant".into()))
            .unwrap();

        std::env::set_var(UNLOCK_METHOD_ENV, "passphrase");
        let loaded = Vault::load(&path).expect("matching override should load cleanly");
        let key = loaded
            .get_provider_key("anthropic")
            .expect("entry should be readable");
        assert_eq!(key.expose_secret(), "sk-ant");

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
        std::env::remove_var(UNLOCK_METHOD_ENV);
    }

    /// `PEKO_UNLOCK_METHOD=keychain` against a passphrase-mode envelope
    /// is a hard error that points the user at the migration subcommand.
    #[test]
    #[serial_test::serial]
    fn override_rejects_mismatched_envelope() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        let pw = SecretString::new("test-passphrase".into());
        std::env::set_var(MASTER_PASSPHRASE_ENV, "test-passphrase");
        let seed = Vault::with_passphrase(&path, &pw).unwrap();
        seed.set_provider_key("openai", &SecretString::new("sk-1".into()))
            .unwrap();

        std::env::set_var(UNLOCK_METHOD_ENV, "keychain");
        let err = Vault::load(&path).expect_err("mismatched override should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("peko vault migrate"),
            "error should point at the migration subcommand, got: {msg}"
        );
        // Debug-formatted UnlockMethod is "Keychain"; compare case-insensitively
        // so the assertion survives any future Debug-format tweak.
        assert!(
            msg.to_lowercase().contains("keychain"),
            "error should name the requested mode, got: {msg}"
        );

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
        std::env::remove_var(UNLOCK_METHOD_ENV);
    }

    /// A typo in `PEKO_UNLOCK_METHOD` logs a warning and falls back to
    /// `Auto` (i.e. the on-disk envelope is trusted). We can observe
    /// the fallback by setting a bogus value against a passphrase-mode
    /// envelope: the load must succeed.
    #[test]
    #[serial_test::serial]
    fn override_invalid_value_falls_back_to_auto() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        let pw = SecretString::new("test-passphrase".into());
        std::env::set_var(MASTER_PASSPHRASE_ENV, "test-passphrase");
        let seed = Vault::with_passphrase(&path, &pw).unwrap();
        seed.set_provider_key("openai", &SecretString::new("sk-x".into()))
            .unwrap();

        std::env::set_var(UNLOCK_METHOD_ENV, "biometric-or-whatever");
        let loaded = Vault::load(&path).expect("invalid override should fall back to Auto");
        assert_eq!(loaded.unlock_method(), UnlockMethod::Passphrase);
        let _ = loaded
            .get_provider_key("openai")
            .expect("entry should be readable");

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
        std::env::remove_var(UNLOCK_METHOD_ENV);
    }

    /// `Vault::load_with_override(_, Auto)` is the explicit form of
    /// the env-var-bypass used by the `peko vault migrate` subcommand:
    /// it loads the on-disk state regardless of what the user has set
    /// in `PEKO_UNLOCK_METHOD`.
    #[test]
    #[serial_test::serial]
    fn load_with_override_auto_bypasses_env_var() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        let pw = SecretString::new("test-passphrase".into());
        std::env::set_var(MASTER_PASSPHRASE_ENV, "test-passphrase");
        let seed = Vault::with_passphrase(&path, &pw).unwrap();
        seed.set_provider_key("openai", &SecretString::new("sk-y".into()))
            .unwrap();

        // Set the override to the *wrong* mode. `load_with_override(Auto)`
        // must still succeed — it observes the on-disk envelope.
        std::env::set_var(UNLOCK_METHOD_ENV, "keychain");
        let loaded = Vault::load_with_override(&path, UnlockMethodOverride::Auto)
            .expect("explicit Auto should bypass the env var");
        assert_eq!(loaded.unlock_method(), UnlockMethod::Passphrase);
        let _ = loaded
            .get_provider_key("openai")
            .expect("entry should be readable");

        std::env::remove_var(MASTER_PASSPHRASE_ENV);
        std::env::remove_var(UNLOCK_METHOD_ENV);
    }

    /// The "keychain DEK has been removed" diagnostic must mention
    /// the service+account by name (so the user can audit the keychain
    /// themselves), the typical cause (macOS unsigned-binary aging),
    /// the manual security-command trigger, and the recovery step
    /// (`mv` the file aside and re-run). Otherwise the user is stuck
    /// — `Vault::load` is non-recoverable when this happens, and
    /// without the diagnostic they have no actionable instruction.
    #[test]
    fn missing_keychain_dek_message_points_at_recovery() {
        let msg = Vault::missing_keychain_dek_message();

        // Service + account are named so the user can verify what's in
        // their keychain (`security find-generic-password -s peko -a vault-key`).
        assert!(
            msg.contains(KEYCHAIN_SERVICE),
            "message must name the service, got: {msg}"
        );
        assert!(
            msg.contains(KEYCHAIN_ACCOUNT),
            "message must name the account, got: {msg}"
        );

        // Causes — at least the typical macOS age-out path and the manual
        // security command — must both be mentioned.
        assert!(
            msg.contains("unsigned"),
            "message must mention the typical unsigned-binary age-out cause, got: {msg}"
        );
        assert!(
            msg.contains("security delete-generic-password"),
            "message must name the manual cleanup command, got: {msg}"
        );

        // Recovery step must be a single user-runnable command, not prose-only.
        assert!(
            msg.contains("mv ") && msg.contains(".enc"),
            "message must give a concrete `mv` recovery command, got: {msg}"
        );

        // Ends on a complete sentence (not a fragment) — otherwise it'll
        // chain oddly inside anyhow's `{:#}` Display.
        assert!(msg.trim_end().ends_with('.'));
    }

    // ------------------------------------------------------------------
    // RP3A: Generic credential API
    // ------------------------------------------------------------------

    /// `set_credential` writes a Credential that survives a reload.
    /// Confirms the v2 on-disk format is being written (the on-disk
    /// file's plaintext version field is 2).
    #[test]
    #[serial_test::serial]
    fn new_credential_roundtrip_persists_as_v2() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c = Credential::now(
            "provider:openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-test".into()),
        );
        vault.set_credential(&c).unwrap();

        // Re-load from disk and confirm version=2 + material survives.
        let reloaded =
            Vault::load_with_passphrase(vault.path(), &SecretString::new("test-passphrase".into()))
                .unwrap();
        let got = reloaded.get_credential(&c.id).unwrap();
        assert_eq!(got.namespace, "provider:openai");
        assert_eq!(got.name, "default");
        assert_eq!(got.kind, CredentialKind::ApiKey);
        assert_eq!(got.material.expose_secret(), "sk-test");
    }

    /// Overwriting a credential by id preserves `created_at` and
    /// bumps `updated_at`.
    #[test]
    fn set_credential_overwrites_preserving_created_at() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c1 = Credential::now(
            "provider:anthropic",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-ant-1".into()),
        );
        let original_created = c1.created_at;
        vault.set_credential(&c1).unwrap();

        // Sleep briefly so updated_at is strictly greater.
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut c2 = c1.clone();
        c2.material = SecretString::new("sk-ant-2".into());
        vault.set_credential(&c2).unwrap();

        let got = vault.get_credential(&c1.id).unwrap();
        assert_eq!(got.created_at, original_created, "created_at preserved");
        assert!(got.updated_at > original_created, "updated_at bumped");
        assert_eq!(got.material.expose_secret(), "sk-ant-2");
    }

    /// `delete_credential` is idempotent: the second call returns false.
    #[test]
    fn delete_credential_idempotent() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c = Credential::now(
            "mcp:analytics",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("m".into()),
        );
        vault.set_credential(&c).unwrap();
        assert!(vault.delete_credential(&c.id).unwrap());
        assert!(!vault.delete_credential(&c.id).unwrap());
    }

    /// `list_credentials` respects the filter on namespace and kind.
    #[test]
    fn list_credentials_with_filter() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        vault
            .set_credential(&Credential::now(
                "provider:openai",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("a".into()),
            ))
            .unwrap();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("b".into()),
            ))
            .unwrap();
        vault
            .set_credential(&Credential::now(
                "oauth:myremote",
                "default",
                CredentialKind::OAuthToken,
                SecretString::new("c".into()),
            ))
            .unwrap();

        let all = vault.list_credentials(&CredentialFilter::default());
        assert_eq!(all.len(), 3);

        let provider_only = vault.list_credentials(&CredentialFilter {
            namespace: Some("provider:openai".to_string()),
            ..Default::default()
        });
        assert_eq!(provider_only.len(), 1);
        assert_eq!(provider_only[0].namespace, "provider:openai");

        let oauth_only = vault.list_credentials(&CredentialFilter {
            kind: Some(CredentialKind::OAuthToken),
            ..Default::default()
        });
        assert_eq!(oauth_only.len(), 1);
        assert_eq!(oauth_only[0].namespace, "oauth:myremote");

        // Material is redacted in summaries.
        assert!(all.iter().all(|s| s.has_key));
    }

    /// `get_material_for` returns `None` when no credential exists at
    /// the requested `(namespace, name)` slot.
    #[test]
    fn get_material_for_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        assert!(vault
            .get_material_for("provider:openai", "default")
            .unwrap()
            .is_none());
        assert!(vault
            .get_material_for("oauth:nope", "default")
            .unwrap()
            .is_none());
    }

    /// `set_binding` + `get_material_with_rotation` returns the
    /// credential materials in the order specified by the binding.
    #[test]
    fn set_binding_then_get_material_with_rotation_orders_credentials() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c1 = Credential::now(
            "provider:anthropic",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-ant-AAA".into()),
        );
        let c2 = Credential::now(
            "provider:anthropic",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-ant-BBB".into()),
        );
        let c3 = Credential::now(
            "provider:anthropic",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-ant-CCC".into()),
        );
        vault.set_credential(&c1).unwrap();
        vault.set_credential(&c2).unwrap();
        vault.set_credential(&c3).unwrap();

        // Bind in a deliberate non-insertion order.
        let binding = RotationBinding {
            strategy: RotationStrategy::RoundRobin,
            ordered_credential_ids: vec![c3.id.clone(), c1.id.clone(), c2.id.clone()],
        };
        vault
            .set_binding(
                &RotationBinding::slot_key("provider:anthropic", "default"),
                &binding,
            )
            .unwrap();

        let materials = vault
            .get_material_with_rotation("provider:anthropic", "default")
            .unwrap();
        let strs: Vec<&str> = materials.iter().map(|s| s.expose_secret()).collect();
        assert_eq!(
            strs,
            vec!["sk-ant-CCC", "sk-ant-AAA", "sk-ant-BBB"],
            "rotation binding order should be honored"
        );

        // List-bindings + delete-binding are symmetric.
        let bindings = vault.list_bindings();
        assert_eq!(bindings.len(), 1);
        let slot_key = &bindings[0].0;
        assert_eq!(
            slot_key,
            &RotationBinding::slot_key("provider:anthropic", "default")
        );
        assert!(vault.delete_binding(slot_key).unwrap());
        assert!(!vault.delete_binding(slot_key).unwrap());

        // After deleting the binding, the resolver falls back to
        // a single material (the first matching credential).
        let materials = vault
            .get_material_with_rotation("provider:anthropic", "default")
            .unwrap();
        assert_eq!(materials.len(), 1);
    }

    /// `record_test` populates `last_tested_at` and `last_tested_ok`
    /// on the matching credential.
    #[test]
    fn record_test_populates_last_tested_at_and_ok() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c = Credential::now(
            "provider:openai",
            "default",
            CredentialKind::ApiKey,
            SecretString::new("sk-test".into()),
        );
        vault.set_credential(&c).unwrap();

        assert!(vault
            .get_credential(&c.id)
            .unwrap()
            .last_tested_at
            .is_none());
        vault.record_test(&c.id, true).unwrap();

        let got = vault.get_credential(&c.id).unwrap();
        assert!(got.last_tested_at.is_some());
        assert_eq!(got.last_tested_ok, Some(true));

        vault.record_test(&c.id, false).unwrap();
        let got = vault.get_credential(&c.id).unwrap();
        assert_eq!(got.last_tested_ok, Some(false));

        // Unknown id surfaces as an error rather than silently
        // dropping the test result.
        assert!(vault.record_test("nonexistent", true).is_err());
    }

    /// A v1 VaultFile loaded into v2 surfaces its legacy entries
    /// as `Credential` records on the credentials map.
    ///
    /// We build the v1 plaintext directly here (no need to go via
    /// `VaultEntry`-typed adapter methods, which now write v2).
    #[test]
    #[serial_test::serial]
    fn legacy_provider_api_key_migrates_on_load() {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        // Manually write a v1 plaintext + envelope so we exercise
        // the migration path.
        let plaintext = serde_json::to_vec(&VaultFileV1 {
            version: 1,
            entries: [(
                "provider:openai".to_string(),
                VaultEntry::ProviderApiKey {
                    provider: "openai".to_string(),
                    key: "sk-legacy".to_string(),
                },
            )]
            .into_iter()
            .collect(),
        })
        .unwrap();

        // Derive a DEK from a known passphrase + a fixed salt and
        // encrypt the v1 plaintext into a v1 envelope.
        let mut salt = [0u8; 32];
        salt[..5].copy_from_slice(b"salts");
        let passphrase = "test-passphrase".to_string();
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(65536, 3, 4, None).unwrap(),
        );
        let mut dek = [0u8; 32];
        argon2
            .hash_password_into(passphrase.as_bytes(), &salt, &mut dek)
            .unwrap();
        let key = Key::<Aes256Gcm>::from_slice(&dek);
        let cipher = Aes256Gcm::new(key);
        let nonce_bytes = [7u8; 12];
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, plaintext.as_ref()).unwrap();

        let envelope = VaultEnvelope {
            version: 1,
            salt: Some(salt.to_vec()),
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        };
        std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

        // Load via `load_with_passphrase`, which exercises
        // `parse_vault_file` and thus the v1→v2 migration.
        std::env::set_var(MASTER_PASSPHRASE_ENV, passphrase.as_str());
        let vault = Vault::load_with_passphrase(&path, &SecretString::new(passphrase.into()))
            .expect("v1 vault should load via migration");
        std::env::remove_var(MASTER_PASSPHRASE_ENV);

        // The legacy entry has been materialized as a Credential.
        let creds = vault.list_credentials(&CredentialFilter {
            namespace: Some("provider:openai".to_string()),
            ..Default::default()
        });
        assert_eq!(creds.len(), 1);
        assert_eq!(creds[0].namespace, "provider:openai");
        assert_eq!(creds[0].kind, CredentialKind::ApiKey);

        // And `get_material_for` returns the legacy value.
        let material = vault
            .get_material_for("provider:openai", "default")
            .unwrap()
            .unwrap();
        assert_eq!(material.expose_secret(), "sk-legacy");
    }

    /// OAuth refresh_token + expires_at survive the v1→v2 migration
    /// in the credential's metadata blob.
    #[test]
    #[serial_test::serial]
    fn legacy_oauth_preserves_refresh_token() {
        use aes_gcm::aead::Aead;
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let dir = TempDir::new().unwrap();
        let path = dir.path().join(VAULT_FILE_NAME);

        let plaintext = serde_json::to_vec(&VaultFileV1 {
            version: 1,
            entries: [(
                "oauth:myremote".to_string(),
                VaultEntry::OAuthToken {
                    server: "myremote".to_string(),
                    access_token: "access-123".to_string(),
                    refresh_token: Some("refresh-456".to_string()),
                    expires_at: Some(1_700_000_000),
                },
            )]
            .into_iter()
            .collect(),
        })
        .unwrap();

        let mut salt = [0u8; 32];
        salt[..5].copy_from_slice(b"salts");
        let passphrase = "test-passphrase".to_string();
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(65536, 3, 4, None).unwrap(),
        );
        let mut dek = [0u8; 32];
        argon2
            .hash_password_into(passphrase.as_bytes(), &salt, &mut dek)
            .unwrap();
        let key = Key::<Aes256Gcm>::from_slice(&dek);
        let cipher = Aes256Gcm::new(key);
        let nonce_bytes = [8u8; 12];
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, plaintext.as_ref()).unwrap();

        let envelope = VaultEnvelope {
            version: 1,
            salt: Some(salt.to_vec()),
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        };
        std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

        std::env::set_var(MASTER_PASSPHRASE_ENV, passphrase.as_str());
        let vault = Vault::load_with_passphrase(&path, &SecretString::new(passphrase.into()))
            .expect("v1 oauth vault should load");
        std::env::remove_var(MASTER_PASSPHRASE_ENV);

        // The legacy OAuthToken has been migrated into the new
        // Credential shape with metadata holding refresh_token +
        // expires_at.
        let token = vault.get_oauth_token("myremote").expect("oauth token");
        assert_eq!(token.access_token, "access-123");
        assert_eq!(token.refresh_token.as_deref(), Some("refresh-456"));
        assert_eq!(token.expires_at, Some(1_700_000_000));
    }

    /// System-owned credentials are hidden from default listings and can
    /// be surfaced with `include_system`.
    #[test]
    fn system_credentials_excluded_by_default_and_included_when_requested() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        vault
            .set_provider_key("openai", &SecretString::new("sk-1".into()))
            .unwrap();
        vault
            .set_identity_private_key(
                "did:key:z6MkTest#keys-1",
                "ed25519-raw-base64",
                &base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
            )
            .unwrap();

        let default_list = vault.list_credentials(&CredentialFilter::default());
        assert_eq!(default_list.len(), 1);
        assert_eq!(default_list[0].namespace, "provider:openai");

        let with_system = vault.list_credentials(&CredentialFilter {
            include_system: true,
            ..Default::default()
        });
        assert_eq!(with_system.len(), 2);
        assert!(with_system
            .iter()
            .any(|s| s.namespace == "identity" && s.system_owned));
    }

    /// The reserved `identity` and `tunnel` namespaces are treated as
    /// system-owned even when the on-disk flag is `false`, protecting
    /// pre-existing v2 credentials.
    #[test]
    fn reserved_namespaces_treated_as_system_regardless_of_flag() {
        let mut c = Credential::now(
            "identity",
            "default",
            CredentialKind::PrivateKey,
            SecretString::new("m".into()),
        );
        c.system_owned = false;
        assert!(c.is_system_owned());
        assert!(!CredentialFilter::default().matches(&c));
        assert!(CredentialFilter {
            include_system: true,
            ..Default::default()
        }
        .matches(&c));
    }

    /// Generic `set_credential` refuses to write a system-owned credential.
    #[test]
    fn generic_set_rejects_system_credential() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        let c = Credential::now(
            "identity",
            "default",
            CredentialKind::PrivateKey,
            SecretString::new("m".into()),
        );
        let err = vault.set_credential(&c).unwrap_err();
        assert!(
            err.to_string().contains("runtime-owned"),
            "error should explain runtime ownership: {err}"
        );
    }

    /// Generic `delete_credential` refuses to delete a system-owned credential.
    #[test]
    fn generic_delete_rejects_system_credential() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();
        let summaries = vault.list_credentials(&CredentialFilter {
            include_system: true,
            ..Default::default()
        });
        let id = summaries[0].id.clone();
        let err = vault.delete_credential(&id).unwrap_err();
        assert!(
            err.to_string().contains("runtime-owned"),
            "error should explain runtime ownership: {err}"
        );
    }

    /// The typed system adapters may mutate system-owned credentials.
    #[test]
    fn typed_adapter_can_delete_system_credential() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::for_test(dir.path(), "test-passphrase");
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();
        assert!(vault.delete_identity_private_key("kid").unwrap());
        assert!(vault
            .list_credentials(&CredentialFilter {
                include_system: true,
                ..Default::default()
            })
            .is_empty());
    }
}
