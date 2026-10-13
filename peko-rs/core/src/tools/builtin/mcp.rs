//! Operational MCP surface; configuration remains ordinary workspace files.
use crate::extensions::mcp::service::PrincipalMcpRuntime;
use anyhow::Result;
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct McpTool {
    runtime: Arc<PrincipalMcpRuntime>,
}
impl McpTool {
    pub fn new(runtime: Arc<PrincipalMcpRuntime>) -> Self {
        Self { runtime }
    }
}
#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        "MCP"
    }
    fn description(&self) -> String {
        "Inspect and refresh your workspace MCP servers, reconnect or authenticate, and retrieve server resources and prompts. Configure servers with Write/Edit under mcp/<id>/server.json. Server methods appear as individual mcp:<server>:<method> tools.".into()
    }
    fn parameters(&self) -> Value {
        let actions = [
            "status",
            "refresh",
            "reconnect",
            "authenticate",
            "resources",
            "resource_templates",
            "read_resource",
            "prompts",
            "get_prompt",
        ];
        let variants:Vec<_> = actions.iter().map(|action| {
            let mut required=vec!["action"]; let mut allowed=vec!["action"];
            if *action != "status" {allowed.push("server");if *action != "refresh" {required.push("server");}}
            if *action == "read_resource" {allowed.push("uri");required.push("uri");}
            if *action == "get_prompt" {allowed.extend(["name","arguments"]);required.push("name");}
            json!({"properties":{"action":{"const":action}},"required":required,"propertyNames":{"enum":allowed}})
        }).collect();
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":actions},"server":{"type":"string","minLength":1},
            "uri":{"type":"string","minLength":1},"name":{"type":"string","minLength":1},
            "arguments":{"type":"object","additionalProperties":{"type":"string"}}
        },"required":["action"],"additionalProperties":false,"oneOf":variants})
    }
    fn parallelizable(&self) -> bool {
        false
    }
    async fn execute(&self, _: Value) -> Result<Value> {
        anyhow::bail!("MCP requires principal invocation context")
    }
    async fn execute_with_context(&self, params: Value, context: &ToolContext) -> Result<Value> {
        let action = params["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("MCP requires action"))?;
        self.runtime
            .operate(action, params["server"].as_str(), &params, context)
            .await
    }
}
