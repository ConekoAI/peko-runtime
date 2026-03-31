//! Integration tests for universal tools
//!
//! These tests verify the full flow: manifest -> adapter -> protocol -> result

use super::*;
use crate::tools::traits::Tool;
use serde_json::json;
use tempfile::TempDir;

/// Create a mock Python tool for testing
async fn create_mock_python_tool(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let script_path = dir.join(format!("{}.py", name));
    
    let script = r#"#!/usr/bin/env python3
import sys
import json

for line in sys.stdin:
    req = json.loads(line)
    req_id = req.get("id")
    method = req.get("method")
    
    if method == "tool/describe":
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "name": "mock_tool",
                "description": "A mock tool",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    elif method == "tool/execute":
        params = req.get("params", {})
        args = params.get("args", {})
        context = params.get("context", {})
        
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "success": True,
                "data": {
                    "received_args": args,
                    "received_context": context
                }
            }
        }
    else:
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32601, "message": "Method not found"}
        }
    
    print(json.dumps(resp), flush=True)
"#;
    
    tokio::fs::write(&script_path, script).await.unwrap();
    
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&script_path).await.unwrap().permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&script_path, perms).await.unwrap();
    }
    
    script_path
}

#[tokio::test]
async fn test_manifest_loading() {
    let temp = TempDir::new().unwrap();
    let manifest_path = temp.path().join("test.json");
    
    let manifest_json = json!({
        "name": "test_tool",
        "description": "A test tool",
        "parameters": {
            "type": "object",
            "properties": {
                "input": {"type": "string"}
            }
        },
        "reserved_parameters": {
            "session_id": {
                "source": {
                    "runtime": {
                        "field": "session_id"
                    }
                }
            }
        }
    });
    
    tokio::fs::write(&manifest_path, manifest_json.to_string())
        .await
        .unwrap();
    
    let manifest = Manifest::from_file(&manifest_path).await.unwrap();
    
    assert_eq!(manifest.name, "test_tool");
    assert!(manifest.is_reserved("session_id"));
    assert!(!manifest.is_reserved("input"));
}

#[tokio::test]
async fn test_parameter_injection() {
    let manifest = Manifest {
        name: "inject_test".to_string(),
        description: "Test injection".to_string(),
        llm_description: None,
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"}
            }
        }),
        reserved_parameters: Some({
            let mut m = std::collections::HashMap::new();
            m.insert(
                "session_id".to_string(),
                ReservedParam {
                    source: ParamSource::Runtime {
                        field: "session_id".to_string(),
                    },
                    description: None,
                },
            );
            m.insert(
                "agent_id".to_string(),
                ReservedParam {
                    source: ParamSource::Runtime {
                        field: "agent_id".to_string(),
                    },
                    description: None,
                },
            );
            m
        }),
        protocol: ProtocolConfig::default(),
        extra: std::collections::HashMap::new(),
    };
    
    let user_params = json!({"query": "hello"});
    let context = ExecutionContext {
        session_id: "sess_123".to_string(),
        agent_id: "agent_456".to_string(),
        peer_id: None,
        workspace: "/tmp".to_string(),
        run_id: None,
    };
    
    let merged = merge_with_injection(&manifest, user_params, &context).unwrap();
    
    assert_eq!(merged["query"], "hello");
    assert_eq!(merged["session_id"], "sess_123");
    assert_eq!(merged["agent_id"], "agent_456");
}

#[tokio::test]
async fn test_discover_universal_tools() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path();
    
    // Create tool subdirectory
    let tool_dir = dir.join("test_tool");
    tokio::fs::create_dir(&tool_dir).await.unwrap();
    
    // Create manifest in subdirectory
    let manifest = json!({
        "name": "test_tool",
        "description": "Test",
        "parameters": {"type": "object"}
    });
    tokio::fs::write(tool_dir.join("manifest.json"), manifest.to_string())
        .await
        .unwrap();
    
    // Create executable in subdirectory
    let script_path = tool_dir.join("test_tool.py");
    let script = r#"#!/usr/bin/env python3
import sys
import json

for line in sys.stdin:
    req = json.loads(line)
    req_id = req.get("id")
    method = req.get("method")
    
    if method == "tool/describe":
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "name": "test_tool",
                "description": "A test tool",
                "parameters": {"type": "object", "properties": {}}
            }
        }
    elif method == "tool/execute":
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"success": True, "data": {}}
        }
    else:
        resp = {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": -32601, "message": "Method not found"}
        }
    
    print(json.dumps(resp), flush=True)
"#;
    tokio::fs::write(&script_path, script).await.unwrap();
    
    let tools = discovery::discover_universal_tools(dir).await.unwrap();
    
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "test_tool");
}

#[test]
fn test_protocol_messages() {
    // Test request creation
    let req = Request::new("tool/execute", json!({"tool": "test"}));
    assert_eq!(req.method, "tool/execute");
    assert!(req.id.is_some());
    assert_eq!(req.jsonrpc, "2.0");
    
    // Test response serialization
    let resp = Response {
        jsonrpc: "2.0".to_string(),
        id: Some("123".to_string()),
        result: ResponseResult::Result(json!({"success": true})),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("result"));
    
    // Test error response
    let err_resp = Response {
        jsonrpc: "2.0".to_string(),
        id: Some("456".to_string()),
        result: ResponseResult::Error(ErrorObject::method_not_found("foo")),
    };
    let json = serde_json::to_string(&err_resp).unwrap();
    assert!(json.contains("error"));
    assert!(json.contains("-32601"));
}

#[test]
fn test_builder_pattern() {
    let manifest = Manifest {
        name: "builder_test".to_string(),
        description: "Test".to_string(),
        llm_description: None,
        parameters: json!({"type": "object"}),
        reserved_parameters: None,
        protocol: ProtocolConfig::default(),
        extra: std::collections::HashMap::new(),
    };
    
    let adapter = UniversalToolBuilder::new()
        .manifest(manifest)
        .executable("/bin/true")
        .build()
        .unwrap();
    
    assert_eq!(adapter.name(), "builder_test");
}

#[test]
fn test_tool_trait_implementation() {
    let manifest = Manifest {
        name: "trait_test".to_string(),
        description: "Test description".to_string(),
        llm_description: Some("LLM optimized".to_string()),
        parameters: json!({
            "type": "object",
            "properties": {
                "q": {"type": "string"}
            }
        }),
        reserved_parameters: None,
        protocol: ProtocolConfig::default(),
        extra: std::collections::HashMap::new(),
    };
    
    let adapter = UniversalToolAdapter::from_manifest_embedded(manifest, "/bin/true");
    
    // Test Tool trait methods
    assert_eq!(adapter.name(), "trait_test");
    assert_eq!(adapter.description(), "Test description");
    assert_eq!(adapter.llm_description(), "LLM optimized");
    assert!(adapter.parameters()["properties"]["q"].is_object());
}
