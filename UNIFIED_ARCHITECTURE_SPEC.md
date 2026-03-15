# Unified Architecture Specification

> **The Single Source of Truth for Pekobot Technical Implementation**

**Version:** 3.1  
**Date:** 2026-03-15  
**Status:** Specification (Aligned with Current Implementation)

---

## 1. Introduction

### 1.1 Core Philosophy: Filesystem-First

Pekobot uses a **filesystem-first** approach where an agent is simply a directory with a `config.toml` file and optional markdown configuration files.

**Key principle:** Run directly from source. No build step required.

### 1.2 Minimal Agent Structure

Only **two things are mandatory**:

```
my-agent/                    # Agent directory
├── config.toml              # REQUIRED: Agent configuration
└── sessions/                # REQUIRED: System-managed sessions
    └── {uuid}.jsonl
```

Everything else is **optional** - the runtime loads what exists.

### 1.3 Optional Markdown Configuration

Optional markdown files in the root provide agent behavior and personality:

```
my-agent/
├── config.toml              # REQUIRED
├── AGENT.md                 # Optional: Agent behavior description
├── BOOTSTRAP.md             # Optional: Initial system prompt
├── IDENTITY.md              # Optional: Identity/personality definition
├── SOUL.md                  # Optional: Core values/personality traits
├── TOOLS.md                 # Optional: Tool usage guidelines
└── sessions/                # REQUIRED (system-managed)
```

**No `bootstrap/` folder** - markdown files live in the agent root.

### 1.4 Optional Content Folders

Additional folders are loaded if present:

```
my-agent/
├── config.toml              # REQUIRED
├── AGENT.md                 # Optional
├── ...                      # Other .md files
├── projects/                # Optional: Knowledge/workspaces
├── memories/                # Optional: Long-term memory
├── tools/                   # Optional: Custom tools
├── skills/                  # Optional: Reusable skill definitions
├── mcp.json                 # Optional: MCP configurations
└── sessions/                # REQUIRED (system-managed)
```

### 1.5 Running an Agent

```bash
# Development - no build needed!
pekobot run ./my-agent/
pekobot run ./my-agent/ --watch    # Auto-reload

# Package for distribution (when ready to share)
pekobot package build ./my-agent/ -t my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0
```

---

## 2. Configuration Files

### 2.1 config.toml (REQUIRED)

```toml
# config.toml - Minimal required configuration
[agent]
name = "my-agent"
version = "1.0.0"
description = "A helpful assistant"

# Provider configuration
[provider]
provider_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
max_tokens = 4096
temperature = 0.7

# Optional: Inherit from base agent
[base]
agent = "pekohub.com/agents/minimal:v1.0"

# Optional: Capabilities
[capabilities]
tools = ["web_search", "fetch"]
mcps = ["browser"]

# Optional: Resource limits
[resources]
max_concurrent_tools = 5
timeout_seconds = 300
```

### 2.2 Markdown Files (Optional, Loaded Dynamically)

The runtime checks for these files and loads them if present:

| File | Purpose | When Loaded |
|------|---------|-------------|
| `AGENT.md` | Agent behavior description | Startup |
| `BOOTSTRAP.md` | Initial system prompt | Startup |
| `IDENTITY.md` | Identity/personality definition | Startup |
| `SOUL.md` | Core values/personality traits | Startup |
| `TOOLS.md` | Tool usage guidelines | Tool registration |
| `SKILLS.md` | Available skills description | Startup |

Example `AGENT.md`:
```markdown
# Agent: Research Assistant

You are a technical research assistant specializing in software architecture.

## Responsibilities
- Analyze technical documentation
- Synthesize findings
- Cite sources accurately

## Guidelines
- Use technical terminology appropriately
- Provide code examples when relevant
```

Example `IDENTITY.md`:
```markdown
# Identity

Name: Archie
Role: Technical Research Assistant
Personality: Curious, thorough, precise
Communication Style: Clear and concise
```

### 2.3 Content Discovery

The runtime discovers content dynamically:

```rust
// Pseudo-code for agent loading
fn load_agent(path: &Path) -> AgentConfig {
    // 1. config.toml is REQUIRED
    let config = read_config_toml(path.join("config.toml"));
    
    // 2. Load markdown files if present
    let agent_desc = read_if_exists(path.join("AGENT.md"));
    let bootstrap = read_if_exists(path.join("BOOTSTRAP.md"));
    let identity = read_if_exists(path.join("IDENTITY.md"));
    let soul = read_if_exists(path.join("SOUL.md"));
    let tools_desc = read_if_exists(path.join("TOOLS.md"));
    
    // 3. Load optional folders if present
    let projects = read_folder_if_exists(path.join("projects"));
    let memories = read_folder_if_exists(path.join("memories"));
    let tools = discover_tools(path.join("tools"));
    
    AgentConfig {
        config,
        agent_desc,
        bootstrap,
        identity,
        soul,
        tools_desc,
        projects,
        memories,
        tools,
    }
}
```


---

## 3. Runtime Loading

### 3.1 FilesystemLoader Implementation

```rust
// src/infrastructure/filesystem/loader.rs

pub struct FilesystemLoader;

impl FilesystemLoader {
    /// Load agent from directory
    /// Only config.toml and sessions/ are required
    pub async fn load(path: &Path) -> Result<AgentConfig, LoadError> {
        // REQUIRED: Load config.toml
        let config = Self::read_config_toml(path.join("config.toml")).await?;
        
        // OPTIONAL: Load markdown files (present or not)
        let markdown_files = Self::load_markdown_files(path).await;
        
        // OPTIONAL: Load content folders (present or not)
        let content = Self::load_content_folders(path).await;
        
        // OPTIONAL: Load base agent if specified
        let base = if let Some(base_ref) = &config.base {
            Some(Self::load_base(base_ref).await?)
        } else {
            None
        };
        
        // Merge: base + local (local takes precedence)
        Ok(AgentConfig {
            config,
            markdown_files,
            content,
            base,
        })
    }
    
    /// Load all .md files from agent root (optional)
    async fn load_markdown_files(path: &Path) -> HashMap<String, String> {
        let mut files = HashMap::new();
        
        // Known markdown files
        let known_files = [
            "AGENT.md",
            "BOOTSTRAP.md", 
            "IDENTITY.md",
            "SOUL.md",
            "TOOLS.md",
            "SKILLS.md",
        ];
        
        for filename in &known_files {
            let filepath = path.join(filename);
            if filepath.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&filepath).await {
                    files.insert(filename.to_string(), content);
                }
            }
        }
        
        files
    }
    
    /// Load optional content folders
    async fn load_content_folders(path: &Path) -> ContentFolders {
        ContentFolders {
            projects: Self::read_folder_if_exists(path.join("projects")).await,
            memories: Self::read_folder_if_exists(path.join("memories")).await,
            tools: Self::discover_tools(path.join("tools")).await,
            skills: Self::read_folder_if_exists(path.join("skills")).await,
        }
    }
    
    /// Read folder contents if it exists
    async fn read_folder_if_exists(path: PathBuf) -> Option<FolderContent> {
        if !path.exists() || !path.is_dir() {
            return None;
        }
        
        // Read and index folder contents
        Some(Self::index_folder(&path).await.ok()?)
    }
    
    /// Discover tools in tools/ folder
    async fn discover_tools(path: PathBuf) -> Vec<Tool> {
        if !path.exists() {
            return Vec::new();
        }
        
        let mut tools = Vec::new();
        let mut entries = tokio::fs::read_dir(&path).await.ok()?;
        
        while let Some(entry) = entries.next_entry().await.ok()? {
            let filepath = entry.path();
            if filepath.is_file() {
                // Check if executable
                if let Ok(metadata) = tokio::fs::metadata(&filepath).await {
                    if metadata.permissions().mode() & 0o111 != 0 {
                        if let Some(tool) = Self::register_tool(&filepath).await {
                            tools.push(tool);
                        }
                    }
                }
            }
        }
        
        tools
    }
}
```

### 3.2 Config Merging

When a base agent is specified, configurations merge:

```rust
impl AgentConfig {
    /// Merge base config with local config
    /// Local always takes precedence
    fn merge(self, base: AgentConfig) -> Self {
        AgentConfig {
            config: self.config,  // Local config.toml wins
            markdown_files: {
                let mut merged = base.markdown_files;
                merged.extend(self.markdown_files);  // Local overrides
                merged
            },
            content: ContentFolders {
                projects: self.projects.or(base.projects),
                memories: self.memories.or(base.memories),
                tools: [base.tools, self.tools].concat(),
                skills: self.skills.or(base.skills),
            },
            base: None,  // Base already resolved
        }
    }
}
```


---

## 4. Optional Packaging for Distribution

### 4.1 When to Package

Package when you need to:
- Share with others
- Deploy to production
- Version and distribute
- Run on different machines

### 4.2 Package Build Process

```rust
// src/infrastructure/package/builder.rs

pub struct PackageBuilder;

impl PackageBuilder {
    /// Build package from filesystem directory
    pub async fn build(source: &Path, tag: &str) -> Result<Package, BuildError> {
        // 1. Load from filesystem (same as development)
        let config = FilesystemLoader::load(source).await?;
        
        // 2. Create content-addressable layers
        let layers = vec![
            self.create_layer(&config.config, "config").await?,
            self.create_layer(&config.markdown_files, "markdown").await?,
            self.create_layer(&config.content.projects, "projects").await?,
            self.create_layer(&config.content.memories, "memories").await?,
            self.create_layer(&config.content.tools, "tools").await?,
        ];
        
        // 3. Create package manifest
        let manifest = PackageManifest {
            name: config.config.agent.name.clone(),
            version: config.config.agent.version.clone(),
            layers: layers.iter().map(|l| l.digest.clone()).collect(),
        };
        
        // 4. Create tarball
        Ok(self.create_tarball(manifest, layers).await?)
    }
}
```

### 4.3 CLI Commands

```bash
# Development - run directly from filesystem
pekobot run ./my-agent/
pekobot run ./my-agent/ --watch

# Distribution - package when needed
pekobot package build ./my-agent/ -t my-agent:v1.0
pekobot package inspect my-agent:v1.0
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0
pekobot pull pekohub.com/agents/researcher:v2.0

# Run packaged agent
pekobot run my-agent:v1.0
```

---

## 5. Team Composition

### 5.1 Team YAML

```yaml
# team.yaml
team: research-team

agents:
  # Local filesystem agent
  coordinator:
    source: ./agents/coordinator/    # Points to directory with config.toml
    instances: 1
    
  # Packaged agent
  researcher:
    source: pekohub.com/agents/researcher:v2.5
    instances: 3

shared:
  memory:
    type: chroma
```

### 5.2 Team Runtime

```rust
impl TeamService {
    async fn start_agent(&self, spec: &AgentSpec) -> Result<AgentInstance, TeamError> {
        let agent = if spec.source.starts_with("./") || spec.source.starts_with("/") {
            // Filesystem agent - load directly
            FilesystemLoader::load(Path::new(&spec.source)).await?
        } else {
            // Packaged agent - load from registry/cache
            PackageLoader::load(&spec.source).await?
        };
        
        AgentInstance::start(agent).await
    }
}
```

---

## 6. Architecture Layers

### 6.1 Directory Structure

```
src/
├── domain/
│   ├── agent.rs, session.rs, tool.rs, events.rs
│
├── application/
│   ├── ports/
│   │   ├── session_port.rs, tool_port.rs
│   └── services/
│       ├── agent_service.rs
│       └── team_service.rs
│
└── infrastructure/
    ├── filesystem/
    │   ├── loader.rs          # Load agent directory
    │   └── watcher.rs         # Auto-reload
    ├── package/
    │   └── builder.rs         # Optional packaging
    ├── team/
    ├── gateway/
    └── persistence/
```

---

## 7. Comparison: Approaches

### 7.1 Mandatory vs Optional

| Component | Status | Notes |
|-----------|--------|-------|
| `config.toml` | **REQUIRED** | Must exist |
| `sessions/` | **REQUIRED** | Created automatically |
| `AGENT.md` | Optional | Loaded if exists |
| `BOOTSTRAP.md` | Optional | Loaded if exists |
| `IDENTITY.md` | Optional | Loaded if exists |
| `SOUL.md` | Optional | Loaded if exists |
| `TOOLS.md` | Optional | Loaded if exists |
| `projects/` | Optional | Loaded if exists |
| `memories/` | Optional | Loaded if exists |
| `tools/` | Optional | Discovered if exists |
| `skills/` | Optional | Loaded if exists |
| `mcp.json` | Optional | Loaded if exists |

### 7.2 Development Flow

```
Create directory
    │
    ▼
Create config.toml (required)
    │
    ▼
Optionally add markdown files
    │
    ▼
Optionally add folders (projects, memories, tools)
    │
    ▼
Run: pekobot run ./
    │
    ▼
Package when ready to share: pekobot package build ./ -t name:version
```

---

## 8. Summary

### Key Principles

1. **Minimal Requirements**: Only `config.toml` and `sessions/` are mandatory
2. **Optional Everything**: Markdown files and folders loaded if present
3. **No `bootstrap/`**: Markdown files in agent root
4. **Filesystem-First**: Run directly from directory
5. **Optional Packaging**: Package only for distribution

### Benefits

- Simple to get started (just create `config.toml`)
- Flexible (add files/folders as needed)
- Git-friendly (markdown files, clear structure)
- Editor-friendly (any text editor works)
- No build step for development

---

*Version: 3.1*  
*Last Updated: 2026-03-15*  
*Status: Aligned with Current Implementation*
