//! Extension management commands
//!
//! Provides CLI commands for managing extensions:
//! - Install, uninstall, list extensions
//! - Enable/disable extensions
//! - Show extension details
//! - Create bundles from extensions

use crate::commands::GlobalPaths;
use crate::extensions::manager::{ExtensionManager, LoadedExtension};
use crate::extensions::types::ExtensionId;
use clap::Subcommand;
use std::path::PathBuf;

/// Extension management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ExtCommands {
    /// Install an extension
    Install {
        /// Path to the extension directory or manifest
        path: PathBuf,
        /// Extension type (auto-detect if not specified)
        #[arg(long)]
        r#type: Option<String>,
    },

    /// List installed extensions
    List {
        /// Show only enabled extensions
        #[arg(long)]
        enabled_only: bool,
        /// Filter by extension type
        #[arg(long)]
        r#type: Option<String>,
    },

    /// Enable an extension
    Enable {
        /// Extension ID
        id: String,
    },

    /// Disable an extension
    Disable {
        /// Extension ID
        id: String,
    },

    /// Uninstall an extension
    Uninstall {
        /// Extension ID
        id: String,
    },

    /// Show extension details
    Info {
        /// Extension ID
        id: String,
    },

    /// Create a bundle from installed extensions
    Bundle {
        /// Bundle name
        #[arg(short, long)]
        name: String,

        /// Extension IDs to include
        ids: Vec<String>,
    },
}

/// Handle extension subcommands
pub async fn handle_ext_command(command: ExtCommands, _paths: &GlobalPaths) -> anyhow::Result<()> {
    let mut manager = ExtensionManager::new();

    // Load all extensions to populate the manager
    manager.load_all().await?;

    match command {
        ExtCommands::Install { path, r#type } => handle_install(&mut manager, path, r#type).await,
        ExtCommands::List {
            enabled_only,
            r#type,
        } => handle_list(&manager, enabled_only, r#type),
        ExtCommands::Enable { id } => handle_enable(&mut manager, id).await,
        ExtCommands::Disable { id } => handle_disable(&mut manager, id).await,
        ExtCommands::Uninstall { id } => handle_uninstall(&mut manager, id).await,
        ExtCommands::Info { id } => handle_info(&manager, id),
        ExtCommands::Bundle { name, ids } => handle_bundle(&manager, name, ids),
    }
}

/// Handle install command
async fn handle_install(
    manager: &mut ExtensionManager,
    path: PathBuf,
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    println!("Installing extension from: {}", path.display());

    if let Some(ref t) = ext_type {
        println!("   Type: {}", t);
    }

    match manager.install(&path).await {
        Ok(id) => {
            println!("Extension installed successfully");
            println!("   ID: {}", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to install extension: {}", e);
            Err(e)
        }
    }
}

/// Handle list command
fn handle_list(
    manager: &ExtensionManager,
    enabled_only: bool,
    ext_type: Option<String>,
) -> anyhow::Result<()> {
    let extensions = manager.list_extensions();

    if extensions.is_empty() {
        println!("No extensions installed.");
        println!("Use 'pekobot ext install <path>' to install an extension.");
        return Ok(());
    }

    // Filter extensions based on criteria
    let filtered: Vec<&LoadedExtension> = extensions
        .into_iter()
        .filter(|ext| {
            if enabled_only && !ext.enabled {
                return false;
            }
            if let Some(ref t) = ext_type {
                if &ext.extension_type != t {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        println!("No extensions match the specified criteria.");
        return Ok(());
    }

    println!("Installed Extensions:");
    println!();
    println!("{:<20} {:<12} {:<8} {}", "ID", "TYPE", "STATUS", "NAME");
    println!("{}", "-".repeat(60));

    for ext in &filtered {
        let status = if ext.enabled { "enabled" } else { "disabled" };
        println!(
            "{:<20} {:<12} {:<8} {}",
            ext.manifest.id, ext.extension_type, status, ext.manifest.name
        );
    }

    println!();
    println!("Total: {} extension(s)", filtered.len());

    Ok(())
}

/// Handle enable command
async fn handle_enable(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{}' not found", id);
    }

    match manager.enable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' enabled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to enable extension '{}': {}", id, e);
            Err(e)
        }
    }
}

/// Handle disable command
async fn handle_disable(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{}' not found", id);
    }

    match manager.disable(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' disabled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to disable extension '{}': {}", id, e);
            Err(e)
        }
    }
}

/// Handle uninstall command
async fn handle_uninstall(manager: &mut ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    // Check if extension exists
    if manager.get_extension(&ext_id).is_none() {
        anyhow::bail!("Extension '{}' not found", id);
    }

    println!("Uninstalling extension '{}'...", id);

    match manager.uninstall(&ext_id).await {
        Ok(()) => {
            println!("Extension '{}' uninstalled", id);
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to uninstall extension '{}': {}", id, e);
            Err(e)
        }
    }
}

/// Handle info command
fn handle_info(manager: &ExtensionManager, id: String) -> anyhow::Result<()> {
    let ext_id = ExtensionId::new(&id);

    let ext = manager
        .get_extension(&ext_id)
        .ok_or_else(|| anyhow::anyhow!("Extension '{}' not found", id))?;

    println!("Extension Details");
    println!();
    println!("ID:          {}", ext.manifest.id);
    println!("Name:        {}", ext.manifest.name);
    println!("Type:        {}", ext.extension_type);
    println!("Version:     {}", ext.manifest.version);
    println!("Status:      {}", if ext.enabled { "enabled" } else { "disabled" });
    println!("Description: {}", ext.manifest.description);
    println!("Path:        {}", ext.path.display());

    if !ext.hook_ids.is_empty() {
        println!();
        println!("Registered hooks: {}", ext.hook_ids.len());
    }

    Ok(())
}

/// Handle bundle command
fn handle_bundle(
    manager: &ExtensionManager,
    name: String,
    ids: Vec<String>,
) -> anyhow::Result<()> {
    if ids.is_empty() {
        anyhow::bail!("At least one extension ID is required to create a bundle");
    }

    // Validate all extension IDs exist
    let mut ext_ids = Vec::new();
    for id in &ids {
        let ext_id = ExtensionId::new(id);
        if manager.get_extension(&ext_id).is_none() {
            anyhow::bail!("Extension '{}' not found", id);
        }
        ext_ids.push(ext_id);
    }

    println!("Creating bundle '{}' with {} extension(s)...", name, ids.len());

    match manager.create_bundle(ext_ids, &name) {
        Ok(bundle) => {
            println!("Bundle '{}' created successfully", bundle.name);
            println!("Extensions included:");
            for manifest in &bundle.extensions {
                println!("  - {} ({})", manifest.id, manifest.name);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to create bundle: {}", e);
            Err(e)
        }
    }
}
