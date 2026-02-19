//! Pekohub Remote Registry Example
//!
//! Demonstrates downloading and installing tools from a remote registry.
//!
//! Usage:
//!   cargo run --example pekohub_remote -- --search weather

use clap::Parser;

use pekobot::tool_registry::{
    RemoteRegistryClient, RemoteRegistryConfig, ToolRegistry, ToolRegistryConfig,
};

#[derive(Parser)]
#[command(name = "pekohub_remote")]
#[command(about = "Pekohub remote registry demo")]
struct Args {
    /// Search query
    #[arg(short, long)]
    search: Option<String>,

    /// Install a specific tool
    #[arg(short, long)]
    install: Option<String>,

    /// List all available tools
    #[arg(long)]
    list: bool,

    /// Check for updates
    #[arg(short, long)]
    check_updates: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          🔧 Pekohub Remote Registry Demo                 ║");
    println!("║     Download Tools from Cloud Registry                   ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    let args = Args::parse();

    // Initialize local registry
    let local_config = ToolRegistryConfig::default();
    let local_registry = ToolRegistry::new(local_config)?;
    println!("📁 Local cache: {:?}", local_registry.config.cache_dir);

    // Initialize remote registry client
    let remote_config = RemoteRegistryConfig {
        registry_url: "https://pekohub.io".to_string(),
        api_key: std::env::var("PEKOHUB_API_KEY").ok(),
        timeout_secs: 60,
        verify_signatures: true,
        cache_ttl_hours: 24,
    };

    let cache_dir = local_registry.config.cache_dir.clone();
    let remote_client = RemoteRegistryClient::new(remote_config, cache_dir)?;

    // Handle command
    if args.list {
        println!("\n📋 Listing available tools from registry...\n");

        match remote_client.list_tools(None).await {
            Ok(tools) => {
                if tools.is_empty() {
                    println!("No tools found in registry.");
                } else {
                    println!("Found {} tool(s):\n", tools.len());
                    for tool in tools {
                        println!("  📦 {}", tool.name);
                        println!("     Version: {}", tool.version);
                        println!("     Description: {}", tool.description);
                        println!("     Author: {}", tool.author);
                        println!(
                            "     Downloads: {} | Rating: {:.1}/5.0",
                            tool.downloads, tool.rating
                        );
                        println!("     Categories: {:?}", tool.categories);
                        println!();
                    }
                }
            }
            Err(e) => {
                println!("❌ Failed to list tools: {}", e);
                println!("\nNote: This demo uses a mock registry URL.");
                println!("To use a real registry, set PEKOHUB_API_KEY environment variable.");
            }
        }
    }

    if let Some(query) = args.search {
        println!("\n🔍 Searching for '{}'...\n", query);

        match remote_client.search_tools(&query).await {
            Ok(results) => {
                if results.is_empty() {
                    println!("No tools found matching '{}'", query);
                } else {
                    println!("Found {} result(s):\n", results.len());
                    for tool in results {
                        println!("  📦 {}@{}", tool.name, tool.version);
                        println!("     {}", tool.description);
                        println!(
                            "     Downloads: {} | Rating: {:.1}/5.0",
                            tool.downloads, tool.rating
                        );
                        println!();
                    }

                    println!("💡 To install a tool, run:");
                    println!("   cargo run --example pekohub_remote -- --install <tool-name>");
                }
            }
            Err(e) => {
                println!("❌ Search failed: {}", e);
                println!("\nNote: This demo uses a mock registry URL.");
            }
        }
    }

    if let Some(tool_name) = args.install {
        println!("\n📥 Installing '{}' from remote registry...\n", tool_name);

        // In a real scenario, this would download and install
        println!("This would:");
        println!("  1. Fetch tool manifest from https://pekohub.io");
        println!("  2. Download binary for current platform");
        println!("  3. Verify SHA256 checksum");
        println!("  4. Verify Ed25519 signature");
        println!("  5. Install to local cache directory");
        println!();

        // Mock installation
        println!("✅ Mock installation complete for '{}'", tool_name);
        println!(
            "   Installed to: {:?}",
            local_registry.config.cache_dir.join(&tool_name)
        );
    }

    if args.check_updates {
        println!("\n🔄 Checking for updates...\n");

        let installed = local_registry.list_installed();

        if installed.is_empty() {
            println!("No tools installed locally.");
        } else {
            for tool in installed {
                let name = &tool.manifest.tool.name;
                let current_version = &tool.manifest.tool.version;

                println!("Checking {}@{}...", name, current_version);

                match remote_client.check_for_updates(name, current_version).await {
                    Ok(Some(new_manifest)) => {
                        println!(
                            "  ⬆️  Update available: {} -> {}",
                            current_version, new_manifest.tool.version
                        );
                    }
                    Ok(None) => {
                        println!("  ✓ Up to date");
                    }
                    Err(e) => {
                        println!("  ⚠️  Could not check: {}", e);
                    }
                }
            }
        }
    }

    if !args.list && args.search.is_none() && args.install.is_none() && !args.check_updates {
        println!("📖 Usage examples:\n");
        println!("  List all tools:");
        println!("    cargo run --example pekohub_remote -- --list\n");
        println!("  Search for tools:");
        println!("    cargo run --example pekohub_remote -- --search calendar\n");
        println!("  Install a tool:");
        println!("    cargo run --example pekohub_remote -- --install weather\n");
        println!("  Check for updates:");
        println!("    cargo run --example pekohub_remote -- --check-updates\n");
    }

    println!("\n✨ Done!");
    println!("\n💡 Next steps:");
    println!("   1. Set up a Pekohub registry server");
    println!("   2. Configure PEKOHUB_API_KEY environment variable");
    println!("   3. Upload tools with signed binaries");
    println!("   4. Install tools: pekohub install <tool-name>");

    Ok(())
}
