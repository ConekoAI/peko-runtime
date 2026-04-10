//! Universal Tool Adapter
//!
//! SRP: This module adapts external tools to the Tool trait.
//! Handles: transport management, parameter injection, protocol translation.

use super::manifest::{merge_with_injection, Manifest};
use super::protocol::{DescribeResult, ExecuteParams, ExecuteResult, Request, Response, ResponseResult, ExecutionContext};
use super::transport::Transport;
use crate::tools::{Tool, ToolContext};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Adapter that wraps a universal tool for use in Pekobot
pub struct UniversalToolAdapter {
    name: String,
    manifest: Manifest,
    executable: PathBuf,
    // Transport is recreated per execution (process-per-call)
    // For long-lived tools, this could be a persistent connection
}

impl UniversalToolAdapter {
    /// Create a new adapter from manifest
    pub async fn from_manifest(
        manifest_path: impl AsRef<Path>,
        executable: impl AsRef<Path>,
    ) -> Result<Self> {
        let manifest = Manifest::from_file(&manifest_path).await?;
        let executable = executable.as_ref().to_path_buf();

        // Verify executable exists
        if !executable.exists() {
            return Err(anyhow::anyhow!(
                "Executable not found: {:?}",
                executable
            ));
        }

        Ok(Self {
            name: manifest.name.clone(),
            manifest,
            executable,
        })
    }

    /// Create from manifest without separate file (embedded)
    pub fn from_manifest_embedded(manifest: Manifest, executable: impl AsRef<Path>) -> Self {
        Self {
            name: manifest.name.clone(),
            manifest,
            executable: executable.as_ref().to_path_buf(),
        }
    }

    /// Get the underlying manifest
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Execute with injection (internal)
    async fn execute_with_injection(
        &self,
        params: serde_json::Value,
        context: ExecutionContext,
    ) -> Result<ExecuteResult> {
        // Validate user params (ensure no reserved params leaked in)
        self.manifest.validate_params(&params)?;

        // Merge with injection
        let merged = merge_with_injection(&self.manifest, params, &context)?;
        tracing::debug!(
            "UniversalToolAdapter - merged params: {}",
            serde_json::to_string(&merged).unwrap_or_default()
        );

        // Spawn transport and ensure cleanup
        let mut transport: Transport = Transport::spawn(&self.executable).await?;

        // Build execute request
        let exec_params = ExecuteParams {
            tool: self.name.clone(),
            args: merged,
            context,
        };

        let request = Request::new("tool/execute", serde_json::to_value(exec_params)?);

        // Send request and get response
        let result = self.execute_with_transport(&mut transport, request).await;

        // Ensure transport is shut down cleanly (with timeout for zombie prevention)
        if let Err(e) = transport.shutdown().await {
            tracing::warn!("Transport shutdown error (non-fatal): {}", e);
        }

        result
    }

    /// Execute with already-merged parameters (no validation/injection)
    ///
    /// This is used by the Extension Framework when ToolExecutionService
    /// has already handled parameter injection.
    pub async fn execute_raw(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Build execution context (minimal - params already merged by Extension Framework)
        let context = ExecutionContext {
            session_id: "unknown".to_string(),
            agent_id: "unknown".to_string(),
            peer_id: None,
            workspace: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            run_id: None,
        };

        // Execute directly without validation/injection (already done by Extension Framework)
        let mut transport: Transport = Transport::spawn(&self.executable).await?;

        let exec_params = ExecuteParams {
            tool: self.name.clone(),
            args: params,
            context,
        };

        let request = Request::new("tool/execute", serde_json::to_value(exec_params)?);
        let result = self.execute_with_transport(&mut transport, request).await;

        // Cleanup
        if let Err(e) = transport.shutdown().await {
            tracing::warn!("Transport shutdown error (non-fatal): {}", e);
        }

        // Convert to Value
        match result {
            Ok(exec_result) => {
                if exec_result.success {
                    Ok(exec_result.data.unwrap_or(serde_json::Value::Null))
                } else {
                    Err(anyhow::anyhow!(
                        exec_result.error.unwrap_or_else(|| "Unknown error".to_string())
                    ))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Execute request with transport (separated for cleanup handling)
    async fn execute_with_transport(
        &self,
        transport: &mut Transport,
        request: Request,
    ) -> Result<ExecuteResult> {
        // Send request and get response
        let response = transport.request(&request, 30).await?;

        // Parse result
        match response.result {
            ResponseResult::Result(value) => {
                // Try to parse as ExecuteResult first (standard format)
                match serde_json::from_value::<ExecuteResult>(value.clone()) {
                    Ok(result) => {
                        // If success but no data, the tool might be returning the result directly
                        // in the same structure (e.g., {"success": true, "field": value})
                        if result.success && result.data.is_none() {
                            tracing::debug!("ExecuteResult has no data, treating original as data payload");
                            Ok(ExecuteResult {
                                success: true,
                                data: Some(value),
                                error: None,
                                metadata: result.metadata,
                            })
                        } else {
                            Ok(result)
                        }
                    }
                    Err(_) => {
                        // If that fails, treat the entire value as the data payload
                        // This handles tools that return their result directly
                        tracing::debug!("Response not in ExecuteResult format, treating as raw data");
                        Ok(ExecuteResult {
                            success: true,
                            data: Some(value),
                            error: None,
                            metadata: None,
                        })
                    }
                }
            }
            ResponseResult::Error(err) => {
                Err(anyhow::anyhow!("Tool error ({}): {}", err.code, err.message))
            }
        }
    }
}

#[async_trait]
impl Tool for UniversalToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn llm_description(&self) -> String {
        self.manifest.llm_description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.manifest.exposed_parameters().clone()
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // Default context for simple execution (no reserved param injection)
        let context = ExecutionContext {
            session_id: "unknown".to_string(),
            agent_id: "unknown".to_string(),
            peer_id: None,
            workspace: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string()),
            run_id: None,
        };

        let result = self.execute_with_injection(params, context).await?;

        if result.success {
            Ok(result.data.unwrap_or(serde_json::Value::Null))
        } else {
            Err(anyhow::anyhow!(
                result.error.unwrap_or_else(|| "Unknown error".to_string())
            ))
        }
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // Build execution context from ToolContext - use identity fields for reserved param injection
        tracing::info!(
            "UniversalToolAdapter::execute_with_context - ToolContext: agent_id={:?}, session_id={:?}, workspace={:?}",
            ctx.agent_id, ctx.session_id, ctx.workspace
        );
        let exec_context = ExecutionContext {
            session_id: ctx.session_id.clone().unwrap_or_else(|| ctx.run_id.clone()),
            agent_id: ctx.agent_id.clone().unwrap_or_else(|| "unknown".to_string()),
            peer_id: ctx.peer_id.clone(),
            workspace: ctx.workspace.clone().unwrap_or_else(|| ".".to_string()),
            run_id: Some(ctx.run_id.clone()),
        };
        tracing::debug!(
            "UniversalToolAdapter - ExecutionContext: agent_id={}, session_id={}",
            exec_context.agent_id, exec_context.session_id
        );

        let result = self.execute_with_injection(params, exec_context).await?;

        if result.success {
            Ok(result.data.unwrap_or(serde_json::Value::Null))
        } else {
            Err(anyhow::anyhow!(
                result.error.unwrap_or_else(|| "Unknown error".to_string())
            ))
        }
    }
}

/// Builder for creating adapters with custom configuration
pub struct UniversalToolBuilder {
    manifest: Option<Manifest>,
    executable: Option<PathBuf>,
    timeout: Duration,
}

impl UniversalToolBuilder {
    pub fn new() -> Self {
        Self {
            manifest: None,
            executable: None,
            timeout: Duration::from_secs(30),
        }
    }

    pub fn manifest(mut self, manifest: Manifest) -> Self {
        self.manifest = Some(manifest);
        self
    }

    pub fn manifest_file(mut self, path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let manifest = Manifest::from_file_sync(path)?;
        self.manifest = Some(manifest);
        Ok(self)
    }

    pub fn executable(mut self, path: impl AsRef<Path>) -> Self {
        self.executable = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self
    }

    pub fn build(self) -> anyhow::Result<UniversalToolAdapter> {
        let manifest = self.manifest.context("Manifest required")?;
        let executable = self.executable.context("Executable path required")?;

        Ok(UniversalToolAdapter {
            name: manifest.name.clone(),
            manifest,
            executable,
        })
    }
}

impl Default for UniversalToolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_builder_pattern() {
        let manifest = Manifest {
            name: "test".to_string(),
            description: "Test tool".to_string(),
            llm_description: None,
            parameters: json!({"type": "object"}),
            reserved_parameters: None,
            protocol: super::super::manifest::ProtocolConfig::default(),
            extra: std::collections::HashMap::new(),
        };

        let adapter = UniversalToolBuilder::new()
            .manifest(manifest)
            .executable("/bin/true")
            .build();

        assert!(adapter.is_ok());
        let adapter = adapter.unwrap();
        assert_eq!(adapter.name(), "test");
    }

    #[test]
    fn test_adapter_traits() {
        let manifest = Manifest {
            name: "query".to_string(),
            description: "Query tool".to_string(),
            llm_description: Some("Use for querying".to_string()),
            parameters: json!({
                "type": "object",
                "properties": {
                    "q": {"type": "string"}
                }
            }),
            reserved_parameters: None,
            protocol: super::super::manifest::ProtocolConfig::default(),
            extra: std::collections::HashMap::new(),
        };

        let adapter = UniversalToolAdapter::from_manifest_embedded(manifest, "/bin/true");

        assert_eq!(adapter.name(), "query");
        assert_eq!(adapter.description(), "Query tool");
        assert_eq!(adapter.llm_description(), "Use for querying");
        assert!(adapter.parameters()["properties"]["q"].is_object());
    }
}
