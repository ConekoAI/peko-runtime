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
//!   permissions, exposure, capabilities, root prompt)
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
pub mod principal_manifest;
pub mod principal_packager;
pub mod principal_unpackager;
pub mod trust_store;
pub mod types;
pub mod validation;

pub use manifest::AgentManifest;
pub use principal_manifest::PrincipalManifest;
pub use principal_packager::{
    export_principal, PrincipalExportOptions, PrincipalPackager, PrincipalRegistryDescriptor,
};
pub use principal_unpackager::{
    PrincipalImportOptions, PrincipalImportResult, PrincipalUnpackager,
};
pub use trust_store::{TrustPolicy, TrustStatus, TrustStore};
pub use types::{compute_digest, ExtensionRef, ImageDigest, Layer, LayerDigest, LayerType};
pub use validation::ValidationResult;
