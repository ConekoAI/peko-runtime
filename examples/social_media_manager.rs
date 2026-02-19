//! Social Media Manager Agent Example
//!
//! A marketing agency's social media automation assistant that:
//! - Drafts posts for multiple platforms
//! - Schedules posts for optimal times
//! - Publishes content automatically
//! - Tracks engagement metrics
//!
//! ## Setup
//!
//! 1. Set up Twitter/X API credentials:
//!    - Create app at https://developer.twitter.com/
//!    - Get API keys and access tokens
//!
//! 2. Set up LinkedIn API credentials:
//!    - Create app at https://www.linkedin.com/developers/
//!    - Get client ID and secret
//!
//! 3. Copy .env.example to .env and fill in:
//!    - TWITTER_API_KEY, TWITTER_API_SECRET, etc.
//!    - LINKEDIN_CLIENT_ID, LINKEDIN_CLIENT_SECRET, etc.
//!
//! 4. Run: cargo run --example social_media_manager

use pekobot::agent::Agent;
use pekobot::channels::cli::{run_interactive_loop, CliChannel};
use pekobot::tools::social_media::SocialMediaTool;
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use std::collections::HashMap;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    info!("📱 Social Media Manager Agent");
    info!("=============================");
    info!("");

    // Check for social media credentials
    check_social_credentials();

    // Initialize social media tool
    let social_tool = match SocialMediaTool::from_env() {
        Ok(tool) => {
            info!("✅ Social media tool initialized");
            tool
        }
        Err(e) => {
            warn!("⚠️  Failed to initialize social media tool: {}", e);
            warn!("Some features may not work without API credentials.");
            // Create with empty credentials for demo
            SocialMediaTool::from_env().unwrap_or_else(|_| {
                // This will fail but we already warned the user
                panic!("Social media tool requires credentials")
            })
        }
    };

    // Initialize AI agent
    let agent = create_social_media_agent().await?;
    info!("✅ AI agent initialized with DID: {}", agent.did());
    info!("");
    info!("💡 Commands:");
    info!("   'draft <platform> <content>' - Create a new post draft");
    info!("   'schedule <post_id> <datetime>' - Schedule a post");
    info!("   'publish <post_id>' - Publish immediately");
    info!("   'scheduled' - List scheduled posts");
    info!("   'analytics <post_id>' - Get engagement metrics");
    info!("   'help' - Show detailed help");
    info!("");

    // Run interactive CLI
    let mut channel = CliChannel::new();
    run_social_media_loop(agent, &mut channel, social_tool).await?;

    Ok(())
}

/// Check for social media API credentials
fn check_social_credentials() {
    let mut missing = vec![];

    if std::env::var("TWITTER_API_KEY").is_err() {
        missing.push("TWITTER_API_KEY");
    }
    if std::env::var("TWITTER_API_SECRET").is_err() {
        missing.push("TWITTER_API_SECRET");
    }
    if std::env::var("TWITTER_ACCESS_TOKEN").is_err() {
        missing.push("TWITTER_ACCESS_TOKEN");
    }
    if std::env::var("TWITTER_ACCESS_SECRET").is_err() {
        missing.push("TWITTER_ACCESS_SECRET");
    }
    if std::env::var("LINKEDIN_CLIENT_ID").is_err() {
        missing.push("LINKEDIN_CLIENT_ID");
    }
    if std::env::var("LINKEDIN_CLIENT_SECRET").is_err() {
        missing.push("LINKEDIN_CLIENT_SECRET");
    }
    if std::env::var("LINKEDIN_ACCESS_TOKEN").is_err() {
        missing.push("LINKEDIN_ACCESS_TOKEN");
    }

    if !missing.is_empty() {
        warn!("⚠️  Missing social media API credentials:");
        for cred in &missing {
            warn!("   - {}", cred);
        }
        warn!("");
        warn!("Some features will not work without these credentials.");
        warn!("Set them in your .env file or environment.");
        warn!("");
    } else {
        info!("✅ All social media credentials found");
    }
}

/// Create and configure the social media agent
async fn create_social_media_agent() -> anyhow::Result<Agent> {
    let agent_config = AgentConfig {
        name: "social-media-manager".to_string(),
        description: Some(
            "AI social media management assistant for marketing agencies".to_string(),
        ),
        capabilities: vec![AgentCapability::Text, AgentCapability::ToolUse],
        system_prompt: Some(
            r#"
You are a social media manager for a marketing agency.

Your responsibilities:
1. Draft engaging posts for Twitter/X and LinkedIn
2. Schedule posts for optimal engagement times
3. Maintain brand voice consistency
4. Track and report on engagement metrics

Guidelines for content:
- Keep Twitter posts under 280 characters
- Use professional tone for LinkedIn
- Include relevant hashtags (2-3 max)
- Add call-to-action when appropriate
- Avoid posting outside business hours (9 AM - 6 PM)

Best posting times:
- Twitter: Tuesday-Thursday, 9 AM - 11 AM
- LinkedIn: Tuesday-Thursday, 10 AM - 12 PM

Always review posts before scheduling and provide engagement insights.
"#
            .to_string(),
        ),
        metadata: {
            let mut m = HashMap::new();
            m.insert("role".to_string(), "social_media_manager".to_string());
            m.insert("version".to_string(), "1.0".to_string());
            m
        },
    };

    let memory_config = MemoryConfig {
        enabled: true,
        backend: "sqlite".to_string(),
        path: Some("./social_media_manager_memory.db".to_string()),
    };

    let provider_config = ProviderConfig {
        provider_type: ProviderType::OpenAI,
        model: ModelConfig {
            name: "gpt-4o-mini".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(500),
        },
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        api_base: None,
    };

    let agent = Agent::new(agent_config)
        .with_memory(memory_config)
        .with_provider(provider_config)?;

    Ok(agent)
}

/// Custom interactive loop for social media management
async fn run_social_media_loop(
    mut agent: Agent,
    channel: &mut CliChannel,
    social_tool: SocialMediaTool,
) -> anyhow::Result<()> {
    use std::io::{self, Write};

    println!("\n📱 Social Media Manager Ready!\n");

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            println!("Goodbye!");
            break;
        }

        if input.eq_ignore_ascii_case("help") {
            print_help();
            continue;
        }

        // Parse command
        let parts: Vec<&str> = input.splitn(3, ' ').collect();
        if parts.is_empty() {
            continue;
        }

        let command = parts[0];

        match command {
            "draft" => {
                if parts.len() < 3 {
                    println!("❌ Usage: draft <platform> <content>");
                    continue;
                }
                let platform = parts[1];
                let content = parts[2];
                draft_post(&social_tool, platform, content).await?;
            }

            "schedule" => {
                if parts.len() < 3 {
                    println!("❌ Usage: schedule <post_id> <datetime>");
                    continue;
                }
                let post_id = parts[1];
                let datetime = parts[2];
                schedule_post(&social_tool, post_id, datetime).await?;
            }

            "publish" => {
                if parts.len() < 2 {
                    println!("❌ Usage: publish <post_id>");
                    continue;
                }
                let post_id = parts[1];
                publish_post(&social_tool, post_id).await?;
            }

            "scheduled" => {
                list_scheduled(&social_tool).await?;
            }

            "drafts" => {
                list_drafts(&social_tool).await?;
            }

            "analytics" => {
                if parts.len() < 2 {
                    println!("❌ Usage: analytics <post_id>");
                    continue;
                }
                let post_id = parts[1];
                get_analytics(&social_tool, post_id).await?;
            }

            _ => {
                // Default: treat as question for AI
                match agent.execute(input).await {
                    Ok(response) => println!("\n🤖 {}\n", response),
                    Err(e) => error!("❌ Error: {}", e),
                }
            }
        }
    }

    Ok(())
}

/// Draft a new post
async fn draft_post(tool: &SocialMediaTool, platform: &str, content: &str) -> anyhow::Result<()> {
    info!("📝 Drafting post for {}...", platform);

    let result = tool
        .execute(serde_json::json!({
            "command": "draft_post",
            "platform": platform,
            "content": content
        }))
        .await?;

    if result
        .get("success")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        if let Some(post) = result.get("post") {
            if let Some(id) = post.get("id").and_then(|i| i.as_str()) {
                println!("\n✅ Post drafted!");
                println!("   ID: {}", id);
                println!("   Platform: {}", platform);
                println!("   Content: {}...", &content[..content.len().min(50)]);
                println!("\n💡 Use 'schedule {} <datetime>' to schedule", id);
            }
        }
    } else {
        println!("❌ Failed to draft post");
    }

    Ok(())
}

/// Schedule a post
async fn schedule_post(
    tool: &SocialMediaTool,
    post_id: &str,
    datetime: &str,
) -> anyhow::Result<()> {
    info!("📅 Scheduling post {} for {}...", post_id, datetime);

    // Parse datetime (expecting ISO 8601)
    let result = tool
        .execute(serde_json::json!({
            "command": "schedule_post",
            "post_id": post_id,
            "scheduled_at": datetime
        }))
        .await?;

    if result
        .get("success")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        println!("\n✅ Post scheduled!");
        println!("   ID: {}", post_id);
        println!("   Time: {}", datetime);
    } else {
        println!("❌ Failed to schedule post");
    }

    Ok(())
}

/// Publish a post immediately
async fn publish_post(tool: &SocialMediaTool, post_id: &str) -> anyhow::Result<()> {
    info!("🚀 Publishing post {}...", post_id);

    let result = tool
        .execute(serde_json::json!({
            "command": "publish",
            "post_id": post_id
        }))
        .await?;

    if result
        .get("success")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        println!("\n✅ Post published!");
        println!("   ID: {}", post_id);
        println!(
            "   Time: {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    } else {
        println!("❌ Failed to publish post");
    }

    Ok(())
}

/// List scheduled posts
async fn list_scheduled(tool: &SocialMediaTool) -> anyhow::Result<()> {
    let result = tool
        .execute(serde_json::json!({
            "command": "list_scheduled"
        }))
        .await?;

    if let Some(posts) = result.get("posts").and_then(|p| p.as_array()) {
        if posts.is_empty() {
            println!("\n📭 No scheduled posts");
        } else {
            println!("\n📅 Scheduled Posts ({}):", posts.len());
            for post in posts {
                if let (Some(id), Some(platform), Some(time)) = (
                    post.get("id").and_then(|i| i.as_str()),
                    post.get("platform").and_then(|p| p.as_str()),
                    post.get("scheduled_at").and_then(|t| t.as_str()),
                ) {
                    println!("   • [{}] {} - {}", id, platform, time);
                }
            }
        }
    }

    Ok(())
}

/// List drafts
async fn list_drafts(tool: &SocialMediaTool) -> anyhow::Result<()> {
    let result = tool
        .execute(serde_json::json!({
            "command": "list_drafts"
        }))
        .await?;

    if let Some(posts) = result.get("posts").and_then(|p| p.as_array()) {
        if posts.is_empty() {
            println!("\n📝 No drafts");
        } else {
            println!("\n📝 Drafts ({}):", posts.len());
            for post in posts {
                if let (Some(id), Some(platform), Some(preview)) = (
                    post.get("id").and_then(|i| i.as_str()),
                    post.get("platform").and_then(|p| p.as_str()),
                    post.get("content_preview").and_then(|c| c.as_str()),
                ) {
                    println!("   • [{}] {}: {}...", id, platform, preview);
                }
            }
        }
    }

    Ok(())
}

/// Get analytics for a post
async fn get_analytics(tool: &SocialMediaTool, post_id: &str) -> anyhow::Result<()> {
    let result = tool
        .execute(serde_json::json!({
            "command": "get_analytics",
            "post_id": post_id
        }))
        .await?;

    if result
        .get("success")
        .and_then(|s| s.as_bool())
        .unwrap_or(false)
    {
        if let Some(analytics) = result.get("analytics") {
            let likes = analytics.get("likes").and_then(|l| l.as_u64()).unwrap_or(0);
            let replies = analytics
                .get("replies")
                .and_then(|r| r.as_u64())
                .unwrap_or(0);
            let reposts = analytics
                .get("reposts")
                .and_then(|r| r.as_u64())
                .unwrap_or(0);
            let impressions = analytics
                .get("impressions")
                .and_then(|i| i.as_u64())
                .unwrap_or(0);

            println!("\n📊 Analytics for post {}:", post_id);
            println!("   ❤️  Likes: {}", likes);
            println!("   💬 Replies: {}", replies);
            println!("   🔄 Reposts: {}", reposts);
            println!("   👁️  Impressions: {}", impressions);
        }
    } else {
        println!("❌ Failed to get analytics");
    }

    Ok(())
}

/// Print help text
fn print_help() {
    println!(
        r#"
📱 Social Media Manager Commands:

  draft <platform> <content>    - Create a new post draft
  schedule <post_id> <time>   - Schedule a draft for publishing
  publish <post_id>           - Publish a post immediately
  scheduled                  - List all scheduled posts
  drafts                     - List all draft posts
  analytics <post_id>        - Get engagement metrics
  help                       - Show this help message
  quit/exit                  - Exit the program

Examples:
  > draft twitter "Excited to announce our new product!"
  > schedule post_abc123 2026-02-20T14:00:00Z
  > publish post_abc123
  > analytics post_abc123

Platforms: twitter, linkedin

The agent will:
- Draft engaging posts for each platform
- Suggest optimal posting times
- Track engagement metrics
- Maintain brand voice consistency

Prerequisites:
- Twitter API credentials (for Twitter/X posting)
- LinkedIn API credentials (for LinkedIn posting)
"#
    );
}
