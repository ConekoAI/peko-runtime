//! `CronList` tool — list scheduled jobs
//!
//! Lists `CronJob`s through the [`CronRuntime`] port set by the daemon
//! at startup. Results are filtered to the current Principal from the
//! tool execution context.

use crate::tools::{global_runtime, render_job_list};
use async_trait::async_trait;
use peko_tools_core::exec::ToolContext;
use peko_tools_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// `CronList` tool — list scheduled jobs
pub struct CronListTool;

impl CronListTool {
    /// Create a new `CronList` tool
    pub fn new() -> Self {
        Self
    }
}

impl Default for CronListTool {
    fn default() -> Self {
        Self::new()
    }
}

/// `CronList` tool arguments
///
/// Accepts an empty object. Sprint 7 trim (Commit A): `status_filter` and
/// `kind_filter` were declared + schema'd but the body never read them —
/// pure no-op fields. The struct stays as a named token so the
/// deserialization call site remains unchanged; future filters can land
/// here when they have an actual consumer.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronListArgs {}

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &'static str {
        "CronList"
    }

    fn description(&self) -> String {
        "List scheduled jobs stored by the daemon.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        // Sprint 7 Commit A: empty properties — `status_filter` and
        // `kind_filter` were declared but never read in
        // `execute_with_context`. Re-add a property here when an actual
        // filter ships.
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "CronList requires a Principal context; use execute_with_context"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // **Phase B.** Filter by the principal's stable DID rather than
        // its display name — matches `CronJob::principal_id`.
        let principal_id = ctx
            .principal_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("CronList requires a Principal context"))?
            .clone();

        let runtime = global_runtime().ok_or_else(|| {
            anyhow::anyhow!("CronList requires the daemon's cron runtime; not initialized")
        })?;

        let _args: CronListArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid CronList arguments: {e}"))?;

        let jobs = runtime.list_jobs().await?;
        let filtered: Vec<_> = jobs
            .into_iter()
            .filter(|j| j.principal_id.0 == principal_id)
            .collect();
        Ok(render_job_list(filtered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_list_tool_name() {
        let tool = CronListTool::new();
        assert_eq!(tool.name(), "CronList");
    }

    #[test]
    fn test_cron_list_tool_parameters() {
        let tool = CronListTool::new();
        let params = tool.parameters();
        // Sprint 7 Commit A: empty properties block (status_filter /
        // kind_filter were dropped — they had no consumer).
        assert_eq!(params["properties"], serde_json::json!({}));
        assert!(params.get("required").is_none());
    }
}
