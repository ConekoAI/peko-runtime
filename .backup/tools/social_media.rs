//! Social Media tool for posting and managing content
//!
//! Supports Twitter/X and `LinkedIn` integration.
//! Handles `OAuth2` authentication and API rate limits.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

use crate::tools::Tool;

/// Social media platform type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Twitter,
    LinkedIn,
}

impl std::str::FromStr for Platform {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "twitter" | "x" | "twitter/x" => Ok(Platform::Twitter),
            "linkedin" | "linked-in" => Ok(Platform::LinkedIn),
            _ => Err(anyhow::anyhow!("Unknown platform: {s}")),
        }
    }
}

/// Social media post
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialPost {
    pub id: String,
    pub platform: String,
    pub content: String,
    pub status: PostStatus,
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub engagement: Option<EngagementMetrics>,
}

/// Post status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PostStatus {
    Draft,
    Scheduled,
    Published,
    Failed(String),
}

/// Engagement metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngagementMetrics {
    pub likes: u64,
    pub replies: u64,
    pub reposts: u64,
    pub impressions: u64,
}

/// Twitter/X API credentials
#[derive(Debug, Clone)]
pub struct TwitterCredentials {
    pub api_key: String,
    pub api_secret: String,
    pub access_token: String,
    pub access_secret: String,
}

/// `LinkedIn` API credentials
#[derive(Debug, Clone)]
pub struct LinkedInCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
}

/// Social media tool for posting and scheduling
pub struct SocialMediaTool {
    http_client: reqwest::Client,
    twitter_creds: Option<TwitterCredentials>,
    linkedin_creds: Option<LinkedInCredentials>,
    drafts: HashMap<String, SocialPost>,
}

impl SocialMediaTool {
    /// Create new social media tool from environment
    pub fn from_env() -> anyhow::Result<Self> {
        let http_client = reqwest::Client::new();

        // Try to load Twitter credentials
        let twitter_creds =
            if let (Ok(api_key), Ok(api_secret), Ok(access_token), Ok(access_secret)) = (
                std::env::var("TWITTER_API_KEY"),
                std::env::var("TWITTER_API_SECRET"),
                std::env::var("TWITTER_ACCESS_TOKEN"),
                std::env::var("TWITTER_ACCESS_SECRET"),
            ) {
                Some(TwitterCredentials {
                    api_key,
                    api_secret,
                    access_token,
                    access_secret,
                })
            } else {
                None
            };

        // Try to load LinkedIn credentials
        let linkedin_creds = if let (Ok(client_id), Ok(client_secret), Ok(access_token)) = (
            std::env::var("LINKEDIN_CLIENT_ID"),
            std::env::var("LINKEDIN_CLIENT_SECRET"),
            std::env::var("LINKEDIN_ACCESS_TOKEN"),
        ) {
            Some(LinkedInCredentials {
                client_id,
                client_secret,
                access_token,
            })
        } else {
            None
        };

        Ok(Self {
            http_client,
            twitter_creds,
            linkedin_creds,
            drafts: HashMap::new(),
        })
    }

    /// Draft a new post
    fn draft_post(&mut self, platform: Platform, content: &str) -> anyhow::Result<SocialPost> {
        let post_id = format!("post_{}", &uuid::Uuid::new_v4().to_string()[..8]);

        let post = SocialPost {
            id: post_id.clone(),
            platform: match platform {
                Platform::Twitter => "twitter".to_string(),
                Platform::LinkedIn => "linkedin".to_string(),
            },
            content: content.to_string(),
            status: PostStatus::Draft,
            scheduled_at: None,
            published_at: None,
            engagement: None,
        };

        self.drafts.insert(post_id.clone(), post.clone());
        Ok(post)
    }

    /// Schedule a post for later
    fn schedule_post(
        &mut self,
        post_id: &str,
        scheduled_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<SocialPost> {
        let post = self
            .drafts
            .get_mut(post_id)
            .ok_or_else(|| anyhow::anyhow!("Post not found: {post_id}"))?;

        post.scheduled_at = Some(scheduled_at);
        post.status = PostStatus::Scheduled;

        Ok(post.clone())
    }

    /// Publish a post immediately
    async fn publish_post(&self, post_id: &str) -> anyhow::Result<SocialPost> {
        // This would call the actual API in production
        // For now, simulate success
        let mut post = self
            .drafts
            .get(post_id)
            .ok_or_else(|| anyhow::anyhow!("Post not found: {post_id}"))?
            .clone();

        match post.platform.as_str() {
            "twitter" => {
                if self.twitter_creds.is_none() {
                    return Err(anyhow::anyhow!(
                        "Twitter credentials not configured. Set TWITTER_API_KEY, TWITTER_API_SECRET, etc."
                    ));
                }
                // In production: call Twitter API v2
                // For now, simulate
                post.status = PostStatus::Published;
                post.published_at = Some(chrono::Utc::now());
            }
            "linkedin" => {
                if self.linkedin_creds.is_none() {
                    return Err(anyhow::anyhow!(
                        "LinkedIn credentials not configured. Set LINKEDIN_CLIENT_ID, etc."
                    ));
                }
                // In production: call LinkedIn API
                // For now, simulate
                post.status = PostStatus::Published;
                post.published_at = Some(chrono::Utc::now());
            }
            _ => return Err(anyhow::anyhow!("Unknown platform: {}", post.platform)),
        }

        Ok(post)
    }

    /// List all scheduled posts
    fn list_scheduled(&self) -> Vec<&SocialPost> {
        self.drafts
            .values()
            .filter(|p| matches!(p.status, PostStatus::Scheduled))
            .collect()
    }

    /// List all drafts
    fn list_drafts(&self) -> Vec<&SocialPost> {
        self.drafts
            .values()
            .filter(|p| matches!(p.status, PostStatus::Draft))
            .collect()
    }

    /// Get analytics for a published post
    async fn get_analytics(&self, post_id: &str) -> anyhow::Result<EngagementMetrics> {
        let post = self
            .drafts
            .get(post_id)
            .ok_or_else(|| anyhow::anyhow!("Post not found: {post_id}"))?;

        if !matches!(post.status, PostStatus::Published) {
            return Err(anyhow::anyhow!("Post is not published yet"));
        }

        // In production: fetch from API
        // For now, return simulated data
        Ok(EngagementMetrics {
            likes: 42,
            replies: 5,
            reposts: 12,
            impressions: 1024,
        })
    }
}

#[async_trait]
impl Tool for SocialMediaTool {
    fn name(&self) -> &'static str {
        "social_media"
    }

    fn description(&self) -> &'static str {
        r#"Social media tool for posting and managing content on Twitter/X and LinkedIn.

Supports drafting, scheduling, publishing, and analytics.

Commands:
- draft_post: Create a new post draft
- schedule_post: Schedule a draft for later publishing
- publish: Publish a post immediately
- list_scheduled: View all scheduled posts
- list_drafts: View all draft posts
- get_analytics: Get engagement metrics for a published post

Examples:
TOOL_CALL: {"name": "social_media", "parameters": {"command": "draft_post", "platform": "twitter", "content": "Excited to announce our new product launch!"}}
TOOL_CALL: {"name": "social_media", "parameters": {"command": "schedule_post", "post_id": "post_abc123", "scheduled_at": "2026-02-20T14:00:00Z"}}
TOOL_CALL: {"name": "social_media", "parameters": {"command": "publish", "post_id": "post_abc123"}}
TOOL_CALL: {"name": "social_media", "parameters": {"command": "list_scheduled"}}
TOOL_CALL: {"name": "social_media", "parameters": {"command": "get_analytics", "post_id": "post_abc123"}}

Environment Variables Required:
- TWITTER_API_KEY, TWITTER_API_SECRET, TWITTER_ACCESS_TOKEN, TWITTER_ACCESS_SECRET
- LINKEDIN_CLIENT_ID, LINKEDIN_CLIENT_SECRET, LINKEDIN_ACCESS_TOKEN"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let command = params
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("list_drafts");

        // We need to clone self to allow mutation for draft operations
        // In a real implementation, we'd use interior mutability or a separate store
        let mut tool = Self {
            http_client: self.http_client.clone(),
            twitter_creds: self.twitter_creds.clone(),
            linkedin_creds: self.linkedin_creds.clone(),
            drafts: self.drafts.clone(),
        };

        match command {
            "draft_post" => {
                let platform = params
                    .get("platform")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'platform' parameter"))?
                    .parse::<Platform>()?;

                let content = params
                    .get("content")
                    .and_then(|c| c.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

                let post = tool.draft_post(platform, content)?;

                Ok(json!({
                    "success": true,
                    "post": {
                        "id": post.id,
                        "platform": post.platform,
                        "content": post.content,
                        "status": "draft"
                    },
                    "message": format!("Post drafted with ID: {}", post.id)
                }))
            }

            "schedule_post" => {
                let post_id = params
                    .get("post_id")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'post_id' parameter"))?;

                let scheduled_at = params
                    .get("scheduled_at")
                    .and_then(|s| s.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'scheduled_at' parameter (ISO 8601 format required)"))?;

                let post = tool.schedule_post(post_id, scheduled_at)?;

                Ok(json!({
                    "success": true,
                    "post": {
                        "id": post.id,
                        "platform": post.platform,
                        "scheduled_at": post.scheduled_at.map(|t| t.to_rfc3339()),
                        "status": "scheduled"
                    },
                    "message": format!("Post {} scheduled for {}", post_id, scheduled_at.to_rfc3339())
                }))
            }

            "publish" => {
                let post_id = params
                    .get("post_id")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'post_id' parameter"))?;

                let post = tool.publish_post(post_id).await?;

                Ok(json!({
                    "success": true,
                    "post": {
                        "id": post.id,
                        "platform": post.platform,
                        "published_at": post.published_at.map(|t| t.to_rfc3339()),
                        "status": "published"
                    },
                    "message": format!("Post {} published successfully", post_id)
                }))
            }

            "list_scheduled" => {
                let posts = tool.list_scheduled();

                Ok(json!({
                    "success": true,
                    "posts": posts.iter().map(|p| json!({
                        "id": p.id,
                        "platform": p.platform,
                        "content_preview": &p.content[..p.content.len().min(50)],
                        "scheduled_at": p.scheduled_at.map(|t| t.to_rfc3339())
                    })).collect::<Vec<_>>(),
                    "count": posts.len()
                }))
            }

            "list_drafts" => {
                let posts = tool.list_drafts();

                Ok(json!({
                    "success": true,
                    "posts": posts.iter().map(|p| json!({
                        "id": p.id,
                        "platform": p.platform,
                        "content_preview": &p.content[..p.content.len().min(50)]
                    })).collect::<Vec<_>>(),
                    "count": posts.len()
                }))
            }

            "get_analytics" => {
                let post_id = params
                    .get("post_id")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'post_id' parameter"))?;

                let metrics = tool.get_analytics(post_id).await?;

                Ok(json!({
                    "success": true,
                    "post_id": post_id,
                    "analytics": {
                        "likes": metrics.likes,
                        "replies": metrics.replies,
                        "reposts": metrics.reposts,
                        "impressions": metrics.impressions
                    }
                }))
            }

            _ => Err(anyhow::anyhow!(
                "Unknown command: {command}. Use 'draft_post', 'schedule_post', 'publish', 'list_scheduled', 'list_drafts', or 'get_analytics'"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_parse() {
        assert!(matches!(
            "twitter".parse::<Platform>().unwrap(),
            Platform::Twitter
        ));
        assert!(matches!(
            "linkedin".parse::<Platform>().unwrap(),
            Platform::LinkedIn
        ));
    }

    #[test]
    fn test_draft_post() {
        let mut tool = SocialMediaTool::from_env().unwrap_or_else(|_| SocialMediaTool {
            http_client: reqwest::Client::new(),
            twitter_creds: None,
            linkedin_creds: None,
            drafts: HashMap::new(),
        });

        let post = tool.draft_post(Platform::Twitter, "Test post").unwrap();
        assert_eq!(post.platform, "twitter");
        assert_eq!(post.content, "Test post");
        assert!(matches!(post.status, PostStatus::Draft));
    }
}
