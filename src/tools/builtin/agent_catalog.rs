//! Agent catalog tool
//!
//! Provides `agent_catalog` so the root agent can discover the specialist
//! agents available inside the current Principal.

use async_trait::async_trait;
use serde_json::json;

use crate::principal::router::AgentPromptSummary;
use peko_tools_core::traits::Tool;

/// Tool for listing available agents in a Principal.
pub struct AgentCatalogTool {
    agents: Vec<AgentPromptSummary>,
}

impl AgentCatalogTool {
    /// Create a new catalog from the Principal's discovered agents.
    #[must_use]
    pub fn new(agents: Vec<AgentPromptSummary>) -> Self {
        Self { agents }
    }
}

#[async_trait]
impl Tool for AgentCatalogTool {
    fn name(&self) -> &'static str {
        "agent_catalog"
    }

    fn description(&self) -> String {
        r"List the specialist agents available in this Principal.

Returns an array of agents with `id`, `name`, `description`, and
`enabled`. Only agents with `enabled: true` may be spawned via the
`Agent` tool."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let agents: Vec<serde_json::Value> = self
            .agents
            .iter()
            .map(|a| {
                json!({
                    "id": a.id,
                    "name": a.name,
                    "description": a.description,
                    "enabled": a.enabled,
                })
            })
            .collect();

        Ok(json!({ "total": agents.len(), "agents": agents }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_all() {
        let tool = AgentCatalogTool::new(vec![
            AgentPromptSummary {
                id: "math".to_string(),
                name: "math".to_string(),
                description: Some("Math specialist".to_string()),
                enabled: true,
            },
            AgentPromptSummary {
                id: "primary".to_string(),
                name: "Primary".to_string(),
                description: Some("Generalist".to_string()),
                enabled: false,
            },
        ]);

        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["agents"][0]["id"], "math");
        assert_eq!(result["agents"][0]["name"], "math");
        assert_eq!(result["agents"][0]["enabled"], true);
        assert_eq!(result["agents"][1]["id"], "primary");
        assert_eq!(result["agents"][1]["name"], "Primary");
        assert_eq!(result["agents"][1]["enabled"], false);
    }
}
