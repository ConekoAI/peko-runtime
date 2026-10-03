//! Executable content shown to the operator and recorded at snapshot import.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// A command-bearing manifest, retained verbatim so binds, arguments, and
/// malformed definitions remain visible without executing or loading them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutableDefinition {
    pub id: String,
    pub path: String,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutableInventory {
    pub did: String,
    /// Payload files, excluding the inventory manifest itself.
    pub file_count: usize,
    pub hooks: Vec<ExecutableDefinition>,
    pub mcp_servers: Vec<ExecutableDefinition>,
    pub skills: Vec<String>,
}

impl ExecutableInventory {
    pub fn from_files(did: &str, files: &HashMap<String, Vec<u8>>) -> Self {
        let mut hooks = Vec::new();
        let mut mcp_servers = Vec::new();
        let mut skills = BTreeSet::new();
        for (path, bytes) in files {
            let parts: Vec<_> = path.split('/').collect();
            if parts.len() < 2 {
                continue;
            }
            let entry = || ExecutableDefinition {
                id: parts[1].to_string(),
                path: path.clone(),
                definition: String::from_utf8_lossy(bytes).into_owned(),
            };
            match parts[0] {
                "hooks" if parts.len() == 3 && parts[2] == "hook.toml" => hooks.push(entry()),
                "mcp"
                    if parts.len() == 2
                        || (parts.len() == 3
                            && matches!(parts[2], "server.json" | "manifest.yaml")) =>
                {
                    mcp_servers.push(entry())
                }
                "skills" => {
                    skills.insert(parts[1].to_string());
                }
                _ => {}
            }
        }
        hooks.sort_by(|a, b| a.path.cmp(&b.path));
        mcp_servers.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            did: did.into(),
            file_count: files.len() - usize::from(files.contains_key("manifest.toml")),
            hooks,
            mcp_servers,
            skills: skills.into_iter().collect(),
        }
    }

    /// No truncation: every command and hook binding is operator-visible.
    pub fn render(&self) -> String {
        let mut text = format!(
            "Snapshot inventory: DID {}, {} files\n",
            self.did, self.file_count
        );
        for (kind, entries) in [("Hook", &self.hooks), ("MCP server", &self.mcp_servers)] {
            for entry in entries {
                text.push_str(&format!(
                    "  {kind}: {} ({})\n{}\n",
                    entry.id, entry.path, entry.definition
                ));
            }
        }
        text.push_str(&format!("  Skills: {}\n", self.skills.join(", ")));
        text
    }
}
