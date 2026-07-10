use serde::{Deserialize, Serialize};

/// Agent runtime state - simplified to Idle/Busy
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentState {
    /// Agent is idle and ready for work
    #[serde(rename = "idle")]
    Idle,
    /// Agent is busy processing a task
    #[serde(rename = "busy")]
    Busy,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "idle"),
            AgentState::Busy => write!(f, "busy"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::agent_config::AgentConfig;

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.name, "unnamed-agent");
        // Per-agent toggles default to on. The numeric/timeout fields
        // they replaced have moved to principal-level config; their
        // round-trip coverage lives on `PrincipalRoutingConfig`.
        assert!(config.enable_task_tools);
        assert!(config.enable_async_tools);
        // Issue #28: `agent_did` is `None` by default — back-filled on
        // first `Agent::new()` and persisted into config.toml.
        assert!(config.agent_did.is_none());
    }

    /// Issue #28: `wire_agent_id` must return the DID when present
    /// (cross-runtime wire) and the local name as a fallback
    /// (single-runtime back-compat). The empty-DID guard is
    /// inherited from `Subject::principal_wire_id` (review of #34
    /// concern #3) and is pinned here so the shim doesn't drift.
    #[test]
    fn test_wire_agent_id_prefers_did_over_name() {
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some("did:peko:local:abc123".to_string());
        assert_eq!(config.wire_agent_id(), "did:peko:local:abc123");
    }

    #[test]
    fn test_wire_agent_id_falls_back_to_name_when_did_missing() {
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = None;
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_wire_agent_id_treats_empty_did_as_missing() {
        // Pin the empty-DID defense: a hand-edited config that left
        // `agent_did = ""` must NOT surface an empty string as the
        // wire id (would serialize as `agentDid: ""` over the
        // tunnel, breaking PekoHub's lookup).
        let mut config = AgentConfig::default();
        config.name = "helper".to_string();
        config.agent_did = Some(String::new());
        assert_eq!(config.wire_agent_id(), "helper");
    }

    #[test]
    fn test_agent_did_toml_round_trip() {
        // An empty `agent_did` round-trips as `None` (legacy config).
        let legacy = AgentConfig {
            name: "legacy-agent".to_string(),
            ..Default::default()
        };
        let toml = toml::to_string_pretty(&legacy).expect("serialize legacy");
        let parsed: AgentConfig = toml::from_str(&toml).expect("parse legacy");
        assert!(parsed.agent_did.is_none());
        assert_eq!(parsed.name, "legacy-agent");

        // A populated `agent_did` round-trips verbatim.
        let mut modern = AgentConfig::default();
        modern.name = "modern-agent".to_string();
        modern.agent_did = Some("did:peko:local:deadbeef".to_string());
        let toml = toml::to_string_pretty(&modern).expect("serialize modern");
        let parsed: AgentConfig = toml::from_str(&toml).expect("parse modern");
        assert_eq!(parsed.agent_did.as_deref(), Some("did:peko:local:deadbeef"));
        assert_eq!(parsed.name, "modern-agent");
    }

    #[test]
    fn test_agent_state_display() {
        assert_eq!(AgentState::Idle.to_string(), "idle");
        assert_eq!(AgentState::Busy.to_string(), "busy");
    }
}
