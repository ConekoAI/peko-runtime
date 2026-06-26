//! Principal memory recall/store tool
//!
//! Provides `principal_memory` so the supervisor agent can recall stored
//! artifacts or persist new ones in the Principal's memory namespace.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

use crate::principal::memory::{Artifact, FileArtifact, MemoryArtifact, PrincipalMemory, TodoArtifact};
use crate::tools::core::traits::Tool;

/// Actions supported by `principal_memory`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryAction {
    Recall,
    Store,
}

/// Artifact descriptor for the `store` action.
#[derive(Debug, Deserialize)]
struct ArtifactInput {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    path: String,
}

/// Tool for recalling and storing Principal memory artifacts.
pub struct PrincipalMemoryTool {
    memory: Arc<dyn PrincipalMemory>,
}

impl PrincipalMemoryTool {
    /// Create a new tool backed by the given Principal memory.
    #[must_use]
    pub fn new(memory: Arc<dyn PrincipalMemory>) -> Self {
        Self { memory }
    }

    async fn recall(
        &self,
        query: Option<&str>,
        k: usize,
    ) -> anyhow::Result<serde_json::Value> {
        let query = query.unwrap_or("");
        let artifacts = self.memory.recall(query, k).await?;
        let items: Vec<serde_json::Value> = artifacts.iter().map(artifact_to_json).collect();
        Ok(json!({ "query": query, "k": k, "artifacts": items }))
    }

    async fn store(
        &self,
        artifact: Option<ArtifactInput>,
    ) -> anyhow::Result<serde_json::Value> {
        let input = artifact.ok_or_else(|| anyhow::anyhow!("'store' action requires an 'artifact' object"))?;
        let parsed = parse_artifact_input(input)?;
        self.memory.store(parsed).await?;
        Ok(json!({ "stored": true }))
    }
}

fn parse_artifact_input(input: ArtifactInput) -> anyhow::Result<Artifact> {
    match input.kind.to_lowercase().as_str() {
        "memory" => Ok(Artifact::Memory(MemoryArtifact {
            id: uuid::Uuid::new_v4().to_string(),
            content: input.content,
            kind: "memory".to_string(),
            source: input.source,
        })),
        "todo" => Ok(Artifact::Todo(TodoArtifact {
            id: uuid::Uuid::new_v4().to_string(),
            title: input.content,
            status: input.source,
        })),
        "file" => Ok(Artifact::File(FileArtifact {
            path: std::path::PathBuf::from(input.path),
            content: input.content,
        })),
        other => anyhow::bail!(
            "Unsupported artifact kind '{other}'. Use: memory, todo, file."
        ),
    }
}

fn artifact_to_json(artifact: &Artifact) -> serde_json::Value {
    match artifact {
        Artifact::Session(a) => json!({
            "kind": "session",
            "session_id": a.session_id,
            "peer": a.peer.to_string(),
            "title": a.title,
            "updated_at": a.updated_at.to_rfc3339(),
            "summary": a.summary,
        }),
        Artifact::Memory(a) => json!({
            "kind": "memory",
            "id": a.id,
            "content": a.content,
            "kind_detail": a.kind,
            "source": a.source,
        }),
        Artifact::Todo(a) => json!({
            "kind": "todo",
            "id": a.id,
            "title": a.title,
            "status": a.status,
        }),
        Artifact::File(a) => json!({
            "kind": "file",
            "path": a.path.to_string_lossy(),
            "content": a.content,
        }),
    }
}

#[async_trait]
impl Tool for PrincipalMemoryTool {
    fn name(&self) -> &'static str {
        "principal_memory"
    }

    fn description(&self) -> String {
        r"Recall or store artifacts in the Principal's memory.

Parameters:
- action: 'recall' or 'store' (required)
- query: Search query for 'recall'
- k: Number of artifacts to recall (default: 5)
- artifact: Object for 'store' with:
  - kind: 'memory', 'todo', or 'file'
  - content: text content (or title for todo)
  - source: source tag (or status for todo)
  - path: file path (required for 'file')

Returns recalled artifacts or store confirmation."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["recall", "store"],
                    "description": "Whether to recall existing artifacts or store a new one"
                },
                "query": {
                    "type": "string",
                    "description": "Search query for the 'recall' action"
                },
                "k": {
                    "type": "integer",
                    "default": 5,
                    "description": "Maximum number of artifacts to recall"
                },
                "artifact": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["memory", "todo", "file"],
                            "description": "Artifact kind"
                        },
                        "content": { "type": "string" },
                        "source": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["kind"]
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(
        &self,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let action: MemoryAction = serde_json::from_value(
            params
                .get("action")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing required 'action' parameter"))?,
        )
        .map_err(|e| anyhow::anyhow!("Invalid action: {e}"))?;

        match action {
            MemoryAction::Recall => {
                let query = params.get("query").and_then(|v| v.as_str());
                let k = params.get("k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
                self.recall(query, k).await
            }
            MemoryAction::Store => {
                let artifact = params
                    .get("artifact")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| anyhow::anyhow!("Invalid artifact: {e}"))?;
                self.store(artifact).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Subject;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockMemory {
        artifacts: Mutex<Vec<Artifact>>,
    }

    #[async_trait]
    impl PrincipalMemory for MockMemory {
        async fn record_session(
            &self,
            _artifact: crate::principal::memory::SessionArtifact,
        ) -> Result<(), crate::principal::memory::MemoryError> {
            Ok(())
        }

        async fn find_latest_session_for_peer(
            &self,
            _peer: &Subject,
        ) -> Result<Option<crate::principal::memory::SessionArtifact>, crate::principal::memory::MemoryError> {
            Ok(None)
        }

        async fn list_sessions(
            &self) -> Result<Vec<crate::principal::memory::SessionArtifact>, crate::principal::memory::MemoryError> {
            Ok(Vec::new())
        }

        async fn store(&self, artifact: Artifact) -> Result<(), crate::principal::memory::MemoryError> {
            self.artifacts.lock().unwrap().push(artifact);
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _k: usize,
        ) -> Result<Vec<Artifact>, crate::principal::memory::MemoryError> {
            Ok(self.artifacts.lock().unwrap().clone())
        }

        async fn compact(
            &self,
        ) -> Result<crate::principal::memory::CompactSummary, crate::principal::memory::MemoryError> {
            Ok(crate::principal::memory::CompactSummary::default())
        }

        fn sessions_dir(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn supervisor_session_path(&self) -> std::path::PathBuf {
            std::env::temp_dir().join("supervisor.jsonl")
        }
    }

    #[tokio::test]
    async fn test_store_memory() {
        let memory = Arc::new(MockMemory {
            artifacts: Mutex::new(Vec::new()),
        });
        let tool = PrincipalMemoryTool::new(memory);

        let result = tool
            .execute(json!({
                "action": "store",
                "artifact": {
                    "kind": "memory",
                    "content": "User likes dark mode",
                    "source": "preference"
                }
            }))
            .await
            .unwrap();
        assert!(result["stored"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_recall() {
        let memory = Arc::new(MockMemory {
            artifacts: Mutex::new(vec![Artifact::Memory(MemoryArtifact {
                id: "id1".to_string(),
                content: "dark mode".to_string(),
                kind: "memory".to_string(),
                source: "pref".to_string(),
            })]),
        });
        let tool = PrincipalMemoryTool::new(memory);

        let result = tool
            .execute(json!({"action": "recall", "query": "mode", "k": 5}))
            .await
            .unwrap();
        assert_eq!(result["artifacts"].as_array().unwrap().len(), 1);
    }
}
