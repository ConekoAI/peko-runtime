//! Secret store backed by SQLite

use crate::secrets::{
    crypto::{EncryptedSecret, MasterKey},
    types::{
        AuditEntry, AuditEvent, AuditStats, SecretAccessControl, SecretEntry, SecretMetadata,
        SecretPermission, SecretScope, SecretType,
    },
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use secrecy::ExposeSecret;
use std::path::Path;
use tracing::info;
use uuid::Uuid;

/// Secret store for managing encrypted secrets
pub struct SecretStore {
    conn: Connection,
    master_key: Option<MasterKey>,
}

impl SecretStore {
    /// Open or create a secret store at the given path
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open secret store database")?;
        
        let store = Self {
            conn,
            master_key: None,
        };

        store.initialize()?;
        
        info!("Secret store initialized");
        Ok(store)
    }

    /// Create an in-memory secret store (for testing)
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to create in-memory database")?;
        
        let store = Self {
            conn,
            master_key: None,
        };

        store.initialize()?;
        Ok(store)
    }

    /// Initialize database tables
    fn initialize(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            -- Secrets table
            CREATE TABLE IF NOT EXISTS secrets (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                scope TEXT NOT NULL,
                secret_type TEXT NOT NULL,
                encrypted_value BLOB NOT NULL,
                nonce BLOB NOT NULL,
                salt BLOB NOT NULL,
                metadata TEXT,
                version INTEGER DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(name, scope)
            );

            CREATE INDEX IF NOT EXISTS idx_secrets_scope ON secrets(scope);
            CREATE INDEX IF NOT EXISTS idx_secrets_name ON secrets(name);

            -- Permissions table
            CREATE TABLE IF NOT EXISTS secret_permissions (
                id TEXT PRIMARY KEY,
                secret_id TEXT NOT NULL,
                agent_did TEXT,
                permission TEXT NOT NULL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (secret_id) REFERENCES secrets(id) ON DELETE CASCADE,
                UNIQUE(secret_id, agent_did)
            );

            CREATE INDEX IF NOT EXISTS idx_permissions_secret ON secret_permissions(secret_id);

            -- Audit log table
            CREATE TABLE IF NOT EXISTS secret_audit_log (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                event TEXT NOT NULL,
                secret_name TEXT NOT NULL,
                secret_scope TEXT NOT NULL,
                agent_did TEXT,
                success INTEGER NOT NULL,
                error TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON secret_audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_secret ON secret_audit_log(secret_name);
            "#,
        ).context("Failed to initialize secret store schema")?;

        Ok(())
    }

    /// Unlock the store with a master password
    pub fn unlock(&mut self,
        password: &str,
        salt: &[u8],
    ) -> Result<()> {
        self.master_key = Some(MasterKey::from_password(password, salt)?);
        info!("Secret store unlocked");
        Ok(())
    }

    /// Lock the store (clear master key from memory)
    pub fn lock(&mut self) {
        self.master_key = None;
        info!("Secret store locked");
    }

    /// Check if the store is unlocked
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        self.master_key.is_some()
    }

    /// Store a secret
    pub fn set(
        &self,
        name: &str,
        scope: &SecretScope,
        value: &str,
        secret_type: SecretType,
        metadata: Option<SecretMetadata>,
    ) -> Result<SecretEntry> {
        let master_key = self
            .master_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Secret store is locked"))?;

        let scope_str = scope.as_str();
        let encrypted = master_key.encrypt(value, name, &scope_str)?;

        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = metadata
            .as_ref()
            .map(|m| serde_json::to_string(m).unwrap_or_default())
            .unwrap_or_default();

        // Check if secret already exists
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM secrets WHERE name = ?1 AND scope = ?2",
                params![name, scope_str],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(existing_id) = existing {
            // Update existing secret
            self.conn.execute(
                "UPDATE secrets SET 
                    encrypted_value = ?1, 
                    nonce = ?2, 
                    salt = ?3,
                    metadata = ?4,
                    version = version + 1,
                    updated_at = ?5,
                    secret_type = ?6
                WHERE id = ?7",
                params![
                    encrypted.ciphertext,
                    encrypted.nonce,
                    encrypted.salt,
                    metadata_json,
                    now,
                    secret_type.to_string(),
                    existing_id
                ],
            )?;

            self.log_audit(AuditEvent::SecretUpdated, name, &scope_str, None, true, None)?;

            // Get updated entry
            self.get_entry(name, scope)?
                .ok_or_else(|| anyhow::anyhow!("Failed to retrieve updated secret"))
        } else {
            // Insert new secret
            self.conn.execute(
                "INSERT INTO secrets 
                    (id, name, scope, secret_type, encrypted_value, nonce, salt, metadata, version, created_at, updated_at)
                VALUES 
                    (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10)",
                params![
                    id,
                    name,
                    scope_str,
                    secret_type.to_string(),
                    encrypted.ciphertext,
                    encrypted.nonce,
                    encrypted.salt,
                    metadata_json,
                    now,
                    now
                ],
            )?;

            self.log_audit(AuditEvent::SecretCreated, name, &scope_str, None, true, None)?;

            Ok(SecretEntry {
                id,
                name: name.to_string(),
                scope: scope.clone(),
                secret_type,
                metadata: metadata.unwrap_or_default(),
                version: 1,
                created_at: now.clone(),
                updated_at: now,
            })
        }
    }

    /// Get a secret's decrypted value
    pub fn get(
        &self,
        name: &str,
        scope: &SecretScope,
    ) -> Result<Option<String>> {
        let master_key = self
            .master_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Secret store is locked"))?;

        let scope_str = scope.as_str();

        let row: Option<(Vec<u8>, Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT encrypted_value, nonce, salt FROM secrets WHERE name = ?1 AND scope = ?2",
                params![name, scope_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        match row {
            Some((ciphertext, nonce, salt)) => {
                let encrypted = EncryptedSecret {
                    salt,
                    nonce,
                    ciphertext,
                };

                let decrypted = master_key.decrypt(&encrypted, name, &scope_str)?;
                let value = decrypted.expose_secret().to_string();

                self.log_audit(AuditEvent::SecretAccessed, name, &scope_str, None, true, None)?;

                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Get a secret entry (without value)
    pub fn get_entry(
        &self,
        name: &str,
        scope: &SecretScope,
    ) -> Result<Option<SecretEntry>> {
        let scope_str = scope.as_str();

        self.conn
            .query_row(
                "SELECT id, name, scope, secret_type, metadata, version, created_at, updated_at 
                 FROM secrets WHERE name = ?1 AND scope = ?2",
                params![name, scope_str],
                |row| {
                    let scope_str: String = row.get(2)?;
                    let scope = if scope_str == "global" {
                        SecretScope::Global
                    } else if scope_str.starts_with("agent:") {
                        SecretScope::Agent {
                            did: scope_str.strip_prefix("agent:").unwrap_or(&scope_str).to_string(),
                        }
                    } else {
                        SecretScope::Global
                    };

                    let secret_type_str: String = row.get(3)?;
                    let secret_type = match secret_type_str.as_str() {
                        "api_key" => SecretType::ApiKey,
                        "token" => SecretType::Token,
                        "ssh_key" => SecretType::SshKey,
                        "certificate" => SecretType::Certificate,
                        "password" => SecretType::Password,
                        _ => SecretType::Other,
                    };

                    let metadata_json: String = row.get(4)?;
                    let metadata = serde_json::from_str(&metadata_json).unwrap_or_default();

                    Ok(SecretEntry {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        scope,
                        secret_type,
                        metadata,
                        version: row.get(5)?,
                        created_at: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.into())
    }

    /// List secrets (without values)
    pub fn list(
        &self,
        scope: Option<&SecretScope>,
    ) -> Result<Vec<SecretEntry>> {
        let mut stmt = if let Some(s) = scope {
            self.conn.prepare(
                "SELECT id, name, scope, secret_type, metadata, version, created_at, updated_at 
                 FROM secrets WHERE scope = ?1 ORDER BY name",
            )?
        } else {
            self.conn.prepare(
                "SELECT id, name, scope, secret_type, metadata, version, created_at, updated_at 
                 FROM secrets ORDER BY scope, name",
            )?
        };

        let rows = if let Some(s) = scope {
            stmt.query_map(params![s.as_str()], |row| {
                self.row_to_secret_entry(row)
            })?
            .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], |row| {
                self.row_to_secret_entry(row)
            })?
            .collect::<Result<Vec<_>, _>>()
        };

        Ok(rows?)
    }

    /// Delete a secret
    pub fn delete(&self,
        name: &str,
        scope: &SecretScope,
    ) -> Result<bool> {
        let scope_str = scope.as_str();

        let rows_deleted = self.conn.execute(
            "DELETE FROM secrets WHERE name = ?1 AND scope = ?2",
            params![name, scope_str],
        )?;

        let deleted = rows_deleted > 0;

        if deleted {
            self.log_audit(AuditEvent::SecretDeleted, name, &scope_str, None, true, None)?;
        }

        Ok(deleted)
    }

    /// Check if an agent has permission to access a secret
    ///
    /// Returns the permission level for the given agent. If no specific permission
    /// is set, returns `None` for global secrets (meaning default policy applies)
    /// or `Some(None)` for agent-scoped secrets (meaning only the owner can access).
    pub fn check_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>,
    ) -> Result<SecretPermission> {
        let scope_str = secret_scope.as_str();

        // Get secret ID
        let secret_id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM secrets WHERE name = ?1 AND scope = ?2",
                params![secret_name, scope_str],
                |row| row.get(0),
            )
            .optional()?;

        let secret_id = match secret_id {
            Some(id) => id,
            None => return Ok(SecretPermission::None), // Secret doesn't exist
        };

        // Check for agent-specific permission
        if let Some(did) = agent_did {
            let permission: Option<String> = self
                .conn
                .query_row(
                    "SELECT permission FROM secret_permissions 
                     WHERE secret_id = ?1 AND (agent_did = ?2 OR agent_did IS NULL)
                     ORDER BY agent_did DESC LIMIT 1",
                    params![secret_id, did],
                    |row| row.get(0),
                )
                .optional()?;

            if let Some(p) = permission {
                return Ok(match p.as_str() {
                    "read" => SecretPermission::Read,
                    "write" => SecretPermission::Write,
                    _ => SecretPermission::None,
                });
            }
        }

        // Check for default permission (agent_did IS NULL)
        let default_permission: Option<String> = self
            .conn
            .query_row(
                "SELECT permission FROM secret_permissions 
                 WHERE secret_id = ?1 AND agent_did IS NULL",
                params![secret_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(p) = default_permission {
            return Ok(match p.as_str() {
                "read" => SecretPermission::Read,
                "write" => SecretPermission::Write,
                _ => SecretPermission::None,
            });
        }

        // No explicit permission set - apply default policy
        // For global secrets, default is Read (backward compatible)
        // For agent-scoped secrets, only the agent owner has access
        match secret_scope {
            SecretScope::Global => Ok(SecretPermission::Read),
            SecretScope::Agent { did } => {
                if agent_did == Some(did.as_str()) {
                    Ok(SecretPermission::Write) // Owner has full access
                } else {
                    Ok(SecretPermission::None)
                }
            }
        }
    }

    /// Grant permission to an agent for a secret
    pub fn grant_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>, // None = default permission for all agents
        permission: SecretPermission,
    ) -> Result<SecretAccessControl> {
        let scope_str = secret_scope.as_str();

        // Get secret ID
        let secret_id: String = self
            .conn
            .query_row(
                "SELECT id FROM secrets WHERE name = ?1 AND scope = ?2",
                params![secret_name, scope_str],
                |row| row.get(0),
            )
            .context("Secret not found")?;

        let permission_str = match permission {
            SecretPermission::Read => "read",
            SecretPermission::Write => "write",
            SecretPermission::None => "none",
        };

        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        // Upsert permission
        self.conn.execute(
            "INSERT INTO secret_permissions (id, secret_id, agent_did, permission, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(secret_id, agent_did) DO UPDATE SET
                permission = excluded.permission",
            params![id, secret_id, agent_did, permission_str, now],
        )?;

        self.log_audit(
            AuditEvent::PermissionGranted,
            secret_name,
            &scope_str,
            agent_did,
            true,
            None,
        )?;

        Ok(SecretAccessControl {
            secret_id,
            agent_did: agent_did.map(String::from),
            permission,
        })
    }

    /// Revoke permission from an agent for a secret
    pub fn revoke_permission(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
        agent_did: Option<&str>, // None = remove default permission
    ) -> Result<bool> {
        let scope_str = secret_scope.as_str();

        // Get secret ID
        let secret_id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM secrets WHERE name = ?1 AND scope = ?2",
                params![secret_name, scope_str],
                |row| row.get(0),
            )
            .optional()?;

        let secret_id = match secret_id {
            Some(id) => id,
            None => return Ok(false), // Secret doesn't exist
        };

        let rows_deleted = self.conn.execute(
            "DELETE FROM secret_permissions 
             WHERE secret_id = ?1 AND agent_did IS ?2",
            params![secret_id, agent_did],
        )?;

        let revoked = rows_deleted > 0;

        if revoked {
            self.log_audit(
                AuditEvent::PermissionRevoked,
                secret_name,
                &scope_str,
                agent_did,
                true,
                None,
            )?;
        }

        Ok(revoked)
    }

    /// Get permissions for a secret
    pub fn get_permissions(
        &self,
        secret_name: &str,
        secret_scope: &SecretScope,
    ) -> Result<Vec<SecretAccessControl>> {
        let scope_str = secret_scope.as_str();

        let mut stmt = self.conn.prepare(
            "SELECT p.secret_id, p.agent_did, p.permission
             FROM secret_permissions p
             JOIN secrets s ON p.secret_id = s.id
             WHERE s.name = ?1 AND s.scope = ?2
             ORDER BY p.agent_did NULLS FIRST",
        )?;

        let permissions = stmt
            .query_map(params![secret_name, scope_str], |row| {
                let permission_str: String = row.get(2)?;
                let permission = match permission_str.as_str() {
                    "read" => SecretPermission::Read,
                    "write" => SecretPermission::Write,
                    _ => SecretPermission::None,
                };

                Ok(SecretAccessControl {
                    secret_id: row.get(0)?,
                    agent_did: row.get(1)?,
                    permission,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(permissions)
    }

    /// Get a secret's decrypted value with permission check
    pub fn get_with_permission(
        &self,
        name: &str,
        scope: &SecretScope,
        agent_did: Option<&str>,
    ) -> Result<Option<String>> {
        // Check permission
        let permission = self.check_permission(name, scope, agent_did)?;

        if permission == SecretPermission::None {
            self.log_audit(
                AuditEvent::AccessDenied,
                name,
                &scope.as_str(),
                agent_did,
                false,
                Some("Permission denied"),
            )?;
            anyhow::bail!(
                "Access denied: agent '{}' does not have permission to read secret '{}'",
                agent_did.unwrap_or("(none)"),
                name
            );
        }

        // Get the secret value
        self.get(name, scope)
    }

    /// Helper to convert a database row to SecretEntry
    fn row_to_secret_entry(&self,
        row: &rusqlite::Row,
    ) -> rusqlite::Result<SecretEntry> {
        let scope_str: String = row.get(2)?;
        let scope = if scope_str == "global" {
            SecretScope::Global
        } else if scope_str.starts_with("agent:") {
            SecretScope::Agent {
                did: scope_str.strip_prefix("agent:").unwrap_or(&scope_str).to_string(),
            }
        } else {
            SecretScope::Global
        };

        let secret_type_str: String = row.get(3)?;
        let secret_type = match secret_type_str.as_str() {
            "api_key" => SecretType::ApiKey,
            "token" => SecretType::Token,
            "ssh_key" => SecretType::SshKey,
            "certificate" => SecretType::Certificate,
            "password" => SecretType::Password,
            _ => SecretType::Other,
        };

        let metadata_json: String = row.get(4)?;
        let metadata = serde_json::from_str(&metadata_json).unwrap_or_default();

        Ok(SecretEntry {
            id: row.get(0)?,
            name: row.get(1)?,
            scope,
            secret_type,
            metadata,
            version: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// Log an audit event
    fn log_audit(
        &self,
        event: AuditEvent,
        secret_name: &str,
        secret_scope: &str,
        agent_did: Option<&str>,
        success: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO secret_audit_log (id, timestamp, event, secret_name, secret_scope, agent_did, success, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                now,
                event.to_string(),
                secret_name,
                secret_scope,
                agent_did,
                success as i32,
                error
            ],
        )?;

        Ok(())
    }

    /// Query audit log entries
    pub fn query_audit_log(
        &self,
        secret_name: Option<&str>,
        secret_scope: Option<&SecretScope>,
        agent_did: Option<&str>,
        event_type: Option<AuditEvent>,
        limit: usize,
    ) -> Result<Vec<AuditEntry>> {
        // Use owned values to avoid lifetime issues
        let scope_str = secret_scope.map(|s| s.as_str());
        let event_str = event_type.map(|e| e.to_string());
        
        // Build the query based on which filters are present
        let entries = match (secret_name, scope_str, agent_did, event_str.as_deref()) {
            (Some(name), Some(scope), Some(did), Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND secret_scope = ?2 AND agent_did = ?3 AND event = ?4
                     ORDER BY timestamp DESC LIMIT ?5"
                )?;
                self.query_audit_entries(&mut stmt, params![name, scope, did, event, limit as i64])?
            }
            (Some(name), Some(scope), Some(did), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND secret_scope = ?2 AND agent_did = ?3
                     ORDER BY timestamp DESC LIMIT ?4"
                )?;
                self.query_audit_entries(&mut stmt, params![name, scope, did, limit as i64])?
            }
            (Some(name), Some(scope), None, Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND secret_scope = ?2 AND event = ?3
                     ORDER BY timestamp DESC LIMIT ?4"
                )?;
                self.query_audit_entries(&mut stmt, params![name, scope, event, limit as i64])?
            }
            (Some(name), None, Some(did), Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND agent_did = ?2 AND event = ?3
                     ORDER BY timestamp DESC LIMIT ?4"
                )?;
                self.query_audit_entries(&mut stmt, params![name, did, event, limit as i64])?
            }
            (None, Some(scope), Some(did), Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_scope = ?1 AND agent_did = ?2 AND event = ?3
                     ORDER BY timestamp DESC LIMIT ?4"
                )?;
                self.query_audit_entries(&mut stmt, params![scope, did, event, limit as i64])?
            }
            (Some(name), Some(scope), None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND secret_scope = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![name, scope, limit as i64])?
            }
            (Some(name), None, Some(did), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND agent_did = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![name, did, limit as i64])?
            }
            (Some(name), None, None, Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1 AND event = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![name, event, limit as i64])?
            }
            (None, Some(scope), Some(did), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_scope = ?1 AND agent_did = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![scope, did, limit as i64])?
            }
            (None, Some(scope), None, Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_scope = ?1 AND event = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![scope, event, limit as i64])?
            }
            (None, None, Some(did), Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE agent_did = ?1 AND event = ?2
                     ORDER BY timestamp DESC LIMIT ?3"
                )?;
                self.query_audit_entries(&mut stmt, params![did, event, limit as i64])?
            }
            (Some(name), None, None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_name = ?1
                     ORDER BY timestamp DESC LIMIT ?2"
                )?;
                self.query_audit_entries(&mut stmt, params![name, limit as i64])?
            }
            (None, Some(scope), None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE secret_scope = ?1
                     ORDER BY timestamp DESC LIMIT ?2"
                )?;
                self.query_audit_entries(&mut stmt, params![scope, limit as i64])?
            }
            (None, None, Some(did), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE agent_did = ?1
                     ORDER BY timestamp DESC LIMIT ?2"
                )?;
                self.query_audit_entries(&mut stmt, params![did, limit as i64])?
            }
            (None, None, None, Some(event)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     WHERE event = ?1
                     ORDER BY timestamp DESC LIMIT ?2"
                )?;
                self.query_audit_entries(&mut stmt, params![event, limit as i64])?
            }
            (None, None, None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, timestamp, event, secret_name, secret_scope, agent_did, success, error
                     FROM secret_audit_log 
                     ORDER BY timestamp DESC LIMIT ?1"
                )?;
                self.query_audit_entries(&mut stmt, params![limit as i64])?
            }
        };

        Ok(entries)
    }

    /// Helper to query audit entries from a prepared statement
    fn query_audit_entries(
        &self,
        stmt: &mut rusqlite::Statement,
        params: impl rusqlite::Params,
    ) -> Result<Vec<AuditEntry>> {
        stmt.query_map(params, |row| {
            let event_str: String = row.get(2)?;
            let event = match event_str.as_str() {
                "SECRET_CREATED" => AuditEvent::SecretCreated,
                "SECRET_ACCESSED" => AuditEvent::SecretAccessed,
                "SECRET_UPDATED" => AuditEvent::SecretUpdated,
                "SECRET_DELETED" => AuditEvent::SecretDeleted,
                "PERMISSION_GRANTED" => AuditEvent::PermissionGranted,
                "PERMISSION_REVOKED" => AuditEvent::PermissionRevoked,
                "STORE_UNLOCKED" => AuditEvent::StoreUnlocked,
                "STORE_LOCKED" => AuditEvent::StoreLocked,
                "PASSWORD_CHANGED" => AuditEvent::PasswordChanged,
                "ACCESS_DENIED" => AuditEvent::AccessDenied,
                _ => AuditEvent::AccessDenied,
            };

            let success_val: i32 = row.get(6)?;

            Ok(AuditEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                event,
                secret_name: row.get(3)?,
                secret_scope: row.get(4)?,
                agent_did: row.get(5)?,
                success: success_val != 0,
                error: row.get(7)?,
            })
        })
        .and_then(|iter| iter.collect::<std::result::Result<Vec<_>, _>>())
        .map_err(|e| e.into())
    }

    /// Get audit log statistics
    pub fn get_audit_stats(&self,
        since: Option<&str>, // ISO 8601 timestamp
    ) -> Result<AuditStats> {
        let since_clause = since.map(|s| format!("WHERE timestamp >= '{}'", s)).unwrap_or_default();

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM secret_audit_log {}", since_clause),
            [],
            |row| row.get(0),
        )?;

        let successful: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM secret_audit_log {} AND success = 1", since_clause),
            [],
            |row| row.get(0),
        )?;

        let failed: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM secret_audit_log {} AND success = 0", since_clause),
            [],
            |row| row.get(0),
        )?;

        let access_denied: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM secret_audit_log {} AND event = 'ACCESS_DENIED'", since_clause),
            [],
            |row| row.get(0),
        )?;

        Ok(AuditStats {
            total: total as usize,
            successful: successful as usize,
            failed: failed as usize,
            access_denied: access_denied as usize,
        })
    }

    /// Export audit log to a file
    pub fn export_audit_log(
        &self,
        path: impl AsRef<std::path::Path>,
        since: Option<&str>,
    ) -> Result<usize> {
        let entries = self.query_audit_log(None, None, None, None, 100_000)?;
        
        let file = std::fs::File::create(path)?;
        let mut writer = std::io::BufWriter::new(file);
        
        use std::io::Write;
        
        writeln!(&mut writer, "id,timestamp,event,secret_name,secret_scope,agent_did,success,error")?;
        
        for entry in &entries {
            writeln!(
                &mut writer,
                "{},{},{},{},{},{},{},{}",
                entry.id,
                entry.timestamp,
                entry.event,
                entry.secret_name,
                entry.secret_scope,
                entry.agent_did.as_deref().unwrap_or(""),
                entry.success,
                entry.error.as_deref().unwrap_or("")
            )?;
        }
        
        Ok(entries.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_get() {
        let mut store = SecretStore::open_in_memory().unwrap();
        
        // Unlock with master password
        let salt = vec![0u8; 32];
        store.unlock("master-password", &salt).unwrap();

        // Store a secret
        let entry = store.set(
            "OPENAI_KEY",
            &SecretScope::Global,
            "sk-test123",
            SecretType::ApiKey,
            None,
        ).unwrap();

        assert_eq!(entry.name, "OPENAI_KEY");
        assert_eq!(entry.scope, SecretScope::Global);
        assert_eq!(entry.version, 1);

        // Get the secret
        let value = store.get("OPENAI_KEY", &SecretScope::Global).unwrap();
        assert_eq!(value, Some("sk-test123".to_string()));
    }

    #[test]
    fn test_list_secrets() {
        let mut store = SecretStore::open_in_memory().unwrap();
        let salt = vec![0u8; 32];
        store.unlock("password", &salt).unwrap();

        store.set("KEY1", &SecretScope::Global, "value1", SecretType::ApiKey, None).unwrap();
        store.set("KEY2", &SecretScope::Global, "value2", SecretType::Token, None).unwrap();

        let secrets = store.list(Some(&SecretScope::Global)).unwrap();
        assert_eq!(secrets.len(), 2);
    }

    #[test]
    fn test_delete_secret() {
        let mut store = SecretStore::open_in_memory().unwrap();
        let salt = vec![0u8; 32];
        store.unlock("password", &salt).unwrap();

        store.set("TO_DELETE", &SecretScope::Global, "value", SecretType::ApiKey, None).unwrap();
        
        let deleted = store.delete("TO_DELETE", &SecretScope::Global).unwrap();
        assert!(deleted);

        let value = store.get("TO_DELETE", &SecretScope::Global).unwrap();
        assert!(value.is_none());
    }

    #[test]
    fn test_locked_store_denies_access() {
        let store = SecretStore::open_in_memory().unwrap();
        // Store is locked by default

        let result = store.get("ANY_KEY", &SecretScope::Global);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("locked"));
    }
}
