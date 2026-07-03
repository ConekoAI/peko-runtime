//! Local-first agent directory wrapper.
//!
//! Intercepts `resolve_by_did` for principals that live on the caller's own
//! runtime, so `principal_send` between two principals on the same runtime
//! works even when the hub is offline. The wrapper only knows about local
//! principals; everything else is forwarded to the inner (hub) directory.

use async_trait::async_trait;
use std::sync::Arc;

use crate::principal::PrincipalManager;

use super::hub_directory::{AgentDirectory, AgentResolution, DirectoryError, ResolvedExposure};
use super::protocol::InstanceExposure;

/// Directory wrapper that resolves same-runtime principals locally before
/// falling back to the hub directory.
pub struct LocalFirstAgentDirectory {
    runtime_id: String,
    principal_manager: Arc<PrincipalManager>,
    inner: Arc<dyn AgentDirectory>,
}

impl LocalFirstAgentDirectory {
    /// Wrap the given hub directory with a local same-runtime lookup.
    #[must_use]
    pub fn new(
        runtime_id: impl Into<String>,
        principal_manager: Arc<PrincipalManager>,
        inner: Arc<dyn AgentDirectory>,
    ) -> Self {
        Self {
            runtime_id: runtime_id.into(),
            principal_manager,
            inner,
        }
    }
}

#[async_trait]
impl AgentDirectory for LocalFirstAgentDirectory {
    async fn resolve_by_did(&self, did: &str) -> Result<AgentResolution, DirectoryError> {
        if let Some(principal) = self.principal_manager.find_by_did(did).await {
            let config = principal.config.read().await;
            let exposure = config.exposure.clone();
            let preference = config.transport_preference;
            let owner = config.owner.clone();
            drop(config);
            return Ok(AgentResolution {
                runtime_id: self.runtime_id.clone(),
                instance_id: principal.id.0.clone(),
                agent_did: did.to_string(),
                owner_principal: owner,
                exposure: map_instance_exposure(exposure),
                transport_preference: preference,
                direct_endpoint: None,
            });
        }
        self.inner.resolve_by_did(did).await
    }

    async fn resolve_by_handle(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        self.inner.resolve_by_handle(owner, name).await
    }
}

fn map_instance_exposure(exposure: InstanceExposure) -> ResolvedExposure {
    match exposure {
        InstanceExposure::Public => ResolvedExposure::Public,
        InstanceExposure::Private => ResolvedExposure::Private,
        InstanceExposure::Unexposed => ResolvedExposure::Unexposed,
    }
}
