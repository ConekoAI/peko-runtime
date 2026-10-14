//! The principal's side of the ADR-078 trust floor: who each actor is to
//! this principal, read from its owner, its grants, and the account
//! logged into this runtime's terminal.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use peko_auth::Subject;
use peko_tools_core::Trust;

use crate::common::hub_login::HubLogin;
use crate::principal::Principal;
use crate::security::{trust_for, Stamps, TrustSource};

/// [`TrustSource`] over a live principal's configuration. Grants are
/// read on every call, so `peko permit` / `peko revoke` apply to the
/// next run without a restart.
pub(crate) struct PrincipalTrust {
    principal: Weak<Principal>,
    /// `{config_dir}` — the parent of the principals root. Holds the
    /// terminal's Hub login (ADR-077).
    config_dir: Option<PathBuf>,
}

impl PrincipalTrust {
    pub(crate) fn new(principal: &Arc<Principal>) -> Self {
        // The workspace is `{config_dir}/principals/<name>`.
        let config_dir = principal
            .workspace_path
            .parent()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf);
        Self {
            principal: Arc::downgrade(principal),
            config_dir,
        }
    }
}

#[async_trait]
impl TrustSource for PrincipalTrust {
    async fn trust_of(&self, actor: &Subject) -> Trust {
        let Some(principal) = self.principal.upgrade() else {
            return Trust::Untrusted;
        };
        let login = self
            .config_dir
            .as_deref()
            .and_then(HubLogin::current_subject);
        let config = principal.config.read().await;
        trust_for(&config.owner, &config.permissions, login.as_ref(), actor)
    }

    async fn owner(&self) -> Subject {
        let owner = match self.principal.upgrade() {
            Some(principal) => principal.config.read().await.owner.clone(),
            None => Subject::User(String::new()),
        };
        // An ownerless legacy config belongs to the local operator.
        if owner.subject_id().is_empty() {
            Subject::User("local".into())
        } else {
            owner
        }
    }
}

/// The principal's stamp service.
pub(crate) fn stamps_for(
    principal: &Arc<Principal>,
    channels: Option<Arc<dyn peko_channel::ChannelPort>>,
) -> Stamps {
    Stamps::new(
        principal.id.clone(),
        Arc::new(PrincipalTrust::new(principal)),
        channels,
    )
}

/// The compartment of one of the principal's sessions (ADR-078 D3).
pub(crate) async fn session_compartment(
    principal: &Principal,
    session_id: &str,
) -> peko_tools_core::Compartment {
    let mut manager = peko_session::SessionManager::new()
        .with_sessions_dir_internal(principal.memory.sessions_dir());
    let metas = manager.list_all_sessions(false).await.unwrap_or_default();
    crate::security::compartment_of(&metas, peko_session::SessionId::from(session_id))
}

/// The stamp a cron job's run acts under in `session` (ADR-078 D5): the
/// job's creator, re-entered into the session it fires in. Refused for
/// jobs written before ADR-078 (other than the runtime's seeded jobs)
/// and for a non-owner job whose conversation is gone.
pub(crate) async fn cron_job_stamp(
    principal: &Arc<Principal>,
    channels: Option<Arc<dyn peko_channel::ChannelPort>>,
    job: &peko_cron::CronJob,
    session_id: &str,
) -> Result<peko_tools_core::ActingFor, String> {
    use peko_tools_core::{ActingFor, Origin};
    let creator = match &job.created_by {
        Some(creator) => creator.clone(),
        None if crate::principal::genesis::is_seeded_job(&job.id) => ActingFor::runtime(),
        None => {
            return Err(format!(
                "job '{}' predates acting-for stamps (ADR-078); recreate it so it records \
                 whom it runs for",
                job.name
            ))
        }
    };
    let compartment = session_compartment(principal, session_id).await;
    if !creator.is_owner() && compartment != creator.compartment {
        return Err(format!(
            "job '{}' runs for {} and its conversation is gone; it cannot fire elsewhere",
            job.name, creator.actor
        ));
    }
    let stamps = stamps_for(principal, channels);
    let driver = if creator.origin == Origin::Runtime {
        stamps.owner_driver().await
    } else {
        crate::security::Driver::from(&creator)
    };
    Ok(stamps
        .enter(
            &driver,
            compartment,
            Origin::Cron {
                job_id: job.id.clone(),
            },
        )
        .await)
}
