# Essential Tools Addition Plan

## Goal

Add core tools to match OpenClaw's out-of-box experience. These are native Rust implementations of frequently-used capabilities.

## Tools to Add

### 1. `web_search` - Web Search Tool

**Purpose:** Search the web using Brave Search or DuckDuckGo

**Interface:**
```rust
pub struct WebSearchTool {
    provider: SearchProvider,
    api_key: Option<String>,
}

pub enum SearchProvider {
    Brave,
    DuckDuckGo,  // No API key needed
}

#[derive(Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    pub count: Option<u32>,  // 1-10, default 5
    pub freshness: Option<String>,  // "pd", "pw", "pm", "py"
}

#[derive(Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}
```

**Implementation notes:**
- Brave Search API: `https://api.brb.lt/v1/search?q={query}&count={count}`
- DuckDuckGo: HTML scraping or use `duckduckgo-rs` crate
- Cache results for 15 minutes
- Rate limit: 5 requests/minute per provider

**Config:**
```toml
[tools.web_search]
enabled = true
provider = "brave"  # or "duckduckgo"
api_key = "${secret:BRAVE_API_KEY}"
max_results = 10
cache_ttl_seconds = 900
```

---

### 2. `apply_patch` - Multi-File Code Editing

**Purpose:** Apply structured patches across one or more files (like `git apply` but AI-friendly)

**Interface:**
```rust
pub struct ApplyPatchTool;

#[derive(Serialize, Deserialize)]
pub struct PatchArgs {
    pub patches: Vec<FilePatch>,
    pub dry_run: Option<bool>,  // Preview changes without applying
}

#[derive(Serialize, Deserialize)]
pub struct FilePatch {
    pub path: String,
    pub old_content: String,  // Exact match required
    pub new_content: String,
}

#[derive(Serialize, Deserialize)]
pub struct PatchResult {
    pub applied: Vec<String>,     // Successfully patched files
    pub failed: Vec<PatchError>,  // Failed patches
}
```

**Implementation notes:**
- Exact match required for `old_content` (no fuzzy matching)
- Atomic: all patches succeed or none do (transaction)
- Create backup files (`.bak`) before modifying
- Support for creating new files (empty `old_content`)
- Support for deleting files (empty `new_content`)
- Workspace-only by default (no escaping project root)

**Safety:**
```toml
[tools.apply_patch]
enabled = true
workspace_only = true  # Default: true
allow_deletion = false # Default: false (safer)
```

---

### 3. `fetch` - Simple HTTP Fetch (Lighter than Browser)

**Purpose:** Quick HTTP requests without full browser overhead

**Why separate from `http` tool?**
- `http` is for API calls (structured, auth)
- `fetch` is for quick retrieval (HTML → markdown)

**Interface:**
```rust
pub struct FetchTool;

#[derive(Serialize, Deserialize)]
pub struct FetchArgs {
    pub url: String,
    pub extract_mode: Option<ExtractMode>,  // markdown (default) or text
    pub max_chars: Option<usize>,           // Default: 50000
}

#[derive(Serialize, Deserialize)]
pub struct FetchResult {
    pub url: String,
    pub title: Option<String>,
    pub content: String,
    pub content_type: String,
}
```

**Implementation notes:**
- Use `reqwest` for HTTP
- Use `readable` or `html2text` for extraction
- Handle redirects (max 5)
- Respect robots.txt (optional)
- Cache for 15 minutes

**Config:**
```toml
[tools.fetch]
enabled = true
max_chars_cap = 50000
cache_ttl_seconds = 900
respect_robots_txt = true
```

---

## Implementation Order

### Phase 1: Web Search (2-3 days)
1. Create `src/tools/web_search.rs`
2. Implement Brave Search API client
3. Implement DuckDuckGo scraping (fallback)
4. Add caching layer
5. Add rate limiting
6. Tests

### Phase 2: Apply Patch (2-3 days)
1. Create `src/tools/apply_patch.rs`
2. Implement patch application logic
3. Add atomic transaction support
4. Add backup creation
5. Add workspace-only enforcement
6. Tests

### Phase 3: Fetch (1-2 days)
1. Create `src/tools/fetch.rs`
2. Implement HTTP client
3. Add HTML → markdown extraction
4. Add caching
5. Tests

---

## Files to Create/Modify

### New Files
```
src/tools/
├── web_search.rs      # NEW
├── apply_patch.rs     # NEW
├── fetch.rs           # NEW
└── mod.rs             # MODIFY - add exports
```

### Config Updates
```rust
// src/config.rs
pub struct ToolsConfig {
    // existing...
    pub web_search: Option<WebSearchConfig>,
    pub apply_patch: Option<ApplyPatchConfig>,
    pub fetch: Option<FetchConfig>,
}
```

### Tool Registration
```rust
// src/tools/mod.rs
pub mod apply_patch;
pub mod fetch;
pub mod web_search;

pub use apply_patch::ApplyPatchTool;
pub use fetch::FetchTool;
pub use web_search::WebSearchTool;
```

---

## Testing Strategy

### Unit Tests
- Parse search responses
- Apply patches (success/failure cases)
- Extract HTML to markdown
- Cache hit/miss logic

### Integration Tests
- Real Brave Search API call (mocked)
- Real DuckDuckGo scrape
- Apply patch to temp directory
- Fetch real URL

### E2E Tests
- Agent uses web_search in conversation
- Agent applies patch to fix code
- Agent fetches documentation

---

## Success Criteria

| Tool | Criteria |
|------|----------|
| `web_search` | Returns 5-10 results in <2s, cached |
| `apply_patch` | Applies multi-file changes atomically |
| `fetch` | Retrieves HTML as markdown in <3s |

---

## Deferred (Skills Phase)

These become skills later:
- `calendar` (complex OAuth)
- `email` (IMAP/SMTP)
- `social_media` (multiple APIs)
- `expense`/`inventory` (domain-specific)

---

## Timeline

| Phase | Tool | Duration |
|-------|------|----------|
| 1 | web_search | 2-3 days |
| 2 | apply_patch | 2-3 days |
| 3 | fetch | 1-2 days |

**Total: 1 week**

---

## Dependencies to Add

```toml
[dependencies]
# Web search
brave-search = "0.2"  # or custom impl with reqwest

# HTML extraction
readable = "0.5"
html2text = "0.13"

# Rate limiting
governor = "0.8"

# Caching (if not already present)
moka = "0.12"
```

---

*Drafted: 2026-02-24*
*Status: Ready to implement*
