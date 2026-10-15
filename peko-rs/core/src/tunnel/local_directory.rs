//! Local-first agent directory wrapper.
//!
//! Intercepts `resolve_by_hub_id` for principals that live on the caller's own
//! runtime, so `principal_send` between two principals on the same runtime
//! works even when the hub is offline. The wrapper only knows about local
//! principals; everything else is forwarded to the inner (hub) directory.

use async_trait::async_trait;
use std::sync::Arc;

use crate::principal::PrincipalManager;

use super::hub_directory::{AgentDirectory, AgentResolution, DirectoryError, ResolvedExposure};

pub struct OfflineDirectory;
#[async_trait]
impl AgentDirectory for OfflineDirectory {
    async fn resolve_by_hub_id(&self, _did: &str) -> Result<AgentResolution, DirectoryError> {
        Err(DirectoryError::Transport(
            "remote directory is not configured".into(),
        ))
    }
    async fn resolve_by_handle(
        &self,
        _owner: &str,
        _name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        Err(DirectoryError::Transport(
            "remote directory is not configured".into(),
        ))
    }
}

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

    async fn local_by_hub_id(&self, did: &str) -> Option<AgentResolution> {
        let mut candidates = Vec::new();
        // Snapshot the loaded instances, then await their configs without
        // holding the manager lock. A busy config is not a directory miss.
        for principal in self.principal_manager.list_all().await {
            let hub_id = self.principal_manager.hub_id_of(&principal).await;
            if hub_id.as_deref() != Some(did) {
                continue;
            }
            let config = principal.config.read().await;
            let priority = match config.exposure {
                peko_auth::Exposure::Public => 0,
                peko_auth::Exposure::Private => 1,
                peko_auth::Exposure::LocalOnly => 2,
            };
            candidates.push((
                priority,
                principal.id.0.clone(),
                AgentResolution {
                    name: Some(config.name.clone()),
                    hub_id: hub_id.unwrap_or_default(),
                    local_id: principal.id.0.clone(),
                    identity_key: config.actor_key.clone().unwrap_or_default(),
                    runtime_id: self.runtime_id.clone(),
                    instance_id: principal.id.0.clone(),
                    agent_did: config.did.as_ref().map(|d| d.0.clone()).unwrap_or_default(),
                    owner_principal: config.owner.clone(),
                    exposure: map_instance_exposure(config.exposure),
                },
            ));
        }
        candidates.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        candidates
            .into_iter()
            .next()
            .map(|(_, _, resolution)| resolution)
    }
}

#[async_trait]
impl AgentDirectory for LocalFirstAgentDirectory {
    async fn resolve_by_hub_id_as(
        &self,
        did: &str,
        caller: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        if let Some(resolution) = self.local_by_hub_id(did).await {
            Ok(resolution)
        } else {
            self.inner.resolve_by_hub_id_as(did, caller).await
        }
    }
    async fn resolve_by_handle_as(
        &self,
        owner: &str,
        name: &str,
        caller: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        if owner.is_empty() {
            self.resolve_by_handle(owner, name).await
        } else {
            self.inner.resolve_by_handle_as(owner, name, caller).await
        }
    }
    async fn resolve_by_hub_id(&self, did: &str) -> Result<AgentResolution, DirectoryError> {
        if let Some(resolution) = self.local_by_hub_id(did).await {
            return Ok(resolution);
        }
        self.inner.resolve_by_hub_id(did).await
    }

    async fn resolve_by_handle(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        if owner.is_empty() {
            if let Some(principal) = self
                .principal_manager
                .get(peko_subject::PrincipalId(name.into()))
                .await
                .or(self.principal_manager.get_by_name(name).await)
            {
                let hub_id = self.principal_manager.hub_id_of(&principal).await;
                let config = principal.config.read().await;
                return Ok(AgentResolution {
                    name: Some(config.name.clone()),
                    hub_id: hub_id.unwrap_or_default(),
                    local_id: principal.id.0.clone(),
                    identity_key: config.actor_key.clone().unwrap_or_default(),
                    runtime_id: self.runtime_id.clone(),
                    instance_id: principal.id.0.clone(),
                    agent_did: config.did.as_ref().map(|d| d.0.clone()).unwrap_or_default(),
                    owner_principal: config.owner.clone(),
                    exposure: map_instance_exposure(config.exposure.clone()),
                });
            }
            return Err(DirectoryError::NotFound);
        }
        self.inner.resolve_by_handle(owner, name).await
    }
}

fn map_instance_exposure(exposure: peko_auth::Exposure) -> ResolvedExposure {
    match exposure {
        peko_auth::Exposure::Public => ResolvedExposure::Public,

        peko_auth::Exposure::Private => ResolvedExposure::Private,
        peko_auth::Exposure::LocalOnly => ResolvedExposure::LocalOnly,
    }
}
