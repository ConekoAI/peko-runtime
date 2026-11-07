//! Who may open a conversation with a private peko without a `Chat`
//! grant (ADR-098 D3).
//!
//! A peko's first DM to a private peko is admitted when the two already
//! share something here: the receiver's owner learned the sender as a
//! contact, the pekos are in a group together, they have the same owner,
//! or their owners share a group or a DM. A group invitation needs more:
//! a contact (or the peko's own join request, checked by the caller), so
//! being in one group with someone does not let them pull the peko into
//! others. The conversation still runs at its ceiling (ADR-096): a peko
//! admitted this way is untrusted there.

use peko_channel::{ChannelPort, ChannelStore, ConversationKind};
use peko_subject::ActorRef;

/// What the receiving peko knows of the sender.
pub(crate) struct FirstContact<'a> {
    /// The receiving peko.
    pub me: &'a ActorRef,
    /// The receiving peko's owner.
    pub my_owner: &'a ActorRef,
    /// The receiving peko's learned contacts, as subjects here.
    pub contacts: &'a [ActorRef],
    /// The sending peko.
    pub sender: &'a ActorRef,
    /// The sender's owner's PekoHub account, from the directory.
    pub sender_owner_account: Option<&'a str>,
}

/// Why `contact` may open a conversation of `kind`, or `None`.
pub(crate) async fn admits(
    store: &ChannelStore,
    contact: &FirstContact<'_>,
    kind: ConversationKind,
) -> Option<&'static str> {
    let sender = store.key_of(contact.sender);
    if contact.contacts.iter().any(|c| store.key_of(c) == sender) {
        return Some("a contact its owner added");
    }
    if kind != ConversationKind::Dm {
        return None;
    }
    let groups = |actors: Vec<ActorRef>| async move {
        let mut found = false;
        for channel in store.shared_by(&actors).await.unwrap_or_default() {
            if matches!(
                peko_channel::ChannelPort::conversation_kind(store, &channel).await,
                Ok(ConversationKind::Group)
            ) {
                found = true;
            }
        }
        found
    };
    if groups(vec![contact.me.clone(), contact.sender.clone()]).await {
        return Some("a peko it shares a group with");
    }
    let account = contact.sender_owner_account?;
    let owner_account = match contact.my_owner.route() {
        Some(route) if route.is_hub() && matches!(contact.my_owner, ActorRef::User(_)) => {
            Some(route.id().to_string())
        }
        _ => {
            peko_channel::ChannelPort::actor_info(store, contact.my_owner)
                .await
                .hub_id
        }
    };
    if owner_account.as_deref() == Some(account) {
        return Some("a peko of its own owner");
    }
    let sender_owner = ActorRef::User(peko_subject::Route::hub(account));
    if !store
        .shared_by(&[contact.my_owner.clone(), sender_owner])
        .await
        .unwrap_or_default()
        .is_empty()
    {
        return Some("a peko whose owner shares a conversation with its owner");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, CreateOpts, Registration};
    use peko_subject::{PrincipalId, Route};

    async fn store() -> (tempfile::TempDir, ChannelStore) {
        let temp = tempfile::tempdir().unwrap();
        let store = ChannelStore::new(ChannelConfig {
            runtime_dir: temp.path().into(),
            shared_dir: None,
        });
        (temp, store)
    }

    async fn register(store: &ChannelStore, actor: &ActorRef, hub: &str) {
        store
            .register_actor(
                actor,
                Some(hub),
                None,
                Some(&format!("key-{hub}")),
                Some(hub),
                Registration::Private,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_first_dm_comes_from_pekos_it_shares_something_with() {
        let (_temp, store) = store().await;
        let ada = PrincipalId("prin_ada".into());
        let ada_ref = ActorRef::from(&ada);
        register(&store, &ada_ref, "ada").await;
        let frank = ActorRef::User(Route::hub("frank"));
        let gina = ActorRef::User(Route::hub("gina"));
        let bix = ActorRef::Principal(Route::hub("bix"));
        let stranger = ActorRef::Principal(Route::hub("stranger"));
        for (actor, hub) in [
            (&frank, "frank"),
            (&gina, "gina"),
            (&bix, "bix"),
            (&stranger, "stranger"),
        ] {
            register(&store, actor, hub).await;
        }
        let contact = |sender: &'static ActorRef, account: Option<&'static str>| FirstContact {
            me: Box::leak(Box::new(ada_ref.clone())),
            my_owner: Box::leak(Box::new(frank.clone())),
            contacts: &[],
            sender,
            sender_owner_account: account,
        };
        let bix: &'static ActorRef = Box::leak(Box::new(bix));
        let stranger: &'static ActorRef = Box::leak(Box::new(stranger));

        // Nothing shared: refused, as before.
        assert_eq!(
            admits(&store, &contact(bix, Some("gina")), ConversationKind::Dm).await,
            None
        );

        // The same owner.
        assert!(
            admits(&store, &contact(bix, Some("frank")), ConversationKind::Dm)
                .await
                .is_some()
        );

        // The owners share a group (ada is not in it).
        let people = store
            .create(&ada, CreateOpts::runtime("people"))
            .await
            .unwrap();
        store.invite(&people, &ada, &frank).await.unwrap();
        store.invite(&people, &ada, &gina).await.unwrap();
        store.leave(&people, &ada).await.unwrap();
        assert_eq!(
            admits(&store, &contact(bix, Some("gina")), ConversationKind::Dm).await,
            Some("a peko whose owner shares a conversation with its owner")
        );
        // ...but that does not pull ada into groups.
        assert_eq!(
            admits(&store, &contact(bix, Some("gina")), ConversationKind::Group).await,
            None
        );
        // A stranger whose owner is no one ada's owner knows: refused.
        assert_eq!(
            admits(
                &store,
                &contact(stranger, Some("zed")),
                ConversationKind::Dm
            )
            .await,
            None
        );

        // The pekos share a group.
        let work = store
            .create(&ada, CreateOpts::runtime("work"))
            .await
            .unwrap();
        store.invite(&work, &ada, stranger).await.unwrap();
        assert_eq!(
            admits(&store, &contact(stranger, None), ConversationKind::Dm).await,
            Some("a peko it shares a group with")
        );

        // A contact its owner learned may also invite it to a group.
        let learned = FirstContact {
            contacts: std::slice::from_ref(stranger),
            ..contact(stranger, None)
        };
        assert_eq!(
            admits(&store, &learned, ConversationKind::Group).await,
            Some("a contact its owner added")
        );
    }
}
