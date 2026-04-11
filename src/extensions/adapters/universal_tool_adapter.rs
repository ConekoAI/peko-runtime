//! Universal Tool Adapter for the Extension system
//!
//! This adapter integrates Universal Tools (external tools with manifest.json)
//! into the unified Extension Architecture.
//!
//! # Universal Tool Format
//!
//! Universal tools are external executables with a manifest.json:
//! ```json
//! {
//!   "name": "calculator",
//!   "description": "Perform calculations",
//!   "parameters": {
//!     "type": "object",
//!     "properties": {
//!       "expression": { "type": "string" }
//!     }
//!   }
//! }
//! ```
//!
//! # Hook Points
//!
//! Universal tools hook into:
//! - `ToolRegister` - Registers tools for native calling
//! - `PromptSystemSection { section: "tools" }` - Adds tool descriptions to prompt
//! - `ToolExecute { tool_name }` - Handles tool execution

use crate::extensions::adapters::{ExtensionTypeAdapter, ManifestFormat};
use crate::extensions::core::{
    HookBinding, HookContext, HookHandler, HookHandlerFactory, HookPoint,
    ToolExecutionConfig, // NEW
};
use crate::extensions::services::ReservedParamsConfig; // NEW
use crate::extensions::types::{
    AsyncReceipt, ExtensionId, ExtensionManifest, HookId, HookOutput, HookResult,
};
use crate::agent::async_tool_framework::AsyncTaskStatus;
use crate::tools::Tool;
use uuid::Uuid;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, error, info, trace, warn};

/// Universal tool extension type identifier
pub const UNIVERSAL_TOOL_EXTENSION_TYPE: &str = "universal-tool";

/// Default priority for universal tool hooks
pub const UNIVERSAL_TOOL_HOOK_PRIORITY: i32 = 75;

/// Universal tool adapter for Extension system
#[derive(Debug)]
pub struct UniversalToolAdapter;

impl UniversalToolAdapter {
    /// Create a new universal tool adapter
    pub fn new() -> Self {
        Self
    }

    /// Discover universal tools from a directory
    ///
    /// REFACTORED: Previously called deprecated discover_universal_tools().
    /// Now implements its own directory scanning logic.
    pub async fn discover_tools(&self, path: &Path) -> Vec<DiscoveredUniversalTool> {
        let mut tools = Vec::new();

        if !path.exists() {
            debug!("Tools directory does not exist: {:?}", path);
            return tools;
        }

        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read tools directory {:?}: {}", path, e);
                return tools;
            }
        };

        // Scan directory for tool subdirectories
        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let tool_path = entry.path();

            // Skip non-directories and hidden
            if !tool_path.is_dir() {
                continue;
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }

            // Look for manifest.json
            let manifest_path = tool_path.join("manifest.json");
            if !manifest_path.exists() {
                trace!("No manifest.json found in {}", tool_path.display());
                continue;
            }

            // Parse manifest to get tool info
            match self.parse_tool_manifest_with_discovery(&manifest_path).await {
                Ok((manifest, tool_name)) => {
                    // Find executable
                    if let Some(executable) = self.find_executable(&tool_path, &tool_name).await {
                        tools.push(DiscoveredUniversalTool {
                            manifest,
                            executable,
                            manifest_path,
                        });
                    }
                }
                Err(e) => {
                    warn!("Failed to parse manifest {:?}: {}", manifest_path, e);
                }
            }
        }

        tools
    }

    /// Parse manifest and extract tool name for discovery
    async fn parse_tool_manifest_with_discovery(
        &self,
        manifest_path: &Path,
    ) -> Result<(ExtensionManifest, String)> {
        let content = tokio::fs::read_to_string(manifest_path)
            .await
            .with_context(|| format!("Failed to read manifest {:?}", manifest_path))?;

        let tool_manifest: crate::tools::universal::Manifest = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse manifest {:?}", manifest_path))?;

        let mut manifest = ExtensionManifest::new(
            &tool_manifest.name,
            UNIVERSAL_TOOL_EXTENSION_TYPE,
            &tool_manifest.name,
            &tool_manifest.description,
            "1.0.0",
            manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        );

        // Store additional metadata
        manifest.set("parameters", tool_manifest.parameters.clone());

        if let Some(llm_desc) = tool_manifest.llm_description {
            manifest.set("llm_description", llm_desc);
        }

        // Store reserved parameters
        let reserved_config = &tool_manifest.reserved_parameters;
        if !reserved_config.is_empty() {
            manifest.set(
                "reserved_parameters",
                serde_json::to_value(&reserved_config).unwrap_or_default(),
            );
        }

        Ok((manifest, tool_manifest.name))
    }

    /// Find executable for a tool (delegates to shared utility)
    async fn find_executable(&self, tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        super::parsing::find_executable(tool_path, tool_name).await
    }

    /// Parse a manifest.json file into an extension manifest
    async fn parse_tool_manifest(
        &self,
        manifest_path: &Path,
        executable: &Path,
    ) -> Result<ExtensionManifest> {
        let content = tokio::fs::read_to_string(manifest_path)
            .await
            .with_context(|| format!("Failed to read manifest {:?}", manifest_path))?;

        let tool_manifest: crate::tools::universal::Manifest = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse manifest {:?}", manifest_path))?;

        let mut manifest = ExtensionManifest::new(
            &tool_manifest.name,
            UNIVERSAL_TOOL_EXTENSION_TYPE,
            &tool_manifest.name,
            &tool_manifest.description,
            "1.0.0", // Tools don't have explicit versioning in manifest
            manifest_path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        );

        // Store additional metadata
        manifest.set("executable", executable.to_string_lossy().to_string());
        manifest.set("manifest_path", manifest_path.to_string_lossy().to_string());
        manifest.set("parameters", tool_manifest.parameters.clone());
        
        if let Some(llm_desc) = tool_manifest.llm_description {
            manifest.set("llm_description", llm_desc);
        }
        
        // Store reserved parameters
        let reserved_config = &tool_manifest.reserved_parameters;
        if !reserved_config.is_empty() {
            manifest.set("reserved_parameters", serde_json::to_value(&reserved_config).unwrap_or_default());
        }

        Ok(manifest)
    }
}

impl Default for UniversalToolAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtensionTypeAdapter for UniversalToolAdapter {
    fn extension_type(&self) -> &'static str {
        UNIVERSAL_TOOL_EXTENSION_TYPE
    }

    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::Json {
            schema: "universal-tool".to_string(),
            file_name: "manifest.json",
        }
    }

    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            // Register the tool for native calling
            HookBinding::new(
                HookPoint::ToolRegister,
                Box::new(UniversalToolRegistrationFactory {
                    manifest: manifest.clone(),
                }),
            ),
            // Add to prompt tools section
            HookBinding::new(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: UNIVERSAL_TOOL_HOOK_PRIORITY,
                },
                Box::new(UniversalToolPromptFactory {
                    manifest: manifest.clone(),
                }),
            ),
            // Handle tool execution
            HookBinding::new(
                HookPoint::ToolExecute {
                    tool_name: manifest.name.clone(),
                },
                Box::new(UniversalToolExecuteFactory {
                    manifest: manifest.clone(),
                }),
            ),
            // Async tool execution
            HookBinding::new(
                HookPoint::ToolExecuteAsync {
                    tool_name: manifest.name.clone(),
                },
                Box::new(UniversalToolExecuteAsyncFactory {
                    manifest: manifest.clone(),
                }),
            ),
            // Check status for async tasks
            HookBinding::new(
                HookPoint::ToolCheckStatus {
                    tool_name: manifest.name.clone(),
                },
                Box::new(UniversalToolCheckStatusFactory {
                    manifest: manifest.clone(),
                }),
            ),
            // Cancel for async tasks
            HookBinding::new(
                HookPoint::ToolCancel {
                    tool_name: manifest.name.clone(),
                },
                Box::new(UniversalToolCancelFactory {
                    manifest: manifest.clone(),
                }),
            ),
        ]
    }
}

/// A discovered universal tool before registration
#[derive(Debug, Clone)]
pub struct DiscoveredUniversalTool {
    /// Extension manifest
    pub manifest: ExtensionManifest,
    /// Path to executable
    pub executable: PathBuf,
    /// Path to manifest
    pub manifest_path: PathBuf,
}

/// Factory for creating tool registration handlers
#[derive(Debug, Clone)]
struct UniversalToolRegistrationFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolRegistrationFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(UniversalToolRegistrationHandler {
            tool_name: self.manifest.name.clone(),
            description: self.manifest.description.clone(),
            parameters: self.manifest.get("parameters").cloned().unwrap_or_default(),
        })
    }
}

/// Handler that registers the tool
#[derive(Debug, Clone)]
struct UniversalToolRegistrationHandler {
    tool_name: String,
    description: String,
    parameters: serde_json::Value,
}

#[async_trait]
impl HookHandler for UniversalToolRegistrationHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Create a ToolDefinition for registration
        let tool_def = crate::providers::ToolDefinition {
            name: self.tool_name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        };

        debug!(tool_name = %self.tool_name, "Registering universal tool");
        HookResult::Continue(HookOutput::Tool(tool_def))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolRegister
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolRegistrationHandler({})", self.tool_name)
    }
}

/// Factory for creating tool prompt handlers
#[derive(Debug, Clone)]
struct UniversalToolPromptFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolPromptFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        let llm_desc = self
            .manifest
            .get("llm_description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.manifest.description.clone());

        Box::new(UniversalToolPromptHandler {
            tool_name: self.manifest.name.clone(),
            description: llm_desc,
        })
    }
}

/// Handler that injects tool description into prompt
#[derive(Debug, Clone)]
struct UniversalToolPromptHandler {
    tool_name: String,
    description: String,
}

#[async_trait]
impl HookHandler for UniversalToolPromptHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let text = format!("### {}\n\n{}", self.tool_name, self.description);
        HookResult::Continue(HookOutput::Text(text))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "tools".to_string(),
            priority: UNIVERSAL_TOOL_HOOK_PRIORITY,
        }
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolPromptHandler({})", self.tool_name)
    }
}

/// Factory for creating tool execution handlers
#[derive(Debug, Clone)]
struct UniversalToolExecuteFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolExecuteFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        let executable = self
            .manifest
            .get("executable")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();

        let manifest_path = self
            .manifest
            .get("manifest_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();
        
        // Extract reserved parameters from manifest
        let reserved_params = self
            .manifest
            .get("reserved_parameters")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        
        // Extract full parameter schema
        let full_schema = self
            .manifest
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));

        Box::new(UniversalToolExecuteHandler {
            tool_name: self.manifest.name.clone(),
            executable,
            manifest_path,
            reserved_params,
            full_schema,
        })
    }
}

/// Handler that executes the tool
#[derive(Debug, Clone)]
struct UniversalToolExecuteHandler {
    tool_name: String,
    executable: PathBuf,
    manifest_path: PathBuf,
    reserved_params: ReservedParamsConfig,
    full_schema: serde_json::Value,
}

#[async_trait]
impl HookHandler for UniversalToolExecuteHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Extract tool call parameters from context
        let params = match ctx.as_tool_call() {
            Some((tool_name, params)) => {
                if tool_name != self.tool_name {
                    return HookResult::PassThrough; // Not for this tool
                }
                params.clone()
            }
            None => {
                // Try to get from JSON input
                match ctx.as_json() {
                    Some(json) => json.clone(),
                    None => return HookResult::PassThrough,
                }
            }
        };

        debug!(
            tool_name = %self.tool_name,
            params = %serde_json::to_string(&params).unwrap_or_default(),
            "Executing universal tool via Extension Framework"
        );

        // Use the unified ToolExecutionService for parameter injection and execution
        let exec_service = ctx.services.tool_execution();
        let exec_config = ToolExecutionConfig::new(
            self.reserved_params.clone(),
            self.full_schema.clone(),
        );

        let result = exec_service
            .execute(
                params,
                &exec_config,
                ctx.as_tool_context(),
                |merged_params| async move {
                    // Create adapter and execute with merged params
                    let adapter = crate::tools::universal::UniversalToolAdapter::from_manifest(
                        &self.manifest_path,
                        &self.executable,
                    )
                    .await?;
                    
                    // Execute with the merged parameters (injection already done)
                    adapter.execute_raw(merged_params).await
                },
            )
            .await;

        match result {
            Ok(output) => HookResult::Continue(HookOutput::Json(output)),
            Err(e) => {
                error!(tool_name = %self.tool_name, error = %e, "Tool execution failed");
                HookResult::Error(e)
            }
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolExecuteHandler({})", self.tool_name)
    }
}

/// Factory for creating async tool execution handlers
#[derive(Debug, Clone)]
struct UniversalToolExecuteAsyncFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolExecuteAsyncFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        let executable = self
            .manifest
            .get("executable")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_default();

        Box::new(UniversalToolExecuteAsyncHandler {
            tool_name: self.manifest.name.clone(),
            executable,
        })
    }
}

/// Handler that executes tools asynchronously
#[derive(Debug, Clone)]
struct UniversalToolExecuteAsyncHandler {
    tool_name: String,
    executable: PathBuf,
}

#[async_trait]
impl HookHandler for UniversalToolExecuteAsyncHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_tool_call() {
            Some((tool_name, _)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        };

        debug!(
            tool_name = %self.tool_name,
            "Executing universal tool asynchronously"
        );

        // Generate task ID
        let task_id = format!("universal:{}:{}", self.tool_name, Uuid::new_v4());

        // Create receipt
        let receipt = AsyncReceipt {
            task_id: task_id.clone(),
            estimated_duration_secs: None,
            check_status_tool: self.tool_name.clone(),
            metadata: Some(serde_json::json!({
                "tool_name": self.tool_name,
                "executable": self.executable.to_string_lossy(),
            })),
        };

        HookResult::Continue(HookOutput::Receipt(receipt))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecuteAsync {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolExecuteAsyncHandler({})", self.tool_name)
    }
}

/// Factory for creating check status handlers
#[derive(Debug, Clone)]
struct UniversalToolCheckStatusFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolCheckStatusFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(UniversalToolCheckStatusHandler {
            tool_name: self.manifest.name.clone(),
        })
    }
}

/// Handler that checks status of async tasks
#[derive(Debug, Clone)]
struct UniversalToolCheckStatusHandler {
    tool_name: String,
}

#[async_trait]
impl HookHandler for UniversalToolCheckStatusHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_task_status() {
            Some((_, tool_name)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        };

        debug!(
            tool_name = %self.tool_name,
            "Checking universal tool task status"
        );

        // Universal tools don't have native async tracking
        HookResult::Continue(HookOutput::TaskStatus(AsyncTaskStatus::Pending))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCheckStatus {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolCheckStatusHandler({})", self.tool_name)
    }
}

/// Factory for creating cancel handlers
#[derive(Debug, Clone)]
struct UniversalToolCancelFactory {
    manifest: ExtensionManifest,
}

impl HookHandlerFactory for UniversalToolCancelFactory {
    fn create(&self, _manifest: ExtensionManifest) -> Box<dyn HookHandler> {
        Box::new(UniversalToolCancelHandler {
            tool_name: self.manifest.name.clone(),
        })
    }
}

/// Handler that cancels async tasks
#[derive(Debug, Clone)]
struct UniversalToolCancelHandler {
    tool_name: String,
}

#[async_trait]
impl HookHandler for UniversalToolCancelHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Validate this is the right tool
        match ctx.as_task_cancel() {
            Some((_, tool_name)) if tool_name != self.tool_name => {
                return HookResult::PassThrough;
            }
            None => return HookResult::PassThrough,
            _ => {}
        };

        debug!(
            tool_name = %self.tool_name,
            "Cancelling universal tool task"
        );

        // Universal tools don't have native cancel support
        HookResult::Continue(HookOutput::Bool(false))
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolCancel {
            tool_name: self.tool_name.clone(),
        }
    }

    fn priority(&self) -> i32 {
        UNIVERSAL_TOOL_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        format!("UniversalToolCancelHandler({})", self.tool_name)
    }
}

/// Helper to load tools from directory using the adapter
pub async fn load_tools_from_directory(path: &Path) -> Vec<DiscoveredUniversalTool> {
    let adapter = UniversalToolAdapter::new();
    adapter.discover_tools(path).await
}

/// Register universal tools with an ExtensionCore
pub async fn register_tools_with_core(
    core: &crate::extensions::ExtensionCore,
    tools: Vec<DiscoveredUniversalTool>,
) -> Result<Vec<HookId>> {
    let mut hook_ids = Vec::new();

    for tool in tools {
        let extension_id = ExtensionId::new(&tool.manifest.id.0);

        // Register tool registration handler
        let reg_handler = Arc::new(UniversalToolRegistrationHandler {
            tool_name: tool.manifest.name.clone(),
            description: tool.manifest.description.clone(),
            parameters: tool.manifest.get("parameters").cloned().unwrap_or_default(),
        });

        let reg = core
            .register_hook(HookPoint::ToolRegister, reg_handler, &extension_id)
            .await?;
        hook_ids.push(reg.id);

        // Register prompt handler
        let llm_desc = tool
            .manifest
            .get("llm_description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| tool.manifest.description.clone());

        let prompt_handler = Arc::new(UniversalToolPromptHandler {
            tool_name: tool.manifest.name.clone(),
            description: llm_desc,
        });

        let prompt_reg = core
            .register_hook(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: UNIVERSAL_TOOL_HOOK_PRIORITY,
                },
                prompt_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(prompt_reg.id);

        // Register execution handler
        // Extract reserved parameters from manifest
        let reserved_params = tool.manifest
            .get("reserved_parameters")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        
        let full_schema = tool.manifest
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        
        let exec_handler = Arc::new(UniversalToolExecuteHandler {
            tool_name: tool.manifest.name.clone(),
            executable: tool.executable.clone(),
            manifest_path: tool.manifest_path.clone(),
            reserved_params,
            full_schema,
        });

        let exec_reg = core
            .register_hook(
                HookPoint::ToolExecute {
                    tool_name: tool.manifest.name.clone(),
                },
                exec_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(exec_reg.id);

        // Register async execution handler
        let exec_async_handler = Arc::new(UniversalToolExecuteAsyncHandler {
            tool_name: tool.manifest.name.clone(),
            executable: tool.executable.clone(),
        });

        let exec_async_reg = core
            .register_hook(
                HookPoint::ToolExecuteAsync {
                    tool_name: tool.manifest.name.clone(),
                },
                exec_async_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(exec_async_reg.id);

        // Register check status handler
        let check_status_handler = Arc::new(UniversalToolCheckStatusHandler {
            tool_name: tool.manifest.name.clone(),
        });

        let check_status_reg = core
            .register_hook(
                HookPoint::ToolCheckStatus {
                    tool_name: tool.manifest.name.clone(),
                },
                check_status_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(check_status_reg.id);

        // Register cancel handler
        let cancel_handler = Arc::new(UniversalToolCancelHandler {
            tool_name: tool.manifest.name.clone(),
        });

        let cancel_reg = core
            .register_hook(
                HookPoint::ToolCancel {
                    tool_name: tool.manifest.name.clone(),
                },
                cancel_handler,
                &extension_id,
            )
            .await?;
        hook_ids.push(cancel_reg.id);

        info!(
            tool_name = %tool.manifest.name,
            hook_count = 6,
            "Registered universal tool with ExtensionCore (including async hooks)"
        );
    }

    Ok(hook_ids)
}

/// Convenience function to load and register universal tools
pub async fn load_and_register_tools(
    core: &crate::extensions::ExtensionCore,
    tools_dir: impl AsRef<Path>,
) -> Result<usize> {
    let tools = load_tools_from_directory(tools_dir.as_ref()).await;
    let hook_ids = register_tools_with_core(core, tools).await?;
    Ok(hook_ids.len() / 6) // Each tool registers 6 hooks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::{ExtensionServices, HookInput};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn create_test_tool(dir: &Path, name: &str, description: &str) -> PathBuf {
        let tool_dir = dir.join(name);
        std::fs::create_dir(&tool_dir).unwrap();

        let manifest = serde_json::json!({
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            }
        });

        let manifest_path = tool_dir.join("manifest.json");
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();

        // Create a dummy executable (script)
        let script_path = tool_dir.join(format!("{}.py", name));
        std::fs::write(&script_path, "#!/usr/bin/env python3\nprint('{}')").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }

        tool_dir
    }

    #[test]
    fn test_universal_tool_adapter_manifest_format() {
        let adapter = UniversalToolAdapter::new();
        let format = adapter.manifest_format();

        assert!(matches!(format, ManifestFormat::Json { .. }));
    }

    #[tokio::test]
    async fn test_discover_tools() {
        let temp = TempDir::new().unwrap();

        create_test_tool(temp.path(), "tool1", "First tool");
        create_test_tool(temp.path(), "tool2", "Second tool");

        let adapter = UniversalToolAdapter::new();
        let tools = adapter.discover_tools(temp.path()).await;

        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t.manifest.name == "tool1"));
        assert!(tools.iter().any(|t| t.manifest.name == "tool2"));
    }

    #[tokio::test]
    async fn test_parse_tool_manifest() {
        let temp = TempDir::new().unwrap();
        let tool_dir = create_test_tool(temp.path(), "calculator", "Calculate things");

        let adapter = UniversalToolAdapter::new();
        let manifest_path = tool_dir.join("manifest.json");
        let executable = tool_dir.join("calculator.py");

        let manifest = adapter
            .parse_tool_manifest(&manifest_path, &executable)
            .await
            .unwrap();

        assert_eq!(manifest.name, "calculator");
        assert_eq!(manifest.description, "Calculate things");
        assert_eq!(manifest.extension_type, "universal-tool");
    }

    #[tokio::test]
    async fn test_tool_registration_handler() {
        let handler = UniversalToolRegistrationHandler {
            tool_name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        };

        let ctx = HookContext::new(
            HookPoint::ToolRegister,
            HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;

        match result {
            HookResult::Continue(HookOutput::Tool(tool_def)) => {
                assert_eq!(tool_def.name, "test_tool");
                assert_eq!(tool_def.description, "A test tool");
            }
            _ => panic!("Expected Continue with Tool, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_tool_prompt_handler() {
        let handler = UniversalToolPromptHandler {
            tool_name: "test_tool".to_string(),
            description: "Does something useful".to_string(),
        };

        let ctx = HookContext::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            HookInput::Unit,
            Arc::new(ExtensionServices::new()),
        );

        let result = handler.handle(ctx).await;

        match result {
            HookResult::Continue(HookOutput::Text(text)) => {
                assert!(text.contains("### test_tool"));
                assert!(text.contains("Does something useful"));
            }
            _ => panic!("Expected Continue with Text, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_register_tools_with_core() {
        let temp = TempDir::new().unwrap();
        create_test_tool(temp.path(), "tool1", "First tool");
        create_test_tool(temp.path(), "tool2", "Second tool");

        let core = crate::extensions::ExtensionCore::new();
        let tools = load_tools_from_directory(temp.path()).await;

        assert_eq!(tools.len(), 2);

        let hook_ids = register_tools_with_core(&core, tools).await.unwrap();

        // Each tool registers 6 hooks (register, prompt, execute, async, status, cancel)
        assert_eq!(hook_ids.len(), 12);
        assert_eq!(core.hook_count().await, 12);
    }
}
