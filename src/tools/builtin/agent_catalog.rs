//! Agent catalog tool
//!
//! Provides `agent_catalog` so the supervisor agent can discover the specialist
//! agents available inside the current Principal.

use async_trait::async_trait;
use serde_json::json;

use crate::principal::router::AgentPromptSummary;
use crate::tools::core::traits::Tool;

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

Parameters:
- role: Optional filter by agent role (e.g., 'specialist', 'router')

Returns an array of agents with name, role, and description."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "description": "Optional filter by agent role"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let role_filter = params.get("role").and_then(|v| v.as_str());

        let agents: Vec<serde_json::Value> = self
            .agents
            .iter()
            .filter(|a| {
                role_filter.map_or(true, |r| {
                    format!("{:?}", a.role).eq_ignore_ascii_case(r)
                })
            })
            .map(|a| {
                json!({
                    "name": a.name,
                    "role": format!("{:?}", a.role).to_lowercase(),
                    "description": a.description,
                })
            })
            .collect();

        Ok(json!({ "total": agents.len(), "agents": agents }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::AgentRole;

    #[tokio::test]
    async fn test_list_all() {
        let tool = AgentCatalogTool::new(vec![
            AgentPromptSummary {
                name: "math".to_string(),
                role: AgentRole::Specialist,
                description: Some("Math specialist".to_string()),
            },
            AgentPromptSummary {
                name: "primary".to_string(),
                role: AgentRole::Default,
                description: Some("Generalist".to_string()),
            },
        ]);

        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
    }

    #[tokio::test]
    async fn test_filter_by_role() {
        let tool = AgentCatalogTool::new(vec![
            AgentPromptSummary {
                name: "math".to_string(),
                role: AgentRole::Specialist,
                description: Some("Math specialist".to_string()),
            },
            AgentPromptSummary {
                name: "primary".to_string(),
                role: AgentRole::Default,
                description: Some("Generalist".to_string()),
            },
        ]);

        let result = tool.execute(json!({"role": "specialist"})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["agents"][0]["name"], "math");
    }
}
