//! Hook context and state definitions
//!
//! This module defines the context passed to hook handlers and the mutable
//! state container for hook invocations.

use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::types::HookInput;
use std::any::Any;
use std::collections::HashMap;
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
    pub services: Arc<super::config::ExtensionServices>,
}

impl HookContext {
    /// Create a new hook context
    pub fn new(
        point: HookPoint,
        input: HookInput,
        services: Arc<super::config::ExtensionServices>,
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
    #[must_use]
    pub fn category(&self) -> &'static str {
        self.point.category()
    }

    /// Get the hook point name
    #[must_use]
    pub fn name(&self) -> String {
        self.point.name()
    }

    /// Check if input is of a specific type
    #[must_use]
    pub fn is_input<T: 'static>(&self) -> bool {
        matches!(&self.input, HookInput::Json(_) if std::any::TypeId::of::<T>() == std::any::TypeId::of::<serde_json::Value>())
    }

    /// Get input as prompt build state if applicable
    #[must_use]
    pub fn as_prompt_build(
        &self,
    ) -> Option<&crate::extensions::framework::types::PromptBuildState> {
        match &self.input {
            HookInput::PromptBuild(state) => Some(state),
            _ => None,
        }
    }

    /// Get input as tool call if applicable
    #[must_use]
    pub fn as_tool_call(&self) -> Option<(&str, &serde_json::Value, Option<&str>)> {
        match &self.input {
            HookInput::ToolCall {
                tool_name,
                params,
                workspace,
                ..
            } => Some((tool_name, params, workspace.as_deref())),
            _ => None,
        }
    }

    /// Get input as session state if applicable
    #[must_use]
    pub fn as_session_state(
        &self,
    ) -> Option<&crate::extensions::framework::types::SessionSnapshot> {
        match &self.input {
            HookInput::SessionState(state) => Some(state),
            _ => None,
        }
    }

    /// Get input as message if applicable
    #[must_use]
    pub fn as_message(&self) -> Option<&crate::extensions::framework::types::MessageEnvelope> {
        match &self.input {
            HookInput::Message(msg) => Some(msg),
            _ => None,
        }
    }

    /// Get input as JSON if applicable
    #[must_use]
    pub fn as_json(&self) -> Option<&serde_json::Value> {
        match &self.input {
            HookInput::Json(v) => Some(v),
            _ => None,
        }
    }

    /// Get the raw input
    #[must_use]
    pub fn input(&self) -> &HookInput {
        &self.input
    }

    /// Get input as task status check if applicable
    #[must_use]
    pub fn as_task_status(&self) -> Option<(&str, &str)> {
        match &self.input {
            HookInput::TaskStatus { task_id, tool_name } => Some((task_id, tool_name)),
            _ => None,
        }
    }

    /// Get input as task cancel request if applicable
    #[must_use]
    pub fn as_task_cancel(&self) -> Option<(&str, &str)> {
        match &self.input {
            HookInput::TaskCancel { task_id, tool_name } => Some((task_id, tool_name)),
            _ => None,
        }
    }

    /// Get a typed value from state
    #[must_use]
    pub fn get_state<T: Any + Send + Sync>(&self, key: &str) -> Option<&T> {
        self.state.get::<T>(key)
    }

    /// Set a typed value in state
    pub fn set_state<T: Any + Send + Sync>(&mut self, key: impl Into<String>, value: T) {
        self.state.insert(key, value);
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.data.contains_key(key)
    }

    /// Clear all state
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::types::HookInput;

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
        let services = Arc::new(super::super::config::ExtensionServices::new());

        let ctx = HookContext::new(point, input, services);

        assert_eq!(ctx.category(), "tool");
        assert!(ctx.invocation_id.starts_with("hook_"));
    }
}
