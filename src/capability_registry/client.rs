//! Capability Registry Client
//!
//! Lightweight client for querying Coneko's capability registry.
//! Pekobot stays minimal — all registry logic lives in Coneko.

use serde::{Deserialize, Serialize};

/// Configuration for the registry client
#[derive(Debug, Clone)]
pub struct RegistryClientConfig {
    /// Coneko registry URL
    pub endpoint: String,
    /// Optional API key for authenticated requests
    pub api_key: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

impl Default for RegistryClientConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:3000".to_string(),
            api_key: None,
            timeout_secs: 30,
        }
    }
}

/// Capability definition (from Coneko registry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub category: String,
    pub parameters: Vec<CapabilityParameter>,
    pub returns: CapabilityReturn,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance: Option<PerformanceCharacteristics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityParameter {
    pub name: String,
    #[serde(rename = "paramType")]
    pub param_type: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityReturn {
    #[serde(rename = "returnType")]
    pub return_type: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceCharacteristics {
    #[serde(rename = "typicalLatencyMs")]
    pub typical_latency_ms: u64,
    #[serde(rename = "maxThroughputPerMin")]
    pub max_throughput_per_min: u64,
    #[serde(rename = "availabilitySla")]
    pub availability_sla: f32,
}

/// Agent capability advertisement (from Coneko registry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilityAdvertisement {
    #[serde(rename = "agentDid")]
    pub agent_did: String,
    #[serde(rename = "createdAt")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "expiresAt")]
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub capabilities: Vec<CapabilityClaim>,
    pub endpoint: String,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "reputationScore")]
    pub reputation_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityClaim {
    #[serde(rename = "capabilityId")]
    pub capability_id: String,
    pub confidence: f32,
    pub constraints: Vec<CapabilityConstraint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<PricingInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityConstraint {
    #[serde(rename = "constraintType")]
    pub constraint_type: String,
    pub description: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingInfo {
    pub currency: String,
    #[serde(rename = "unitPrice")]
    pub unit_price: f64,
    pub unit: String,
}

/// Lightweight client for Coneko's capability registry
pub struct RegistryClient {
    config: RegistryClientConfig,
    http_client: reqwest::Client,
}

impl RegistryClient {
    /// Create a new registry client
    pub fn new(config: RegistryClientConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Create client with default config (localhost:3000)
    pub fn default() -> anyhow::Result<Self> {
        Self::new(RegistryClientConfig::default())
    }

    /// Find agents by capability
    pub async fn find_agents(
        &self,
        capability_id: &str,
        min_reputation: Option<f32>,
    ) -> anyhow::Result<Vec<AgentCapabilityAdvertisement>> {
        let mut url = format!(
            "{}/registry/capabilities/{}/agents",
            self.config.endpoint, capability_id
        );

        if let Some(rep) = min_reputation {
            url.push_str(&format!("?minReputation={rep}"));
        }

        let response = self.http_client.get(&url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Registry query failed: {}", response.status());
        }

        let result: FindAgentsResponse = response.json().await?;
        Ok(result.agents)
    }

    /// Get specific agent's capabilities
    pub async fn get_agent_capabilities(
        &self,
        agent_did: &str,
    ) -> anyhow::Result<Option<AgentCapabilityAdvertisement>> {
        let url = format!("{}/registry/agents/{}", self.config.endpoint, agent_did);

        let response = self.http_client.get(&url).send().await?;

        if response.status() == 404 {
            return Ok(None);
        }

        if !response.status().is_success() {
            anyhow::bail!("Registry query failed: {}", response.status());
        }

        let result: GetAgentResponse = response.json().await?;
        Ok(Some(result.agent))
    }

    /// Query by multiple capabilities
    pub async fn query_capabilities(
        &self,
        capabilities: Vec<String>,
        match_mode: MatchMode,
    ) -> anyhow::Result<Vec<AgentCapabilityAdvertisement>> {
        let url = format!("{}/registry/query", self.config.endpoint);

        let body = QueryRequest {
            capabilities,
            match_mode: match match_mode {
                MatchMode::All => "all",
                MatchMode::Any => "any",
            },
        };

        let response = self.http_client.post(&url).json(&body).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Registry query failed: {}", response.status());
        }

        let result: QueryResponse = response.json().await?;
        Ok(result.agents)
    }

    /// Get capability definition
    pub async fn get_capability(&self, capability_id: &str) -> anyhow::Result<Option<Capability>> {
        let url = format!(
            "{}/registry/capabilities/{}",
            self.config.endpoint, capability_id
        );

        let response = self.http_client.get(&url).send().await?;

        if response.status() == 404 {
            return Ok(None);
        }

        if !response.status().is_success() {
            anyhow::bail!("Registry query failed: {}", response.status());
        }

        let result: GetCapabilityResponse = response.json().await?;
        Ok(Some(result.capability))
    }

    /// Advertise this agent's capabilities to the registry
    pub async fn advertise(
        &self,
        advertisement: &AgentCapabilityAdvertisement,
    ) -> anyhow::Result<()> {
        let url = format!("{}/registry/advertise", self.config.endpoint);

        let response = self
            .http_client
            .post(&url)
            .json(advertisement)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Failed to advertise: {}", response.status());
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MatchMode {
    All,
    Any,
}

// Response types
#[derive(Debug, Deserialize)]
struct FindAgentsResponse {
    agents: Vec<AgentCapabilityAdvertisement>,
}

#[derive(Debug, Deserialize)]
struct GetAgentResponse {
    agent: AgentCapabilityAdvertisement,
}

#[derive(Debug, Deserialize)]
struct QueryResponse {
    agents: Vec<AgentCapabilityAdvertisement>,
}

#[derive(Debug, Deserialize)]
struct GetCapabilityResponse {
    capability: Capability,
}

#[derive(Debug, Serialize)]
struct QueryRequest {
    capabilities: Vec<String>,
    #[serde(rename = "matchMode")]
    match_mode: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_client_default() {
        let client = RegistryClient::default();
        assert!(client.is_ok());
    }
}
