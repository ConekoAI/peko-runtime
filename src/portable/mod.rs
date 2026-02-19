//! Portable agent package system
//!
//! Provides export/import functionality for agents as `.agent` packages.
//! Similar to Docker containers, agents can be packaged with their
//! identity, memory, configuration, and skills.
//!
//! ## Package Format
//!
//! `.agent` files are gzip-compressed tar archives containing:
//! - `manifest.toml` - Package metadata and file checksums
//! - `identity/did.json` - DID document
//! - `identity/keys.enc` - Encrypted private keys (AES-256-GCM)
//! - `config/agent.toml` - Agent configuration
//! - `config/prompts.toml` - System prompts
//! - `config/skills/*.toml` - Bundled skills
//! - `memory/memory.db` - SQLite memory database
//!
//! ## Example
//!
//! ```rust
//! use pekobot::portable::{export_agent, import_agent, ExportOptions, ImportOptions};
//!
//! // Export an agent
//! let options = ExportOptions {
//!     encrypt: true,
//!     passphrase: Some("secret".to_string()),
//!     ..Default::default()
//! };
//! let package_path = export_agent(config, identity, memory_path, options).await?;
//!
//! // Import an agent
//! let options = ImportOptions {
//!     new_name: Some("imported-agent".to_string()),
//!     passphrase: Some("secret".to_string()),
//!     ..Default::default()
//! };
//! let result = import_agent("./my-agent.agent", options).await?;
//! ```

pub mod crypto;
pub mod manifest;
pub mod packager;
pub mod unpackager;
pub mod validation;

pub use crypto::{decrypt_with_passphrase, encrypt_with_passphrase, EncryptedData};
pub use manifest::AgentManifest;
pub use packager::{export_agent, ExportOptions, Packager};
pub use unpackager::{import_agent, inspect_agent, ImportOptions, ImportResult, Unpackager};
pub use validation::{validate_package, ValidationResult};

use std::io::Read;
use std::path::Path;

/// Check if a file is a valid .agent package (quick check)
pub fn is_agent_package(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    
    // Check extension
    if path.extension().and_then(|e| e.to_str()) != Some("agent") {
        return false;
    }
    
    // Try to open and check magic bytes (gzip)
    if let Ok(file) = std::fs::File::open(path) {
        let mut header = [0u8; 2];
        if std::io::Read::read_exact(&mut file.take(2), &mut header).is_ok() {
            // Gzip magic bytes: 0x1f 0x8b
            return header == [0x1f, 0x8b];
        }
    }
    
    false
}

/// Get package info without full extraction
pub async fn get_package_info(
    path: impl AsRef<Path>,
) -> anyhow::Result<PackageInfo> {
    let (manifest, validation) = inspect_agent(path, None).await?;
    
    Ok(PackageInfo {
        name: manifest.agent.name,
        version: manifest.agent.version,
        description: manifest.agent.description,
        did: manifest.agent.did,
        created_at: manifest.agent.created_at,
        export_format: manifest.agent.export_format,
        pekobot_version: manifest.agent.pekobot_version,
        encrypted: manifest.identity.encrypted,
        capabilities: manifest.capabilities.names,
        required_tools: manifest.tools.required,
        valid: validation.is_valid(),
        warnings: validation.warnings.len(),
        errors: validation.errors.len(),
    })
}

/// Package information (lightweight inspection result)
#[derive(Debug, Clone)]
pub struct PackageInfo {
    /// Agent name
    pub name: String,
    /// Package version
    pub version: String,
    /// Description
    pub description: Option<String>,
    /// Agent DID
    pub did: String,
    /// Creation timestamp
    pub created_at: String,
    /// Export format version
    pub export_format: String,
    /// Pekobot version that created this
    pub pekobot_version: String,
    /// Whether package is encrypted
    pub encrypted: bool,
    /// Capabilities
    pub capabilities: Vec<String>,
    /// Required tools
    pub required_tools: Vec<String>,
    /// Whether validation passed
    pub valid: bool,
    /// Number of warnings
    pub warnings: usize,
    /// Number of errors
    pub errors: usize,
}

impl PackageInfo {
    /// Format as human-readable string
    pub fn format(&self) -> String {
        let mut output = String::new();
        
        output.push_str(&format!("📦 {} v{}\n", self.name, self.version));
        if let Some(desc) = &self.description {
            output.push_str(&format!("   {}\n", desc));
        }
        
        output.push_str(&format!("\n🆔 DID: {}\n", self.did));
        output.push_str(&format!("📅 Created: {}\n", self.created_at));
        output.push_str(&format!("🔧 Format: {} (Pekobot {})\n", 
            self.export_format, 
            self.pekobot_version
        ));
        
        if self.encrypted {
            output.push_str("🔒 Encrypted: Yes\n");
        }
        
        output.push_str(&format!("\n⚡ Capabilities ({}):\n", self.capabilities.len()));
        for cap in &self.capabilities {
            output.push_str(&format!("   - {}\n", cap));
        }
        
        if !self.required_tools.is_empty() {
            output.push_str(&format!("\n🛠️  Required Tools ({}):\n", self.required_tools.len()));
            for tool in &self.required_tools {
                output.push_str(&format!("   - {}\n", tool));
            }
        }
        
        if self.valid {
            output.push_str("\n✅ Validation: Passed");
        } else {
            output.push_str(&format!("\n❌ Validation: {} errors, {} warnings",
                self.errors, self.warnings
            ));
        }
        
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_agent_package_extension() {
        // Test that non-.agent files return false
        assert!(!is_agent_package("test.txt"));
        assert!(!is_agent_package("test.tar.gz"));
        
        // Note: is_agent_package for "test.agent" would fail without a real file
        // because it tries to read the gzip magic bytes
    }
}
