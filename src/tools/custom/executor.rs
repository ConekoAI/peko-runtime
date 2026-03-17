//! Custom tool executor
//!
//! Implements the `Tool` trait for custom tools discovered from the `tools/` directory.
//! Handles execution via the JSON stdin/stdout protocol.

use crate::tools::custom::manifest::ToolManifest;
use crate::tools::custom::protocol::{execute_tool, ExecutionContext, ToolRequest};
use crate::tools::{Tool, ToolContext, ToolError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;
use tracing::{instrument, trace, warn};

/// A custom tool discovered from the `tools/` directory
pub struct CustomTool {
    /// Tool name (normalized)
    name: String,
    /// Path to the executable
    executable_path: PathBuf,
    /// Tool manifest (may be auto-generated if no sidecar exists)
    manifest: ToolManifest,
    /// Estimated execution duration in milliseconds
    estimated_duration_ms: u64,
}

impl CustomTool {
    /// Create a new custom tool
    ///
    /// # Arguments
    /// * `name` - Tool name (normalized, lowercase with underscores)
    /// * `executable_path` - Path to the executable
    /// * `manifest` - Optional manifest (will generate minimal one if None)
    pub fn new(
        name: impl Into<String>,
        executable_path: impl Into<PathBuf>,
        manifest: Option<ToolManifest>,
    ) -> Self {
        let name = name.into();
        let executable_path = executable_path.into();
        let manifest = manifest
            .unwrap_or_else(|| crate::tools::custom::manifest::generate_minimal_manifest(&name));

        // Estimate duration based on name heuristics
        let estimated_duration = estimate_tool_duration(&name);

        Self {
            name,
            executable_path,
            manifest,
            estimated_duration_ms: estimated_duration,
        }
    }

    /// Get the executable path
    pub fn executable_path(&self) -> &PathBuf {
        &self.executable_path
    }

    /// Get the manifest
    pub fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    /// Validate parameters against the manifest schema
    pub fn validate_params(&self, params: &serde_json::Value) -> anyhow::Result<()> {
        self.manifest.validate_params(params)
    }
}

#[async_trait]
impl Tool for CustomTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        // The manifest stores the description, return a static reference to it
        // We use a default description since the manifest may not have one
        static DEFAULT_DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();

        if let Some(desc) = &self.manifest.description {
            // Leak to get a 'static reference (acceptable for tool descriptions)
            Box::leak(desc.clone().into_boxed_str())
        } else {
            DEFAULT_DESC.get_or_init(|| format!("Custom tool: {}", self.name))
        }
    }

    fn llm_description(&self) -> String {
        let base_desc = self
            .manifest
            .description_or(&format!("Custom tool: {}", self.name));
        format!("{} (custom tool)", base_desc)
    }

    fn parameters(&self) -> serde_json::Value {
        self.manifest.parameters()
    }

    #[instrument(skip(self, params), fields(tool_name = %self.name))]
    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        trace!(
            "Executing custom tool '{}' with params: {:?}",
            self.name,
            params
        );

        // Validate parameters
        if let Err(e) = self.validate_params(&params) {
            warn!("Parameter validation failed for '{}': {}", self.name, e);
            return Err(e);
        }

        // Build execution context
        // Note: In a real scenario, we'd get these from the runtime context
        // For now, we use placeholder values that can be overridden
        let context = ExecutionContext {
            instance_id: std::env::var("PEKOBOT_INSTANCE_ID")
                .unwrap_or_else(|_| "unknown".to_string()),
            session_id: std::env::var("PEKOBOT_SESSION_ID")
                .unwrap_or_else(|_| "unknown".to_string()),
            workspace: std::env::var("PEKOBOT_WORKSPACE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        };

        // Build request
        let tool_call_id = format!("tc_custom_{}", generate_short_id());
        let request = ToolRequest {
            tool_call_id: tool_call_id.clone(),
            tool: self.name.clone(),
            args: params,
            timeout_ms: 30000, // 30 second default
            context,
        };

        // Execute with timeout
        let timeout = Duration::from_millis(request.timeout_ms);
        let response = execute_tool(&self.executable_path, &request, timeout).await?;

        // Convert response to JSON value
        if let Some(error) = response.error {
            // Tool returned an error
            Err(anyhow::anyhow!("Tool '{}' failed: {}", self.name, error))
        } else if let Some(output) = response.output {
            // Try to parse output as JSON, otherwise wrap as string
            match serde_json::from_str(&output) {
                Ok(json) => Ok(json),
                Err(_) => Ok(serde_json::json!({
                    "success": true,
                    "output": output
                })),
            }
        } else {
            // No output and no error
            Ok(serde_json::json!({
                "success": true,
                "output": null
            }))
        }
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        use std::time::Instant;

        // Check abort before starting
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout before starting
        let start_time = Instant::now();
        ctx.check_timeout(start_time)?;

        // Report start status
        ctx.report_status(format!("Starting custom tool: {}", self.name))
            .await;

        // Execute the tool
        let result = self.execute(params).await;

        // Check abort after completion
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // Check timeout after completion
        ctx.check_timeout(start_time)?;

        // Report completion status
        match &result {
            Ok(_) => {
                ctx.report_status(format!("Completed custom tool: {}", self.name))
                    .await;
            }
            Err(e) => {
                ctx.report_status(format!("Failed custom tool '{}': {}", self.name, e))
                    .await;
            }
        }

        result
    }

    fn supports_progress(&self) -> bool {
        // Custom tools don't currently support progress callbacks
        false
    }

    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        self.estimated_duration_ms
    }
}

impl std::fmt::Debug for CustomTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomTool")
            .field("name", &self.name)
            .field("executable_path", &self.executable_path)
            .field("has_manifest", &!self.manifest.name.is_none())
            .finish()
    }
}

/// Generate a short random ID for tool calls
fn generate_short_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let mut hasher = DefaultHasher::new();
    timestamp.hash(&mut hasher);
    let hash = hasher.finish();

    // Convert to base36-like string (6 chars)
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut result = String::with_capacity(6);
    let mut value = hash;
    for _ in 0..6 {
        result.push(CHARSET[(value % 36) as usize] as char);
        value /= 36;
    }
    result
}

/// Estimate tool duration based on name heuristics
fn estimate_tool_duration(name: &str) -> u64 {
    let name_lower = name.to_lowercase();

    // Fast operations (milliseconds)
    if name_lower.contains("read")
        || name_lower.contains("get")
        || name_lower.contains("list")
        || name_lower.contains("search")
        || name_lower.contains("find")
    {
        return 500; // 500ms
    }

    // Medium operations (seconds)
    if name_lower.contains("write")
        || name_lower.contains("create")
        || name_lower.contains("update")
        || name_lower.contains("delete")
    {
        return 2000; // 2s
    }

    // Slow operations (network/external calls)
    if name_lower.contains("fetch")
        || name_lower.contains("download")
        || name_lower.contains("upload")
        || name_lower.contains("http")
        || name_lower.contains("request")
        || name_lower.contains("browser")
    {
        return 5000; // 5s
    }

    // Very slow operations
    if name_lower.contains("build")
        || name_lower.contains("compile")
        || name_lower.contains("test")
        || name_lower.contains("run")
    {
        return 30000; // 30s
    }

    // Default
    1000 // 1s
}

/// Create CustomTool instances from discovered tools
pub async fn create_custom_tools(
    tools_dir: impl AsRef<std::path::Path>,
) -> anyhow::Result<Vec<CustomTool>> {
    use crate::tools::custom::discovery::discover_tools;

    let discovered = discover_tools(tools_dir).await?;
    let mut tools = Vec::with_capacity(discovered.len());

    for (name, info) in discovered {
        // Load manifest if exists
        let manifest = if let Some(manifest_path) = info.manifest_path {
            match ToolManifest::from_file(&manifest_path).await {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!("Failed to load manifest for '{}': {}", name, e);
                    None
                }
            }
        } else {
            None
        };

        tools.push(CustomTool::new(name, info.executable_path, manifest));
    }

    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_custom_tool_creation() {
        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("my_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("my_tool", &tool_path, None);

        assert_eq!(tool.name(), "my_tool");
        assert!(tool.description().contains("my_tool"));
        assert_eq!(tool.executable_path(), &tool_path);
    }

    #[test]
    fn test_custom_tool_with_manifest() {
        let manifest = ToolManifest {
            name: Some("custom_tool".to_string()),
            description: Some("A custom tool".to_string()),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "arg1": { "type": "string" }
                }
            }),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("custom_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("custom_tool", &tool_path, Some(manifest));

        assert_eq!(tool.name(), "custom_tool");
        assert_eq!(tool.description(), "A custom tool");

        let params = tool.parameters();
        assert!(params.get("properties").is_some());
    }

    #[test]
    fn test_estimate_tool_duration() {
        assert_eq!(estimate_tool_duration("read_file"), 500);
        assert_eq!(estimate_tool_duration("search_code"), 500);
        assert_eq!(estimate_tool_duration("write_data"), 2000);
        assert_eq!(estimate_tool_duration("fetch_url"), 5000);
        assert_eq!(estimate_tool_duration("build_project"), 30000);
        assert_eq!(estimate_tool_duration("unknown"), 1000);
    }

    #[test]
    fn test_generate_short_id() {
        let id1 = generate_short_id();
        let id2 = generate_short_id();

        assert_eq!(id1.len(), 6);
        assert_eq!(id2.len(), 6);
        // IDs should be alphanumeric
        assert!(id1.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(id2.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_custom_tool_llm_description() {
        let manifest = ToolManifest {
            name: Some("test_tool".to_string()),
            description: Some("Test description".to_string()),
            parameters: serde_json::json!({}),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("test_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("test_tool", &tool_path, Some(manifest));

        let llm_desc = tool.llm_description();
        assert!(llm_desc.contains("Test description"));
        assert!(llm_desc.contains("custom tool"));
    }

    #[test]
    fn test_custom_tool_parameters() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 }
                },
                "required": ["query"]
            }),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("param_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("param_tool", &tool_path, Some(manifest));

        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["query"].is_object());
        assert!(params["properties"]["limit"].is_object());
    }

    #[test]
    fn test_custom_tool_validate_params_valid() {
        let manifest = ToolManifest {
            name: None,
            description: None,
            parameters: serde_json::json!({
                "type": "object",
                "required": ["query"]
            }),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("validate_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("validate_tool", &tool_path, Some(manifest));

        let valid_params = serde_json::json!({"query": "test"});
        assert!(tool.validate_params(&valid_params).is_ok());

        let invalid_params = serde_json::json!({});
        assert!(tool.validate_params(&invalid_params).is_err());
    }

    #[test]
    fn test_custom_tool_estimated_duration() {
        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("duration_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("fast_read_tool", &tool_path, None);
        assert_eq!(tool.estimated_duration_ms(&serde_json::json!({})), 500);

        let tool2 = CustomTool::new("slow_build_tool", &tool_path, None);
        assert_eq!(tool2.estimated_duration_ms(&serde_json::json!({})), 30000);

        let tool3 = CustomTool::new("unknown_operation", &tool_path, None);
        assert_eq!(tool3.estimated_duration_ms(&serde_json::json!({})), 1000);
    }

    #[test]
    fn test_custom_tool_debug() {
        let manifest = ToolManifest {
            name: Some("debug_tool".to_string()),
            description: None,
            parameters: serde_json::json!({}),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("debug_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("debug_tool", &tool_path, Some(manifest));

        let debug_str = format!("{:?}", tool);
        assert!(debug_str.contains("CustomTool"));
        assert!(debug_str.contains("debug_tool"));
    }

    #[test]
    fn test_custom_tool_executable_path() {
        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("path_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("path_tool", &tool_path, None);
        assert_eq!(tool.executable_path(), &tool_path);
    }

    #[test]
    fn test_custom_tool_manifest_accessor() {
        let manifest = ToolManifest {
            name: Some("accessor_test".to_string()),
            description: Some("Test".to_string()),
            parameters: serde_json::json!({}),
            extra: std::collections::HashMap::new(),
        };

        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("accessor_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("accessor_test", &tool_path, Some(manifest));

        assert_eq!(tool.manifest().name, Some("accessor_test".to_string()));
    }

    #[test]
    fn test_custom_tool_without_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let tool_path = temp_dir.path().join("no_manifest_tool");
        std::fs::write(&tool_path, "#!/bin/sh\necho test").unwrap();

        let tool = CustomTool::new("no_manifest_tool", &tool_path, None);

        // Should have auto-generated description
        assert!(tool.description().contains("no_manifest_tool"));

        // Should have minimal parameters schema
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
    }

    #[tokio::test]
    async fn test_create_custom_tools_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("empty_tools");
        tokio::fs::create_dir(&tools_dir).await.unwrap();

        let tools = create_custom_tools(&tools_dir).await.unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_create_custom_tools_missing_dir() {
        let temp_dir = TempDir::new().unwrap();
        let tools_dir = temp_dir.path().join("nonexistent");

        // Should return error or empty
        let result = create_custom_tools(&tools_dir).await;
        assert!(result.is_ok()); // Returns empty, not error
    }

    #[test]
    fn test_estimate_tool_duration_variations() {
        // Test all duration categories
        assert_eq!(estimate_tool_duration("read"), 500);
        assert_eq!(estimate_tool_duration("get"), 500);
        assert_eq!(estimate_tool_duration("list"), 500);
        assert_eq!(estimate_tool_duration("search"), 500);
        assert_eq!(estimate_tool_duration("find"), 500);

        assert_eq!(estimate_tool_duration("write"), 2000);
        assert_eq!(estimate_tool_duration("create"), 2000);
        assert_eq!(estimate_tool_duration("update"), 2000);
        assert_eq!(estimate_tool_duration("delete"), 2000);

        assert_eq!(estimate_tool_duration("fetch"), 5000);
        assert_eq!(estimate_tool_duration("download"), 5000);
        assert_eq!(estimate_tool_duration("upload"), 5000);
        assert_eq!(estimate_tool_duration("browser"), 5000);
        assert_eq!(estimate_tool_duration("http"), 5000);

        assert_eq!(estimate_tool_duration("build"), 30000);
        assert_eq!(estimate_tool_duration("compile"), 30000);
        assert_eq!(estimate_tool_duration("test"), 30000);
        assert_eq!(estimate_tool_duration("run"), 30000);
    }
}
