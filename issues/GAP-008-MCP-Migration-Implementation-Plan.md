# GAP-008: MCP Migration Implementation Plan

**Priority:** 🟡 Medium  
**Status:** Planning  
**Target:** v0.7.0  
**Est. Effort:** 3 weeks  
**Date:** 2026-03-14

---

## Executive Summary

This plan details the migration of heavy built-in tools to MCP (Model Context Protocol) servers:

| MCP Server | Tools | Lines | Binary Impact |
|------------|-------|-------|---------------|
| `mcp-web` | web_search, fetch, http | ~1,469 | ~-2MB |
| `mcp-browser` | browser automation | ~638 | ~-1MB |
| `mcp-memory` | memory operations | ~305 | ~-0.5MB |
| **Total** | | **~2,412** | **~-3.5MB** |

**Target binary size:** 8MB → 4-5MB

---

## 1. Project Structure

```
pekobot/
├── Cargo.toml
├── src/
│   └── tools/
│       └── ... (core tools only after migration)
└── mcp-servers/                    # NEW
    ├── Cargo.toml                  # Workspace manifest
    ├── mcp-web/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── main.rs
    │       └── tools/
    │           ├── mod.rs
    │           ├── web_search.rs
    │           ├── fetch.rs
    │           └── http.rs
    ├── mcp-browser/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── main.rs
    │       └── browser.rs
    └── mcp-memory/
        ├── Cargo.toml
        └── src/
            ├── main.rs
            └── memory.rs
```

---

## 2. MCP-Web Server

### 2.1 Purpose
Combines web search, HTTP fetching, and general HTTP requests into a single MCP server.

### 2.2 Cargo.toml
```toml
[package]
name = "mcp-web"
version = "1.0.0"
edition = "2021"
authors = ["Pekora <peko@coneko.ai>"]
description = "MCP server for web search and HTTP operations"
license = "MIT"

[[bin]]
name = "mcp-web"
path = "src/main.rs"

[dependencies]
# MCP SDK - using pekobot's mcp types
mcp-types = { path = "../../src/mcp/types" }

# Async runtime
tokio = { version = "1.35", features = ["full"] }

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# HTTP client
reqwest = { version = "0.11", features = ["json", "stream"] }

# HTML parsing (extracted from pekobot core)
scraper = "0.19"

# Text extraction (extracted from pekobot core)
readability = "0.3"

# Error handling
anyhow = "1.0"
thiserror = "1.0"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"

# URL handling
url = "2.5"

# HTML entities
html-escape = "0.2"

[dev-dependencies]
tokio-test = "0.4"
wiremock = "0.6"
```

### 2.3 Tools Provided

| Tool | Description | Input Schema | Output Schema |
|------|-------------|--------------|---------------|
| `web_search` | Search web with Brave/DuckDuckGo | `{ query: string, count?: number, engine?: "brave" \| "ddg" }` | `{ results: [{title, url, snippet}] }` |
| `fetch` | Fetch and extract webpage | `{ url: string, extract_text?: bool, follow_redirects?: bool }` | `{ url, title, content, text_content? }` |
| `http` | General HTTP requests | `{ method: string, url: string, headers?: object, body?: string }` | `{ status, headers, body }` |

### 2.4 Implementation: main.rs
```rust
//! MCP Web Server
//! 
//! Provides web search, content fetching, and HTTP request capabilities
//! via the Model Context Protocol.

use mcp_types::{McpServer, ServerCapabilities, Tool};
use std::sync::Arc;
use tracing::{info, warn};

mod tools;
use tools::{FetchTool, HttpTool, WebSearchTool};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    info!("Starting MCP Web Server v{}", env!("CARGO_PKG_VERSION"));
    
    // Create server
    let server = McpServer::new("mcp-web", "1.0.0")
        .with_capabilities(ServerCapabilities {
            tools: Some(true),
            ..Default::default()
        })
        .with_tool(Arc::new(WebSearchTool::new()))
        .with_tool(Arc::new(FetchTool::new()))
        .with_tool(Arc::new(HttpTool::new()));
    
    // Run on stdio (MCP standard)
    server.run_stdio().await?;
    
    Ok(())
}
```

### 2.5 Tool Implementation: web_search.rs
```rust
//! Web search tool - extracted from pekobot/src/tools/web_search.rs

use async_trait::async_trait;
use mcp_types::{Tool, ToolError, ToolResult};
use serde_json::Value;

pub struct WebSearchTool {
    brave_api_key: Option<String>,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            brave_api_key: std::env::var("BRAVE_API_KEY").ok(),
        }
    }
    
    async fn search_brave(&self, query: &str, count: u32) -> ToolResult {
        // Implementation extracted from pekobot
        // Uses reqwest to call Brave Search API
        // Returns structured search results
    }
    
    async fn search_ddg(&self, query: &str, count: u32) -> ToolResult {
        // Implementation extracted from pekobot
        // Uses DuckDuckGo HTML scraping
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }
    
    fn description(&self) -> &str {
        "Search the web using Brave Search or DuckDuckGo"
    }
    
    fn schema(&self) -> Value {
        serde_json::json!({
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
                    "description": "Search engine to use",
                    "default": "brave"
                }
            },
            "required": ["query"]
        })
    }
    
    async fn execute(&self, params: Value) -> ToolResult {
        let query = params["query"].as_str()
            .ok_or(ToolError::InvalidParams("query is required".into()))?;
        
        let count = params["count"].as_u64().unwrap_or(10) as u32;
        let engine = params["engine"].as_str().unwrap_or("brave");
        
        match engine {
            "brave" => self.search_brave(query, count).await,
            "ddg" => self.search_ddg(query, count).await,
            _ => Err(ToolError::InvalidParams(format!("Unknown engine: {}", engine))),
        }
    }
}
```

### 2.6 Tool Implementation: fetch.rs
```rust
//! Fetch tool - extracted from pekobot/src/tools/fetch.rs

use async_trait::async_trait;
use mcp_types::{Tool, ToolError, ToolResult};
use readability::extractor;
use scraper::{Html, Selector};
use serde_json::Value;

pub struct FetchTool {
    client: reqwest::Client,
}

impl FetchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }
    
    async fn fetch(&self, url: &str, extract_text: bool) -> ToolResult {
        let response = self.client.get(url).send().await
            .map_err(|e| ToolError::Execution(format!("Request failed: {}", e)))?;
        
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.text().await
            .map_err(|e| ToolError::Execution(format!("Failed to read body: {}", e)))?;
        
        let title = self.extract_title(&body);
        let text_content = if extract_text {
            Some(self.extract_text(&body)?)
        } else {
            None
        };
        
        Ok(serde_json::json!({
            "url": url,
            "status": status.as_u16(),
            "title": title,
            "content": body,
            "text_content": text_content,
            "headers": headers.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect::<std::collections::HashMap<_, _>>()
        }))
    }
    
    fn extract_title(&self, html: &str) -> String {
        let document = Html::parse_document(html);
        let selector = Selector::parse("title").unwrap();
        document.select(&selector).next()
            .map(|e| e.text().collect())
            .unwrap_or_default()
    }
    
    fn extract_text(&self, html: &str) -> Result<String, ToolError> {
        extractor::extract(html.as_bytes(), None)
            .map(|product| product.text)
            .map_err(|e| ToolError::Execution(format!("Text extraction failed: {}", e)))
    }
}

#[async_trait]
impl Tool for FetchTool {
    fn name(&self) -> &str {
        "fetch"
    }
    
    fn description(&self) -> &str {
        "Fetch a URL and extract its content"
    }
    
    fn schema(&self) -> Value {
        serde_json::json!({
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
                },
                "follow_redirects": {
                    "type": "boolean",
                    "description": "Follow HTTP redirects",
                    "default": true
                }
            },
            "required": ["url"]
        })
    }
    
    async fn execute(&self, params: Value) -> ToolResult {
        let url = params["url"].as_str()
            .ok_or(ToolError::InvalidParams("url is required".into()))?;
        
        let extract_text = params["extract_text"].as_bool().unwrap_or(true);
        
        self.fetch(url, extract_text).await
    }
}
```

---

## 3. MCP-Browser Server

### 3.1 Purpose
Provides browser automation via Playwright/Chrome.

### 3.2 Cargo.toml
```toml
[package]
name = "mcp-browser"
version = "1.0.0"
edition = "2021"

[[bin]]
name = "mcp-browser"
path = "src/main.rs"

[dependencies]
mcp-types = { path = "../../src/mcp/types" }
tokio = { version = "1.35", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
tracing = "0.1"
tracing-subscriber = "0.3"

# Browser automation
# Note: Using chromiumoxide (Rust) or playwright-rs
chromiumoxide = { version = "0.5", features = ["tokio-runtime"] }
```

### 3.3 Tools Provided

| Tool | Description | Input | Output |
|------|-------------|-------|--------|
| `browser_navigate` | Navigate to URL | `{ url: string }` | `{ url, title, loaded: bool }` |
| `browser_click` | Click element | `{ selector: string }` | `{ success: bool }` |
| `browser_type` | Type into input | `{ selector: string, text: string }` | `{ success: bool }` |
| `browser_eval` | Evaluate JS | `{ script: string }` | `{ result: any }` |
| `browser_screenshot` | Take screenshot | `{ selector?: string, full_page?: bool }` | `{ data: base64 }` |
| `browser_extract` | Extract data | `{ selector: string, attribute?: string }` | `{ data: string[] }` |

### 3.4 Implementation Notes
- Uses `chromiumoxide` (Rust-native) or `playwright-rs` (bindings)
- Requires Chrome/Chromium installation
- Maintains browser session across tool calls
- Supports screenshots as base64 for LLM consumption

---

## 4. MCP-Memory Server

### 4.1 Purpose
Provides SQLite-based memory operations with embeddings.

### 4.2 Cargo.toml
```toml
[package]
name = "mcp-memory"
version = "1.0.0"
edition = "2021"

[[bin]]
name = "mcp-memory"
path = "src/main.rs"

[dependencies]
mcp-types = { path = "../../src/mcp/types" }
tokio = { version = "1.35", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
anyhow = "1.0"
tracing = "0.1"

# Database
rusqlite = { version = "0.30", features = ["bundled"] }

# Embeddings (optional feature)
# fastembed = { version = "0.1", optional = true }

# Time
chrono = { version = "0.4", features = ["serde"] }

[features]
default = ["sqlite"]
sqlite = []
# chroma = ["dep:fastembed"]  # Future: ChromaDB backend
```

### 4.3 Tools Provided

| Tool | Description | Input | Output |
|------|-------------|-------|--------|
| `memory_store` | Store information | `{ content: string, namespace?: string, metadata?: object }` | `{ id: string }` |
| `memory_recall` | Recall by ID | `{ id: string }` | `{ content, metadata }` |
| `memory_search` | Semantic search | `{ query: string, namespace?: string, limit?: number }` | `{ results: [{id, content, score}] }` |
| `memory_list` | List entries | `{ namespace?: string, limit?: number }` | `{ entries: [{id, content}] }` |
| `memory_delete` | Delete entry | `{ id: string }` | `{ success: bool }` |

### 4.4 Implementation Notes
- SQLite-backed with full-text search
- Embeddings support (optional)
- Namespace isolation per agent
- JSON metadata storage

---

## 5. Pekobot Core Changes

### 5.1 Cargo.toml Updates
```toml
[features]
default = ["core-tools"]
# Full feature set includes all built-in tools (legacy)
full = ["core-tools", "web-tools", "browser-tool", "memory-tool"]
# Minimal core only
core-tools = []
# Optional tool features (to be migrated to MCP)
web-tools = ["scraper", "readability"]
browser-tool = []
memory-tool = []

[dependencies]
# Web dependencies (make optional)
scraper = { version = "0.19", optional = true }
readability = { version = "0.3", optional = true }
```

### 5.2 Tool Factory Changes
```rust
impl ToolFactory {
    pub async fn create_tools(config: &ToolFactoryConfig) -> Vec<Arc<dyn Tool>> {
        let mut tools = create_core_tools(config);
        
        // Try MCP first
        if config.use_mcp {
            tools.extend(load_mcp_tools(config).await);
        }
        
        // Fallback to built-ins if MCP not available and features enabled
        #[cfg(feature = "web-tools")]
        if !has_mcp_web(&tools) {
            tools.extend(create_embedded_web_tools(config));
            warn!("Using deprecated embedded web tools");
        }
        
        tools
    }
}
```

### 5.3 MCP Auto-Discovery
```rust
/// Auto-detect and connect to MCP servers
pub async fn discover_mcp_servers(&self) -> Result<Vec<Arc<dyn Tool>>> {
    let mut tools = vec![];
    
    // Check for mcp-web
    if let Some(client) = self.try_connect_mcp("web").await? {
        tools.extend(client.list_tools().await?);
    }
    
    // Check for mcp-browser
    if let Some(client) = self.try_connect_mcp("browser").await? {
        tools.extend(client.list_tools().await?);
    }
    
    // Check for mcp-memory
    if let Some(client) = self.try_connect_mcp("memory").await? {
        tools.extend(client.list_tools().await?);
    }
    
    Ok(tools)
}
```

---

## 6. Migration Strategy

### 6.1 Phase 1: Development (Week 1)
- Create `mcp-servers/` directory structure
- Implement `mcp-web` with all three tools
- Test MCP server independently

### 6.2 Phase 2: Integration (Week 2)
- Add optional features to pekobot Cargo.toml
- Implement MCP auto-discovery in ToolFactory
- Add fallback logic (MCP → built-in)
- Test with both MCP and built-in modes

### 6.3 Phase 3: Migration Path (Week 3)
- Create `pekobot mcp install <name>` command
- Add `mcp-browser` and `mcp-memory`
- Write migration documentation
- Deprecation warnings for embedded tools

### 6.4 User Migration Guide

**Fresh Install (Recommended):**
```bash
# Install pekobot
cargo install pekobot

# Install MCP servers
pekobot mcp install web
pekobot mcp install browser
pekobot mcp install memory

# Verify
pekobot mcp list
```

**Existing Users:**
```bash
# One-command migration
pekobot migrate-to-mcp

# Or manual:
pekobot mcp install web browser memory
# Edit config to use MCP mode
pekobot config set tools.mode mcp
```

---

## 7. Testing Strategy

### 7.1 Unit Tests
```rust
#[tokio::test]
async fn test_web_search() {
    let tool = WebSearchTool::new();
    let result = tool.execute(json!({
        "query": "rust programming",
        "count": 5
    })).await.unwrap();
    
    assert!(result["results"].as_array().unwrap().len() > 0);
}
```

### 7.2 Integration Tests
```rust
#[tokio::test]
async fn test_mcp_server_lifecycle() {
    // Start MCP server
    let mut server = McpWebServer::new();
    
    // Test tool listing
    let tools = server.list_tools().await.unwrap();
    assert_eq!(tools.len(), 3);
    
    // Test tool execution
    let result = server.call_tool("web_search", params).await.unwrap();
    assert!(result["results"].is_array());
}
```

### 7.3 E2E Tests
```bash
#!/bin/bash
# test_mcp_migration.sh

# Start MCP server
mcp-web &
MCP_PID=$!

# Test through pekobot
pekobot agent create test-agent
pekobot agent start test-agent -M "Search for Rust programming language"

# Cleanup
kill $MCP_PID
```

---

## 8. Success Criteria

- [ ] `mcp-web` server runs standalone
- [ ] `mcp-browser` server runs standalone
- [ ] `mcp-memory` server runs standalone
- [ ] Pekobot can discover and use MCP servers
- [ ] Binary size reduced by >40% (8MB → <5MB)
- [ ] All existing tests pass
- [ ] Migration command works
- [ ] Documentation complete

---

## 9. Risks & Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| MCP server startup time | Medium | Connection pooling, lazy init |
| Breaking existing agents | High | Backward compat, fallback mode |
| Chrome dependency (browser) | Medium | Clear error messages, optional install |
| Feature parity | Low | Thorough testing of all tool functions |

---

## References

- [GAP-008 Original](./GAP-008-tool-migration.md)
- [Tool Inventory Analysis](./TOOL_INVENTORY_ANALYSIS.md)
- [MCP Specification](https://modelcontextprotocol.io/)
- Current tools: `src/tools/{web_search,fetch,browser,memory}.rs`
