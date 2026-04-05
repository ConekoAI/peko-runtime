//! Tool Management Commands
//!
//! Universal Tools are installed system-wide in `{data_dir}/tools/`
//! and are enabled per-agent via `tools.enabled` in agent config.
//!
//! # Deprecation
//!
//! This module is deprecated. Use `pekobot tools universal` instead.
//! The commands here are re-exported from `tool_management::commands::ToolsCommands::Universal`.

use crate::commands::GlobalPaths;
use crate::tools::traits::Tool;
use clap::Subcommand;
use std::path::PathBuf;

/// Tool management subcommands
///
/// Universal Tools are installed system-wide and can be used by any agent
/// that has them enabled in its configuration.
///
/// Examples:
///   # List all installed tools
///   pekobot tool list
///
///   # Install from local file
///   pekobot tool install ./my_tool.py
///
///   # Install from directory
///   pekobot tool install ./my_tool/
///
///   # Show tool details
///   pekobot tool info calculator_tool
///
///   # Uninstall a tool
///   pekobot tool uninstall calculator_tool
///
///   # Test a tool
///   pekobot tool test calculator_tool --args '{"a": 1, "b": 2}'
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ToolCommands {
    /// List installed Universal Tools
    List {
        /// Show all details including manifests
        #[arg(short, long)]
        long: bool,
    },

    /// Install a Universal Tool from local file or directory
    ///
    /// Installs the tool system-wide so it can be used by any agent.
    /// The tool must have a manifest file (.json) alongside the executable.
    ///
    /// Examples:
    ///   # Install from Python file (looks for my_tool.json)
    ///   pekobot tool install ./my_tool.py
    ///
    ///   # Install from directory containing tool files
    ///   pekobot tool install ./my_tool/
    Install {
        /// Path to tool executable, manifest, or directory
        path: PathBuf,
        /// Force reinstall if already exists
        #[arg(short, long)]
        force: bool,
    },

    /// Uninstall a Universal Tool
    Uninstall {
        /// Tool name (as defined in manifest)
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

    /// Test a Universal Tool
    ///
    /// This command tests an installed tool without running a full agent.
    /// Useful for debugging and validating tool configuration.
    ///
    /// Examples:
    ///   # Test installed tool
    ///   pekobot tool test calculator_tool
    ///
    ///   # Test with custom arguments
    ///   pekobot tool test calculator_tool --args '{"operation": "add", "a": 1, "b": 2}'
    Test {
        /// Tool name (as defined in manifest)
        name: String,

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

/// Get the system-wide tools directory
fn tools_dir() -> PathBuf {
    crate::tools::universal::discovery::default_tools_dir()
}

/// Handle list command
async fn handle_list(long: bool) -> anyhow::Result<()> {
    use crate::tools::universal::discover_universal_tools;

    let tools_dir = tools_dir();
    println!("🔧 Installed Universal Tools:");
    println!("   Location: {}", tools_dir.display());
    println!();

    if !tools_dir.exists() {
        println!("  No tools directory found.");
        println!("  Install tools with: pekobot tool install <path>");
        return Ok(());
    }

    let discovered = discover_universal_tools(&tools_dir).await?;

    if discovered.is_empty() {
        println!("  No tools installed.");
        println!("  Install tools with: pekobot tool install <path>");
        return Ok(());
    }

    for tool in &discovered {
        if long {
            println!("  📦 {}", tool.name);
            println!("     Executable: {}", tool.executable.display());
            if let Some(ref m) = tool.manifest {
                println!("     Manifest: {}", m.display());
            }
            println!();
        } else {
            println!("  📦 {}", tool.name);
        }
    }

    println!();
    println!("Enable tools in your agent's config.toml:");
    println!("  [tools]");
    println!("  enabled = [\"shell\", \"filesystem{}, \"...\"]",
        discovered.iter().map(|t| format!(", \"{}\"", t.name)).collect::<String>()
    );

    Ok(())
}

/// Handle install command
async fn handle_install(path: PathBuf, force: bool) -> anyhow::Result<()> {
    use crate::tools::universal::Manifest;

    println!("📥 Installing tool from: {}", path.display());

    let tools_dir = tools_dir();
    tokio::fs::create_dir_all(&tools_dir).await?;

    // Determine what we're installing
    let path_clone = path.clone(); // For later use when copying additional files
    let (manifest_path, executable_path, tool_name) = if path.is_dir() {
        // Installing from directory - find manifest and executable
        let mut manifest = None;
        let mut executable = None;

        let mut entries = tokio::fs::read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let entry_path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if name.ends_with(".json") && manifest.is_none() {
                manifest = Some(entry_path);
            } else if (name.ends_with(".py") || name.ends_with(".js")) && executable.is_none() {
                executable = Some(entry_path);
            }
        }

        let manifest_path = manifest.ok_or_else(|| {
            anyhow::anyhow!("No manifest (.json) found in directory: {}", path.display())
        })?;
        let executable_path = executable.ok_or_else(|| {
            anyhow::anyhow!("No executable (.py or .js) found in directory: {}", path.display())
        })?;

        let manifest_content = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest: Manifest = serde_json::from_str(&manifest_content)?;
        let tool_name = manifest.name.clone();

        (manifest_path, executable_path, tool_name)
    } else if path.extension().map_or(false, |e| e == "json") {
        // Path is a manifest
        let manifest_content = tokio::fs::read_to_string(&path).await?;
        let manifest: Manifest = serde_json::from_str(&manifest_content)?;
        let tool_name = manifest.name.clone();

        // Look for executable with same base name
        let exe_path = if path.with_extension("").exists() {
            path.with_extension("")
        } else if path.with_extension("py").exists() {
            path.with_extension("py")
        } else if path.with_extension("js").exists() {
            path.with_extension("js")
        } else {
            anyhow::bail!("Could not find executable for manifest: {}", path.display());
        };

        (path, exe_path, tool_name)
    } else {
        // Path is an executable
        let manifest_path = path.with_extension("json");
        if !manifest_path.exists() {
            anyhow::bail!("Manifest not found: {}.json", path.display());
        }

        let manifest_content = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest: Manifest = serde_json::from_str(&manifest_content)?;
        let tool_name = manifest.name.clone();

        (manifest_path, path, tool_name)
    };

    // Check if already exists
    let dest_dir = tools_dir.join(&tool_name);
    if dest_dir.exists() {
        if force {
            println!("  🗑️  Removing existing installation...");
            tokio::fs::remove_dir_all(&dest_dir).await?;
        } else {
            anyhow::bail!(
                "Tool '{}' already installed. Use --force to reinstall.",
                tool_name
            );
        }
    }

    // Create tool directory
    tokio::fs::create_dir(&dest_dir).await?;

    // Copy files
    let dest_manifest = dest_dir.join("manifest.json");
    let dest_executable = dest_dir.join(executable_path.file_name().unwrap());

    tokio::fs::copy(&manifest_path, &dest_manifest).await?;
    tokio::fs::copy(&executable_path, &dest_executable).await?;

    // Copy any additional files from source directory
    if path_clone.is_dir() {
        let mut entries = tokio::fs::read_dir(&path_clone).await?;
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = src_path.file_name().unwrap();

            // Skip already copied files
            if src_path == manifest_path || src_path == executable_path {
                continue;
            }

            let dest_path = dest_dir.join(file_name);
            if src_path.is_file() {
                tokio::fs::copy(&src_path, &dest_path).await?;
            }
        }
    }

    println!("  ✅ Installed '{}' to {}", tool_name, dest_dir.display());
    println!();
    println!("Enable it in your agent's config.toml:");
    println!("  [tools]");
    println!("  enabled = [\"shell\", \"filesystem\", \"{}\"]", tool_name);

    Ok(())
}

/// Handle uninstall command
async fn handle_uninstall(name: String, force: bool) -> anyhow::Result<()> {
    let tools_dir = tools_dir();
    let tool_dir = tools_dir.join(&name);

    if !tool_dir.exists() {
        anyhow::bail!("Tool '{}' not found in {}", name, tools_dir.display());
    }

    if !force {
        println!("⚠️  Are you sure you want to uninstall '{}'?", name);
        println!("   Location: {}", tool_dir.display());
        println!("   Use --force to skip this confirmation.");
        return Ok(());
    }

    println!("🗑️  Uninstalling '{}'...", name);
    tokio::fs::remove_dir_all(&tool_dir).await?;
    println!("  ✅ Uninstalled successfully");

    Ok(())
}

/// Handle info command
async fn handle_info(name: String) -> anyhow::Result<()> {
    use crate::tools::universal::Manifest;

    let tools_dir = tools_dir();
    let tool_dir = tools_dir.join(&name);
    let manifest_path = tool_dir.join("manifest.json");

    if !manifest_path.exists() {
        anyhow::bail!(
            "Tool '{}' not found in {}. Is it installed?",
            name,
            tools_dir.display()
        );
    }

    let manifest_content = tokio::fs::read_to_string(&manifest_path).await?;
    let manifest: Manifest = serde_json::from_str(&manifest_content)?;

    println!("📦 {}", manifest.name);
    println!("   Description: {}", manifest.description);
    if let Some(ref llm_desc) = manifest.llm_description {
        println!("   LLM Description: {}", llm_desc);
    }
    println!();
    println!("   Location: {}", tool_dir.display());
    println!();
    println!("   Parameters:");
    println!("{}", serde_json::to_string_pretty(&manifest.parameters)?);

    if let Some(ref reserved) = manifest.reserved_parameters {
        println!();
        println!("   Reserved Parameters (injected at runtime):");
        for (name, param) in reserved {
            println!("     - {}: {:?}", name, param.source);
        }
    }

    println!();
    println!("Enable in agent config:");
    println!("  [tools]");
    println!("  enabled = [\"...\", \"{}\"]", manifest.name);

    Ok(())
}

/// Handle test command
async fn handle_test(
    name: String,
    args: Option<String>,
    raw: bool,
    _timeout_secs: u64,
) -> anyhow::Result<()> {
    use crate::tools::universal::{Manifest, adapter::UniversalToolAdapter};
    use serde_json::json;

    let tools_dir = tools_dir();
    let tool_dir = tools_dir.join(&name);
    let manifest_path = tool_dir.join("manifest.json");

    if !manifest_path.exists() {
        anyhow::bail!(
            "Tool '{}' not found in {}.\nInstall it with: pekobot tool install <path>",
            name,
            tools_dir.display()
        );
    }

    println!("🔧 Testing tool: {}", name);

    // Load manifest
    let manifest_content = tokio::fs::read_to_string(&manifest_path).await?;
    let manifest: Manifest = serde_json::from_str(&manifest_content)?;

    // Find executable
    let executable_path = if tool_dir.join(format!("{}.py", name)).exists() {
        tool_dir.join(format!("{}.py", name))
    } else if tool_dir.join(format!("{}.js", name)).exists() {
        tool_dir.join(format!("{}.js", name))
    } else {
        // Look for any .py or .js file
        let mut entries = tokio::fs::read_dir(&tool_dir).await?;
        let mut found = None;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext == "py" || ext == "js" {
                    found = Some(path);
                    break;
                }
            }
        }
        found.ok_or_else(|| anyhow::anyhow!("No executable found for tool '{}'", name))?
    };

    println!("  📄 Manifest: {}", manifest_path.display());
    println!("  ⚙️  Executable: {}", executable_path.display());

    // Create adapter
    let adapter = UniversalToolAdapter::from_manifest_embedded(manifest, &executable_path);

    // Build test arguments
    let test_args = if let Some(args_str) = args {
        serde_json::from_str(&args_str)?
    } else {
        json!({})
    };

    println!("\n📤 Sending test request...");
    if raw {
        println!("  Args: {}", serde_json::to_string_pretty(&test_args)?);
    }

    // Execute
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
                } else {
                    println!("\n📊 Result:");
                    println!("{}", serde_json::to_string_pretty(&output)?);
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
    // JSON output not yet implemented for most commands
    if json {
        println!("{{\"warning\": \"JSON output not yet implemented\"}}");
    }

    match cmd {
        ToolCommands::List { long } => handle_list(long).await,
        ToolCommands::Install { path, force } => handle_install(path, force).await,
        ToolCommands::Uninstall { name, force } => handle_uninstall(name, force).await,
        ToolCommands::Info { name } => handle_info(name).await,
        ToolCommands::Test { name, args, raw, timeout } => {
            handle_test(name, args, raw, timeout).await
        }
    }
}
