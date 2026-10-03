//! Runtime-local `.peko` snapshots: tar.gz files with a flat TOML inventory.
//! Identity, configuration, roles, sessions, authored state, and workspace
//! tooling travel together. Seeds are grounded through `peko create -s`.

pub mod inventory;
pub mod path_safety;
pub mod principal_manifest;
pub mod principal_packager;
pub mod principal_unpackager;
pub mod validation;

pub use inventory::ExecutableInventory;
pub use principal_manifest::PrincipalManifest;
pub use principal_packager::{export_principal, PrincipalExportOptions, PrincipalPackager};
pub use principal_unpackager::{
    PrincipalImportOptions, PrincipalImportResult, PrincipalUnpackager,
};
pub use validation::ValidationResult;
