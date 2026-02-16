//! Memory tool for agent memory operations

use async_trait::async_trait;
use serde_json::json;
use anyhow::{Context, Result};

use crate::tools::Tool;
use crate::memory::sqlite::SqliteMemory;
use std::sync::Arc;

/// Memory tool for storing and retrieving agent memories
pub struct MemoryTool {
    memory: Arc<SqliteMemory>,
    agent_did: String,
}

impl MemoryTool {
    /// Create a new memory tool
    pub fn new(memory: SqliteMemory, agent_did: String) -> Self {
        Self {
            memory: Arc::new(memory),
            agent_did,
        }
    }

    /// Create with shared memory reference
    pub fn with_arc(memory: Arc<SqliteMemory>, agent_did: String) -> Self {
        Self { memory, agent_did }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Store and retrieve memories. Actions: 'store' (save content), 'search' (find by query), 'get' (by id), 'recent' (latest N), 'clear' (delete all). Parameters: {\"action\": string, \"content\": string (for store), \"query\": string (for search), \"id\": string (for get), \"limit\": number (for recent)}"
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: action"))?;

        match action {
            "store" | "save" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: content for store action"))?;

                let metadata = params.get("metadata").cloned();
                
                let id = self.memory.store(content, metadata)
                    .context("Failed to store memory")?;

                Ok(json!({
                    "success": true,
                    "id": id,
                    "action": "store",
                    "agent_did": self.agent_did,
                }))
            }

            "search" | "find" => {
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query for search action"))?;

                let limit = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(10);

                let entries = self.memory.search(query, limit)
                    .context("Failed to search memories")?;

                let results: Vec<serde_json::Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "content": e.content,
                        "metadata": e.metadata,
                        "timestamp": e.timestamp,
                        "access_count": e.access_count,
                        "last_accessed": e.last_accessed_at,
                    })
                }).collect();

                Ok(json!({
                    "success": true,
                    "action": "search",
                    "query": query,
                    "count": results.len(),
                    "results": results,
                }))
            }

            "get" | "retrieve" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: id for get action"))?;

                match self.memory.get(id) {
                    Ok(Some(entry)) => {
                        Ok(json!({
                            "success": true,
                            "action": "get",
                            "found": true,
                            "entry": {
                                "id": entry.id,
                                "content": entry.content,
                                "metadata": entry.metadata,
                                "timestamp": entry.timestamp,
                                "access_count": entry.access_count,
                                "last_accessed": entry.last_accessed_at,
                            }
                        }))
                    }
                    Ok(None) => {
                        Ok(json!({
                            "success": true,
                            "action": "get",
                            "found": false,
                            "id": id,
                        }))
                    }
                    Err(e) => Err(e.into()),
                }
            }

            "recent" | "latest" => {
                let limit = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(10);

                let entries = self.memory.recent(limit)
                    .context("Failed to get recent memories")?;

                let results: Vec<serde_json::Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "content": e.content,
                        "metadata": e.metadata,
                        "timestamp": e.timestamp,
                    })
                }).collect();

                Ok(json!({
                    "success": true,
                    "action": "recent",
                    "count": results.len(),
                    "results": results,
                }))
            }

            "delete" | "remove" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: id for delete action"))?;

                let success = self.memory.delete(id)
                    .context("Failed to delete memory")?;

                Ok(json!({
                    "success": success,
                    "action": "delete",
                    "id": id,
                    "deleted": success,
                }))
            }

            "clear" | "wipe" => {
                self.memory.clear()
                    .context("Failed to clear memories")?;

                Ok(json!({
                    "success": true,
                    "action": "clear",
                    "message": "All memories cleared",
                }))
            }

            _ => Err(anyhow::anyhow!("Unknown memory action: {}. Valid actions: store, search, get, recent, delete, clear", action)),
        }
    }
}

/// Factory for creating memory tools without needing direct SqliteMemory access
pub struct MemoryToolFactory;

impl MemoryToolFactory {
    /// Create a memory tool if memory is available
    pub fn create(memory: Option<Arc<SqliteMemory>>, agent_did: String) -> Option<MemoryTool> {
        memory.map(|m| MemoryTool::with_arc(m, agent_did))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_memory() -> (SqliteMemory, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_memory.db");
        let memory = SqliteMemory::new(&db_path, "test_namespace").unwrap();
        (memory, temp_dir)
    }

    #[tokio::test]
    async fn test_memory_store() {
        let (memory, _temp) = create_test_memory();
        let tool = MemoryTool::new(memory, "did:pekobot:test".to_string());

        let params = json!({
            "action": "store",
            "content": "Test memory content",
            "metadata": {"tag": "test"}
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
        assert_eq!(response.get("action").unwrap().as_str().unwrap(), "store");
    }

    #[tokio::test]
    async fn test_memory_search() {
        let (memory, _temp) = create_test_memory();
        
        // Store something first
        memory.store("Hello world test", None).unwrap();
        memory.store("Another entry", None).unwrap();

        let tool = MemoryTool::new(memory, "did:pekobot:test".to_string());

        let params = json!({
            "action": "search",
            "query": "hello",
            "limit": 5
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());
        assert!(response.get("count").unwrap().as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn test_memory_recent() {
        let (memory, _temp) = create_test_memory();
        
        // Store a few entries
        memory.store("First", None).unwrap();
        memory.store("Second", None).unwrap();
        memory.store("Third", None).unwrap();

        let tool = MemoryTool::new(memory, "did:pekobot:test".to_string());

        let params = json!({
            "action": "recent",
            "limit": 2
        });

        let result = tool.execute(params).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response.get("count").unwrap().as_u64().unwrap(), 2);
    }

    #[tokio::test]
    async fn test_memory_invalid_action() {
        let (memory, _temp) = create_test_memory();
        let tool = MemoryTool::new(memory, "did:pekobot:test".to_string());

        let params = json!({
            "action": "invalid_action"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
    }
}
