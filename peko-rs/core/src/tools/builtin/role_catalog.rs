//! Role catalog tool
//!
//! Provides `RoleCatalog` so an agent can discover the role templates
//! (`roles/<id>.md` / `roles/<id>/ROLE.md`, ADR-064) in its Principal's
//! workspace. Roles are rescanned on every call, so new files are visible
//! without a restart.

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

use peko_tools_core::traits::Tool;

/// Tool for listing the role templates in a Principal workspace.
pub struct AgentCatalogTool {
    workspace: PathBuf,
}

impl AgentCatalogTool {
    /// A stable principal-owned tool which discovers workspace roles at call time.
    #[must_use]
    pub fn from_workspace(workspace: PathBuf) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for AgentCatalogTool {
    fn name(&self) -> &'static str {
        "RoleCatalog"
    }

    fn description(&self) -> String {
        r"List the role templates available in this Principal.

Returns `{ total, agents }`; each entry has `id`, `name`,
`description`, and `enabled`. Pass an enabled entry's `id` as the
`Agent` tool's `role` argument."
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
        let roles_dir = self.workspace.join("roles");
        let mut roles = tokio::task::spawn_blocking(move || {
            crate::extensions::role::RoleAdapter::new().discover_roles(&roles_dir)
        })
        .await?;
        roles.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        // Presence = availability (ADR-066): every discovered role is enabled.
        let agents: Vec<serde_json::Value> = roles
            .into_iter()
            .map(|role| {
                json!({
                    "id": role.manifest.id,
                    "name": role.manifest.name,
                    "description": role.manifest.description,
                    "enabled": true,
                })
            })
            .collect();

        Ok(json!({ "total": agents.len(), "agents": agents }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\nYou are {name}.\n")
    }

    #[tokio::test]
    async fn lists_flat_and_directory_roles_sorted_by_id() {
        let workspace = tempfile::tempdir().unwrap();
        let roles = workspace.path().join("roles");
        std::fs::create_dir_all(roles.join("reviewer")).unwrap();
        std::fs::write(roles.join("writer.md"), role("Writer", "Drafts prose")).unwrap();
        std::fs::write(
            roles.join("reviewer").join("ROLE.md"),
            role("Reviewer", "Reviews drafts"),
        )
        .unwrap();

        let tool = AgentCatalogTool::from_workspace(workspace.path().to_path_buf());
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(
            result["agents"],
            json!([
                {"id":"reviewer", "name":"Reviewer", "description":"Reviews drafts", "enabled":true},
                {"id":"writer", "name":"Writer", "description":"Drafts prose", "enabled":true},
            ])
        );
    }

    #[tokio::test]
    async fn missing_roles_directory_is_an_empty_catalog() {
        let workspace = tempfile::tempdir().unwrap();
        let tool = AgentCatalogTool::from_workspace(workspace.path().to_path_buf());
        assert_eq!(
            tool.execute(json!({})).await.unwrap(),
            json!({"total":0, "agents":[]})
        );
    }

    #[tokio::test]
    async fn invalid_role_files_are_skipped_and_new_files_appear_without_restart() {
        let workspace = tempfile::tempdir().unwrap();
        let roles = workspace.path().join("roles");
        std::fs::create_dir_all(&roles).unwrap();
        std::fs::write(roles.join("no-frontmatter.md"), "just a body").unwrap();
        std::fs::write(roles.join("unnamed.md"), role("", "Has no name")).unwrap();
        std::fs::write(roles.join("notes.txt"), role("Notes", "Not markdown")).unwrap();
        let tool = AgentCatalogTool::from_workspace(workspace.path().to_path_buf());
        assert_eq!(tool.execute(json!({})).await.unwrap()["total"], 0);

        std::fs::write(roles.join("planner.md"), role("Planner", "Plans work")).unwrap();
        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["total"], 1);
        assert_eq!(result["agents"][0]["id"], "planner");
    }
}
