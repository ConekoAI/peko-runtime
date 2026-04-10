//! Tool Execution Service
//!
//! Centralized service for tool execution with parameter injection.
//! Part of ExtensionCore's shared services.
//!
//! # Pipeline
//!
//! 1. **Validation**: Ensure user params don't contain reserved parameter names
//! 2. **Injection**: Merge reserved parameters from config into user params
//! 3. **Execution**: Call the tool-specific executor with merged params
//! 4. **Result**: Return the execution result
//!
//! # Usage
//!
//! ```rust,ignore
//! let exec_service = ExtensionCore::tool_execution();
//!
//! let result = exec_service.execute(
//!     user_params,
//!     &ToolExecutionConfig {
//!         reserved_params: config,
//!         full_schema: schema,
//!     },
//!     Some(&tool_context),
//!     |merged_params| async {
//!         // Tool-specific execution
//!         adapter.execute_raw(merged_params).await
//!     }
//! ).await?;
//! ```

use crate::extensions::services::reserved_params::ReservedParamsConfig;
use crate::tools::ToolContext;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;

/// Tool execution service
///
/// Handles parameter injection and validation for all tool executions
/// in the Extension system.
#[derive(Debug, Default)]
pub struct ToolExecutionService;

impl ToolExecutionService {
    /// Create a new tool execution service
    pub fn new() -> Self {
        Self
    }

    /// Execute a tool with parameter injection
    ///
    /// This is the unified entry point for all tool execution in the extension system.
    ///
    /// # Pipeline
    /// 1. Validate user params (no reserved params allowed from user)
    /// 2. Inject reserved parameters from config
    /// 3. Execute via provided executor
    /// 4. Return result
    ///
    /// # Arguments
    /// * `params` - User-provided parameters
    /// * `config` - Execution configuration including reserved params
    /// * `ctx` - Optional tool context for runtime parameter resolution
    /// * `executor` - Async closure that performs the actual tool execution
    ///
    /// # Returns
    /// The result of the tool execution
    pub async fn execute<F, Fut>(
        &self,
        params: Value,
        config: &ToolExecutionConfig,
        ctx: Option<&ToolContext>,
        executor: F,
    ) -> Result<Value>
    where
        F: FnOnce(Value) -> Fut,
        Fut: std::future::Future<Output = Result<Value>>,
    {
        // Step 1: Validate user params
        Self::validate_user_params(&params, &config.reserved_params)?;

        tracing::debug!(
            "ToolExecutionService: validated params, reserved count = {}",
            config.reserved_params.len()
        );

        // Step 2: Inject reserved parameters
        let merged = Self::inject_reserved_params(params, &config.reserved_params, ctx);

        tracing::debug!(
            "ToolExecutionService: injected {} reserved params",
            config.reserved_params.len()
        );

        // Step 3: Execute
        executor(merged).await
    }

    /// Validate that user params don't contain reserved parameter names
    ///
    /// This prevents users from providing values for parameters that should
    /// be injected by the runtime (security + correctness).
    ///
    /// # Arguments
    /// * `params` - User-provided parameters
    /// * `reserved` - Reserved parameter configuration
    ///
    /// # Returns
    /// Ok if valid, Err if user tried to provide a reserved param
    pub fn validate_user_params(
        params: &Value,
        reserved: &ReservedParamsConfig,
    ) -> Result<()> {
        if reserved.is_empty() {
            return Ok(());
        }

        if let Some(obj) = params.as_object() {
            for name in reserved.names() {
                if obj.contains_key(name) {
                    anyhow::bail!(
                        "Parameter '{}' is reserved and cannot be provided by user. \
                         It will be injected by the runtime.",
                        name
                    );
                }
            }
        }

        Ok(())
    }

    /// Inject reserved parameters into user params
    ///
    /// Takes user-provided parameters and merges in reserved parameters
    /// from the configuration, resolving runtime fields using the context.
    ///
    /// # Arguments
    /// * `params` - User-provided parameters (will be mutated)
    /// * `reserved` - Reserved parameter configuration
    /// * `ctx` - Optional tool context for runtime field resolution
    ///
    /// # Returns
    /// Merged parameters with reserved params injected
    pub fn inject_reserved_params(
        mut params: Value,
        reserved: &ReservedParamsConfig,
        ctx: Option<&ToolContext>,
    ) -> Value {
        if reserved.is_empty() {
            return params;
        }

        if let Some(obj) = params.as_object_mut() {
            let resolved = reserved.resolve(ctx);
            for (name, value) in resolved {
                tracing::trace!("Injecting reserved param '{}' = {:?}", name, value);
                obj.insert(name, value);
            }
        }

        params
    }

    /// Filter reserved parameters from schema (for LLM visibility)
    ///
    /// Creates a modified schema that hides reserved parameters from the LLM,
    /// preventing confusion and security issues.
    ///
    /// # Arguments
    /// * `schema` - Full parameter schema
    /// * `reserved` - Reserved parameter configuration
    ///
    /// # Returns
    /// Filtered schema without reserved parameters
    pub fn filter_schema_for_llm(
        &self,
        schema: &Value,
        reserved: &ReservedParamsConfig,
    ) -> Value {
        use crate::tools::shared::filter_reserved_params;

        let reserved_set: HashSet<String> = reserved.names().cloned().collect();
        filter_reserved_params(schema, &reserved_set)
    }

    /// Get exposed parameters (schema without reserved params)
    ///
    /// Convenience method that filters the schema for LLM visibility.
    pub fn get_exposed_schema(
        &self,
        full_schema: &Value,
        reserved: &ReservedParamsConfig,
    ) -> Value {
        self.filter_schema_for_llm(full_schema, reserved)
    }
}

/// Configuration for tool execution
#[derive(Debug, Clone)]
pub struct ToolExecutionConfig {
    /// Reserved parameter configuration
    pub reserved_params: ReservedParamsConfig,
    /// Full parameter schema (with reserved params for validation)
    pub full_schema: Value,
}

impl ToolExecutionConfig {
    /// Create new execution config
    pub fn new(reserved_params: ReservedParamsConfig, full_schema: Value) -> Self {
        Self {
            reserved_params,
            full_schema,
        }
    }

    /// Create config with empty reserved params
    pub fn with_schema(full_schema: Value) -> Self {
        Self {
            reserved_params: ReservedParamsConfig::new(),
            full_schema,
        }
    }

    /// Add reserved params to config (builder pattern)
    pub fn with_reserved_params(mut self, reserved: ReservedParamsConfig) -> Self {
        self.reserved_params = reserved;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::services::reserved_params::{ParamSource, ReservedParamsConfig};
    use serde_json::json;

    #[tokio::test]
    async fn test_execute_pipeline() {
        let service = ToolExecutionService::new();

        let config = ToolExecutionConfig {
            reserved_params: ReservedParamsConfig::new().with_static("injected", "value"),
            full_schema: json!({
                "type": "object",
                "properties": {
                    "user_param": { "type": "string" },
                    "injected": { "type": "string" }
                }
            }),
        };

        let user_params = json!({"user_param": "hello"});

        let result = service
            .execute(user_params, &config, None, |merged| async move {
                // Verify injection happened
                assert_eq!(merged["user_param"], "hello");
                assert_eq!(merged["injected"], "value");
                Ok(json!({"success": true}))
            })
            .await;

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_user_params_ok() {
        let service = ToolExecutionService::new();

        let reserved = ReservedParamsConfig::new().with_runtime("agent_id", "agent_id");

        let user_params = json!({"query": "test"});

        assert!(ToolExecutionService::validate_user_params(&user_params, &reserved).is_ok());
    }

    #[test]
    fn test_validate_user_params_fails_on_reserved() {
        let service = ToolExecutionService::new();

        let reserved = ReservedParamsConfig::new().with_runtime("agent_id", "agent_id");

        let user_params = json!({"query": "test", "agent_id": "evil"});

        let result = ToolExecutionService::validate_user_params(&user_params, &reserved);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("agent_id"));
    }

    #[test]
    fn test_inject_reserved_params() {
        let service = ToolExecutionService::new();

        let reserved = ReservedParamsConfig::new()
            .with_static("static_param", "static_value")
            .with_env("env_param", "PATH"); // PATH usually exists

        let user_params = json!({"user_param": "user_value"});

        let merged = ToolExecutionService::inject_reserved_params(user_params, &reserved, None);

        assert_eq!(merged["user_param"], "user_value");
        assert_eq!(merged["static_param"], "static_value");
        // env_param might be null if PATH doesn't exist in test env
        assert!(merged.as_object().unwrap().contains_key("env_param"));
    }

    #[test]
    fn test_inject_empty_config() {
        let user_params = json!({"param": "value"});
        let reserved = ReservedParamsConfig::new();

        let merged = ToolExecutionService::inject_reserved_params(user_params.clone(), &reserved, None);

        assert_eq!(merged, user_params);
    }

    #[test]
    fn test_filter_schema_for_llm() {
        let service = ToolExecutionService::new();

        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "agent_id": { "type": "string" },
                "session_id": { "type": "string" }
            },
            "required": ["query", "agent_id"]
        });

        let reserved = ReservedParamsConfig::new()
            .with_runtime("agent_id", "agent_id")
            .with_runtime("session_id", "session_id");

        let filtered = service.filter_schema_for_llm(&schema, &reserved);

        let props = filtered["properties"].as_object().unwrap();
        assert!(props.contains_key("query"));
        assert!(!props.contains_key("agent_id"));
        assert!(!props.contains_key("session_id"));

        // Check required array is also filtered
        let required = filtered["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
        assert!(!required.contains(&json!("agent_id")));
    }

    #[test]
    fn test_config_builder() {
        let config = ToolExecutionConfig::with_schema(json!({"type": "object"}))
            .with_reserved_params(ReservedParamsConfig::new().with_static("key", "value"));

        assert!(config.full_schema.is_object());
        assert_eq!(config.reserved_params.len(), 1);
    }
}
