//! Reputation System Client
//!
//! Lightweight client for querying Coneko's reputation system.
//! Pekobot stays minimal — all reputation logic lives in Coneko.

use serde::{Deserialize, Serialize};

/// Reputation client configuration
#[derive(Debug, Clone)]
pub struct ReputationClientConfig {
    /// Coneko reputation API URL
    pub endpoint: String,
    /// Optional API key
    pub api_key: Option<String>,
    /// Request timeout
    pub timeout_secs: u64,
}

impl Default for ReputationClientConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000".to_string(),
            api_key: None,
            timeout_secs: 30,
        }
    }
}

/// Reputation score from Coneko
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationScore {
    #[serde(rename = "agentDid")]
    pub agent_did: String,
    #[serde(rename = "overallScore")]
    pub overall_score: f64,
    pub metrics: ReputationMetrics,
    pub history: Vec<ReputationEvent>,
    #[serde(rename = "calculatedAt")]
    pub calculated_at: chrono::DateTime<chrono::Utc>,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationMetrics {
    #[serde(rename = "uptimePercent")]
    pub uptime_percent: f64,
    #[serde(rename = "successRate")]
    pub success_rate: f64,
    #[serde(rename = "avgResponseTimeMs")]
    pub avg_response_time_ms: u64,
    #[serde(rename = "ratingAvg")]
    pub rating_avg: f64,
    #[serde(rename = "ratingCount")]
    pub rating_count: u32,
    #[serde(rename = "complaintCount")]
    pub complaint_count: u32,
    #[serde(rename = "totalTasksCompleted")]
    pub total_tasks_completed: u32,
    #[serde(rename = "totalTasksFailed")]
    pub total_tasks_failed: u32,
    #[serde(rename = "daysActive")]
    pub days_active: u32,
    #[serde(rename = "attestationCount")]
    pub attestation_count: u32,
    #[serde(rename = "disputeCount")]
    pub dispute_count: u32,
    #[serde(rename = "disputeResolutionRate")]
    pub dispute_resolution_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationEvent {
    pub id: String,
    #[serde(rename = "agentDid")]
    pub agent_did: String,
    #[serde(rename = "eventType")]
    pub event_type: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub score: f64,
    pub description: String,
    pub source: String,
    pub evidence: Option<serde_json::Value>,
}

/// Rating submitted by another agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rating {
    pub id: String,
    #[serde(rename = "targetAgentDid")]
    pub target_agent_did: String,
    #[serde(rename = "sourceAgentDid")]
    pub source_agent_did: String,
    pub rating: u8, // 1-5
    pub review: Option<String>,
    #[serde(rename = "taskId")]
    pub task_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Attestation (verifiable credential)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attestation {
    pub id: String,
    #[serde(rename = "targetAgentDid")]
    pub target_agent_did: String,
    #[serde(rename = "attestorDid")]
    pub attestor_did: String,
    #[serde(rename = "attestationType")]
    pub attestation_type: String,
    pub claim: String,
    pub signature: String,
    #[serde(rename = "createdAt")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Reputation query criteria
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReputationQuery {
    #[serde(rename = "minOverallScore")]
    pub min_overall_score: Option<f64>,
    #[serde(rename = "minSuccessRate")]
    pub min_success_rate: Option<f64>,
    #[serde(rename = "minRatingCount")]
    pub min_rating_count: Option<u32>,
    #[serde(rename = "maxResponseTimeMs")]
    pub max_response_time_ms: Option<u64>,
    #[serde(rename = "hasAttestation")]
    pub has_attestation: Option<String>,
}

/// Lightweight client for Coneko's reputation system
pub struct ReputationClient {
    config: ReputationClientConfig,
    http_client: reqwest::Client,
}

impl ReputationClient {
    /// Create a new reputation client
    pub fn new(config: ReputationClientConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Create client with default config
    pub fn default_client() -> anyhow::Result<Self> {
        Self::new(ReputationClientConfig::default())
    }

    /// Get reputation score for an agent
    pub async fn get_score(
        &self,
        agent_did: &str,
        recalculate: bool,
    ) -> anyhow::Result<Option<ReputationScore>> {
        let mut url = format!("{}/reputation/scores/{}", self.config.endpoint, agent_did);
        
        if recalculate {
            url.push_str("?recalculate=true");
        }

        let response = self.http_client.get(&url).send().await?;
        
        if response.status() == 404 {
            return Ok(None);
        }
        
        if !response.status().is_success() {
            anyhow::bail!("Reputation query failed: {}", response.status());
        }

        let result: GetScoreResponse = response.json().await?;
        Ok(Some(result.score))
    }

    /// Force recalculation of reputation score
    pub async fn calculate_score(
        &self,
        agent_did: >str,
    ) -> anyhow::Result<ReputationScore> {
        let url = format!(
            "{}/reputation/scores/{}/calculate",
            self.config.endpoint, agent_did
        );

        let response = self.http_client.post(&url).send().await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Score calculation failed: {}", response.status());
        }

        let result: GetScoreResponse = response.json().await?;
        Ok(result.score)
    }

    /// Submit a rating for another agent
    pub async fn submit_rating(
        &self,
        target_did: &str,
        source_did: &str,
        rating: u8,
        review: Option<&str>,
        task_id: Option<&str>,
    ) -> anyhow::Result<Rating> {
        let url = format!("{}/reputation/ratings", self.config.endpoint);
        
        let body = SubmitRatingRequest {
            target_agent_did: target_did.to_string(),
            source_agent_did: source_did.to_string(),
            rating,
            review: review.map(|s| s.to_string()),
            task_id: task_id.map(|s| s.to_string()),
        };
        
        let response = self.http_client
            .post(&url)
            .json(&body)
            .send()
            .await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to submit rating: {}", response.status());
        }

        let result: SubmitRatingResponse = response.json().await?;
        Ok(result.rating)
    }

    /// Get ratings for an agent
    pub async fn get_ratings(
        &self,
        agent_did: &str,
    ) -> anyhow::Result<Vec<Rating>> {
        let url = format!("{}/reputation/ratings/{}", self.config.endpoint, agent_did);
        
        let response = self.http_client.get(&url).send().await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to get ratings: {}", response.status());
        }

        let result: GetRatingsResponse = response.json().await?;
        Ok(result.ratings)
    }

    /// Query agents by reputation criteria
    pub async fn query_agents(
        &self,
        query: &ReputationQuery,
    ) -> anyhow::Result<Vec<ReputationScore>> {
        let url = format!("{}/reputation/query", self.config.endpoint);
        
        let response = self.http_client
            .post(&url)
            .json(query)
            .send()
            .await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Query failed: {}", response.status());
        }

        let result: QueryResponse = response.json().await?;
        Ok(result.agents)
    }

    /// Get top agents by reputation (leaderboard)
    pub async fn get_leaderboard(
        &self,
        limit: u32,
    ) -> anyhow::Result<Vec<LeaderboardEntry>> {
        let url = format!(
            "{}/reputation/leaderboard?limit={}",
            self.config.endpoint, limit
        );
        
        let response = self.http_client.get(&url).send().await?;
        
        if !response.status().is_success() {
            anyhow::bail!("Failed to get leaderboard: {}", response.status());
        }

        let result: LeaderboardResponse = response.json().await?;
        Ok(result.leaderboard)
    }

    /// Check if agent meets minimum reputation threshold
    pub async fn check_reputation(
        &self,
        agent_did: &str,
        min_score: f64,
    ) -> anyhow::Result<bool> {
        match self.get_score(agent_did, false).await? {
            Some(score) => Ok(score.overall_score >= min_score),
            None => Ok(false),
        }
    }
}

/// Leaderboard entry
#[derive(Debug, Clone, Deserialize)]
pub struct LeaderboardEntry {
    #[serde(rename = "agentDid")]
    pub agent_did: String,
    #[serde(rename = "overallScore")]
    pub overall_score: f64,
    #[serde(rename = "ratingAvg")]
    pub rating_avg: f64,
    #[serde(rename = "ratingCount")]
    pub rating_count: u32,
    #[serde(rename = "successRate")]
    pub success_rate: f64,
}

// Request/Response types
#[derive(Debug, Deserialize)]
struct GetScoreResponse {
    score: ReputationScore,
}

#[derive(Debug, Serialize)]
struct SubmitRatingRequest {
    #[serde(rename = "targetAgentDid")]
    target_agent_did: String,
    #[serde(rename = "sourceAgentDid")]
    source_agent_did: String,
    rating: u8,
    review: Option<String>,
    #[serde(rename = "taskId")]
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubmitRatingResponse {
    rating: Rating,
}

#[derive(Debug, Deserialize)]
struct GetRatingsResponse {
    ratings: Vec<Rating>,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    agents: Vec<ReputationScore>,
}

#[derive(Debug, Deserialize)]
struct LeaderboardResponse {
    leaderboard: Vec<LeaderboardEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reputation_client_default() {
        let client = ReputationClient::default_client();
        assert!(client.is_ok());
    }
}
