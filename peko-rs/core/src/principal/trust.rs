//! The principal's side of the ADR-078 trust floor: who each actor is to
//! this principal, read from its owner and its grants (ADR-081 D4).

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use peko_auth::{ActorKeys, ActorRef};
use peko_tools_core::Trust;

use crate::principal::Principal;
use crate::security::{trust_for, Stamps, TrustSource};

/// [`TrustSource`] over a live principal's configuration. Grants are
/// read on every call, so `peko permit` / `peko revoke` apply to the
/// next run without a restart.
pub(crate) struct PrincipalTrust {
    principal: Weak<Principal>,
}

impl PrincipalTrust {
    pub(crate) fn new(principal: &Arc<Principal>) -> Self {
        Self {
            principal: Arc::downgrade(principal),
        }
    }
}

#[async_trait]
impl TrustSource for PrincipalTrust {
    async fn trust_of(&self, actor: &ActorRef, keys: &(dyn ActorKeys + Sync)) -> Trust {
        let Some(principal) = self.principal.upgrade() else {
            return Trust::Untrusted;
        };
        let config = principal.config.read().await;
        trust_for(&config.owner, &config.permissions, keys, actor)
    }

    async fn owner(&self) -> ActorRef {
        match self.principal.upgrade() {
            Some(principal) => principal.config.read().await.owner.clone(),
            None => ActorRef::User("local".into()),
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
        // ADR-081 D8: trust is re-derived at fire time and only ever
        // lowered, so revoking a grant or changing the owner reaches
        // jobs that already exist. A group counterpart is no actor; the
        // conversation's ceiling caps it below.
        let mut driver = crate::security::Driver::from(&creator);
        if let Ok(actor) = creator.actor.parse::<ActorRef>() {
            driver.trust = driver.trust.min(stamps.trust_of(&actor).await);
        }
        driver
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
