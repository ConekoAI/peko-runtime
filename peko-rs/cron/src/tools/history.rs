//! `CronHistory` tool — read a scheduled job's run history
//!
//! Reads `CronRun` rows (status, timestamps, output, error) through the
//! [`CronRuntime`] port. The data already lives in the principal's
//! schedule file (1000-run retention cap); this tool is the agent-facing
//! reader so "why did my reminder not fire / fail" is answerable without
//! shelling out to the schedule file.

use crate::tools::delete::{resolve_id_by_label, verify_id_belongs_to_principal};
use crate::tools::global_runtime;
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronHistory` tool — read a scheduled job's run history
pub struct CronHistoryTool;

impl CronHistoryTool {
    /// Create a new `CronHistory` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronHistoryTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronHistory` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronHistoryArgs {
    /// Job ID to read history for
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional label (alternative to `id`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Max runs to return (most recent first). Defaults to 10, capped at 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[async_trait]
impl Tool for CronHistoryTool {
    fn name(&self) -> &'static str {
        "CronHistory"
    }

    fn description(&self) -> String {
        "Read a scheduled job's run history by ID (or label): per-fire status, start/finish timestamps, output, and error message — most recent first. Use it to debug why a job failed or what it did (CronList only shows the LAST fire's status, not the error text or trend).".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ID of the scheduled job"
                },
                "label": {
                    "type": "string",
                    "description": "Label of the scheduled job (alternative to id)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max runs to return (most recent first). Defaults to 10, capped at 50."
                }
            },
            "oneOf": [
                { "required": ["id"] },
                { "required": ["label"] }
            ]
        })
    }

    /// Read-only, but shares the cron port with the write tools; keep
    /// it out of parallel dispatch for consistency with them.
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronHistory requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let principal_id = ctx
            .principal_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CronHistory requires a Principal context"))?
            .clone();

        let runtime = global_runtime().ok_or_else(|| {
            anyhow::anyhow!("CronHistory requires the daemon's cron runtime; not initialized")
        })?;

        let args: CronHistoryArgs = serde_json::from_value(params.clone())
            .map_err(|e| anyhow::anyhow!("Invalid CronHistory arguments: {e}"))?;

        let job_id = if let Some(id) = args.id.filter(|s| !s.is_empty()) {
            verify_id_belongs_to_principal(&*runtime, &id, &principal_id).await?;
            id
        } else if let Some(label) = args.label {
            resolve_id_by_label(&*runtime, &label, &principal_id).await?
        } else {
            return Err(anyhow::anyhow!(
                "Either id or label is required for CronHistory"
            ));
        };

        let limit = args.limit.unwrap_or(10).clamp(1, 50);
        let runs = runtime.run_history(&job_id, limit).await?;
        Ok(json!({
            "job_id": job_id,
            "count": runs.len(),
            "runs": runs,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_history_tool_name() {
        let tool = CronHistoryTool::new();
        assert_eq!(tool.name(), "CronHistory");
    }

    #[test]
    fn test_cron_history_tool_parameters() {
        let tool = CronHistoryTool::new();
        let params = tool.parameters();
        let branches = params
            .get("oneOf")
            .expect("CronHistory schema must use oneOf for id-or-label");
        assert_eq!(branches.as_array().unwrap().len(), 2);
        let props = params.get("properties").unwrap();
        assert!(props.get("limit").is_some());
    }
}
