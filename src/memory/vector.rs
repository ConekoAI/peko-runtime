//! Vector memory with embedding support and cosine similarity search

use super::types::MemoryEntry;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use tracing::{debug, info, warn};

/// Vector memory store with embedding support
pub struct VectorMemory {
    conn: Connection,
    namespace: String,
    embedding_dim: usize,
}

impl VectorMemory {
    /// Create a new vector memory store
    pub fn new<P: AsRef<Path>>(path: P, namespace: &str, embedding_dim: usize) -> Result<Self> {
        let conn = Connection::open(path)
            .context("Failed to open SQLite database")?;
        
        let store = Self {
            conn,
            namespace: namespace.to_string(),
            embedding_dim,
        };
        
        store.initialize()?;
        
        info!("Vector memory initialized for namespace: {} (dim: {})", namespace, embedding_dim);
        Ok(store)
    }

    /// Initialize database tables
    fn initialize(&self) -> Result<()> {
        self.conn.execute_batch(
            &format!(
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

                CREATE TABLE IF NOT EXISTS memory_embeddings (
                    entry_id TEXT PRIMARY KEY,
                    embedding BLOB NOT NULL,  -- Stored as binary f32 array
                    model TEXT,
                    FOREIGN KEY (entry_id) REFERENCES memory_entries(id) ON DELETE CASCADE
                );

                -- Virtual table for vector similarity using sqlite-vss (if available)
                -- Fallback to manual cosine similarity
                "#,
            ),
        ).context("Failed to initialize memory tables")?;

        Ok(())
    }

    /// Store a memory entry with embedding
    pub fn store(
        &self,
        content: &str,
        embedding: Vec<f32>,
        model: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        if embedding.len() != self.embedding_dim {
            return Err(anyhow::anyhow!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.embedding_dim,
                embedding.len()
            ));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let metadata_json = metadata.map(|m| m.to_string());

        let tx = self.conn.unchecked_transaction()?;

        // Store entry
        tx.execute(
            r#"
            INSERT INTO memory_entries 
            (id, namespace, content, metadata, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![id, self.namespace, content, metadata_json, now, now],
        ).context("Failed to store memory entry")?;

        // Store embedding as binary
        let embedding_bytes = embedding_to_bytes(&embedding);
        tx.execute(
            r#"
            INSERT INTO memory_embeddings (entry_id, embedding, model)
            VALUES (?1, ?2, ?3)
            "#,
            params![id, embedding_bytes, model],
        ).context("Failed to store embedding")?;

        tx.commit()?;

        debug!("Stored memory entry with embedding: {}", id);
        Ok(id)
    }

    /// Search by semantic similarity using cosine similarity
    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: usize,
        min_similarity: f32,
    ) -> Result<Vec<SimilarityResult>> {
        if query_embedding.len() != self.embedding_dim {
            return Err(anyhow::anyhow!(
                "Query embedding dimension mismatch: expected {}, got {}",
                self.embedding_dim,
                query_embedding.len()
            ));
        }

        let query_norm = vector_norm(query_embedding);
        
        // Fetch all embeddings for this namespace and compute cosine similarity
        let mut stmt = self.conn.prepare(
            r#"
            SELECT e.id, e.content, e.metadata, e.created_at, 
                   e.access_count, e.last_accessed_at, m.embedding, m.model
            FROM memory_entries e
            JOIN memory_embeddings m ON e.id = m.entry_id
            WHERE e.namespace = ?1
            "#,
        )?;

        let entries = stmt
            .query_map(params![self.namespace], |row| {
                let id: String = row.get(0)?;
                let content: String = row.get(1)?;
                let metadata_json: Option<String> = row.get(2)?;
                let metadata = metadata_json
                    .map(|m| serde_json::from_str(&m).ok())
                    .flatten();
                let created_at: String = row.get(3)?;
                let access_count: i64 = row.get(4)?;
                let last_accessed: Option<String> = row.get(5)?;
                let embedding_bytes: Vec<u8> = row.get(6)?;
                let model: Option<String> = row.get(7)?;
                
                let embedding = bytes_to_embedding(&embedding_bytes);
                
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

                let entry = MemoryEntry {
                    id,
                    content,
                    metadata,
                    timestamp,
                    access_count,
                    last_accessed_at,
                };

                Ok((entry, embedding, model))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Compute cosine similarity for each entry
        let mut results: Vec<SimilarityResult> = entries
            .into_iter()
            .filter_map(|(entry, embedding, model)| {
                let similarity = cosine_similarity(query_embedding, &embedding, query_norm);
                if similarity >= min_similarity {
                    Some(SimilarityResult {
                        entry,
                        similarity,
                        embedding_model: model,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort by similarity (highest first)
        results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());
        results.truncate(limit);

        debug!("Found {} similar entries above threshold {}", results.len(), min_similarity);
        Ok(results)
    }

    /// Find entries similar to a given entry ID
    pub fn find_similar_to(
        &self,
        entry_id: &str,
        limit: usize,
        min_similarity: f32,
    ) -> Result<Vec<SimilarityResult>> {
        // Get the embedding for the reference entry
        let embedding_bytes: Option<Vec<u8>> = self.conn.query_row(
            r#"
            SELECT m.embedding 
            FROM memory_embeddings m
            JOIN memory_entries e ON m.entry_id = e.id
            WHERE e.id = ?1 AND e.namespace = ?2
            "#,
            params![entry_id, self.namespace],
            |row| row.get(0),
        ).optional()?;

        let embedding = match embedding_bytes {
            Some(bytes) => bytes_to_embedding(&bytes),
            None => return Ok(vec![]),
        };

        // Search for similar entries (excluding the reference)
        let mut results = self.search_similar(&embedding, limit + 1, min_similarity)?;
        results.retain(|r| r.entry.id != entry_id);
        results.truncate(limit);

        Ok(results)
    }

    /// Get a memory entry by ID
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

    /// Delete a memory entry
    pub fn delete(&self, id: &str) -> Result<bool> {
        let affected = self.conn.execute(
            "DELETE FROM memory_entries WHERE id = ?1 AND namespace = ?2",
            params![id, self.namespace],
        )?;

        Ok(affected > 0)
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

    /// Clear all entries in namespace
    pub fn clear(&self) -> Result<usize> {
        let affected = self.conn.execute(
            "DELETE FROM memory_entries WHERE namespace = ?1",
            params![self.namespace],
        )?;

        warn!("Cleared {} entries from namespace: {}", affected, self.namespace);
        Ok(affected)
    }
}

/// Result of a similarity search
#[derive(Debug, Clone)]
pub struct SimilarityResult {
    pub entry: MemoryEntry,
    pub similarity: f32,
    pub embedding_model: Option<String>,
}

/// Convert a vector of f32 to bytes for storage
fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    embedding
        .iter()
        .flat_map(|&f| f.to_le_bytes())
        .collect()
}

/// Convert bytes back to a vector of f32
fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let bytes: [u8; 4] = chunk.try_into().unwrap();
            f32::from_le_bytes(bytes)
        })
        .collect()
}

/// Compute cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32], a_norm: f32) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let b_norm = vector_norm(b);

    if a_norm == 0.0 || b_norm == 0.0 {
        return 0.0;
    }

    dot_product / (a_norm * b_norm)
}

/// Compute the L2 norm of a vector
fn vector_norm(v: &[f32]) -> f32 {
    v.iter().map(|&x| x * x).sum::<f32>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_store() -> (VectorMemory, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let store = VectorMemory::new(&db_path, "test-namespace", 384).unwrap();
        (store, temp_dir)
    }

    fn create_test_embedding(dim: usize, value: f32) -> Vec<f32> {
        vec![value; dim]
    }

    #[test]
    fn test_store_and_retrieve() {
        let (store, _temp) = create_test_store();
        
        let embedding = create_test_embedding(384, 0.1);
        let id = store.store("Test content", embedding, Some("test-model"), None).unwrap();
        assert!(!id.is_empty());

        let entry = store.get(&id).unwrap().unwrap();
        assert_eq!(entry.content, "Test content");
    }

    #[test]
    fn test_similarity_search() {
        let (store, _temp) = create_test_store();
        
        // Store entries with different embeddings
        let embedding1 = create_test_embedding(384, 0.1);
        store.store("Content A", embedding1.clone(), None, None).unwrap();
        
        let embedding2 = create_test_embedding(384, 0.9);
        store.store("Content B", embedding2.clone(), None, None).unwrap();
        
        let embedding3 = create_test_embedding(384, -0.5);
        store.store("Content C", embedding3, None, None).unwrap();

        // Search with embedding similar to B
        let query = create_test_embedding(384, 0.85);
        let results = store.search_similar(&query, 10, -1.0).unwrap();  // Use -1.0 to include all similarities
        
        // Should have at least 2 results (Content A and B have positive similarity with query,
        // Content C has negative similarity but should still be included with threshold -1.0)
        assert!(results.len() >= 2, "Expected at least 2 results, got {}", results.len());
        // Content B should be most similar (highest similarity score)
        assert_eq!(results[0].entry.content, "Content B");
        assert!(results[0].similarity > 0.99);
    }

    #[test]
    fn test_find_similar_to() {
        let (store, _temp) = create_test_store();
        
        let embedding1 = create_test_embedding(384, 0.1);
        let id1 = store.store("Reference content", embedding1.clone(), None, None).unwrap();
        
        let embedding2 = create_test_embedding(384, 0.11); // Very similar
        store.store("Similar content", embedding2, None, None).unwrap();
        
        let embedding3 = create_test_embedding(384, -0.9); // Very different
        store.store("Different content", embedding3, None, None).unwrap();

        let results = store.find_similar_to(&id1, 5, 0.5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.content, "Similar content");
    }

    #[test]
    fn test_dimension_mismatch() {
        let (store, _temp) = create_test_store();
        
        let wrong_embedding = create_test_embedding(100, 0.1);
        let result = store.store("Test", wrong_embedding, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_cosine_similarity_calculation() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b, vector_norm(&a));
        assert!((sim - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &c, vector_norm(&a));
        assert!(sim.abs() < 0.001); // Orthogonal = 0 similarity
    }
}
