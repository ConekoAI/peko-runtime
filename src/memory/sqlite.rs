//! SQLite memory backend

use super::types::MemoryEntry;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use tracing::{debug, info, warn};

/// SQLite memory store
pub struct SqliteMemory {
    conn: Connection,
    namespace: String,
}

impl SqliteMemory {
    /// Create a new SQLite memory store
    pub fn new<P: AsRef<Path>>(path: P, namespace: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .context("Failed to open SQLite database")?;
        
        let store = Self {
            conn,
            namespace: namespace.to_string(),
        };
        
        store.initialize()?;
        
        info!("SQLite memory initialized for namespace: {}", namespace);
        Ok(store)
    }

    /// Initialize database tables
    fn initialize(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id TEXT PRIMARY KEY,
                namespace TEXT NOT NULL,
                content TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                access_count INTEGER DEFAULT 0,
                last_accessed_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_namespace ON memory_entries(namespace);
            CREATE INDEX IF NOT EXISTS idx_created ON memory_entries(created_at);
            CREATE INDEX IF NOT EXISTS idx_content ON memory_entries(content);

            CREATE TABLE IF NOT EXISTS memory_embeddings (
                entry_id TEXT PRIMARY KEY,
                embedding BLOB,
                model TEXT,
                FOREIGN KEY (entry_id) REFERENCES memory_entries(id) ON DELETE CASCADE
            );
            "#,
        ).context("Failed to initialize memory tables")?;

        Ok(())
    }

    /// Store a memory entry
    pub fn store(&self, content: &str, metadata: Option<serde_json::Value>) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = metadata.map(|m| m.to_string());

        self.conn.execute(
            r#"
            INSERT INTO memory_entries 
            (id, namespace, content, metadata, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![id, self.namespace, content, metadata_json, now, now],
        ).context("Failed to store memory entry")?;

        debug!("Stored memory entry: {}", id);
        Ok(id)
    }

    /// Retrieve a memory entry by ID
    pub fn get(&self, id: &str) -> Result<Option<MemoryEntry>> {
        let row = self.conn
            .query_row(
                r#"
                SELECT id, content, metadata, created_at, access_count, last_accessed_at 
                FROM memory_entries
                WHERE id = ?1 AND namespace = ?2
                "#,
                params![id, self.namespace],
                |row| {
                    let metadata_json: Option<String> = row.get(2)?;
                    let metadata = metadata_json
                        .map(|m| serde_json::from_str(&m).ok())
                        .flatten();
                    let last_accessed: Option<String> = row.get(5)?;
                    let last_accessed_at = last_accessed
                        .map(|la| chrono::DateTime::parse_from_rfc3339(&la).ok())
                        .flatten()
                        .map(|dt| dt.with_timezone(&chrono::Utc));
                    
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        metadata,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        last_accessed_at,
                    ))
                },
            )
            .optional()?;

        if let Some((id, content, metadata, created_at, access_count, last_accessed_at)) = row {
            // Update access stats
            let _ = self.update_access_stats(&id);
            
            let timestamp = chrono::DateTime::parse_from_rfc3339(&created_at)
                .map_err(|e| anyhow::anyhow!("Invalid timestamp: {}", e))?
                .with_timezone(&chrono::Utc);

            Ok(Some(MemoryEntry {
                id,
                content,
                metadata,
                timestamp,
                access_count,
                last_accessed_at,
            }))
        } else {
            Ok(None)
        }
    }

    /// Search memory entries by content (simple substring search)
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>> {
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, content, metadata, created_at, access_count, last_accessed_at 
            FROM memory_entries
            WHERE namespace = ?1 AND content LIKE ?2
            ORDER BY created_at DESC
            LIMIT ?3
            "#,
        )?;

        let entries = stmt
            .query_map(params![self.namespace, pattern, limit], |row| {
                let id: String = row.get(0)?;
                let content: String = row.get(1)?;
                let metadata_json: Option<String> = row.get(2)?;
                let metadata = metadata_json
                    .map(|m| serde_json::from_str(&m).ok())
                    .flatten();
                let created_at: String = row.get(3)?;
                let access_count: i64 = row.get(4)?;
                let last_accessed: Option<String> = row.get(5)?;
                let last_accessed_at = last_accessed
                    .map(|la| chrono::DateTime::parse_from_rfc3339(&la).ok())
                    .flatten()
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                
                let timestamp = chrono::DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))?
                    .with_timezone(&chrono::Utc);

                Ok(MemoryEntry {
                    id,
                    content,
                    metadata,
                    timestamp,
                    access_count,
                    last_accessed_at,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        debug!("Found {} entries for query: {}", entries.len(), query);
        Ok(entries)
    }

    /// Get recent memory entries
    pub fn recent(&self, limit: usize) -> Result<Vec<MemoryEntry>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, content, metadata, created_at, access_count, last_accessed_at 
            FROM memory_entries
            WHERE namespace = ?1
            ORDER BY created_at DESC
            LIMIT ?2
            "#,
        )?;

        let entries = stmt
            .query_map(params![self.namespace, limit], |row| {
                let id: String = row.get(0)?;
                let content: String = row.get(1)?;
                let metadata_json: Option<String> = row.get(2)?;
                let metadata = metadata_json
                    .map(|m| serde_json::from_str(&m).ok())
                    .flatten();
                let created_at: String = row.get(3)?;
                let access_count: i64 = row.get(4)?;
                let last_accessed: Option<String> = row.get(5)?;
                let last_accessed_at = last_accessed
                    .map(|la| chrono::DateTime::parse_from_rfc3339(&la).ok())
                    .flatten()
                    .map(|dt| dt.with_timezone(&chrono::Utc));
                
                let timestamp = chrono::DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    ))?
                    .with_timezone(&chrono::Utc);

                Ok(MemoryEntry {
                    id,
                    content,
                    metadata,
                    timestamp,
                    access_count,
                    last_accessed_at,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    /// Delete a memory entry
    pub fn delete(&self, id: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "DELETE FROM memory_entries WHERE id = ?1 AND namespace = ?2",
            params![id, self.namespace],
        )?;

        Ok(affected > 0)
    }

    /// Clear all entries in namespace
    pub fn clear(&self) -> Result<usize> {
        let affected = self.conn.execute(
            "DELETE FROM memory_entries WHERE namespace = ?1",
            params![self.namespace],
        )?;

        warn!("Cleared {} entries from namespace: {}", affected, self.namespace);
        Ok(affected)
    }

    /// Count entries in namespace
    pub fn count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE namespace = ?1",
            params![self.namespace],
            |row| row.get(0),
        )?;

        Ok(count as usize)
    }

    /// Update access statistics
    fn update_access_stats(&self, id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            UPDATE memory_entries 
            SET access_count = access_count + 1, last_accessed_at = ?1
            WHERE id = ?2
            "#,
            params![now, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_store() -> (SqliteMemory, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let store = SqliteMemory::new(&db_path, "test-namespace").unwrap();
        (store, temp_dir)
    }

    #[test]
    fn test_store_and_retrieve() {
        let (store, _temp) = create_test_store();
        
        let id = store.store("Test content", None).unwrap();
        assert!(!id.is_empty());

        let entry = store.get(&id).unwrap().unwrap();
        assert_eq!(entry.content, "Test content");
    }

    #[test]
    fn test_search() {
        let (store, _temp) = create_test_store();
        
        store.store("Hello world", None).unwrap();
        store.store("Hello rust", None).unwrap();
        store.store("Goodbye world", None).unwrap();

        let results = store.search("Hello", 10).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_recent() {
        let (store, _temp) = create_test_store();
        
        store.store("First", None).unwrap();
        store.store("Second", None).unwrap();
        store.store("Third", None).unwrap();

        let results = store.recent(2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_delete() {
        let (store, _temp) = create_test_store();
        
        let id = store.store("To delete", None).unwrap();
        assert!(store.get(&id).unwrap().is_some());

        assert!(store.delete(&id).unwrap());
        assert!(store.get(&id).unwrap().is_none());
    }

    #[test]
    fn test_count() {
        let (store, _temp) = create_test_store();
        
        assert_eq!(store.count().unwrap(), 0);
        store.store("One", None).unwrap();
        store.store("Two", None).unwrap();
        assert_eq!(store.count().unwrap(), 2);
    }
}
