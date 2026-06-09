//! Agent Validation Service
//!
//! Centralized agent existence validation.
//! This service provides early validation to prevent creating
//! session infrastructure for non-existent agents.

use crate::common::services::config_authority::{
    AgentConfigEntry, ConfigAuthority, ConfigAuthorityImpl,
};
use anyhow::Result;
use std::sync::Arc;
use tracing::{debug, info};

/// Agent validation service
///
/// Provides centralized validation for agent operations.
/// All validation should happen early to prevent side effects.
pub struct AgentValidator {
    config_service: Arc<ConfigAuthorityImpl>,
}

impl AgentValidator {
    /// Create a new agent validator
    pub fn new(config_service: Arc<ConfigAuthorityImpl>) -> Self {
        Self { config_service }
    }

    /// Validate that an agent exists
    ///
    /// # Arguments
    /// * `agent_name` - Name of the agent to validate
    ///
    /// # Returns
    /// * `Ok(AgentConfigEntry)` - Agent exists, returns config entry
    /// * `Err` - Agent not found or other error
    ///
    /// # Example
    /// ```rust,ignore
    /// let validator = AgentValidator::new(config_service);
    /// match validator.validate_exists("myagent").await {
    ///     Ok(entry) => println!("Found agent: {}", entry.name),
    ///     Err(e) => println!("Agent not found: {}", e),
    /// }
    /// ```
    pub async fn validate_exists(&self, agent_name: &str) -> Result<AgentConfigEntry> {
        debug!("Validating agent '{}' exists", agent_name);

        if let Some(entry) = self.config_service.get(agent_name).await? {
            info!("Validated agent '{}'", entry.name);
            Ok(entry)
        } else {
            Err(anyhow::anyhow!("Agent '{agent_name}' not found"))
        }
    }

    /// Check if agent exists (non-failing)
    ///
    /// Returns true if agent exists, false otherwise.
    /// This method does not fail on other errors.
    pub async fn exists(&self, agent_name: &str) -> bool {
        self.config_service
            .exists(agent_name)
            .await
            .unwrap_or(false)
    }

    /// Validate multiple agents exist
    ///
    /// Returns Ok only if all agents exist.
    /// Returns Err on first missing agent.
    pub async fn validate_all_exist(&self, agents: &[&str]) -> Result<Vec<AgentConfigEntry>> {
        let mut results = Vec::with_capacity(agents.len());

        for agent_name in agents {
            results.push(self.validate_exists(agent_name).await?);
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::types::agent::AgentConfig;
    use tempfile::TempDir;

    async fn create_test_validator() -> (AgentValidator, TempDir) {
        let temp = TempDir::new().unwrap();
        let path_resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver));
        let validator = AgentValidator::new(config_service.clone());

        // Create a test agent
        let config = AgentConfig::default();
        config_service.save("test-agent", &config).await.unwrap();

        (validator, temp)
    }

    #[tokio::test]
    async fn test_validate_exists_found() {
        let (validator, _temp) = create_test_validator().await;

        let result = validator.validate_exists("test-agent").await;
        assert!(result.is_ok());

        let entry = result.unwrap();
        assert_eq!(entry.name, "test-agent");
    }

    #[tokio::test]
    async fn test_validate_exists_not_found() {
        let (validator, _temp) = create_test_validator().await;

        let result = validator.validate_exists("nonexistent").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_exists_true() {
        let (validator, _temp) = create_test_validator().await;

        assert!(validator.exists("test-agent").await);
    }

    #[tokio::test]
    async fn test_exists_false() {
        let (validator, _temp) = create_test_validator().await;

        assert!(!validator.exists("nonexistent").await);
    }

    #[tokio::test]
    async fn test_validate_all_exist() {
        let (validator, _temp) = create_test_validator().await;

        // This should succeed with one agent
        let result = validator.validate_all_exist(&["test-agent"]).await;
        assert!(result.is_ok());

        // This should fail with missing agent
        let result = validator
            .validate_all_exist(&["test-agent", "nonexistent"])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_no_directory_created_on_validation_failure() {
        use crate::common::paths::PathResolver;
        use std::path::PathBuf;

        let temp = TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        let path_resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            data_dir.clone(),
            temp.path().join("cache"),
        );

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver));
        let validator = AgentValidator::new(config_service);

        // Validate non-existent agent (should fail)
        let result = validator.validate_exists("nonexistentagent123").await;
        assert!(result.is_err());

        // Verify NO session directory was created
        let expected_session_dir: PathBuf = data_dir
            .join("sessions")
            .join("default")
            .join("nonexistentagent123");
        assert!(
            !expected_session_dir.exists(),
            "Session directory should NOT be created for non-existent agent: {}",
            expected_session_dir.display()
        );
    }
}
