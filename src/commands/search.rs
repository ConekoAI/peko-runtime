//! Registry Search Commands
//!
//! Search the PekoHub registry for agents, teams, and extensions.
//!
//! Examples:
//!   peko search researcher
//!   peko search researcher --type agent --page 1 --per-page 10
//!   peko search info acme/researcher

use crate::commands::GlobalPaths;
use crate::common::services::CredentialsService;
use crate::registry::config::RegistryConfig;
use anyhow::{Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

/// Search subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SearchCommands {
    /// Search the registry for bundles
    Query {
        /// Search query string
        query: String,

        /// Page number (1-based)
        #[arg(long, default_value = "1")]
        page: u32,

        /// Items per page
        #[arg(long, default_value = "20")]
        per_page: u32,

        /// Filter by bundle type (agent, team, extension)
        #[arg(long, value_name = "TYPE")]
        r#type: Option<String>,
    },

    /// Show detailed information about a bundle
    Info {
        /// Bundle reference in namespace/name format
        bundle: String,
    },
}

/// Handle search commands
pub async fn handle_search(cmd: SearchCommands, paths: &GlobalPaths, json: bool) -> Result<()> {
    match cmd {
        SearchCommands::Query {
            query,
            page,
            per_page,
            r#type,
        } => handle_search_query(paths, &query, page, per_page, r#type.as_deref(), json).await,
        SearchCommands::Info { bundle } => handle_search_info(paths, &bundle, json).await,
    }
}

// ---------------------------------------------------------------------------
// Search query
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
    total: u32,
    page: u32,
    per_page: u32,
    total_pages: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct SearchItem {
    namespace: String,
    name: String,
    version: String,
    description: Option<String>,
    author: Option<String>,
    #[serde(rename = "bundleType")]
    bundle_type: Option<String>,
    #[serde(rename = "extensionType")]
    extension_type: Option<String>,
    tags: Option<Vec<String>>,
    #[serde(rename = "pullCount")]
    pull_count: Option<u32>,
    #[serde(rename = "starCount")]
    star_count: Option<u32>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
}

async fn handle_search_query(
    paths: &GlobalPaths,
    query: &str,
    page: u32,
    per_page: u32,
    bundle_type: Option<&str>,
    json: bool,
) -> Result<()> {
    let registry = RegistryConfig::default().default;
    let mut url = format!(
        "https://{}/api/v1/search?q={}&page={}&perPage={}",
        registry,
        urlencoding::encode(query),
        page,
        per_page
    );

    if let Some(bt) = bundle_type {
        url.push_str(&format!("&filters.bundleType={}", urlencoding::encode(bt)));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let mut req = client.get(&url).header("Accept", "application/json");

    // Add auth header if registry token is available
    let creds = CredentialsService::new(paths.clone())?;
    if let Some(token) = creds.get_registry_token()? {
        req = req.bearer_auth(token.token);
    }

    let resp = req
        .send()
        .await
        .context("Failed to connect to registry. Please check your internet connection.")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Registry returned error ({}): {}", status, body);
    }

    let data: SearchResponse = resp
        .json()
        .await
        .context("Failed to parse registry response")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).context("Failed to serialize response")?
        );
        return Ok(());
    }

    if data.items.is_empty() {
        println!("No results found for '{}'", query);
        return Ok(());
    }

    // Print header
    println!(
        "Found {} result(s) for '{}' (page {} of {})",
        data.total, query, data.page, data.total_pages
    );
    println!();

    // Print table header
    println!(
        "{:<30} {:<10} {:<12} {:<8} {:<8} DESCRIPTION",
        "NAME", "VERSION", "TYPE", "PULLS", "STARS"
    );
    println!("{}", "-".repeat(100));

    for item in &data.items {
        let name = format!("{}/{}", item.namespace, item.name);
        let name_display = if name.len() > 28 {
            format!("{}..", &name[..28])
        } else {
            name
        };

        let bundle_type = item
            .bundle_type
            .as_deref()
            .or(item.extension_type.as_deref())
            .unwrap_or("unknown");

        let pulls = item.pull_count.map_or("-".to_string(), |n| n.to_string());
        let stars = item.star_count.map_or("-".to_string(), |n| n.to_string());

        let desc = item.description.as_deref().unwrap_or("");
        let desc_display = if desc.len() > 35 {
            format!("{}..", &desc[..35])
        } else {
            desc.to_string()
        };

        println!(
            "{:<30} {:<10} {:<12} {:<8} {:<8} {}",
            name_display, item.version, bundle_type, pulls, stars, desc_display
        );
    }

    if data.page < data.total_pages {
        println!();
        println!("Use --page {} to see more results", data.page + 1);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Bundle info
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
struct BundleDetailResponse {
    namespace: String,
    name: String,
    versions: Vec<BundleVersion>,
    metadata: BundleMetadata,
    readme: Option<String>,
    #[serde(rename = "pullCount")]
    pull_count: Option<PullCounts>,
    #[serde(rename = "installCommand")]
    install_command: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BundleVersion {
    version: String,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BundleMetadata {
    name: Option<String>,
    description: Option<String>,
    author: Option<String>,
    #[serde(rename = "bundleType")]
    bundle_type: Option<String>,
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PullCounts {
    daily: Option<u32>,
    weekly: Option<u32>,
    monthly: Option<u32>,
    #[serde(rename = "allTime")]
    all_time: Option<u32>,
}

async fn handle_search_info(paths: &GlobalPaths, bundle: &str, json: bool) -> Result<()> {
    let (namespace, name) = parse_bundle_ref(bundle)?;

    let registry = RegistryConfig::default().default;
    let url = format!("https://{}/api/v1/bundles/{}/{}", registry, namespace, name);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let mut req = client.get(&url).header("Accept", "application/json");

    // Add auth header if registry token is available
    let creds = CredentialsService::new(paths.clone())?;
    if let Some(token) = creds.get_registry_token()? {
        req = req.bearer_auth(token.token);
    }

    let resp = req
        .send()
        .await
        .context("Failed to connect to registry. Please check your internet connection.")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("Bundle '{}' not found in registry", bundle);
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Registry returned error ({}): {}", status, body);
    }

    let data: BundleDetailResponse = resp
        .json()
        .await
        .context("Failed to parse registry response")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).context("Failed to serialize response")?
        );
        return Ok(());
    }

    // Human-readable output
    println!("📦 {}/{}", data.namespace, data.name);

    if let Some(desc) = &data.metadata.description {
        println!("   {}", desc);
    }
    println!();

    if let Some(bt) = &data.metadata.bundle_type {
        println!("   Type:        {}", bt);
    }
    if let Some(author) = &data.metadata.author {
        println!("   Author:      {}", author);
    }

    if let Some(pulls) = &data.pull_count {
        let parts: Vec<String> = [
            pulls.all_time.map(|n| format!("all-time: {}", n)),
            pulls.monthly.map(|n| format!("monthly: {}", n)),
            pulls.weekly.map(|n| format!("weekly: {}", n)),
            pulls.daily.map(|n| format!("daily: {}", n)),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !parts.is_empty() {
            println!("   Pulls:       {}", parts.join(", "));
        }
    }

    if let Some(tags) = &data.metadata.tags {
        if !tags.is_empty() {
            println!("   Tags:        {}", tags.join(", "));
        }
    }

    println!();

    if !data.versions.is_empty() {
        println!("   Versions:");
        for v in &data.versions {
            let created = v.created_at.as_deref().unwrap_or("");
            if created.is_empty() {
                println!("     - {}", v.version);
            } else {
                println!("     - {} ({})", v.version, created);
            }
        }
        println!();
    }

    if let Some(cmd) = &data.install_command {
        println!("   Install:");
        println!("     {}", cmd);
    }

    if let Some(readme) = &data.readme {
        if !readme.trim().is_empty() {
            println!();
            println!("---");
            println!();
            // Print first 80 lines of readme to avoid flooding the terminal
            let lines: Vec<&str> = readme.lines().collect();
            let preview: Vec<&str> = lines.iter().copied().take(80).collect();
            println!("{}", preview.join("\n"));
            if lines.len() > 80 {
                println!();
                println!("... ({} more lines)", lines.len() - 80);
            }
        }
    }

    Ok(())
}

/// Parse a bundle reference like "acme/researcher" into (namespace, name)
fn parse_bundle_ref(bundle: &str) -> Result<(&str, &str)> {
    let parts: Vec<&str> = bundle.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "Invalid bundle reference '{}'. Expected format: namespace/name",
            bundle
        );
    }
    Ok((parts[0], parts[1]))
}
