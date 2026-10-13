use super::{config::ReservedParameter, PrincipalMcpRuntime, Server};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use peko_tools_core::{Tool, ToolContext};
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

pub(super) struct MethodTool {
    runtime: Arc<PrincipalMcpRuntime>,
    server: Arc<Server>,
    descriptor: Value,
    name: String,
    schema: Value,
}

impl MethodTool {
    pub(super) fn new(
        runtime: Arc<PrincipalMcpRuntime>,
        server: Arc<Server>,
        descriptor: Value,
    ) -> Self {
        let name = format!(
            "mcp:{}:{}",
            server.id,
            descriptor["name"].as_str().unwrap_or_default()
        );
        let reserved: HashSet<String> = server
            .definition
            .reserved_parameters
            .keys()
            .cloned()
            .collect();
        let schema =
            crate::extensions::framework::protocols::shared::schema_filter::filter_reserved_params(
                &descriptor["inputSchema"],
                &reserved,
            );
        Self {
            runtime,
            server,
            descriptor,
            name,
            schema,
        }
    }
}

#[async_trait]
impl Tool for MethodTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> String {
        self.descriptor["description"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
    fn parameters(&self) -> Value {
        self.schema.clone()
    }
    fn supports_progress(&self) -> bool {
        true
    }
    async fn execute(&self, _: Value) -> Result<Value> {
        anyhow::bail!("MCP requires principal invocation context")
    }
    async fn execute_with_context(
        &self,
        mut params: Value,
        context: &ToolContext,
    ) -> Result<Value> {
        self.runtime.validate_owner(context)?;
        ensure!(!context.is_aborted(), "MCP request cancelled");
        self.runtime.reconcile().await?;
        ensure!(
            self.server.current.load(Ordering::Acquire),
            "McpCatalogChanged: server definition or credentials changed; refresh the catalog"
        );
        let client = self.runtime.start(&self.server).await?;
        if client.changed.load(Ordering::Acquire) {
            self.runtime.discover(&self.server, &client).await?;
        }
        let admission = self.server.admission.lock().await;
        ensure!(
            self.server.current.load(Ordering::Acquire),
            "McpServerRemoved: server no longer available"
        );
        let matches = self
            .server
            .inventory
            .read()
            .await
            .as_ref()
            .is_some_and(|inventory| {
                inventory
                    .tools
                    .iter()
                    .any(|descriptor| descriptor == &self.descriptor)
            });
        ensure!(
            matches,
            "McpCatalogChanged: advertised tool definition changed; refresh the catalog"
        );
        let object = params
            .as_object_mut()
            .context("MCP tool arguments must be an object")?;
        for (name, source) in &self.server.definition.reserved_parameters {
            ensure!(
                !object.contains_key(name),
                "reserved parameter '{name}' cannot be supplied by the caller"
            );
            let value = match source {
                ReservedParameter::Static { value } => value.clone(),
                ReservedParameter::Credential { credential } => {
                    Value::String(self.runtime.secret(&self.server.id, credential)?)
                }
                ReservedParameter::Runtime { field } => {
                    let value = match field.as_str() {
                        "principal_id" => context.principal_id.as_deref(),
                        "principal_name" => context.principal_name.as_deref(),
                        "session_id" => context.session_id.as_deref(),
                        "agent_id" => context.agent_id.as_deref(),
                        "workspace" => context.workspace.as_deref(),
                        "run_id" => Some(context.run_id.as_str()),
                        "tool_call_id" => Some(context.tool_id.as_str()),
                        _ => None,
                    }
                    .filter(|value| !value.is_empty())
                    .with_context(|| {
                        format!("missing invocation context for reserved parameter '{name}'")
                    })?;
                    Value::String(value.into())
                }
            };
            object.insert(name.clone(), value);
        }
        drop(admission);
        client
            .request_with_context(
                "tools/call",
                serde_json::json!({"name":self.descriptor["name"],"arguments":params}),
                self.server
                    .definition
                    .tool_timeout_ms
                    .map(Duration::from_millis),
                Some(context),
            )
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "{}",
                    self.runtime.sanitize(&self.server.id, format!("{error:#}"))
                )
            })
    }
}
