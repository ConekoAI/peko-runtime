# Agent Capability Registry Design Document

## Overview

The Agent Capability Registry is the foundation of Layer 2 (Trust Network). It provides a standardized way for agents to:
1. Advertise what they can do (capabilities)
2. Discover other agents with specific capabilities
3. Verify capability claims through attestation

## Core Concepts

### Capability

A **capability** is a semantic description of what an agent can do. Not the implementation (tool), but the promise of functionality.

```rust
pub struct Capability {
    /// Unique identifier for this capability type
    pub id: String,
    
    /// Human-readable name
    pub name: String,
    
    /// Description of what this capability provides
    pub description: String,
    
    /// Version of this capability specification
    pub version: String,
    
    /// Category for grouping (e.g., "communication", "scheduling", "data_processing")
    pub category: String,
    
    /// Required parameters for this capability
    pub parameters: Vec<CapabilityParameter>,
    
    /// Return type/output of this capability
    pub returns: CapabilityReturn,
    
    /// Optional: Performance characteristics
    pub performance: Option<PerformanceCharacteristics>,
}

pub struct CapabilityParameter {
    pub name: String,
    pub param_type: String, // "string", "number", "boolean", "object", "array"
    pub required: bool,
    pub description: String,
}

pub struct CapabilityReturn {
    pub return_type: String,
    pub description: String,
}

pub struct PerformanceCharacteristics {
    /// Typical response time in milliseconds
    pub typical_latency_ms: u64,
    
    /// Maximum throughput (requests per minute)
    pub max_throughput_per_min: u64,
    
    /// Availability SLA (0.0 - 1.0)
    pub availability_sla: f32,
}
```

### Agent Capability Advertisement

How an agent publishes what it can do:

```rust
pub struct AgentCapabilityAdvertisement {
    /// Agent's DID
    pub agent_did: String,
    
    /// When this advertisement was created
    pub created_at: chrono::DateTime<chrono::Utc>,
    
    /// When this advertisement expires
    pub expires_at: chrono::DateTime<chrono::Utc>,
    
    /// Capabilities this agent claims
    pub capabilities: Vec<CapabilityClaim>,
    
    /// Verifiable credential attesting to these capabilities (optional)
    pub attestation: Option<Attestation>,
    
    /// Agent's reputation score (from reputation system)
    pub reputation_score: Option<f32>,
    
    /// Contact endpoint for this agent
    pub endpoint: String,
    
    /// Supported A2A protocol version
    pub protocol_version: String,
}

pub struct CapabilityClaim {
    /// Reference to the capability definition
    pub capability_id: String,
    
    /// Agent's confidence in providing this capability (0.0 - 1.0)
    pub confidence: f32,
    
    /// Any constraints or limitations
    pub constraints: Vec<CapabilityConstraint>,
    
    /// Pricing if applicable (for economic layer)
    pub pricing: Option<PricingInfo>,
}

pub struct CapabilityConstraint {
    pub constraint_type: String, // "rate_limit", "availability_hours", "max_payload_size"
    pub description: String,
    pub value: serde_json::Value,
}

pub struct PricingInfo {
    pub currency: String,
    pub unit_price: f64,
    pub unit: String, // "per_request", "per_1000_tokens", "per_hour"
}

pub struct Attestation {
    /// DID of the attestor (could be root CA, federated trust node)
    pub attestor_did: String,
    
    /// Cryptographic signature of the advertisement
    pub signature: String,
    
    /// Timestamp of attestation
    pub attested_at: chrono::DateTime<chrono::Utc>,
}
```

## Registry Interface

```rust
#[async_trait]
pub trait CapabilityRegistry {
    /// Register an agent's capabilities
    async fn register(
        &mut self,
        advertisement: AgentCapabilityAdvertisement,
    ) -> anyhow::Result<()>;

    /// Find agents by capability
    async fn find_agents(
        &self,
        capability_id: &str,
        filters: Option<AgentFilters>,
    ) -> anyhow::Result<Vec<AgentCapabilityAdvertisement>>;

    /// Get specific agent's capabilities
    async fn get_agent_capabilities(
        &self,
        agent_did: &str,
    ) -> anyhow::Result<Option<AgentCapabilityAdvertisement>>;

    /// Query by multiple capabilities (AND/OR)
    async fn find_by_capabilities(
        &self,
        query: CapabilityQuery,
    ) -> anyhow::Result<Vec<AgentCapabilityAdvertisement>>;

    /// Verify an agent's attestation
    async fn verify_attestation(
        &self,
        agent_did: &str,
    ) -> anyhow::Result<bool>;

    /// List all registered agents
    async fn list_agents(
        &self,
        pagination: PaginationParams,
    ) -> anyhow::Result<Vec<AgentCapabilityAdvertisement>>;
}

pub struct AgentFilters {
    pub min_reputation: Option<f32>,
    pub max_price: Option<PricingInfo>,
    pub available_now: bool,
    pub protocol_version: Option<String>,
}

pub struct CapabilityQuery {
    pub capabilities: Vec<String>,
    pub match_mode: MatchMode, // All, Any
    pub filters: Option<AgentFilters>,
}

pub enum MatchMode {
    All, // Agent must have ALL listed capabilities
    Any, // Agent must have ANY of the listed capabilities
}

pub struct PaginationParams {
    pub offset: usize,
    pub limit: usize,
}
```

## Storage Schema (SQLite)

```sql
-- Capability definitions (canonical registry)
CREATE TABLE capabilities (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    version TEXT NOT NULL,
    category TEXT NOT NULL,
    parameters_json TEXT NOT NULL, -- JSON array
    returns_json TEXT NOT NULL,    -- JSON object
    performance_json TEXT,          -- JSON object (optional)
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- Agent advertisements
CREATE TABLE agent_advertisements (
    agent_did TEXT PRIMARY KEY,
    created_at DATETIME NOT NULL,
    expires_at DATETIME NOT NULL,
    endpoint TEXT NOT NULL,
    protocol_version TEXT NOT NULL,
    reputation_score REAL,
    attestation_json TEXT,          -- JSON object (optional)
    advertisement_json TEXT NOT NULL -- Full advertisement as JSON
);

-- Capability claims (junction table)
CREATE TABLE capability_claims (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_did TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    confidence REAL NOT NULL,
    constraints_json TEXT,          -- JSON array
    pricing_json TEXT,              -- JSON object (optional)
    FOREIGN KEY (agent_did) REFERENCES agent_advertisements(agent_did),
    FOREIGN KEY (capability_id) REFERENCES capabilities(id),
    UNIQUE(agent_did, capability_id)
);

-- Indexes for performance
CREATE INDEX idx_caps_category ON capabilities(category);
CREATE INDEX idx_caps_agent ON capability_claims(agent_did);
CREATE INDEX idx_caps_capability ON capability_claims(capability_id);
CREATE INDEX idx_agent_expires ON agent_advertisements(expires_at);
CREATE INDEX idx_agent_reputation ON agent_advertisements(reputation_score);
```

## Standard Capability Definitions

These are the canonical capability IDs that agents can claim:

```rust
pub mod standard_capabilities {
    /// Communication capabilities
    pub const CHAT_RESPONSE: &str = "communication.chat_response";
    pub const EMAIL_DRAFT: &str = "communication.email_draft";
    pub const EMAIL_SEND: &str = "communication.email_send";
    pub const NOTIFICATION: &str = "communication.notification";
    
    /// Scheduling capabilities
    pub const CALENDAR_READ: &str = "scheduling.calendar_read";
    pub const CALENDAR_WRITE: &str = "scheduling.calendar_write";
    pub const SCHEDULE_MEETING: &str = "scheduling.schedule_meeting";
    pub const FIND_AVAILABILITY: &str = "scheduling.find_availability";
    
    /// Document processing
    pub const DOCUMENT_READ: &str = "document.read";
    pub const DOCUMENT_PARSE: &str = "document.parse";
    pub const OCR: &str = "document.ocr";
    pub const GENERATE_REPORT: &str = "document.generate_report";
    
    /// Social media
    pub const SOCIAL_DRAFT: &str = "social_media.draft_post";
    pub const SOCIAL_PUBLISH: &str = "social_media.publish";
    pub const SOCIAL_SCHEDULE: &str = "social_media.schedule";
    pub const SOCIAL_ANALYTICS: &str = "social_media.analytics";
    
    /// Data processing
    pub const DATA_ANALYSIS: &str = "data.analysis";
    pub const DATA_EXTRACTION: &str = "data.extraction";
    pub const DATA_TRANSFORMATION: &str = "data.transformation";
    
    /// Integration
    pub const HTTP_REQUEST: &str = "integration.http_request";
    pub const WEBHOOK_RECEIVE: &str = "integration.webhook_receive";
    pub const API_INTEGRATION: &str = "integration.api";
}
```

## Usage Examples

### Registering Capabilities

```rust
let registry = LocalCapabilityRegistry::new().await?;

let advertisement = AgentCapabilityAdvertisement {
    agent_did: "did:pekobot:local:agent123".to_string(),
    created_at: Utc::now(),
    expires_at: Utc::now() + Duration::days(30),
    capabilities: vec![
        CapabilityClaim {
            capability_id: standard_capabilities::CALENDAR_READ.to_string(),
            confidence: 0.95,
            constraints: vec![],
            pricing: None,
        },
        CapabilityClaim {
            capability_id: standard_capabilities::CALENDAR_WRITE.to_string(),
            confidence: 0.90,
            constraints: vec![CapabilityConstraint {
                constraint_type: "availability_hours".to_string(),
                description: "Available 9 AM - 6 PM UTC".to_string(),
                value: json!({"start": "09:00", "end": "18:00", "timezone": "UTC"}),
            }],
            pricing: None,
        },
    ],
    attestation: None, // Will be added by root CA or federated node
    reputation_score: None, // Will be added by reputation system
    endpoint: "https://agent123.example.com/a2a".to_string(),
    protocol_version: "0.1.0".to_string(),
};

registry.register(advertisement).await?;
```

### Finding Agents

```rust
// Find agents that can schedule meetings
let agents = registry.find_agents(
    standard_capabilities::SCHEDULE_MEETING,
    Some(AgentFilters {
        min_reputation: Some(0.8),
        max_price: None,
        available_now: true,
        protocol_version: Some("0.1.0".to_string()),
    }),
).await?;

// Complex query: agents with calendar AND email capabilities
let agents = registry.find_by_capabilities(
    CapabilityQuery {
        capabilities: vec![
            standard_capabilities::CALENDAR_READ.to_string(),
            standard_capabilities::EMAIL_DRAFT.to_string(),
        ],
        match_mode: MatchMode::All,
        filters: None,
    },
).await?;
```

## Integration with Other Layer 2 Components

### Reputation System
The registry queries the reputation system to get current scores for agents.

### Federated Trust
Multiple registries can federate and sync capability advertisements.

### Economic Layer
Capability claims can include pricing, enabling agent marketplaces.

## Implementation Phases

**Phase 1 (This Week):** Local SQLite registry with basic CRUD
**Phase 2:** Attestation support (root CA signatures)
**Phase 3:** Federation protocol (multi-registry sync)
**Phase 4:** Integration with reputation system

## Next Steps

1. Implement `LocalCapabilityRegistry` with SQLite storage
2. Create capability definition loader (TOML files)
3. Add attestation verification
4. Write tests for all registry operations
