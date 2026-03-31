//! Tool Management Commands

use crate::commands::GlobalPaths;
use crate::tools::traits::Tool;
use clap::Subcommand;
use std::path::PathBuf;

/// Tool management subcommands
///
/// Tools extend agent capabilities. Built-in tools are always available,
/// and additional tools can be installed from the Pekohub registry.
///
/// Examples:
///   # List all installed tools
///   pekobot tool list
///
///   # Search for tools in the registry
///   pekobot tool search "database"
///
///   # Install a tool
///   pekobot tool install postgres
///
///   # Install specific version
///   pekobot tool install postgres --version 1.2.0
///
///   # Show tool details
///   pekobot tool info postgres
///
///   # Uninstall a tool
///   pekobot tool uninstall postgres
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ToolCommands {
    /// List installed tools
    List {
        /// Show all details
        #[arg(short, long)]
        long: bool,
    },

    /// Search Pekohub registry
    Search {
        /// Search query
        query: String,
        /// Limit results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Install tool from Pekohub
    Install {
        /// Tool name
        name: String,
        /// Specific version
        #[arg(long)]
        version: Option<String>,
        /// Force reinstall if exists
        #[arg(short, long)]
        force: bool,
    },

    /// Uninstall a tool
    Uninstall {
        /// Tool name
        name: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Show tool information
    Info {
        /// Tool name
        name: String,
    },

    /// Test a universal tool directly
    ///
    /// This command tests a tool without running a full agent.
    /// Useful for debugging and validating tool configuration.
    ///
    /// Examples:
    ///   # Test with a manifest file
    ///   pekobot tool test ./my_tool.json
    ///
    ///   # Test with executable only (auto-detects manifest)
    ///   pekobot tool test ./my_tool.py
    ///
    ///   # Test with custom arguments
    ///   pekobot tool test ./my_tool.json --arg '{"query": "test"}'
    Test {
        /// Path to tool manifest or executable
        path: PathBuf,

        /// JSON arguments to pass to the tool
        #[arg(short, long)]
        args: Option<String>,

        /// Show raw protocol output
        #[arg(short, long)]
        raw: bool,

        /// Timeout in seconds
        #[arg(short, long, default_value = "30")]
        timeout: u64,
    },
}

/// Handle test tool command
async fn handle_test_tool(
    path: PathBuf,
    args: Option<String>,
    raw: bool,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    use crate::tools::universal::{Manifest, adapter::UniversalToolAdapter};
    use serde_json::json;

    println!("🔧 Testing tool: {}", path.display());

    // Determine manifest and executable paths
    let (manifest_path, executable_path) = if path.extension().map_or(false, |e| e == "json") {
        // Path is a manifest
        let exe_path = if path.with_extension("").exists() {
            path.with_extension("")
        } else if path.with_extension("py").exists() {
            path.with_extension("py")
        } else {
            anyhow::bail!("Could not find executable for manifest: {}", path.display());
        };
        (path.clone(), exe_path)
    } else {
        // Path is an executable, look for manifest
        let manifest = path.with_extension("json");
        (manifest, path.clone())
    };

    // Load manifest if it exists
    let manifest = if manifest_path.exists() {
        println!("  📄 Manifest: {}", manifest_path.display());
        Some(Manifest::from_file(&manifest_path).await?)
    } else {
        println!("  ⚠️  No manifest found, using defaults");
        None
    };

    println!("  ⚙️  Executable: {}", executable_path.display());

    if !executable_path.exists() {
        anyhow::bail!("Executable not found: {}", executable_path.display());
    }

    // Create adapter
    let adapter = if let Some(m) = manifest {
        UniversalToolAdapter::from_manifest_embedded(m, &executable_path)
    } else {
        anyhow::bail!("Manifest is required for testing universal tools");
    };

    // Build test arguments
    let test_args = if let Some(args_str) = args {
        serde_json::from_str(&args_str)?
    } else {
        // Use default values from schema
        json!({})
    };

    println!("\n📤 Sending test request...");
    if raw {
        println!("  Args: {}", serde_json::to_string_pretty(&test_args)?);
    }

    // Execute with injection
    let start = std::time::Instant::now();
    let result = adapter.execute(test_args).await;
    let elapsed = start.elapsed();

    println!("\n📥 Response (took {:?}):", elapsed);
    
    match result {
        Ok(output) => {
            if raw {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("✅ Tool executed successfully");
                if let Some(data) = output.get("data") {
                    println!("\n📊 Result:");
                    println!("{}", serde_json::to_string_pretty(data)?);
                }
                if let Some(metadata) = output.get("metadata") {
                    println!("\n📋 Metadata:");
                    println!("{}", serde_json::to_string_pretty(metadata)?);
                }
            }
            Ok(())
        }
        Err(e) => {
            println!("❌ Tool execution failed: {}", e);
            Err(e)
        }
    }
}

/// Handle tool commands
pub async fn handle_tool(
    cmd: ToolCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        ToolCommands::List { long } => {
            if json {
                println!("{{\"tools\": []}}");
            } else {
                println!("🔧 Installed Tools:");
                if long {
                    println!("  (Use --long for details)");
                }
            }
            Ok(())
        }
        ToolCommands::Search { query, limit } => {
            println!("🔍 Searching Pekohub for '{query}' (limit: {limit})...");
            println!("  (Pekohub integration coming soon)");
            Ok(())
        }
        ToolCommands::Install {
            name,
            version,
            force,
        } => {
            println!("📥 Installing tool '{name}'...");
            if let Some(v) = version {
                println!("  Version: {v}");
            }
            if force {
                println!("  Force: true");
            }
            println!("  (Tool installation coming soon)");
            Ok(())
        }
        ToolCommands::Uninstall { name, force } => {
            if force {
                println!("🗑️  Uninstalling tool '{name}'...");
            } else {
                println!("🗑️  Uninstalling tool '{name}' (use --force to skip confirmation)...");
            }
            Ok(())
        }
        ToolCommands::Info { name } => {
            println!("📋 Tool Information: {name}");
            Ok(())
        }
        ToolCommands::Test {
            path,
            args,
            raw,
            timeout,
        } => {
            handle_test_tool(path, args, raw, timeout).await
        }
    }
}
