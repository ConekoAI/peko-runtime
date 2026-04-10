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
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

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
    /// 
    /// If a JSON manifest file is provided, it will be used directly.
    /// If no manifest is found, the tool will be run with `tool/describe`
    /// to automatically generate the manifest (requires Python/Node runtime).
    ///
    /// Examples:
    ///   # Install from Python file (auto-generates manifest if .json not found)
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

/// Generate manifest by running tool/describe on the executable
///
/// This allows installing SDK-based tools without a separate JSON manifest.
/// The tool is spawned, sent a tool/describe request, and the response
/// is used to generate the manifest.
async fn generate_manifest_from_tool(executable: &PathBuf) -> anyhow::Result<crate::tools::universal::Manifest> {
    use serde_json::Value;
    
    println!("  🔍 No JSON manifest found, generating from tool/describe...");
    
    // Determine how to run the executable based on extension
    let (cmd, args): (&str, Vec<&str>) = if executable.extension().map(|e| e == "py").unwrap_or(false) {
        ("python", vec![executable.to_str().unwrap()])
    } else if executable.extension().map(|e| e == "js").unwrap_or(false) {
        ("node", vec![executable.to_str().unwrap()])
    } else {
        // Assume it's a binary
        (executable.to_str().unwrap(), vec![])
    };
    
    // Spawn the tool process
    let mut child = Command::new(cmd)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn tool: {}. Is {} installed?", e, cmd))?;
    
    // Send tool/describe request
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "tool/describe"
    });
    
    let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("Failed to open stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("Failed to open stdout"))?;
    
    // Write request
    let mut stdin = stdin;
    stdin.write_all(format!("{}\n", request.to_string()).as_bytes()).await?;
    stdin.flush().await?;
    drop(stdin); // Close stdin
    
    // Read response with timeout
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    
    let line = timeout(Duration::from_secs(10), lines.next_line())
        .await
        .map_err(|_| anyhow::anyhow!("Timeout waiting for tool/describe response"))?
        .map_err(|e| anyhow::anyhow!("Failed to read response: {}", e))?
        .ok_or_else(|| anyhow::anyhow!("No response from tool"))?;
    
    // Kill the process
    let _ = child.kill().await;
    
    // Parse response
    let response: Value = serde_json::from_str(&line)
        .map_err(|e| anyhow::anyhow!("Invalid JSON response: {}", e))?;
    
    let result = response.get("result")
        .ok_or_else(|| anyhow::anyhow!("Response missing 'result' field: {}", line))?;
    
    // Convert the describe result to a Manifest
    let name = result.get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Response missing 'name' field"))?
        .to_string();
    
    let description = result.get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    
    let parameters = result.get("parameters")
        .cloned()
        .unwrap_or(serde_json::json!({"type": "object"}));
    
    // Handle reserved_parameters from the describe response
    #[allow(deprecated)]
    let reserved_parameters = result.get("reserved_parameters")
        .and_then(|v| {
            // Convert from SDK format to manifest format
            let mut reserved = std::collections::HashMap::new();
            if let Some(obj) = v.as_object() {
                for (key, _) in obj {
                    reserved.insert(key.clone(), crate::tools::universal::ReservedParam {
                        source: crate::tools::universal::ParamSourceLegacy::Runtime { 
                            field: key.clone() 
                        },
                        description: None,
                    });
                }
            }
            Some(reserved)
        });
    
    let manifest = crate::tools::universal::Manifest {
        name,
        description,
        llm_description: result.get("llm_description").and_then(|v| v.as_str()).map(|s| s.to_string()),
        parameters,
        reserved_parameters,
        protocol: crate::tools::universal::ProtocolConfig::default(),
        extra: std::collections::HashMap::new(),
    };
    
    println!("  ✅ Generated manifest for '{}'", manifest.name);
    Ok(manifest)
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
    
    // First, find the executable and manifest (if exists)
    let (manifest_opt, executable_path) = if path.is_dir() {
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

        let executable_path = executable.ok_or_else(|| {
            anyhow::anyhow!("No executable (.py or .js) found in directory: {}", path.display())
        })?;

        (manifest, executable_path)
    } else if path.extension().map_or(false, |e| e == "json") {
        // Path is a manifest - look for executable with same base name
        let manifest = Some(path.clone());
        
        let exe_path = if path.with_extension("").exists() {
            path.with_extension("")
        } else if path.with_extension("py").exists() {
            path.with_extension("py")
        } else if path.with_extension("js").exists() {
            path.with_extension("js")
        } else {
            anyhow::bail!("Could not find executable for manifest: {}", path.display());
        };

        (manifest, exe_path)
    } else {
        // Path is an executable - look for manifest with same name
        let manifest_path = path.with_extension("json");
        let manifest = if manifest_path.exists() {
            Some(manifest_path)
        } else {
            None
        };

        (manifest, path.clone())
    };

    // Get manifest - either from file or generate from tool
    let (manifest, manifest_source_path): (Manifest, Option<PathBuf>) = if let Some(manifest_path) = manifest_opt {
        // Use existing JSON manifest
        let manifest_content = tokio::fs::read_to_string(&manifest_path).await?;
        let manifest: Manifest = serde_json::from_str(&manifest_content)?;
        (manifest, Some(manifest_path))
    } else {
        // Generate manifest from tool/describe
        let manifest = generate_manifest_from_tool(&executable_path).await?;
        (manifest, None)
    };
    
    let tool_name = manifest.name.clone();

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

    // Write or copy manifest
    let dest_manifest = dest_dir.join("manifest.json");
    let manifest_content = serde_json::to_string_pretty(&manifest)?;
    tokio::fs::write(&dest_manifest, manifest_content).await?;
    
    // Copy executable
    let dest_executable = dest_dir.join(executable_path.file_name().unwrap());
    tokio::fs::copy(&executable_path, &dest_executable).await?;

    // Copy any additional files from source directory
    if path_clone.is_dir() {
        let mut entries = tokio::fs::read_dir(&path_clone).await?;
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = src_path.file_name().unwrap();

            // Skip already copied files
            if src_path == executable_path {
                continue;
            }
            // Skip the source manifest if we generated one (we already wrote our own)
            if manifest_source_path.as_ref().map(|p| src_path == *p).unwrap_or(false) {
                continue;
            }

            let dest_path = dest_dir.join(file_name);
            if src_path.is_file() {
                tokio::fs::copy(&src_path, &dest_path).await?;
            }
        }
    }

    // Copy additional files and directories recursively
    if path_clone.is_dir() {
        copy_dir_recursive(&path_clone, &dest_dir, &executable_path, manifest_source_path.as_ref()).await?;
    }

    println!("  ✅ Installed '{}' to {}", tool_name, dest_dir.display());
    println!();
    println!("Enable it in your agent's config.toml:");
    println!("  [tools]");
    println!("  enabled = [\"shell\", \"filesystem\", \"{}\"]", tool_name);

    Ok(())
}

/// Recursively copy directory contents, skipping specific files
///
/// # Arguments
/// * `src` - Source directory
/// * `dst` - Destination directory
/// * `skip_file` - File to skip (the main executable)
/// * `skip_manifest` - Optional manifest file to skip (if auto-generated)
async fn copy_dir_recursive(
    src: &PathBuf,
    dst: &PathBuf,
    skip_file: &PathBuf,
    skip_manifest: Option<&PathBuf>,
) -> anyhow::Result<()> {
    let mut entries = tokio::fs::read_dir(src).await?;
    
    while let Some(entry) = entries.next_entry().await? {
        let src_path = entry.path();
        let file_name = entry.file_name();
        
        // Skip the main executable (already copied)
        if src_path == *skip_file {
            continue;
        }
        
        // Skip the source manifest if we generated one
        if skip_manifest.map(|p| src_path == *p).unwrap_or(false) {
            continue;
        }
        
        // Skip hidden files/directories (starting with .)
        if file_name.to_string_lossy().starts_with('.') {
            continue;
        }
        
        let dest_path = dst.join(&file_name);
        
        if src_path.is_dir() {
            // Create subdirectory and recurse
            tokio::fs::create_dir_all(&dest_path).await?;
            
            // Recursive copy for subdirectory
            // We pass None for skip_file and skip_manifest in subdirectories
            // since those are only at the root level
            Box::pin(copy_dir_recursive(&src_path, &dest_path, skip_file, skip_manifest)).await?;
        } else if src_path.is_file() {
            // Copy file
            tokio::fs::copy(&src_path, &dest_path).await?;
        }
        // Skip symlinks and other special files
    }
    
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
