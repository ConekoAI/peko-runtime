//! Provider management commands
//!
//! List and inspect available LLM providers from the registry.

use crate::providers::registry::list_providers;
use anyhow::Result;

/// Provider commands
#[derive(clap::Subcommand)]
pub enum ProviderCommands {
    /// List all available providers
    List {
        /// Show detailed information including API URLs
        #[arg(long)]
        detailed: bool,
    },
}

/// Execute provider commands
pub async fn execute(cmd: ProviderCommands) -> Result<()> {
    match cmd {
        ProviderCommands::List { detailed } => list_providers_cmd(detailed).await,
    }
}

async fn list_providers_cmd(detailed: bool) -> Result<()> {
    let providers = list_providers();

    println!("Available LLM Providers:\n");

    for meta in providers {
        let aliases = if meta.aliases.is_empty() {
            String::new()
        } else {
            format!(", aliases: {})", meta.aliases.join(", "))
        };

        println!(
            "  {} - {}{}",
            meta.id, meta.display_name, aliases
        );

        if detailed {
            println!("    API Type: {}", meta.api_type.as_str());
            println!("    Base URL: {}", meta.base_url);
            println!("    Default Model: {}", meta.default_model);
            println!(
                "    API Key: set one of {}",
                meta.api_key_env.join(", ")
            );
            println!();
        }
    }

    println!("\nUse with: pekobot agent create <name> --provider <provider-id>");
    println!("\nMost providers are OpenAI-compatible and use the same API format.");

    Ok(())
}
