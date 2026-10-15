//! Deriving acting-for stamps (ADR-078 D1–D3).
//!
//! The stamp *type* lives in `peko-tools-core`; this module decides its
//! values. Runs start in three places (ingress, cron, completion wakes),
//! and every derived run inherits its caller's stamp, re-capped against
//! the session it enters. Nothing here imports `principal/`: the
//! principal layer supplies each actor's trust through [`TrustSource`].

use std::sync::Arc;

use async_trait::async_trait;
use peko_auth::{ActorRef, Permission, PermissionGrant};
use peko_channel::{ChannelId, ChannelPort};
use peko_session::{SessionId, SessionMetadata};
use peko_subject::{ActorKeys, CanonicalKeys, PrincipalId};
use peko_tools_core::{ActingFor, Compartment, Origin, Trust};

/// Who an actor is to one principal (ADR-078 §2).
#[async_trait]
pub trait TrustSource: Send + Sync {
    /// The trust the principal's owner consented to for `actor`, with
    /// identity decided by `keys` (ADR-081 D1).
    async fn trust_of(&self, actor: &ActorRef, keys: &(dyn ActorKeys + Sync)) -> Trust;
    /// The principal's owner.
    async fn owner(&self) -> ActorRef;
}

/// Trust for `actor` under an owner and a grant list (ADR-078 §2,
/// ADR-081 D4): `Owner` exactly when `actor` is the owner, `Trusted` when
/// a grant names it with the owner's consent bit, else `Untrusted`.
#[must_use]
pub fn trust_for(
    owner: &ActorRef,
    grants: &[PermissionGrant],
    keys: &dyn ActorKeys,
    actor: &ActorRef,
) -> Trust {
    if keys.same_actor(owner, actor) {
        return Trust::Owner;
    }
    // The consent bit must name the actor: a public grant never trusts.
    let trusted = grants.iter().any(|grant| {
        grant.permission == Permission::Trusted && keys.same_actor(&grant.subject, actor)
    });
    if trusted {
        Trust::Trusted
    } else {
        Trust::Untrusted
    }
}

/// The compartment of `session` within `metas`: its stamp, else (for
/// sessions created before ADR-078) the nearest channel-bound session on
/// its ancestor chain, else the principal's internal tree.
#[must_use]
pub fn compartment_of(metas: &[SessionMetadata], session: SessionId) -> Compartment {
    let mut current = Some(session);
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = current.filter(|id| seen.insert(*id)) {
        let Some(meta) = metas.iter().find(|m| m.session_id == id) else {
            break;
        };
        if let Some(compartment) = &meta.compartment {
            return compartment.clone();
        }
        if let Some(channel) = &meta.channel_id {
            return Compartment::Conversation {
                channel: channel.clone(),
            };
        }
        current = meta.parent_session_id;
    }
    Compartment::Internal
}

/// The sessions a run may see: those it may reach, plus their ancestors
/// as path anchors so slug paths resolve the same for every caller.
/// Anchors are never reachable themselves; callers check
/// [`may_reach`] on every resolved target.
#[must_use]
pub fn visible_metas(stamp: &ActingFor, metas: &[SessionMetadata]) -> Vec<SessionMetadata> {
    if stamp.is_owner() {
        return metas.to_vec();
    }
    let mut keep = std::collections::HashSet::new();
    for meta in metas {
        if !stamp.may_reach(&compartment_of(metas, meta.session_id)) {
            continue;
        }
        let mut current = Some(meta.session_id);
        while let Some(id) = current.filter(|id| keep.insert(*id)) {
            current = metas
                .iter()
                .find(|m| m.session_id == id)
                .and_then(|m| m.parent_session_id);
        }
    }
    metas
        .iter()
        .filter(|m| keep.contains(&m.session_id))
        .cloned()
        .collect()
}

/// Whether `stamp` may reach `session`.
#[must_use]
pub fn may_reach(stamp: &ActingFor, metas: &[SessionMetadata], session: SessionId) -> bool {
    stamp.may_reach(&compartment_of(metas, session))
}

/// The refusal a run gets for a session outside its reach. It reads as
/// "not found" so it does not confirm the session exists.
#[must_use]
pub fn err_out_of_reach(reference: &str) -> anyhow::Error {
    anyhow::anyhow!("Session not found: {reference}")
}

/// The party driving a run into a session.
#[derive(Debug, Clone)]
pub struct Driver {
    pub actor: String,
    pub name: Option<String>,
    pub trust: Trust,
}

impl From<&ActingFor> for Driver {
    fn from(stamp: &ActingFor) -> Self {
        Self {
            actor: stamp.actor.clone(),
            name: stamp.name.clone(),
            trust: stamp.trust,
        }
    }
}

/// Mints and re-caps stamps for one principal.
#[derive(Clone)]
pub struct Stamps {
    principal: PrincipalId,
    trust: Arc<dyn TrustSource>,
    channels: Option<Arc<dyn ChannelPort>>,
}

impl Stamps {
    #[must_use]
    pub fn new(
        principal: PrincipalId,
        trust: Arc<dyn TrustSource>,
        channels: Option<Arc<dyn ChannelPort>>,
    ) -> Self {
        Self {
            principal,
            trust,
            channels,
        }
    }

    /// `actor`'s trust, with identity decided by this runtime's registry.
    pub async fn trust_of(&self, actor: &ActorRef) -> Trust {
        match &self.channels {
            Some(port) => {
                self.trust
                    .trust_of(actor, &peko_channel::PortKeys(port.as_ref()))
                    .await
            }
            None => self.trust.trust_of(actor, &CanonicalKeys).await,
        }
    }

    /// The actor address the stamp uses for `subject` (ADR-073/077).
    pub async fn address(&self, subject: &ActorRef) -> (String, Option<String>) {
        match &self.channels {
            Some(port) => {
                let info = port.actor_info(subject).await;
                (info.address, info.name)
            }
            None => (
                peko_channel::ActorInfo::from_subject(subject, None).address,
                None,
            ),
        }
    }

    /// The owner as a driver.
    pub async fn owner_driver(&self) -> Driver {
        let owner = self.trust.owner().await;
        let (actor, name) = self.address(&owner).await;
        Driver {
            actor,
            name,
            trust: Trust::Owner,
        }
    }

    /// A human (or principal) driver: `author` is who speaks, `authority`
    /// is the subject permissions are checked against (ADR-057 D2).
    pub async fn driver(&self, author: &ActorRef, authority: &ActorRef) -> Driver {
        let (actor, name) = self.address(author).await;
        Driver {
            actor,
            name,
            trust: self.trust_of(authority).await,
        }
    }

    /// The highest trust any run in `compartment` may have: the trust of
    /// the least-trusted participant whose words reach its context.
    pub async fn ceiling(&self, compartment: &Compartment) -> Trust {
        let channel = match compartment {
            Compartment::Internal => return Trust::Owner,
            Compartment::Detached => return Trust::Untrusted,
            Compartment::Conversation { channel } => channel,
        };
        let Some(port) = &self.channels else {
            return Trust::Untrusted;
        };
        let Ok(members) = port.list_members(&ChannelId(channel.clone())).await else {
            return Trust::Untrusted;
        };
        let me = port
            .actor_info(&ActorRef::from(&self.principal))
            .await
            .identity_key;
        let mut ceiling = None::<Trust>;
        for member in members {
            if port.actor_info(&member).await.identity_key == me {
                continue;
            }
            let trust = self.trust_of(&member).await;
            ceiling = Some(ceiling.map_or(trust, |c| c.min(trust)));
        }
        ceiling.unwrap_or(Trust::Untrusted)
    }

    /// Whom a conversation speaks for when no member drives the run: the
    /// one other participant of a DM, or the channel itself for a group.
    async fn counterpart(&self, channel: &str) -> (String, Option<String>) {
        if let Some(port) = &self.channels {
            let me = port
                .actor_info(&ActorRef::from(&self.principal))
                .await
                .identity_key;
            if let Ok(participants) = port.participants(&ChannelId(channel.to_string())).await {
                let others: Vec<_> = participants
                    .into_iter()
                    .filter(|p| p.identity_key != me)
                    .collect();
                if let [only] = others.as_slice() {
                    return (only.address.clone(), only.name.clone());
                }
            }
        }
        (channel.to_string(), None)
    }

    async fn is_member(&self, channel: &str, actor: &str) -> bool {
        let Some(port) = &self.channels else {
            return false;
        };
        port.participants(&ChannelId(channel.to_string()))
            .await
            .is_ok_and(|ps| ps.iter().any(|p| p.address == actor))
    }

    /// The stamp for a run that `driver` starts (or continues) in a
    /// session of `compartment`. Trust is capped at the compartment's
    /// ceiling. In a conversation the run acts for the driver when the
    /// driver takes part in it, and for the conversation otherwise.
    pub async fn enter(
        &self,
        driver: &Driver,
        compartment: Compartment,
        origin: Origin,
    ) -> ActingFor {
        let ceiling = self.ceiling(&compartment).await;
        let (actor, name) = match &compartment {
            Compartment::Conversation { channel }
                if !self.is_member(channel, &driver.actor).await =>
            {
                self.counterpart(channel).await
            }
            _ => (driver.actor.clone(), driver.name.clone()),
        };
        ActingFor {
            actor,
            name,
            trust: driver.trust.min(ceiling),
            compartment,
            origin,
        }
    }

    /// The stamp a run inherits when `caller` drives a session in
    /// `target`.
    pub async fn derive(&self, caller: &ActingFor, target: Compartment) -> ActingFor {
        if caller.compartment == target {
            let ceiling = self.ceiling(&target).await;
            return caller.clone().capped(ceiling);
        }
        self.enter(&Driver::from(caller), target, caller.origin.clone())
            .await
    }

    /// The stamp of a session continuing its own work (a completion
    /// wake): the owner for the internal tree, the conversation's own
    /// party at its ceiling otherwise.
    pub async fn natural(&self, compartment: Compartment, origin: Origin) -> ActingFor {
        match &compartment {
            Compartment::Conversation { channel } => {
                let (actor, name) = self.counterpart(channel).await;
                ActingFor {
                    actor,
                    name,
                    trust: self.ceiling(&compartment).await,
                    compartment,
                    origin,
                }
            }
            _ => {
                let owner = self.owner_driver().await;
                self.enter(&owner, compartment, origin).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(subject: ActorRef, permission: Permission) -> PermissionGrant {
        PermissionGrant {
            subject,
            permission,
            granted_at: String::new(),
            granted_by: ActorRef::User("local".into()),
        }
    }

    #[test]
    fn only_the_owner_is_owner() {
        let owner = ActorRef::User("hub:owner".into());
        let keys = CanonicalKeys;
        assert_eq!(trust_for(&owner, &[], &keys, &owner), Trust::Owner);
        // ADR-081 D4: neither the local user nor any other account is an
        // implicit owner.
        assert_eq!(
            trust_for(&owner, &[], &keys, &ActorRef::User("local".into())),
            Trust::Untrusted
        );
        assert_eq!(
            trust_for(&owner, &[], &keys, &ActorRef::User("hub:me".into())),
            Trust::Untrusted
        );
        let local_owner = ActorRef::User("local".into());
        assert_eq!(
            trust_for(
                &local_owner,
                &[],
                &keys,
                &ActorRef::User("local:local".into())
            ),
            Trust::Owner
        );
    }

    #[test]
    fn trusted_requires_a_named_grant() {
        let owner = ActorRef::User("hub:owner".into());
        let alice = ActorRef::User("hub:alice".into());
        let grants = vec![
            grant(alice.clone(), Permission::Chat),
            grant(ActorRef::Public, Permission::Trusted),
        ];
        assert_eq!(
            trust_for(&owner, &grants, &CanonicalKeys, &alice),
            Trust::Untrusted
        );
        let grants = vec![grant(alice.clone(), Permission::Trusted)];
        assert_eq!(
            trust_for(&owner, &grants, &CanonicalKeys, &alice),
            Trust::Trusted
        );
        let bob = ActorRef::Principal("prin_bob".into());
        assert_eq!(
            trust_for(&owner, &grants, &CanonicalKeys, &bob),
            Trust::Untrusted
        );
    }

    fn meta(id: u128, parent: Option<u128>, channel: Option<&str>) -> SessionMetadata {
        let mut entry = SessionMetadata::new(SessionId(uuid::Uuid::from_u128(id)), "root", "x");
        entry.parent_session_id = parent.map(|p| SessionId(uuid::Uuid::from_u128(p)));
        entry.channel_id = channel.map(str::to_string);
        entry
    }

    fn sid(id: u128) -> SessionId {
        SessionId(uuid::Uuid::from_u128(id))
    }

    #[test]
    fn compartment_prefers_the_stamp_then_the_channel_chain() {
        let mut metas = vec![
            meta(1, None, None),
            meta(2, Some(1), Some("dm_x")),
            meta(3, Some(2), None),
            meta(4, Some(1), None),
        ];
        assert_eq!(compartment_of(&metas, sid(1)), Compartment::Internal);
        let dm = Compartment::Conversation {
            channel: "dm_x".into(),
        };
        assert_eq!(compartment_of(&metas, sid(3)), dm);
        assert_eq!(compartment_of(&metas, sid(4)), Compartment::Internal);
        // A stamped session keeps its compartment wherever it is moved.
        metas[3].compartment = Some(dm.clone());
        assert_eq!(compartment_of(&metas, sid(4)), dm);
    }

    #[test]
    fn visible_metas_keep_reachable_sessions_and_their_anchors() {
        let metas = vec![
            meta(1, None, None),
            meta(2, Some(1), Some("dm_x")),
            meta(3, Some(2), None),
            meta(4, Some(1), Some("dm_owner")),
        ];
        let guest = ActingFor {
            actor: "user:hub:x".into(),
            name: None,
            trust: Trust::Untrusted,
            compartment: Compartment::Conversation {
                channel: "dm_x".into(),
            },
            origin: Origin::Ingress { channel: None },
        };
        let ids: Vec<_> = visible_metas(&guest, &metas)
            .iter()
            .map(|m| m.session_id)
            .collect();
        assert_eq!(ids, vec![sid(1), sid(2), sid(3)]);
        assert!(!may_reach(&guest, &metas, sid(1)));
        assert!(may_reach(&guest, &metas, sid(3)));
        assert!(!may_reach(&guest, &metas, sid(4)));
    }
}
