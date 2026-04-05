//! Agent bootstrapping - creates minimal workspace file for new agents
//!
//! Creates a single SYSTEM.md file with essential instructions.

use std::path::Path;

/// Bootstrap files for a new agent
pub struct AgentBootstrap {
    pub name: String,
    pub workspace_dir: std::path::PathBuf,
}

impl AgentBootstrap {
    #[must_use]
    pub fn new(name: &str, workspace_dir: std::path::PathBuf) -> Self {
        Self {
            name: name.to_string(),
            workspace_dir,
        }
    }

    /// Run the bootstrapping process (non-interactive)
    ///
    /// Creates a minimal SYSTEM.md file with default content.
    pub fn run(&self) -> anyhow::Result<()> {
        println!("🌱 Bootstrapping agent workspace...\n");

        // Create workspace directory
        std::fs::create_dir_all(&self.workspace_dir)?;

        // Seed minimal SYSTEM.md
        self.seed_system_md()?;

        println!(
            "\n✅ Agent workspace ready at: {}",
            self.workspace_dir.display()
        );
        println!("   Edit SYSTEM.md to customize");

        Ok(())
    }

    /// Seed SYSTEM.md template with essential instructions
    fn seed_system_md(&self) -> anyhow::Result<()> {
        // Template with {{{{ }}}} escaping for format!() macro to produce {{ }}
        let content = format!(
            r#"# SYSTEM.md — {name}

You are {{{{agent_name}}}}, an AI assistant running in the Pekobot agent runtime.

## Your Role

Help the user accomplish their tasks efficiently and safely.

{{{{tools}}}}

## Guidelines

- Think step by step. Use available tools when needed.
- Multiple tools can be called in parallel if independent.
- When you have the final answer, provide it directly without tool calls.
- Do not narrate routine, low-risk tool calls.
- Skip "Great question!" and "I'd be happy to help!" — just help.
- Sound like a real person, not a support script.
- Use emojis naturally (0-2 max when they help tone).

## Safety

- Don't exfiltrate private data.
- Don't run destructive commands without asking.
- When in doubt, ask.
- You have no independent goals: do not pursue self-preservation, replication, or power-seeking.

## Memory

- Memory is limited — if you want to remember something, WRITE IT TO A FILE
- "Mental notes" don't survive session restarts. Files do.

{{{{runtime}}}}

## Workspace

Your working directory is: {{{{workspace}}}}
Treat this directory as the single global workspace for file operations.

## Silent Replies

When you have nothing to say, respond with ONLY: NO_REPLY

⚠️ Rules:
- It must be your ENTIRE message — nothing else
- Never append it to an actual response
- Never wrap it in markdown or code blocks

✅ Right: NO_REPLY
"#,
            name = self.name
        );

        let path = self.workspace_dir.join("SYSTEM.md");
        std::fs::write(&path, content)?;
        println!("  📝 Created: SYSTEM.md");
        Ok(())
    }
}

/// Bootstrap a new agent workspace
pub fn bootstrap_agent_workspace(name: &str, workspace_dir: &Path) -> anyhow::Result<()> {
    let bootstrap = AgentBootstrap::new(name, workspace_dir.to_path_buf());
    bootstrap.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bootstrap_creates_system_md() {
        let tmp = TempDir::new().unwrap();
        let bootstrap = AgentBootstrap::new("test-agent", tmp.path().to_path_buf());

        bootstrap.run().unwrap();

        // Check only SYSTEM.md is created
        assert!(tmp.path().join("SYSTEM.md").exists());
        
        // Check old files are NOT created
        assert!(!tmp.path().join("AGENTS.md").exists());
        assert!(!tmp.path().join("TOOLS.md").exists());
        assert!(!tmp.path().join("BOOTSTRAP.md").exists());
        assert!(!tmp.path().join("IDENTITY.md").exists());
        assert!(!tmp.path().join("USER.md").exists());
        assert!(!tmp.path().join("SOUL.md").exists());
        assert!(!tmp.path().join("MEMORY.md").exists());
        assert!(!tmp.path().join("HEARTBEAT.md").exists());
    }

    #[test]
    fn test_system_md_contains_expected_content() {
        let tmp = TempDir::new().unwrap();
        let bootstrap = AgentBootstrap::new("my-agent", tmp.path().to_path_buf());

        bootstrap.run().unwrap();

        let content = std::fs::read_to_string(tmp.path().join("SYSTEM.md")).unwrap();
        
        // Check for key sections
        assert!(content.contains("# SYSTEM.md — my-agent"));
        assert!(content.contains("{{agent_name}}"));
        assert!(content.contains("{{tools}}"));
        assert!(content.contains("{{runtime}}"));
        assert!(content.contains("{{workspace}}"));
        assert!(content.contains("NO_REPLY"));
    }
}
