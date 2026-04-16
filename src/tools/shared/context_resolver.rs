//! Unified Context Resolver
//!
//! Provides a single source of truth for resolving runtime context fields
//! (agent_id, session_id, etc.) from ToolContext or ExecutionContext.
//!
//! This eliminates duplication between Universal Tools and MCP implementations.

use serde_json::Value;

/// Unified context resolver for runtime fields
///
/// This struct provides a single source of truth for resolving context fields,
/// ensuring consistent behavior across all tool types (built-in, Universal, MCP).
pub struct ContextResolver;

/// Context sources that can be resolved
pub trait ContextSource {
    fn get_session_id(&self) -> Option<String>;
    fn get_agent_id(&self) -> Option<String>;
    fn get_peer_id(&self) -> Option<String>;
    fn get_workspace(&self) -> Option<String>;
    fn get_run_id(&self) -> Option<String>;
}

/// ToolContext adapter for ContextSource
pub struct ToolContextAdapter<'a> {
    ctx: &'a crate::tools::ToolContext,
}

impl<'a> std::fmt::Debug for ToolContextAdapter<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContextAdapter")
            .field("agent_id", &self.ctx.agent_id)
            .field("session_id", &self.ctx.session_id)
            .finish_non_exhaustive()
    }
}

impl<'a> ToolContextAdapter<'a> {
    pub fn new(ctx: &'a crate::tools::ToolContext) -> Self {
        Self { ctx }
    }
}

impl<'a> ContextSource for ToolContextAdapter<'a> {
    fn get_session_id(&self) -> Option<String> {
        self.ctx.session_id.clone()
    }

    fn get_agent_id(&self) -> Option<String> {
        self.ctx.agent_id.clone()
    }

    fn get_peer_id(&self) -> Option<String> {
        self.ctx.peer_id.clone()
    }

    fn get_workspace(&self) -> Option<String> {
        self.ctx.workspace.clone()
    }

    fn get_run_id(&self) -> Option<String> {
        Some(self.ctx.run_id.clone())
    }
}

/// ExecutionContext adapter for ContextSource
pub struct ExecutionContextAdapter {
    ctx: crate::tools::universal::protocol::ExecutionContext,
}

impl std::fmt::Debug for ExecutionContextAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionContextAdapter")
            .field("agent_id", &self.ctx.agent_id)
            .field("session_id", &self.ctx.session_id)
            .finish_non_exhaustive()
    }
}

impl ExecutionContextAdapter {
    pub fn new(ctx: crate::tools::universal::protocol::ExecutionContext) -> Self {
        Self { ctx }
    }
}

impl ContextSource for ExecutionContextAdapter {
    fn get_session_id(&self) -> Option<String> {
        Some(self.ctx.session_id.clone())
    }

    fn get_agent_id(&self) -> Option<String> {
        Some(self.ctx.agent_id.clone())
    }

    fn get_peer_id(&self) -> Option<String> {
        self.ctx.peer_id.clone()
    }

    fn get_workspace(&self) -> Option<String> {
        Some(self.ctx.workspace.clone())
    }

    fn get_run_id(&self) -> Option<String> {
        self.ctx.run_id.clone()
    }
}

impl ContextResolver {
    /// Resolve a runtime field by name from any context source
    ///
    /// # Supported Fields
    /// - `session_id`: The current session identifier
    /// - `agent_id`: The current agent identifier  
    /// - `peer_id`: The peer/user identifier (optional)
    /// - `workspace`: The workspace directory path
    /// - `run_id`: The unique run identifier
    ///
    /// # Returns
    /// - `Value::String` if the field exists and has a value
    /// - `Value::Null` if the field is not set or unknown
    pub fn resolve_field(source: &dyn ContextSource, field: &str) -> Value {
        match field {
            "session_id" => source
                .get_session_id()
                .map(Value::String)
                .unwrap_or(Value::Null),
            "agent_id" => source
                .get_agent_id()
                .map(Value::String)
                .unwrap_or(Value::Null),
            "peer_id" => source
                .get_peer_id()
                .map(Value::String)
                .unwrap_or(Value::Null),
            "workspace" => source
                .get_workspace()
                .map(Value::String)
                .unwrap_or(Value::Null),
            "run_id" => source
                .get_run_id()
                .map(Value::String)
                .unwrap_or(Value::Null),
            _ => {
                tracing::warn!("Unknown context field requested: {}", field);
                Value::Null
            }
        }
    }

    /// Get all available field names
    pub fn available_fields() -> &'static [&'static str] {
        &["session_id", "agent_id", "peer_id", "workspace", "run_id"]
    }
}

/// Convenience trait for converting contexts to Value
pub trait ToContextValue {
    fn to_context_value(&self, field: &str) -> Value;
}

impl<T: ContextSource> ToContextValue for T {
    fn to_context_value(&self, field: &str) -> Value {
        ContextResolver::resolve_field(self, field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockContext {
        session_id: Option<String>,
        agent_id: Option<String>,
        peer_id: Option<String>,
        workspace: Option<String>,
        run_id: Option<String>,
    }

    impl ContextSource for MockContext {
        fn get_session_id(&self) -> Option<String> {
            self.session_id.clone()
        }

        fn get_agent_id(&self) -> Option<String> {
            self.agent_id.clone()
        }

        fn get_peer_id(&self) -> Option<String> {
            self.peer_id.clone()
        }

        fn get_workspace(&self) -> Option<String> {
            self.workspace.clone()
        }

        fn get_run_id(&self) -> Option<String> {
            self.run_id.clone()
        }
    }

    #[test]
    fn test_resolve_all_fields() {
        let ctx = MockContext {
            session_id: Some("sess_123".to_string()),
            agent_id: Some("agent_test".to_string()),
            peer_id: Some("peer_456".to_string()),
            workspace: Some("/tmp/test".to_string()),
            run_id: Some("run_789".to_string()),
        };

        assert_eq!(
            ContextResolver::resolve_field(&ctx, "session_id"),
            Value::String("sess_123".to_string())
        );
        assert_eq!(
            ContextResolver::resolve_field(&ctx, "agent_id"),
            Value::String("agent_test".to_string())
        );
        assert_eq!(
            ContextResolver::resolve_field(&ctx, "peer_id"),
            Value::String("peer_456".to_string())
        );
        assert_eq!(
            ContextResolver::resolve_field(&ctx, "workspace"),
            Value::String("/tmp/test".to_string())
        );
        assert_eq!(
            ContextResolver::resolve_field(&ctx, "run_id"),
            Value::String("run_789".to_string())
        );
    }

    #[test]
    fn test_resolve_missing_fields() {
        let ctx = MockContext {
            session_id: None,
            agent_id: None,
            peer_id: None,
            workspace: None,
            run_id: None,
        };

        assert_eq!(
            ContextResolver::resolve_field(&ctx, "session_id"),
            Value::Null
        );
        assert_eq!(
            ContextResolver::resolve_field(&ctx, "agent_id"),
            Value::Null
        );
    }

    #[test]
    fn test_resolve_unknown_field() {
        let ctx = MockContext {
            session_id: Some("test".to_string()),
            agent_id: None,
            peer_id: None,
            workspace: None,
            run_id: None,
        };

        assert_eq!(
            ContextResolver::resolve_field(&ctx, "unknown_field"),
            Value::Null
        );
    }

    #[test]
    fn test_available_fields() {
        let fields = ContextResolver::available_fields();
        assert!(fields.contains(&"session_id"));
        assert!(fields.contains(&"agent_id"));
        assert!(fields.contains(&"peer_id"));
        assert!(fields.contains(&"workspace"));
        assert!(fields.contains(&"run_id"));
    }
}
