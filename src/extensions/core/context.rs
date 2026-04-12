//! Hook context and handler trait definitions
//!
//! This module defines the context passed to hook handlers and the trait
//! that all hook handlers must implement.

use crate::extensions::core::hook_points::HookPoint;
use crate::extensions::types::{HookId, HookInput, HookOutput, HookPriority, HookResult};
use async_trait::async_trait;
use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Context passed to hook handlers during invocation
///
/// This provides handlers with information about the hook point being invoked,
/// the input data, mutable state, and access to extension services.
#[derive(Debug)]
pub struct HookContext {
    /// The hook point being invoked
    pub point: HookPoint,
    
    /// Unique invocation ID for tracing
    pub invocation_id: String,
    
    /// Input data (type depends on hook point)
    pub input: HookInput,
    
    /// Mutable state for this invocation
    pub state: HookState,
    
    /// Extension services access
    pub services: Arc<ExtensionServices>,
}

impl HookContext {
    /// Create a new hook context
    pub fn new(
        point: HookPoint,
        input: HookInput,
        services: Arc<ExtensionServices>,
    ) -> Self {
        Self {
            invocation_id: format!("hook_{}", uuid::Uuid::new_v4()),
            point,
            input,
            state: HookState::new(),
            services,
        }
    }
    
    /// Get the hook point category
    pub fn category(&self) -> &'static str {
        self.point.category()
    }
    
    /// Get the hook point name
    pub fn name(&self) -> String {
        self.point.name()
    }
    
    /// Check if input is of a specific type
    pub fn is_input<T: 'static>(&self) -> bool {
        matches!(&self.input, HookInput::Json(_) if std::any::TypeId::of::<T>() == std::any::TypeId::of::<serde_json::Value>())
    }
    
    /// Get input as prompt build state if applicable
    pub fn as_prompt_build(&self) -> Option<&crate::extensions::types::PromptBuildState> {
        match &self.input {
            HookInput::PromptBuild(state) => Some(state),
            _ => None,
        }
    }
    
    /// Get input as tool call if applicable
    pub fn as_tool_call(&self) -> Option<(&str, &serde_json::Value)> {
        match &self.input {
            HookInput::ToolCall { tool_name, params } => Some((tool_name, params)),
            _ => None,
        }
    }
    
    /// Get input as system event if applicable
    pub fn as_system_event(&self) -> Option<&crate::hooks::SystemEvent> {
        match &self.input {
            HookInput::SystemEvent(event) => Some(event),
            _ => None,
        }
    }
    
    /// Get input as session state if applicable
    pub fn as_session_state(&self) -> Option<&crate::extensions::types::SessionSnapshot> {
        match &self.input {
            HookInput::SessionState(state) => Some(state),
            _ => None,
        }
    }
    
    /// Get input as message if applicable
    pub fn as_message(&self) -> Option<&crate::extensions::types::MessageEnvelope> {
        match &self.input {
            HookInput::Message(msg) => Some(msg),
            _ => None,
        }
    }
    
    /// Get input as JSON if applicable
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match &self.input {
            HookInput::Json(v) => Some(v),
            _ => None,
        }
    }
    
    /// Get the raw input
    pub fn input(&self) -> &HookInput {
        &self.input
    }
    
    /// Get input as task status check if applicable
    pub fn as_task_status(&self) -> Option<(&str, &str)> {
        match &self.input {
            HookInput::TaskStatus { task_id, tool_name } => Some((task_id, tool_name)),
            _ => None,
        }
    }
    
    /// Get input as task cancel request if applicable
    pub fn as_task_cancel(&self) -> Option<(&str, &str)> {
        match &self.input {
            HookInput::TaskCancel { task_id, tool_name } => Some((task_id, tool_name)),
            _ => None,
        }
    }
    
    /// Get tool context from state if available
    ///
    /// This is used for runtime parameter resolution during tool execution.
    pub fn as_tool_context(&self) -> Option<&crate::tools::ToolContext> {
        self.state.get::<crate::tools::ToolContext>("tool_context")
    }
}

/// Mutable state for a hook invocation
///
/// This allows handlers to store temporary data during the invocation
/// that can be accessed by subsequent handlers.
#[derive(Debug, Default)]
pub struct HookState {
    /// Internal state storage
    data: HashMap<String, Box<dyn Any + Send + Sync>>,
}

impl HookState {
    /// Create new empty state
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }
    
    /// Insert a value into state
    pub fn insert<T: Any + Send + Sync>(&mut self, key: impl Into<String>, value: T) {
        self.data.insert(key.into(), Box::new(value));
    }
    
    /// Get a value from state
    pub fn get<T: Any + Send + Sync>(&self, key: &str) -> Option<&T> {
        self.data.get(key).and_then(|v| v.downcast_ref::<T>())
    }
    
    /// Get a value from state (mutable)
    pub fn get_mut<T: Any + Send + Sync>(&mut self, key: &str) -> Option<&mut T> {
        self.data.get_mut(key).and_then(|v| v.downcast_mut::<T>())
    }
    
    /// Remove a value from state
    pub fn remove<T: Any + Send + Sync>(&mut self, key: &str) -> Option<T> {
        self.data
            .remove(key)
            .and_then(|v| v.downcast::<T>().ok())
            .map(|v| *v)
    }
    
    /// Check if a key exists
    pub fn contains(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }
    
    /// Clear all state
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

/// Extension services available to hook handlers
///
/// This provides access to shared services like logging, configuration,
/// and other cross-cutting concerns.
#[derive(Debug)]
pub struct ExtensionServices {
    /// Configuration service
    config: ExtensionConfig,
    
    /// Telemetry/metrics service
    telemetry: TelemetryService,
    
    /// Tool execution service (handles parameter injection)
    tool_execution: crate::extensions::services::ToolExecutionService,
    
    /// Reserved parameters service
    reserved_params: crate::extensions::services::ReservedParamsService,
    
    /// Async execution router
    async_router: crate::extensions::services::AsyncExecutionRouter,
}

impl ExtensionServices {
    /// Create new extension services
    pub fn new() -> Self {
        Self {
            config: ExtensionConfig::default(),
            telemetry: TelemetryService::new(),
            tool_execution: crate::extensions::services::ToolExecutionService::new(),
            reserved_params: crate::extensions::services::ReservedParamsService::new(),
            async_router: crate::extensions::services::AsyncExecutionRouter::new(),
        }
    }
    
    /// Get configuration
    pub fn config(&self) -> &ExtensionConfig {
        &self.config
    }
    
    /// Get telemetry service
    pub fn telemetry(&self) -> &TelemetryService {
        &self.telemetry
    }
    
    /// Get tool execution service
    pub fn tool_execution(&self) -> &crate::extensions::services::ToolExecutionService {
        &self.tool_execution
    }
    
    /// Get reserved parameters service
    pub fn reserved_params(&self) -> &crate::extensions::services::ReservedParamsService {
        &self.reserved_params
    }
    
    /// Get async execution router
    pub fn async_router(&self) -> &crate::extensions::services::AsyncExecutionRouter {
        &self.async_router
    }
    
    /// Record a hook invocation
    pub fn record_invocation(&self, hook_id: &HookId, point: &HookPoint, duration_ms: u64) {
        self.telemetry.record_hook_invocation(hook_id, point, duration_ms);
    }
}

impl Default for ExtensionServices {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for extensions
#[derive(Debug, Default)]
pub struct ExtensionConfig {
    /// Maximum hook execution time in milliseconds
    pub max_hook_duration_ms: u64,
    
    /// Enable hook tracing
    pub enable_tracing: bool,
    
    /// Extension-specific configuration
    pub extension_settings: HashMap<String, serde_json::Value>,
}

impl ExtensionConfig {
    /// Create default configuration
    pub fn new() -> Self {
        Self {
            max_hook_duration_ms: 5000, // 5 seconds default
            enable_tracing: false,
            extension_settings: HashMap::new(),
        }
    }
    
    /// Get a setting for a specific extension
    pub fn get_extension_setting(&self, extension_id: &str, key: &str) -> Option<&serde_json::Value> {
        self.extension_settings
            .get(extension_id)
            .and_then(|v| v.get(key))
    }
}

/// Telemetry service for hook metrics
#[derive(Debug)]
pub struct TelemetryService {
    /// Invocation counts by hook point
    invocation_counts: std::sync::Mutex<HashMap<String, u64>>,
    
    /// Total execution time by hook point
    execution_times: std::sync::Mutex<HashMap<String, u64>>,
}

impl TelemetryService {
    /// Create new telemetry service
    pub fn new() -> Self {
        Self {
            invocation_counts: std::sync::Mutex::new(HashMap::new()),
            execution_times: std::sync::Mutex::new(HashMap::new()),
        }
    }
    
    /// Record a hook invocation
    pub fn record_hook_invocation(
        &self,
        _hook_id: &HookId,
        point: &HookPoint,
        duration_ms: u64,
    ) {
        let name = point.name();
        
        if let Ok(mut counts) = self.invocation_counts.lock() {
            *counts.entry(name.clone()).or_insert(0) += 1;
        }
        
        if let Ok(mut times) = self.execution_times.lock() {
            *times.entry(name).or_insert(0) += duration_ms;
        }
    }
    
    /// Get invocation count for a hook point
    pub fn get_invocation_count(&self, point: &HookPoint) -> u64 {
        if let Ok(counts) = self.invocation_counts.lock() {
            counts.get(&point.name()).copied().unwrap_or(0)
        } else {
            0
        }
    }
    
    /// Get average execution time for a hook point
    pub fn get_average_execution_time(&self, point: &HookPoint) -> u64 {
        let name = point.name();
        
        let count = if let Ok(counts) = self.invocation_counts.lock() {
            counts.get(&name).copied().unwrap_or(0)
        } else {
            0
        };
        
        if count == 0 {
            return 0;
        }
        
        let total_time = if let Ok(times) = self.execution_times.lock() {
            times.get(&name).copied().unwrap_or(0)
        } else {
            0
        };
        
        total_time / count
    }
}

/// Trait for hook handlers
///
/// All extension types implement this trait to hook into the agent lifecycle.
#[async_trait]
pub trait HookHandler: Send + Sync + fmt::Debug {
    /// Handle the hook invocation
    ///
    /// This method is called when the hook point is triggered.
    /// Handlers should examine the context and return an appropriate result.
    ///
    /// # Arguments
    /// * `ctx` - The hook context containing input, state, and services
    ///
    /// # Returns
    /// A `HookResult` indicating how to proceed
    async fn handle(&self, ctx: HookContext) -> HookResult;
    
    /// Get the hook point this handler attaches to
    fn hook_point(&self) -> HookPoint;
    
    /// Get the handler priority (higher = earlier execution)
    ///
    /// Default is 100 (normal priority)
    fn priority(&self) -> HookPriority {
        100
    }
    
    /// Get the handler name for debugging/tracing
    fn name(&self) -> String {
        format!("{:?}", self)
    }
}

/// Wrapper for closures as hook handlers
pub struct ClosureHookHandler<F> {
    point: HookPoint,
    priority: HookPriority,
    name: String,
    handler: F,
}

impl<F> ClosureHookHandler<F> {
    /// Create a new closure-based handler
    pub fn new(
        point: HookPoint,
        priority: HookPriority,
        name: impl Into<String>,
        handler: F,
    ) -> Self {
        Self {
            point,
            priority,
            name: name.into(),
            handler,
        }
    }
}

impl<F> fmt::Debug for ClosureHookHandler<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClosureHookHandler")
            .field("point", &self.point)
            .field("priority", &self.priority)
            .field("name", &self.name)
            .finish()
    }
}

#[async_trait]
impl<F, Fut> HookHandler for ClosureHookHandler<F>
where
    F: Fn(HookContext) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = HookResult> + Send,
{
    async fn handle(&self, ctx: HookContext) -> HookResult {
        (self.handler)(ctx).await
    }
    
    fn hook_point(&self) -> HookPoint {
        self.point.clone()
    }
    
    fn priority(&self) -> HookPriority {
        self.priority
    }
    
    fn name(&self) -> String {
        self.name.clone()
    }
}

/// Factory for creating hook handlers from manifests
#[async_trait]
pub trait HookHandlerFactory: Send + Sync + fmt::Debug {
    /// Create a handler instance
    fn create(&self, manifest: crate::extensions::types::ExtensionManifest) -> Box<dyn HookHandler>;
}

/// Binding between a hook point and a handler factory
#[derive(Debug)]
pub struct HookBinding {
    /// The hook point to bind to
    pub point: HookPoint,
    
    /// Factory for creating the handler
    pub handler_factory: Box<dyn HookHandlerFactory>,
}

impl HookBinding {
    /// Create a new hook binding
    pub fn new(point: HookPoint, factory: Box<dyn HookHandlerFactory>) -> Self {
        Self {
            point,
            handler_factory: factory,
        }
    }
}

/// Convenience builder for common hook bindings
pub struct HookBindingBuilder;

impl HookBindingBuilder {
    /// Create a tool registration binding
    pub fn tool_register<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ToolRegister,
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create a prompt section binding
    pub fn prompt_section<F>(section: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::PromptSystemSection {
                section: section.into(),
                priority: 100,
            },
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create a channel input binding
    pub fn channel_input<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ChannelInput,
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create a channel output binding
    pub fn channel_output<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ChannelOutput,
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create an event subscription binding
    pub fn event_subscribe<F>(topic_pattern: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::EventSubscribe {
                topic_pattern: topic_pattern.into(),
            },
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create an event emission binding
    pub fn event_emit<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::EventEmit,
            handler_factory: Box::new(factory),
        }
    }
    
    /// Create a tool execution binding
    pub fn tool_execute<F>(tool_name: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ToolExecute {
                tool_name: tool_name.into(),
            },
            handler_factory: Box::new(factory),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_hook_state() {
        let mut state = HookState::new();
        
        state.insert("key", "value".to_string());
        assert!(state.contains("key"));
        assert_eq!(state.get::<String>("key"), Some(&"value".to_string()));
        
        let value = state.remove::<String>("key");
        assert_eq!(value, Some("value".to_string()));
        assert!(!state.contains("key"));
    }
    
    #[test]
    fn test_hook_context_creation() {
        let point = HookPoint::ToolRegister;
        let input = HookInput::Unit;
        let services = Arc::new(ExtensionServices::new());
        
        let ctx = HookContext::new(point, input, services);
        
        assert_eq!(ctx.category(), "tool");
        assert!(ctx.invocation_id.starts_with("hook_"));
    }
    
    #[test]
    fn test_extension_config() {
        let config = ExtensionConfig::new();
        assert_eq!(config.max_hook_duration_ms, 5000);
        assert!(!config.enable_tracing);
    }
    
    #[test]
    fn test_telemetry_service() {
        let telemetry = TelemetryService::new();
        let point = HookPoint::ToolRegister;
        let hook_id = HookId::new();
        
        telemetry.record_hook_invocation(&hook_id, &point, 100);
        telemetry.record_hook_invocation(&hook_id, &point, 200);
        
        assert_eq!(telemetry.get_invocation_count(&point), 2);
        assert_eq!(telemetry.get_average_execution_time(&point), 150);
    }
}
