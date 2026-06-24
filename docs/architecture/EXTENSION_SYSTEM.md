# Peko Extension System

**Version:** 0.1.0 (Post-ADR-017 Implementation)
**Date:** 2026-06-23 (review pass)
**Status:** Current  

---

## Overview

The Peko Extension System provides a unified architecture for adding capabilities to agents. All extensions—whether tools, skills, MCP servers, or gateways—use the same hook-based registration mechanism and lifecycle management.

---

## Extension Types

### 1. Type-Specific Adapters (Guided Approach)

For common extension types, Peko provides guided adapters with constrained hook points:

| Extension Type | Manifest | Primary Hooks | Best For |
|----------------|----------|---------------|----------|
| **Skill** | `SKILL.md` | `PromptSystemSection` | Teaching agents specific behaviors |
| **MCP Server** | `config.json` | `ToolRegister`, `ToolExecute`, `AgentInit`, `AgentShutdown` | External tool servers |
| **Universal Tool** | `manifest.json` | `ToolRegister`, `ToolExecute` | Executable command-line tools |
| **Channel** | `CHANNEL.toml` | `ChannelInput`, `ChannelOutput` | I/O adapters |
| **Hook** | `HOOK.toml` | `EventSubscribe`, `EventEmit` | Event-driven triggers |
| **Gateway** | `GATEWAY.toml` | `ChannelInput`, `ChannelOutput`, `EventEmit` | Platform integrations |
| **Builtin Tool** | Native code | `ToolRegister`, `ToolExecute` | Core built-in tools |

### 2. General Extension Adapter (Power User Approach)

For advanced use cases, the General Extension Adapter provides access to all 22 hook points:

```yaml
# extension.yaml
---
id: "advanced-deploy-helper"
name: "Advanced Deployment Helper"
version: "1.0.0"
extension_type: "general"

hooks:
  - point: "prompt.system_section"
    section: "deployment"
    priority: 100
    handler: "generate_deployment_guide"
    
  - point: "tool.execute"
    tool_name: "deploy:*"
    handler: "handle_deploy_tool"
    
  - point: "event.subscribe"
    topic_pattern: "instance.created"
    handler: "on_instance_created"
    
  - point: "agent.init"
    handler: "initialize_state"
```

---

## The 22 Hook Points

### Prompt Lifecycle

| Hook Point | Description | Payload |
|------------|-------------|---------|
| `PromptSystemSection` | Contribute to system prompt sections | `section: String, priority: i32` |
| `PromptPreProcess` | Modify user input before sending to LLM | User message |
| `PromptPostProcess` | Modify LLM output before returning | Assistant message |

### Tool Lifecycle

| Hook Point | Description | When Invoked |
|------------|-------------|--------------|
| `ToolRegister` | Register tool definitions with LLM | Agent initialization |
| `ToolExecute` | Execute synchronous tool calls | Tool call received |
| `ToolExecuteAsync` | Start async tool operations | Async tool call received |
| `ToolCheckStatus` | Check async operation status | Polling async operation |
| `ToolCancel` | Cancel async operation | Cancellation requested |
| `ToolResultTransform` | Transform tool results | After tool execution |

### Session Lifecycle

| Hook Point | Description | When Invoked |
|------------|-------------|--------------|
| `SessionStateChange` | React to session state changes | State transition |
| `SessionCompaction` | Preserve context during compaction | Compaction triggered |
| `SessionContextBuild` | Modify context before LLM call | Before each LLM request |

### I/O Lifecycle

| Hook Point | Description | When Invoked |
|------------|-------------|--------------|
| `ChannelInput` | Process incoming messages | Message received |
| `ChannelOutput` | Format outgoing messages | Message sent |
| `MessagePreSend` | Intercept before sending | Before transmission |
| `MessagePostReceive` | Process after receiving | After reception |

### Event Lifecycle

| Hook Point | Description | When Invoked |
|------------|-------------|--------------|
| `EventSubscribe` | Subscribe to event topics | Agent initialization |
| `EventEmit` | Emit events to bus | Event published |

### Agent Lifecycle

| Hook Point | Description | When Invoked |
|------------|-------------|--------------|
| `AgentInit` | Initialize extension state | Agent cold-start |
| `AgentShutdown` | Cleanup extension state | Agent shutdown |
| `AgentIteration` | Hook into agentic loop | Each iteration |

---

## Extension Manifest Formats

### Skill Manifest (SKILL.md)

```markdown
---
name: github
description: GitHub CLI operations
version: "1.0.0"
author: Peko Team
---

# GitHub Skill

Use this skill when working with GitHub repositories.

## Commands

```bash
gh pr list --repo owner/repo
gh issue create --title "Bug" --body "Description"
```
```

### MCP Server Manifest (config.json)

```json
{
  "name": "filesystem",
  "description": "File system operations",
  "version": "1.0.0",
  "command": "npx -y @modelcontextprotocol/server-filesystem /tmp",
  "env": {
    "ALLOWED_PATHS": "/tmp,/home/user"
  }
}
```

### Universal Tool Manifest (manifest.json)

```json
{
  "name": "web_search",
  "description": "Search the web for information",
  "version": "1.0.0",
  "parameters": {
    "type": "object",
    "properties": {
      "query": {
        "type": "string",
        "description": "Search query"
      }
    },
    "required": ["query"]
  }
}
```

### General Extension Manifest (extension.yaml)

```yaml
---
id: "my-complex-extension"
name: "My Complex Extension"
version: "1.0.0"
extension_type: "general"
description: "Does multiple things"
author: "Your Name"

hooks:
  - point: "prompt.system_section"
    section: "custom"
    priority: 50
    handler: "add_custom_instructions"
    
  - point: "tool.execute"
    tool_name: "custom:*"
    handler: "handle_custom_tools"
    
  - point: "agent.init"
    handler: "setup_custom_state"
```

---

## Extension Lifecycle

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Discover   │───►│   Install   │───►│   Enable    │───►│   Execute   │
│             │    │             │    │             │    │             │
│ Scan paths  │    │ Copy to     │    │ Register    │    │ Invoke      │
│ Detect type │    │ extensions/ │    │ hooks with  │    │ hooks at    │
│ Parse manifest│  │ directory   │    │ ExtensionCore│   │ lifecycle   │
└─────────────┘    └─────────────┘    └──────┬──────┘    └──────┬──────┘
                                             │                    │
                                        ┌────┴────┐          ┌────┴────┐
                                        │ Disable │◄─────────│  Error  │
                                        │         │          │         │
                                        │ Unreg   │          │ Cleanup │
                                        │ hooks   │          │ Retry   │
                                        └────┬────┘          └─────────┘
                                             │
                                        ┌────┴────┐
                                        │Uninstall│
                                        │         │
                                        │ Remove  │
                                        │ files   │
                                        └─────────┘
```

---

## Creating Extensions

### The Easy Way: `peko ext init` (ADR-036)

Peko provides scaffolding for all extension types. One command creates a working extension:

```bash
# Skill extension
peko ext init my-skill --type skill

# Universal tool with Python handler
peko ext init my-tool --type universal-tool --lang python

# MCP server (bare server.json)
peko ext init my-mcp --type mcp --bare

# Gateway (Discord bot, HTTP webhook, etc.)
peko ext init my-gateway --type gateway --lang javascript --gateway-type out-of-process

# General multi-hook extension
peko ext init my-extension --type general
```

Each command creates a directory with:
- The correct manifest file for the type
- Stub code (when applicable)
- A `README.md` with usage instructions
- A `.gitignore`

### Manual Creation (Expert Mode)

If you prefer to write manifests by hand, follow the format references below.

#### Quick Start: Skill Extension

1. Create directory:
```bash
mkdir -p ~/.peko/extensions/my-skill
```

2. Create SKILL.md:
```bash
cat > ~/.peko/extensions/my-skill/SKILL.md << 'EOF'
---
name: my-skill
description: My custom skill
version: "1.0.0"
---

# My Skill

Instructions for the agent...
EOF
```

3. Enable:
```bash
peko ext enable my-skill
```

#### Quick Start: General Extension

1. Create directory:
```bash
mkdir -p ~/.peko/extensions/my-extension
```

2. Create manifest.yaml:
```bash
cat > ~/.peko/extensions/my-extension/manifest.yaml << 'EOF'
id: my-extension
name: My Extension
version: "1.0.0"
extension_type: general

hooks:
  - point: prompt.system_section
    section: tools
    priority: 100
    handler: my_handler
EOF
```

3. Create handler (Rust, Python, or any executable):
```bash
# Handler receives JSON on stdin, outputs JSON on stdout
```

---

## Extension Manager CLI

### Installation

```bash
# From local directory
peko ext install ./my-extension

# From registry
peko ext install pekohub.com/extensions/my-extension

# From URL
peko ext install https://example.com/my-extension.tar.gz
```

### Management

```bash
# List all extensions
peko ext list

# List with filtering
peko ext list --enabled-only
peko ext list --type skill

# Enable/disable
peko ext enable my-extension
peko ext disable my-extension

# Get info
peko ext info my-extension

# Uninstall
peko ext uninstall my-extension
```

### Validation and Debugging

```bash
# Validate manifest (static check)
peko ext validate ./my-extension

# Semantic validation — checks referenced files, commands in PATH, schemas
peko ext validate ./my-extension --semantic

# Show resolved hooks
peko ext debug my-extension
```

### Bundling and Publishing

```bash
# Create bundle with multiple extensions
peko ext bundle create production-bundle \
    --with skill1 \
    --with mcp-server1 \
    --with tool1

# Install bundle
peko ext bundle install ./production-bundle.tar.gz

# Push to registry
peko ext push my-extension pekohub.com/user/my-extension:latest

# Push with bundled dependencies
peko ext push my-extension pekohub.com/user/my-extension:latest --with-deps
```

---

## Best Practices

### Extension Design

1. **Single Responsibility**: Each extension should do one thing well
2. **Minimal Hook Points**: Use only the hooks you need
3. **Priority Awareness**: Be mindful of hook priority (higher = earlier)
4. **Error Handling**: Handle failures gracefully, don't crash the agent

### Performance

1. **Lazy Initialization**: Do heavy work in `AgentInit`, not at install
2. **Async When Possible**: Use `ToolExecuteAsync` for long operations
3. **Efficient Hook Handlers**: Keep hook handlers fast (<10ms)

### Security

1. **Sandbox Awareness**: Respect agent sandbox boundaries
2. **Input Validation**: Validate all parameters
3. **No Secrets in Code**: Use configuration for credentials

---

## Migration from Legacy Systems

### From Legacy Skills

Old: `~/.peko/skills/my-skill/SKILL.md`  
New: `~/.peko/extensions/my-skill/SKILL.md` (same format!)

Migration is automatic on first run.

### From Legacy Tools

Old: `~/.peko/tools/my-tool/manifest.json`  
New: `~/.peko/extensions/my-tool/manifest.json` (same format!)

Migration is automatic on first run.

### From mcp.toml

Old: `~/.peko/mcp.toml` with multiple servers  
New: Individual extensions in `~/.peko/extensions/<name>/config.json`

Migration is automatic on first run.

---

## Troubleshooting

### Extension Not Loading

1. Check manifest syntax:
   ```bash
   peko ext validate ./my-extension
   ```

2. Check extension is enabled:
   ```bash
   peko ext list
   ```

3. Check logs for errors:
   ```bash
   peko system logs --level error
   ```

### Hook Not Firing

1. Verify hook point name:
   ```bash
   peko ext debug my-extension
   ```

2. Check priority ordering:
   - Higher priority fires first
   - Previous handler may have stopped propagation

3. Enable debug logging:
   ```bash
   RUST_LOG=debug peko agent start my-agent
   ```

---

## Related Documentation

- [ADR-017: Unified Extension Architecture](adr/ADR-017.md)
- [ADR-018a/b/c: Tool execution unification, registry, naming](adr/ADR-018a-tool-execution-unification.md)
- [ADR-024: Unified Extension Manifest](adr/ADR-024-unified-extension-manifest.md)
- [ADR-036: `peko ext init` and semantic validation](adr/ADR-036-extension-developer-experience.md)
- [User's Guide: Extensions section](../user-guide/USERS_GUIDE.md#extensions)

---

*Version 0.1.0 · Unified Extension Architecture · 2026-06-23*
