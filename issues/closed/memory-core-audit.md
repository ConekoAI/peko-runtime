# Memory Core Audit: Migration to Plugin Architecture

## Summary

Per architectural decision, **memory should be a plugin feature** (external MCPs/tools) rather than core functionality. This document audits current core memory implementations for future deprecation.

## Status: COMPLETED

Core memory has been removed from pekobot. Users should use external MCP memory servers instead.

## Changes Made

### Removed
- `src/memory/` - entire directory (sqlite, vector, hybrid, embeddings, hygiene, markdown, types)
- `src/types/memory.rs` - MemoryEntry, MemoryScope, MemoryQuery, MemoryConfig types
- `AgentConfig.memory` field
- `ToolConfig.memory` field
- `MemoryToolConfig` struct
- `Agent::init_memory()`, `Agent::search_memory()`, `Agent::store_memory()` methods
- `pub mod memory` from lib.rs

### Deprecated (Backward Compatible)
- Portable package `include_memory` and `import_memory` options
  - Show deprecation warnings when used
  - No actual memory data is exported/imported
  - Manifest sections kept for backward compatibility with legacy packages

## Migration Path

Users who need memory functionality should use external MCP memory servers:
- `mcp-sqlite-memory` - SQLite-backed memory
- `mcp-vector-memory` - Vector DB memory (Chroma, Qdrant, etc.)
- `mcp-file-memory` - Markdown/file-based memory

## Related

- Portable packages currently bundle: identity, config, skills, workspace, sessions (memory removed)
- Gap: Universal Tools and MCP servers are NOT bundled in portable packages
