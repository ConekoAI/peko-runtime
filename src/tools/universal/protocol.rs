//! Universal Tool Protocol - JSON-RPC 2.0 based
//!
//! SRP: This module ONLY handles protocol message types.
//! No transport, no execution logic, just pure data structures.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version
pub const PROTOCOL_VERSION: &str = "2.0";

/// JSON-RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Option<String>,
    pub method: String,
    pub params: Option<Value>,
}

impl Request {
    /// Create a new request
    pub fn new(method: impl Into<String>, params: impl Into<Value>) -> Self {
        Self {
            jsonrpc: PROTOCOL_VERSION.to_string(),
            id: Some(generate_id()),
            method: method.into(),
            params: Some(params.into()),
        }
    }

    /// Create a notification (no id, no response expected)
    pub fn notification(method: impl Into<String>, params: impl Into<Value>) -> Self {
        Self {
            jsonrpc: PROTOCOL_VERSION.to_string(),
            id: None,
            method: method.into(),
            params: Some(params.into()),
        }
    }
}

/// JSON-RPC response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<String>,
    #[serde(flatten)]
    pub result: ResponseResult,
}

/// Response can be success or error
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseResult {
    Result(Value),
    Error(ErrorObject),
}

/// JSON-RPC error object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    /// Parse error (-32700)
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: msg.into(),
            data: None,
        }
    }

    /// Invalid request (-32600)
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
            data: None,
        }
    }

    /// Method not found (-32601)
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method '{method}' not found"),
            data: Some(Value::String(method.to_string())),
        }
    }

    /// Invalid params (-32602)
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }

    /// Internal error (-32603)
    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

/// Tool execution parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteParams {
    pub tool: String,
    pub args: Value,
    pub context: ExecutionContext,
}

/// Runtime context injected by Pekobot
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionContext {
    pub session_id: String,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl ExecuteResult {
    pub fn success(data: impl Into<Value>) -> Self {
        Self {
            success: true,
            data: Some(data.into()),
            error: None,
            metadata: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
            metadata: None,
        }
    }
}

/// Tool description response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DescribeResult {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_description: Option<String>,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved_parameters: Option<Value>,
}

fn generate_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut hasher = DefaultHasher::new();
    now.hash(&mut hasher);
    let hash = hasher.finish();

    format!("{hash:x}")[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = Request::new(
            "tool/execute",
            serde_json::json!({"tool": "test", "args": {}}),
        );
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("jsonrpc"));
        assert!(json.contains("2.0"));
        assert!(json.contains("tool/execute"));
    }

    #[test]
    fn test_response_success() {
        let resp = Response {
            jsonrpc: PROTOCOL_VERSION.to_string(),
            id: Some("123".to_string()),
            result: ResponseResult::Result(serde_json::json!({"success": true})),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("result"));
    }

    #[test]
    fn test_response_error() {
        let resp = Response {
            jsonrpc: PROTOCOL_VERSION.to_string(),
            id: Some("123".to_string()),
            result: ResponseResult::Error(ErrorObject::method_not_found("foo")),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("-32601"));
    }

    #[test]
    fn test_execution_context() {
        let ctx = ExecutionContext {
            session_id: "sess_123".to_string(),
            agent_id: "agent_test".to_string(),
            peer_id: Some("peer_456".to_string()),
            workspace: "/tmp/test".to_string(),
            run_id: Some("run_789".to_string()),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("sess_123"));
        assert!(json.contains("agent_test"));
    }

    #[test]
    fn test_execute_result() {
        let result = ExecuteResult::success(serde_json::json!({"data": "value"}));
        assert!(result.success);
        assert!(result.data.is_some());

        let error = ExecuteResult::error("something failed");
        assert!(!error.success);
        assert!(error.error.is_some());
    }
}
