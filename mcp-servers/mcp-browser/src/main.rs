//! MCP Browser Server
//!
//! Provides browser automation capabilities via headless Chrome.

use std::io::{self, BufRead, Write};
use tracing::{debug, error, info};

mod tools;

const MCP_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "mcp-browser";
const SERVER_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: serde_json::Value,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

impl JsonRpcError {
    fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }

    fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

struct McpServer {
    tools: Vec<Tool>,
    initialized: bool,
}

impl McpServer {
    fn new() -> Self {
        Self {
            tools: vec![
                Tool {
                    name: "browser_navigate".to_string(),
                    description: "Navigate to a URL".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "url": { "type": "string", "description": "URL to navigate to" },
                            "wait_for": { "type": "string", "description": "CSS selector to wait for" }
                        },
                        "required": ["url"]
                    }),
                },
                Tool {
                    name: "browser_click".to_string(),
                    description: "Click an element on the page".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "selector": { "type": "string", "description": "CSS selector for element to click" }
                        },
                        "required": ["selector"]
                    }),
                },
                Tool {
                    name: "browser_type".to_string(),
                    description: "Type text into an input element".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "selector": { "type": "string", "description": "CSS selector for input" },
                            "text": { "type": "string", "description": "Text to type" }
                        },
                        "required": ["selector", "text"]
                    }),
                },
                Tool {
                    name: "browser_screenshot".to_string(),
                    description: "Take a screenshot of the current page".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "selector": { "type": "string", "description": "CSS selector for element (optional, defaults to full page)" }
                        }
                    }),
                },
                Tool {
                    name: "browser_get_content".to_string(),
                    description: "Get page content or text".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "format": { "type": "string", "enum": ["html", "text"], "default": "text" }
                        }
                    }),
                },
            ],
            initialized: false,
        }
    }

    fn handle_request(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "tools/list" => self.handle_tools_list(request),
            "tools/call" => self.handle_tool_call(request),
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                    data: None,
                }),
            },
        }
    }

    fn handle_initialize(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        self.initialized = true;
        info!("MCP client initialized");

        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(serde_json::json!({
                "protocolVersion": MCP_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
            })),
            error: None,
        }
    }

    fn handle_tools_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(serde_json::json!({ "tools": self.tools })),
            error: None,
        }
    }

    fn handle_tool_call(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let params = match request.params {
            Some(p) => p,
            None => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: None,
                    error: Some(JsonRpcError::invalid_params("Missing params")),
                };
            }
        };

        let name = params.get("name").and_then(|n| n.as_str());
        let arguments = params.get("arguments").cloned().unwrap_or(serde_json::json!({}));

        let name = match name {
            Some(n) => n,
            None => {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: request.id,
                    result: None,
                    error: Some(JsonRpcError::invalid_params("Missing tool name")),
                };
            }
        };

        info!("Calling tool: {}", name);

        // Create runtime for async execution
        let result = tokio::runtime::Runtime::new()
            .map_err(|e| format!("Failed to create runtime: {}", e))
            .and_then(|rt| {
                rt.block_on(async {
                    match name {
                        "browser_navigate" => tools::navigate::execute(arguments).await,
                        "browser_click" => tools::click::execute(arguments).await,
                        "browser_type" => tools::type_text::execute(arguments).await,
                        "browser_screenshot" => tools::screenshot::execute(arguments).await,
                        "browser_get_content" => tools::get_content::execute(arguments).await,
                        _ => Err(format!("Unknown tool: {}", name)),
                    }
                })
            });

        match result {
            Ok(content) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(serde_json::json!({
                    "content": [{"type": "text", "text": content}]
                })),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError::internal_error(e)),
            },
        }
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
        )
        .init();

    info!("Starting MCP Browser Server v{}", SERVER_VERSION);

    let mut server = McpServer::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to read line: {}", e);
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        debug!("Received: {}", line);

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to parse request: {}", e);
                let response = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: serde_json::Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                        data: None,
                    }),
                };
                let response_json = serde_json::to_string(&response).unwrap();
                writeln!(stdout, "{}", response_json).unwrap();
                stdout.flush().unwrap();
                continue;
            }
        };

        let response = server.handle_request(request);
        let response_json = serde_json::to_string(&response).unwrap();
        debug!("Sending: {}", response_json);
        writeln!(stdout, "{}", response_json).unwrap();
        stdout.flush().unwrap();
    }

    info!("MCP Browser Server shutting down");
}
