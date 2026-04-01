//! Injectable MCP Tool Proxy
//!
//! Wraps McpToolProxy with reserved parameter injection support.
//! This allows MCP tools to receive runtime context (agent_id, session_id, etc.)
//! that is hidden from the LLM but injected by the Pekobot runtime.
//!
//! # Example Configuration
//!
//! ```toml
//! [[server]]
//! name = "memory"
//! transport = "stdio"
//! command = "mcp-memory"
//!
//! [server.reserved_parameters]
//! agent_id = { source = "runtime", field = "agent_id" }
//! session_id = { source = "runtime", field = "session_id" }
//! ```

use crate::mcp::{
    config::ReservedParamConfig,
    tool_proxy::McpToolProxy,
    types::Tool as McpTool,
};
use crate::tools::{Tool, ToolContext};
use crate::tools::shared::proxy_utils::execute_with_context_handling;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, trace};

/// An MCP tool proxy with reserved parameter injection
///
/// This wraps an McpToolProxy and adds the ability to inject reserved parameters
/// from runtime context into tool calls. The reserved parameters are hidden from
/// the LLM (not shown in the tool schema) but are injected at execution time.
pub struct InjectableMcpToolProxy {
    /// The underlying MCP tool proxy
    inner: McpToolProxy,
    /// Reserved parameter configurations (name -> config)
    reserved_params: HashMap<String, ReservedParamConfig>,
    /// Modified schema with reserved params removed (for LLM visibility)
    filtered_schema: Value,
}

impl InjectableMcpToolProxy {
    /// Create a new injectable tool proxy
    ///
    /// # Arguments
    /// * `server_name` - Name of the MCP server
    /// * `tool` - The MCP tool definition
    /// * `manager` - Reference to the MCP manager
    /// * `reserved_params` - Map of parameter name to its configuration
    pub fn new(
        server_name: String,
        tool: McpTool,
        manager: Arc<RwLock<crate::mcp::manager::McpManager>>,
        reserved_params: HashMap<String, ReservedParamConfig>,
    ) -> Self {
        let inner = McpToolProxy::new(server_name, tool.clone(), manager);

        // Filter reserved params from the schema so LLM doesn't see them
        let filtered_schema = Self::filter_schema(&tool.input_schema, &reserved_params);

        Self {
            inner,
            reserved_params,
            filtered_schema,
        }
    }

    /// Create a new injectable tool proxy with custom estimated duration
    pub fn with_duration(
        server_name: String,
        tool: McpTool,
        manager: Arc<RwLock<crate::mcp::manager::McpManager>>,
        reserved_params: HashMap<String, ReservedParamConfig>,
        estimated_duration_ms: u64,
    ) -> Self {
        let inner =
            McpToolProxy::with_duration(server_name, tool.clone(), manager, estimated_duration_ms);

        // Filter reserved params from the schema so LLM doesn't see them
        let filtered_schema = Self::filter_schema(&tool.input_schema, &reserved_params);

        Self {
            inner,
            reserved_params,
            filtered_schema,
        }
    }

    /// Get the server name
    #[must_use]
    pub fn server_name(&self) -> &str {
        self.inner.server_name()
    }

    /// Get the underlying MCP tool definition
    #[must_use]
    pub fn mcp_tool(&self) -> &McpTool {
        self.inner.mcp_tool()
    }

    /// Check if this proxy has any reserved parameters configured
    #[must_use]
    pub fn has_reserved_params(&self) -> bool {
        !self.reserved_params.is_empty()
    }

    /// Get the reserved parameter configurations
    #[must_use]
    pub fn reserved_params(&self) -> &HashMap<String, ReservedParamConfig> {
        &self.reserved_params
    }

    /// Filter reserved parameters from the JSON schema
    ///
    /// This creates a modified schema that hides the reserved parameters from the LLM,
    /// while still validating them internally.
    ///
    /// Uses the shared schema filter for consistency with Universal Tools.
    fn filter_schema(schema: &Value, reserved: &HashMap<String, ReservedParamConfig>) -> Value {
        use crate::tools::shared::filter_reserved_params;
        use std::collections::HashSet;
        
        let reserved_set: HashSet<String> = reserved.keys().cloned().collect();
        filter_reserved_params(schema, &reserved_set)
    }

    /// Inject reserved parameters into the arguments
    ///
    /// Takes the LLM-provided arguments and merges in the reserved parameters
    /// from the runtime context.
    fn inject_params(
        &self,
        mut params: Value,
        ctx: Option<&ToolContext>,
    ) -> anyhow::Result<Value> {
        if self.reserved_params.is_empty() {
            return Ok(params);
        }

        // Ensure params is an object
        if !params.is_object() {
            return Err(anyhow::anyhow!(
                "Tool arguments must be an object, got: {}",
                params
            ));
        }

        let obj = params.as_object_mut().unwrap();

        // Inject each reserved parameter
        for (name, config) in &self.reserved_params {
            let value = config.resolve(ctx);
            trace!("Injecting reserved param '{}' = {:?}", name, value);
            obj.insert(name.clone(), value);
        }

        Ok(params)
    }

    /// Execute with parameter injection
    async fn do_execute(&self, params: Value, ctx: Option<&ToolContext>) -> anyhow::Result<Value> {
        // Inject reserved parameters from context
        let merged = self.inject_params(params, ctx)?;

        trace!(
            "Executing {} with {} reserved params injected",
            self.name(),
            self.reserved_params.len()
        );

        // Delegate to inner proxy
        self.inner.execute(merged).await
    }
}

#[async_trait]
impl Tool for InjectableMcpToolProxy {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn llm_description(&self) -> String {
        self.inner.llm_description()
    }

    fn parameters(&self) -> Value {
        // Return the filtered schema (without reserved params)
        self.filtered_schema.clone()
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        // Without context, we can't inject runtime params
        // This is a fallback - ideally all calls go through execute_with_context
        if !self.reserved_params.is_empty() {
            debug!(
                "Executing {} without context - reserved params may be null",
                self.name()
            );
        }

        self.do_execute(params, None).await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        // Use the shared context handling utility to eliminate duplication
        // We pass a closure that captures self and calls our do_execute method
        let tool_name = self.name().to_string();
        let server_name = self.server_name().to_string();
        
        execute_with_context_handling(
            ctx,
            &tool_name,
            Some(&server_name),
            || async move {
                // Inject reserved parameters and execute
                self.do_execute(params, Some(ctx)).await
            },
        )
        .await
    }

    fn supports_progress(&self) -> bool {
        self.inner.supports_progress()
    }

    fn estimated_duration_ms(&self, params: &Value) -> u64 {
        self.inner.estimated_duration_ms(params)
    }
}

impl std::fmt::Debug for InjectableMcpToolProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectableMcpToolProxy")
            .field("server_name", &self.server_name())
            .field("tool_name", &self.name())
            .field("reserved_params", &self.reserved_params.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::ReservedParamConfig;
    use serde_json::json;

    #[test]
    fn test_filter_schema_removes_reserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "key": {"type": "string"},
                "value": {"type": "string"},
                "agent_id": {"type": "string"},
                "session_id": {"type": "string"}
            },
            "required": ["key", "agent_id"]
        });

        let mut reserved = HashMap::new();
        reserved.insert(
            "agent_id".to_string(),
            ReservedParamConfig::runtime("agent_id"),
        );
        reserved.insert(
            "session_id".to_string(),
            ReservedParamConfig::runtime("session_id"),
        );

        let filtered = InjectableMcpToolProxy::filter_schema(&schema, &reserved);

        // Reserved params should be removed from properties
        let props = filtered["properties"].as_object().unwrap();
        assert!(props.contains_key("key"));
        assert!(props.contains_key("value"));
        assert!(!props.contains_key("agent_id"));
        assert!(!props.contains_key("session_id"));

        // Reserved params should be removed from required
        let required = filtered["required"].as_array().unwrap();
        assert!(required.contains(&json!("key")));
        assert!(!required.contains(&json!("agent_id")));
    }

    #[test]
    fn test_filter_schema_empty_reserved() {
        let schema = json!({
            "type": "object",
            "properties": {
                "key": {"type": "string"}
            }
        });

        let reserved = HashMap::new();
        let filtered = InjectableMcpToolProxy::filter_schema(&schema, &reserved);

        // Schema should be unchanged
        assert_eq!(filtered, schema);
    }

    #[test]
    fn test_reserved_param_config_resolve() {
        // Create a ToolContext using the constructor
        let abort_signal = crate::tools::AbortSignal::new();
        let ctx = abort_signal
            .create_context("run_abc", "tool_1", "test_tool")
            .with_agent_id("agent_456")
            .with_session_id("sess_123")
            .with_peer_id("peer_789")
            .with_workspace("/tmp/test");

        let agent_config = ReservedParamConfig::runtime("agent_id");
        assert_eq!(
            agent_config.resolve(Some(&ctx)),
            json!("agent_456")
        );

        let session_config = ReservedParamConfig::runtime("session_id");
        assert_eq!(
            session_config.resolve(Some(&ctx)),
            json!("sess_123")
        );

        let peer_config = ReservedParamConfig::runtime("peer_id");
        assert_eq!(
            peer_config.resolve(Some(&ctx)),
            json!("peer_789")
        );

        // Test static resolution
        let static_config = ReservedParamConfig::static_value("hardcoded");
        assert_eq!(
            static_config.resolve(None),
            json!("hardcoded")
        );
    }

    #[test]
    fn test_reserved_param_config_resolve_no_context() {
        let config = ReservedParamConfig::runtime("agent_id");
        // Without context, runtime params resolve to null
        assert_eq!(config.resolve(None), json!(null));
    }
}
