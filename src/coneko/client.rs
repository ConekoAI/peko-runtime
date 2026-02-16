//! Coneko HTTP client

use super::ConekoAdapter;

impl ConekoAdapter {
    pub async fn register_agent(&self,
        _did: &str,
        _capabilities: Vec<String>,
    ) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        // TODO: Implement registration
        Ok(())
    }

    pub async fn discover_agents(
        &self,
        _capability: Option<&str>,
    ) -> anyhow::Result<Vec<AgentInfo>> {
        if !self.enabled {
            return Ok(vec![]);
        }
        // TODO: Implement discovery
        Ok(vec![])
    }
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub did: String,
    pub endpoint: String,
    pub capabilities: Vec<String>,
}
