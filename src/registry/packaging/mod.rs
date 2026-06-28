//! Portable principal package system
//!
//! Provides export/import functionality for Principals as `.principal`
//! packages. After the principal-as-single-actor migration (Phases 1-5),
//! `.principal` is the canonical archive format; `.agent` and `.team`
//! archives were retired alongside the standalone agent CRUD surface.
//!
//! Similar to Docker containers, a Principal can be packaged with its
//! identity, memory, configuration, capabilities, agent prompts, and
//! session history.
//!
//! ## Package Format
//!
//! `.principal` files are gzip-compressed tar archives containing:
//! - `manifest.toml` - Package metadata and file checksums
//! - `identity/did.json` - DID document
//! - `identity/keys.enc` - Encrypted private keys (AES-256-GCM)
//! - `config/principal.toml` - Principal configuration (owner,
//!   permissions, exposure, capabilities, supervisor prompt)
//! - `workspace/agents/` - The Principal's agent prompts (`AGENT.md`)
//! - `workspace/memory/` - Memory index and session JSONL (optional)
//! - `extensions/` - Embedded extension packages (optional, air-gapped bundles)
//!
//! ## Example
//!
//! ```rust,ignore
//! use peko::registry::packaging::{PrincipalPackager, PrincipalUnpackager};
//!
//! // Export a principal
//! let packager = PrincipalPackager::new(workspace_path);
//! let package_path = packager.export(options).await?;
//!
//! // Import a principal
//! let unpackager = PrincipalUnpackager::new(target_dir);
//! let result = unpackager.import("./my-principal.principal", options).await?;
//! ```

#![allow(dead_code)]

pub mod manifest;
pub mod packager;
pub mod principal_manifest;
pub mod principal_packager;
pub mod principal_unpackager;
pub mod signature;
pub mod team_layer_builder;
pub mod team_layer_reconstructor;
pub mod team_packager;
pub mod team_unpackager;
pub mod types;
pub mod unpackager;
pub mod validation;

pub use manifest::AgentManifest;
pub use packager::{ExportOptions, Packager};
pub use principal_manifest::PrincipalManifest;
pub use principal_packager::{export_principal, PrincipalExportOptions, PrincipalPackager, PrincipalRegistryDescriptor};
pub use principal_unpackager::{PrincipalImportOptions, PrincipalImportResult, PrincipalUnpackager};
pub use team_layer_builder::{decompose_team_archive, DecomposedTeamLayers, LayerBytes};
pub use team_layer_reconstructor::{
    extract_team_config_index, reconstruct_agent_files, reconstruct_team, ReconstructedTeam,
};
pub use team_packager::{
    export_team, export_team_with_config_dir, AgentLayerRef, TeamAgentIndex, TeamExportOptions,
    TeamManifest, TeamPackager, TeamPackagingMetadata,
};
pub use team_unpackager::{
    import_team, import_team_with_base_dir, inspect_team, TeamImportOptions, TeamImportResult,
    TeamUnpackager,
};
pub use types::{compute_digest, ExtensionRef, ImageDigest, Layer, LayerDigest, LayerType};
pub use unpackager::{inspect_agent, ImportOptions, ImportResult, Unpackager};
pub use validation::{validate_package, ValidationResult};

use std::path::Path;

/// Get package info without full extraction
pub async fn get_package_info(path: impl AsRef<Path>) -> anyhow::Result<PackageInfo> {
    let (manifest, validation) = inspect_agent(path, None).await?;

    Ok(PackageInfo {
        name: manifest.agent.name,
        version: manifest.agent.version,
        description: manifest.agent.description,
        did: manifest.agent.did,
        created_at: manifest.agent.created_at,
        export_format: manifest.agent.export_format,
        peko_version: manifest.agent.peko_version,
        encrypted: manifest.identity.encrypted,
        layers: Vec::new(),
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
    /// Peko version that created this
    pub peko_version: String,
    /// Whether package is encrypted
    pub encrypted: bool,
    /// Layer digests (content-addressable) — layer name → digest
    pub layers: Vec<(String, String)>,
    /// Whether validation passed
    pub valid: bool,
    /// Number of warnings
    pub warnings: usize,
    /// Number of errors
    pub errors: usize,
}

impl PackageInfo {
    /// Format as human-readable string
    #[must_use]
    pub fn format(&self) -> String {
        let mut output = String::new();

        output.push_str(&format!("📦 {} v{}\n", self.name, self.version));
        if let Some(desc) = &self.description {
            output.push_str(&format!("   {desc}\n"));
        }

        output.push_str(&format!("\n🆔 DID: {}\n", self.did));
        output.push_str(&format!("📅 Created: {}\n", self.created_at));
        output.push_str(&format!(
            "🔧 Format: {} (Peko {})\n",
            self.export_format, self.peko_version
        ));

        if self.encrypted {
            output.push_str("🔒 Encrypted: Yes\n");
        }

        if self.valid {
            output.push_str("\n✅ Validation: Passed");
        } else {
            output.push_str(&format!(
                "\n❌ Validation: {} errors, {} warnings",
                self.errors, self.warnings
            ));
        }

        output
    }
}
