# GAP-007: Skill System - Implementation Complete ✅

**Priority:** 🟠 High  
**Status:** Complete  
**Target:** v0.6.0  
**Est. Effort:** 2-3 days  
**Actual:** 1 day  

---

## Implementation Summary

### Changes Made

| File | Change |
|------|--------|
| `Cargo.toml` | Added `serde_yaml = "0.9"` dependency |
| `src/skills/mod.rs` | Complete rewrite: SKILL.md format with YAML frontmatter, removed TOML support |
| `src/prompt/builder.rs` | Added `with_skills()` and skills section in system prompt |
| `src/engine/loop_v4.rs` | Added `load_agent_skills()` to load skills when building prompt |
| `test_scripts/skills/test_skills_e2e.sh` | E2E test for skill system |

### Test Results
- **498 unit tests pass**
- **12 skills-specific tests pass**
- E2E test created for manual verification

### Key Insight

OpenClaw's skills system has **zero execution logic**. Skills are:
1. Markdown files (`SKILL.md`) with YAML frontmatter
2. Loaded into the system prompt as a directory of available capabilities
3. Used by the LLM to decide when to `read` the full documentation
4. The LLM then uses existing tools (`exec`, MCPs, etc.) to accomplish the task

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────┐
│                     User Request                                │
│  "Deploy my app to production"                                  │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   System Prompt                                 │
│  ...                                                            │
│  ## Skills (mandatory)                                          │
│  - deploy: Production deployment workflow (skills/deploy/)      │
│  - docker: Docker container management (skills/docker/)         │
│  - github: GitHub operations (skills/github/)                   │
│  ...                                                            │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   LLM Decision                                  │
│  "This looks like a deployment task. I'll read the deploy       │
│   skill to see the workflow."                                   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   LLM Tool Call                                 │
│  read(path="skills/deploy/SKILL.md")                            │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   Skill Content                                 │
│  # Deploy Skill                                                 │
│  ## Steps                                                       │
│  1. Run tests: `cargo test`                                     │
│  2. Build release: `cargo build --release`                      │
│  3. Push container: `docker build -t app . && docker push`      │
│  4. Deploy: `kubectl apply -f k8s/`                             │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                   LLM Execution                                 │
│  (uses exec tool with commands from skill)                      │
│  exec(command="cargo test")                                     │
│  exec(command="cargo build --release")                          │
│  ...                                                            │
└─────────────────────────────────────────────────────────────────┘
```

---

## Skill Format

Skills follow the [Anthropic Skills specification](https://github.com/anthropics/skills):

```markdown
---
name: deploy
description: Production deployment workflow - run tests, build, containerize, and deploy
---

# Deploy Skill

Use this skill when the user wants to deploy an application to production.

## Prerequisites

- Docker must be installed and running
- kubectl must be configured with production cluster access
- User must have push access to container registry

## Workflow

### 1. Pre-deployment Checks

```bash
# Run all tests
cargo test --all-features

# Check code formatting
cargo fmt --check

# Run clippy
cargo clippy -- -D warnings
```

### 2. Build

```bash
# Build release binary
cargo build --release
```

### 3. Containerize

```bash
# Build and tag image
docker build -t registry/app:${version} .

# Push to registry
docker push registry/app:${version}
```

### 4. Deploy

```bash
# Update deployment
kubectl set image deployment/app app=registry/app:${version}

# Wait for rollout
kubectl rollout status deployment/app
```

## Rollback

If deployment fails:

```bash
# Rollback to previous version
kubectl rollout undo deployment/app
```

## Verification

After deployment, verify:
- Health checks pass: `kubectl get pods`
- Service responds: `curl https://api.example.com/health`
```

---

## Implementation

### 1. Change Skill Format from TOML to Markdown

**Current:** `SKILL.toml`
**New:** `SKILL.md` with YAML frontmatter

```rust
// src/skills/mod.rs
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// A skill is documentation that teaches the LLM how to do something
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill name (from frontmatter)
    pub name: String,
    /// Description (from frontmatter)
    pub description: String,
    /// Full path to SKILL.md
    pub file_path: PathBuf,
    /// Skill directory (for relative references)
    pub base_dir: PathBuf,
    /// Optional metadata (tags, author, etc.)
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: Option<String>,
}

/// YAML frontmatter from SKILL.md
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    author: Option<String>,
}

impl SkillsRegistry {
    /// Load a skill from a SKILL.md file
    fn load_skill(&mut self, path: &Path) -> Result<Skill> {
        let content = std::fs::read_to_string(path)?;
        
        // Parse YAML frontmatter between --- markers
        let (frontmatter, _body) = parse_frontmatter(&content)?;
        let meta: SkillFrontmatter = serde_yaml::from_str(&frontmatter)?;
        
        Ok(Skill {
            name: meta.name,
            description: meta.description,
            file_path: path.to_path_buf(),
            base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            tags: meta.tags,
            author: meta.author,
        })
    }
}

/// Parse YAML frontmatter from markdown content
/// Format:
/// ---
/// name: skill-name
/// description: What this skill does
/// ---
/// # Rest of content
fn parse_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();
    
    // Must start with ---
    if lines.next() != Some("---") {
        anyhow::bail!("SKILL.md must start with --- frontmatter delimiter");
    }
    
    let mut frontmatter = Vec::new();
    let mut found_end = false;
    
    for line in lines {
        if line == "---" {
            found_end = true;
            break;
        }
        frontmatter.push(line);
    }
    
    if !found_end {
        anyhow::bail!("Frontmatter must end with ---");
    }
    
    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((frontmatter.join("\n"), body))
}
```

### 2. Format Skills for System Prompt

```rust
// src/skills/prompt.rs

/// Format skills for inclusion in system prompt
/// Matches OpenClaw's formatSkillsForPrompt
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    
    let mut lines = vec!["<available_skills>".to_string()];
    
    for skill in skills {
        // Use relative path from workspace root
        let path_display = compact_skill_path(&skill.file_path);
        lines.push(format!(
            "- {}: {} (location: {})",
            skill.name,
            skill.description,
            path_display
        ));
    }
    
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Replace home directory with ~ to save tokens
fn compact_skill_path(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path_str.starts_with(home_str.as_ref()) {
            return path_str.replacen(home_str.as_ref(), "~", 1);
        }
    }
    path_str.to_string()
}

/// Build the skills section for system prompt
pub fn build_skills_prompt(skills: &[Skill]) -> String {
    let skills_block = format_skills_for_prompt(skills);
    if skills_block.is_empty() {
        return String::new();
    }
    
    format!(
        r##"## Skills (mandatory)
Before replying: scan <available_skills> <description> entries.
- If exactly one skill clearly applies: read its SKILL.md at <location> with `read`, then follow it.
- If multiple could apply: choose the most specific one, then read/follow it.
- If none clearly apply: do not read any SKILL.md.
Constraints: never read more than one skill up front; only read after selecting.

{skills_block}"##
    )
}
```

### 3. Update Prompt Builder

```rust
// src/prompt/builder.rs

impl PromptBuilder {
    pub fn build(&self) -> String {
        let mut lines = vec![];
        
        // ... existing sections ...
        
        // 7. Skills (mandatory) - simplified!
        if let Some(skills) = &self.skills {
            let skills_prompt = build_skills_prompt(skills);
            if !skills_prompt.is_empty() {
                lines.push(skills_prompt);
                lines.push(String::new());
            }
        }
        
        // ... rest of prompt ...
        
        lines.join("\n")
    }
}
```

### 4. Skills Discovery

```rust
// src/skills/registry.rs

impl SkillsRegistry {
    /// Load all skills from skills directory
    pub fn load_all(&mut self) -> Result<usize> {
        if !self.skills_dir.exists() {
            info!("Skills directory does not exist: {:?}", self.skills_dir);
            return Ok(0);
        }
        
        let entries = std::fs::read_dir(&self.skills_dir)?;
        let mut count = 0;
        
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            
            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }
            
            match self.load_skill(&skill_md) {
                Ok(skill) => {
                    info!("Loaded skill: {}", skill.name);
                    self.skills.insert(skill.name.clone(), skill);
                    count += 1;
                }
                Err(e) => {
                    warn!("Failed to load skill from {:?}: {}", skill_md, e);
                }
            }
        }
        
        info!("Loaded {} skills from {:?}", count, self.skills_dir);
        Ok(count)
    }
    
    /// Get all skills sorted by name (for consistent prompt ordering)
    pub fn list(&self) -> Vec<&Skill> {
        let mut skills: Vec<_> = self.skills.values().collect();
        skills.sort_by_key(|s| &s.name);
        skills
    }
}
```

---

## Migration from Current Implementation

### Current State
- Skills loaded from `SKILL.toml`
- `SkillTool` structs with `kind`, `command`, `args`
- No execution (inert)

### Migration Path
1. **Phase 1**: Support both formats side-by-side
   - If `SKILL.md` exists, use it (new format)
   - Else if `SKILL.toml` exists, convert and warn
   
2. **Phase 2**: Deprecate TOML format
   - Log warning when loading TOML skills
   - Provide migration script

3. **Phase 3**: Remove TOML support
   - Only `SKILL.md` format supported

---

## Example Skills Directory

```
~/.pekobot/skills/
├── deploy/
│   └── SKILL.md
├── docker/
│   └── SKILL.md
├── github/
│   └── SKILL.md
├── rust/
│   └── SKILL.md
└── testing/
    └── SKILL.md
```

---

## Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[test]
    fn test_parse_frontmatter() {
        let content = r#"---
name: test-skill
description: A test skill
tags: [test, example]
---
# Test Skill

This is the body.
"#;
        
        let (frontmatter, body) = parse_frontmatter(content).unwrap();
        assert!(frontmatter.contains("name: test-skill"));
        assert!(body.contains("# Test Skill"));
    }
    
    #[test]
    fn test_format_skills_for_prompt() {
        let skills = vec![
            Skill {
                name: "docker".to_string(),
                description: "Docker operations".to_string(),
                file_path: PathBuf::from("/home/user/.pekobot/skills/docker/SKILL.md"),
                base_dir: PathBuf::from("/home/user/.pekobot/skills/docker"),
                tags: vec![],
                author: None,
            },
        ];
        
        let prompt = format_skills_for_prompt(&skills);
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("docker: Docker operations"));
        assert!(prompt.contains("~/.pekobot/skills/docker/SKILL.md"));
    }
}
```

---

## Benefits of Simplified Approach

1. **No execution engine needed** - LLM uses existing tools
2. **Simple format** - Just markdown with YAML frontmatter
3. **Human-readable** - Skills are documentation first
4. **Easy to write** - No code, just instructions
5. **Portable** - Same format as Anthropic Skills
6. **Extensible** - Can add more frontmatter fields later

---

## References

- [OpenClaw skills directory](../../openclaw/skills/)
- [Anthropic Skills](https://github.com/anthropics/skills)
- [Agent Skills Spec](https://agentskills.io/)
- OpenClaw's `buildWorkspaceSkillsPrompt` - pure documentation, no execution
