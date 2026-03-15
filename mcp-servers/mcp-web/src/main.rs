//! MCP Web Server
//!
//! Provides web search, content fetching, and HTTP request capabilities
//! via the Model Context Protocol over stdio.

use std::io::{self, BufRead, Write};
use tracing::{debug, error, info};

mod tools;
use tools::{fetch_tool, http_tool, web_search_tool};

/// MCP Protocol version
const MCP_VERSION: &str = "2024-11-05";

/// Server information
const SERVER_NAME: &str = "mcp-web";
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

/// MCP Server state
struct McpServer {
    tools: Vec<Tool>,
    initialized: bool,
}

impl McpServer {
    fn new() -> Self {
        Self {
            tools: vec![
                Tool {
                    name: "web_search".to_string(),
                    description: "Search the web using Brave Search or DuckDuckGo".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Search query"
                            },
                            "count": {
                                "type": "integer",
                                "description": "Number of results (1-20)",
                                "default": 10
                            },
                            "engine": {
                                "type": "string",
                                "enum": ["brave", "ddg"],
                                "description": "Search engine",
                                "default": "ddg"
                            }
                        },
                        "required": ["query"]
                    }),
                },
                Tool {
                    name: "fetch".to_string(),
                    description: "Fetch a URL and extract its content".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "url": {
                                "type": "string",
                                "description": "URL to fetch"
                            },
                            "extract_text": {
                                "type": "boolean",
                                "description": "Extract main text content",
                                "default": true
                            }
                        },
                        "required": ["url"]
                    }),
                },
                Tool {
                    name: "http".to_string(),
                    description: "Make HTTP requests (GET, POST, PUT, DELETE, etc.)".to_string(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "method": {
                                "type": "string",
                                "enum": ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"],
                                "default": "GET"
                            },
                            "url": {
                                "type": "string",
                                "description": "Request URL"
                            },
                            "headers": {
                                "type": "object",
                                "description": "Request headers",
                                "additionalProperties": { "type": "string" }
                            },
                            "body": {
                                "type": "string",
                                "description": "Request body"
                            }
                        },
                        "required": ["url"]
                    }),
                },
            ],
            initialized: false,
        }
    }

    fn handle_request(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        debug!("Handling method: {}", request.method);

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
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            })),
            error: None,
        }
    }

    fn handle_tools_list(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(serde_json::json!({
                "tools": self.tools
            })),
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

        let result = match name {
            "web_search" => {
                let runtime = tokio::runtime::Handle::try_current()
                    .or_else(|_| {
                        tokio::runtime::Runtime::new()
                            .map(|rt| rt.handle().clone())
                    });
                
                match runtime {
                    Ok(handle) => {
                        handle.block_on(async {
                            web_search_tool::execute(arguments).await
                        })
                    }
                    Err(e) => Err(format!("Failed to get runtime: {}", e)),
                }
            }
            "fetch" => {
                let runtime = tokio::runtime::Handle::try_current()
                    .or_else(|_| {
                        tokio::runtime::Runtime::new()
                            .map(|rt| rt.handle().clone())
                    });
                
                match runtime {
                    Ok(handle) => {
                        handle.block_on(async {
                            fetch_tool::execute(arguments).await
                        })
                    }
                    Err(e) => Err(format!("Failed to get runtime: {}", e)),
                }
            }
            "http" => {
                let runtime = tokio::runtime::Handle::try_current()
                    .or_else(|_| {
                        tokio::runtime::Runtime::new()
                            .map(|rt| rt.handle().clone())
                    });
                
                match runtime {
                    Ok(handle) => {
                        handle.block_on(async {
                            http_tool::execute(arguments).await
                        })
                    }
                    Err(e) => Err(format!("Failed to get runtime: {}", e)),
                }
            }
            _ => Err(format!("Unknown tool: {}", name)),
        };

        match result {
            Ok(content) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id,
                result: Some(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": content
                    }]
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

#[tokio::main]
async fn main() {
    // Initialize logging to stderr (so it doesn't interfere with stdio protocol)
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
        )
        .init();

    info!("Starting MCP Web Server v{}", SERVER_VERSION);

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
        let response_json = match serde_json::to_string(&response) {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize response: {}", e);
                continue;
            }
        };

        debug!("Sending: {}", response_json);
        writeln!(stdout, "{}", response_json).unwrap();
        stdout.flush().unwrap();
    }

    info!("MCP Web Server shutting down");
}
