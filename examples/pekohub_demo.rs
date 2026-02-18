//! Pekohub Example - Local Tool Management
//!
//! Demonstrates how to use the Pekohub tool registry to load and manage tools.
//!
//! Usage:
//!   cargo run --example pekohub_demo

use std::path::PathBuf;

use pekobot::tool_registry::{ToolRegistry, ToolRegistryConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          🔧 Pekohub Tool Registry Demo                   ║");
    println!("║     Local Tool Loading and Management System            ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // Initialize tool registry
    let config = ToolRegistryConfig::default();
    let mut registry = ToolRegistry::new(config)?;

    println!("📁 Tool cache directory: {:?}", registry.config.cache_dir);
    println!();

    // Create sample tool manifest
    let sample_manifest = r#"[tool]
name = "weather"
version = "1.0.0"
description = "Get current weather and forecasts"
author = "Pekohub Contributors"
license = "MIT"
category = "utility"
keywords = ["weather", "forecast", "temperature"]

[capabilities]
provides = ["weather.current", "weather.forecast"]
permissions = ["network"]

[install]
unix = "chmod +x weather-tool"
windows = ""

[security]
sandbox = "network"
"#;

    // Save sample manifest
    let tools_dir = PathBuf::from("./example_tools");
    std::fs::create_dir_all(&tools_dir)?;
    let manifest_path = tools_dir.join("weather.toml");
    std::fs::write(&manifest_path, sample_manifest)?;
    println!("✅ Created sample tool manifest: {:?}", manifest_path);

    // Load tool from manifest
    println!("\n📄 Loading tool manifest...");
    match registry.load_tool_from_path(&manifest_path) {
        Ok(manifest) => {
            println!("✅ Loaded: {}@{}", manifest.tool.name, manifest.tool.version);
            println!("   Description: {}", manifest.tool.description);
            println!("   Category: {:?}", manifest.tool.category);
            println!("   Provides: {:?}", manifest.capabilities.provides);
            if let Some(ref keywords) = manifest.tool.keywords {
                println!("   Keywords: {:?}", keywords);
            }
        }
        Err(e) => {
            println!("❌ Failed to load: {}", e);
        }
    }

    // Install the tool
    println!("\n🔧 Installing tool...");
    match registry.install_local_tool(&manifest_path).await {
        Ok(installed) => {
            println!("✅ Installed: {}@{}", 
                installed.manifest.tool.name,
                installed.manifest.tool.version);
            println!("   Path: {:?}", installed.install_path);
            println!("   Installed at: {}", installed.installed_at);
        }
        Err(e) => {
            println!("❌ Installation failed: {}", e);
        }
    }

    // List installed tools
    println!("\n📋 Installed Tools:");
    let installed = registry.list_installed();
    if installed.is_empty() {
        println!("   No tools installed");
    } else {
        for tool in installed {
            println!("   • {}@{} - {}",
                tool.manifest.tool.name,
                tool.manifest.tool.version,
                tool.manifest.tool.description);
        }
    }

    // Find tools by capability
    println!("\n🔍 Finding tools with 'weather.current' capability...");
    let weather_tools = registry.find_by_capability("weather.current");
    for tool in weather_tools {
        println!("   Found: {}@{}",
            tool.manifest.tool.name,
            tool.manifest.tool.version);
    }

    // Create another sample tool
    let calendar_manifest = r#"[tool]
name = "calendar"
version = "2.1.0"
description = "Google Calendar and Outlook integration"
category = "productivity"

[capabilities]
provides = ["scheduling.calendar_read", "scheduling.calendar_write", "scheduling.schedule_meeting"]
permissions = ["network", "calendar"]
"#;

    let calendar_path = tools_dir.join("calendar.toml");
    std::fs::write(&calendar_path, calendar_manifest)?;

    println!("\n📄 Installing calendar tool...");
    if let Err(e) = registry.install_local_tool(&calendar_path).await {
        println!("   Note: {}", e);
    }

    // Show scheduling tools
    println!("\n🔍 Finding scheduling tools...");
    let scheduling_tools = registry.find_by_capability("scheduling.calendar_read");
    for tool in scheduling_tools {
        println!("   Found: {}@{}",
            tool.manifest.tool.name,
            tool.manifest.tool.version);
    }

    // Scan directory for tools
    println!("\n📂 Scanning tools directory...");
    match registry.scan_tools_directory(&tools_dir) {
        Ok(manifests) => {
            println!("   Found {} tool manifest(s)", manifests.len());
            for manifest in manifests {
                println!("   • {}@{}", manifest.tool.name, manifest.tool.version);
            }
        }
        Err(e) => {
            println!("   Error: {}", e);
        }
    }

    // Cleanup
    println!("\n🧹 Cleaning up...");
    let _ = std::fs::remove_dir_all(&tools_dir);
    
    // Uninstall tools
    if registry.get_tool("weather").is_some() {
        registry.uninstall_tool("weather")?;
    }
    if registry.get_tool("calendar").is_some() {
        registry.uninstall_tool("calendar")?;
    }

    println!("\n✨ Demo complete!");
    println!("\n💡 Next steps:");
    println!("   1. Create your own tool manifests in TOML format");
    println!("   2. Install tools: registry.install_local_tool(path)");
    println!("   3. Find tools by capability: registry.find_by_capability(\"capability\")");
    println!("   4. In Phase 2: Download tools from remote registry");

    Ok(())
}
