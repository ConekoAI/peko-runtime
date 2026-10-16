use peko_channel::Registration;
use peko_channel::{
    dm_channel_id, ChannelConfig, ChannelEvent, ChannelId, ChannelPort, ChannelStore, Checkpoint,
    CreateOpts, PostMsg,
};
use peko_subject::{ActorRef, PrincipalId};

fn store(dir: &std::path::Path) -> ChannelStore {
    ChannelStore::new(ChannelConfig {
        runtime_dir: dir.into(),
        shared_dir: None,
    })
}

#[tokio::test]
async fn registration_preserves_local_identity_conversation_and_authorship() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let principal = PrincipalId("prin_a".into());
    let user = ActorRef::User("hub:account".into());
    port.register_actor(
        &ActorRef::from(&principal),
        None,
        Some("did:key:signer"),
        Some("actor-key"),
        Some("Alice"),
        Registration::LocalOnly,
    )
    .await
    .unwrap();
    port.register_actor(
        &user,
        Some("account"),
        None,
        None,
        Some("Human"),
        Registration::Registered,
    )
    .await
    .unwrap();
    let before = port.ensure_dm(&principal, &user, "/human").await.unwrap();
    port.post(&before, &user, PostMsg::root("hello"))
        .await
        .unwrap();
    port.register_actor(
        &ActorRef::from(&principal),
        Some("hub-a"),
        Some("did:key:signer"),
        Some("actor-key"),
        Some("Alice"),
        Registration::Private,
    )
    .await
    .unwrap();
    let local = port.resolve_actor("principal:local:prin_a").await.unwrap();
    let hub = port.resolve_actor("principal:hub:hub-a").await.unwrap();
    assert_eq!(local, hub);
    assert_eq!(local.hub_address().as_deref(), Some("principal:hub:hub-a"));
    assert_eq!(local.registration, "private");
    assert_eq!(
        port.ensure_dm(&principal, &"user:hub:account".parse().unwrap(), "/human")
            .await
            .unwrap(),
        before
    );
    let events = port.peek(&before, &Checkpoint::default()).await.unwrap();
    assert!(events.iter().any(
        |ev| matches!(ev, ChannelEvent::Posted { author,.. } if author == "user:hub:account")
    ));
    assert_eq!(port.list_members(&before).await.unwrap().len(), 2);
    // Both IDs and the immutable pair survive another store handle / restart.
    assert_eq!(
        store(temp.path())
            .resolve_actor("principal:hub:hub-a")
            .await
            .unwrap(),
        local
    );
    port.register_actor(
        &ActorRef::from(&principal),
        None,
        None,
        Some("actor-key"),
        Some("Alice"),
        Registration::LocalOnly,
    )
    .await
    .unwrap();
    assert!(port.resolve_actor("principal:hub:hub-a").await.is_err());
    assert_eq!(
        port.ensure_dm(&principal, &user, "/human").await.unwrap(),
        before
    );
}

#[tokio::test]
async fn immutable_actor_keys_and_hub_aliases_cannot_be_reassigned() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let a = ActorRef::Principal("prin_a".into());
    port.register_actor(
        &a,
        Some("hub-a"),
        Some("did:key:a"),
        Some("key-a"),
        None,
        Registration::Public,
    )
    .await
    .unwrap();
    assert!(port
        .register_actor(
            &a,
            Some("hub-a"),
            Some("did:key:a"),
            Some("different-key"),
            None,
            Registration::Public
        )
        .await
        .is_err());
    assert!(port
        .register_actor(
            &ActorRef::Principal("prin_b".into()),
            Some("hub-a"),
            Some("did:key:b"),
            Some("key-b"),
            None,
            Registration::Public
        )
        .await
        .is_err());
    assert!(port.resolve_actor("principal:did:key:a").await.is_err());
    assert!(port.resolve_actor("principal:prin_a").await.is_err());
    // Kind is part of identity: an account and a visitor with identical IDs remain distinct.
    for kind in ["user", "visitor"] {
        port.register_actor(
            &format!("{kind}:local_same").parse().unwrap(),
            Some("same"),
            None,
            None,
            None,
            Registration::Registered,
        )
        .await
        .unwrap();
    }
    assert_ne!(
        port.resolve_actor("user:hub:same")
            .await
            .unwrap()
            .identity_key,
        port.resolve_actor("visitor:hub:same")
            .await
            .unwrap()
            .identity_key
    );
}

#[tokio::test]
async fn concurrent_pair_provisioning_keeps_independent_channel_bindings() {
    let temp = tempfile::tempdir().unwrap();
    let a = store(temp.path());
    let b = store(temp.path());
    let aid = PrincipalId("prin_a".into());
    let bid = PrincipalId("prin_b".into());
    a.register_actor(
        &ActorRef::from(&aid),
        None,
        None,
        Some("key-a"),
        Some("Alice"),
        Registration::LocalOnly,
    )
    .await
    .unwrap();
    b.register_actor(
        &ActorRef::from(&bid),
        None,
        None,
        Some("key-b"),
        Some("Bob"),
        Registration::LocalOnly,
    )
    .await
    .unwrap();
    let actor_a = ActorRef::from(&aid);
    let actor_b = ActorRef::from(&bid);
    let (ab, ba) = tokio::join!(
        a.ensure_dm(&aid, &actor_b, "/bob"),
        b.ensure_dm(&bid, &actor_a, "/alice")
    );
    let channel = ab.unwrap();
    assert_eq!(channel, ba.unwrap());
    assert_eq!(
        a.binding_for(&channel, &aid)
            .await
            .unwrap()
            .map(|b| b.session)
            .as_deref(),
        Some("/bob")
    );
    assert_eq!(
        b.binding_for(&channel, &bid)
            .await
            .unwrap()
            .map(|b| b.session)
            .as_deref(),
        Some("/alice")
    );
    assert_eq!(
        a.binding_for(&channel, &PrincipalId("prin_c".into()))
            .await
            .unwrap(),
        None
    );
    let actors = a.participants(&channel).await.unwrap();
    assert_eq!(
        actors
            .iter()
            .map(|a| a.address.as_str())
            .collect::<Vec<_>>(),
        vec!["principal:local:prin_a", "principal:local:prin_b"]
    );
    assert!(peko_channel::short_name_unambiguous(
        &actors,
        "Alice",
        &actors[0].address
    ));
}

#[tokio::test]
async fn group_roster_tracks_membership_instead_of_invitation_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let principal = PrincipalId("prin_a".into());
    let channel = port
        .create(&principal, CreateOpts::runtime("team"))
        .await
        .unwrap();
    let visitor = ActorRef::Principal("prin_guest".into());
    port.invite(&channel, &principal, &visitor).await.unwrap();
    assert_eq!(port.participants(&channel).await.unwrap().len(), 2);
    port.leave(&channel, &PrincipalId("prin_guest".into()))
        .await
        .unwrap();
    assert_eq!(port.participants(&channel).await.unwrap().len(), 1);
}

#[test]
fn pair_ids_are_opaque_order_independent_and_collision_resistant() {
    let ab = dm_channel_id("a", "b");
    assert_eq!(ab, dm_channel_id("b", "a"));
    assert_ne!(ab, dm_channel_id("c", "b"));
    assert_ne!(dm_channel_id("ab", "c"), dm_channel_id("a", "bc"));
    assert_eq!(ChannelId::parse(ab.as_str()), Some(ab));
    assert!(ChannelId::parse("dm_../../invalid").is_none());
}

#[tokio::test]
async fn verified_remote_leave_removes_all_aliases_from_current_roster() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let owner = PrincipalId::generate();
    let remote = ActorRef::Principal("hub:remote-hub".into());
    port.register_actor(
        &remote,
        Some("remote-hub"),
        Some("did:key:remote"),
        Some("remote-key"),
        None,
        Registration::Public,
    )
    .await
    .unwrap();
    let channel = port
        .create(&owner, CreateOpts::runtime("group"))
        .await
        .unwrap();
    port.invite(&channel, &owner, &remote).await.unwrap();
    port.add_remote_member(&channel, "runtime-other", "did:key:remote")
        .await
        .unwrap();
    assert_eq!(port.participants(&channel).await.unwrap().len(), 2);
    port.append_remote_event(
        &channel,
        &ChannelEvent::MemberLeft {
            channel: channel.clone(),
            member: "principal:hub:remote-hub".into(),
            at: "2026-10-13T00:00:00Z".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(port.participants(&channel).await.unwrap().len(), 1);
    assert!(port.list_remote_members(&channel).await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_actors_are_hub_only_and_keep_their_namespace_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let owner = PrincipalId::generate();
    let local = port
        .register_actor(
            &ActorRef::from(&owner),
            None,
            None,
            Some("owner-key"),
            None,
            Registration::LocalOnly,
        )
        .await
        .unwrap();
    assert_eq!(local.local_id.as_deref(), Some(owner.0.as_str()));
    assert_eq!(local.hub_id, None);
    let channel = port
        .create(&owner, CreateOpts::runtime("remote contacts"))
        .await
        .unwrap();
    for kind in ["principal", "user", "visitor"] {
        let reference = format!("{kind}:hub:remote");
        let subject: ActorRef = reference.parse().unwrap();
        let info = port
            .register_actor(
                &subject,
                Some("remote"),
                None,
                None,
                Some(kind),
                Registration::Registered,
            )
            .await
            .unwrap();
        assert_eq!(info.local_id, None);
        assert_eq!(info.hub_id.as_deref(), Some("remote"));
        assert_eq!(info.address, reference);
        assert_eq!(info.subject(), subject);
        if kind == "principal" {
            assert_eq!(info.mention.as_deref(), Some("@principal:hub:remote"));
        }
        port.invite(&channel, &owner, &subject).await.unwrap();
        port.post(&channel, &subject, PostMsg::root(kind))
            .await
            .unwrap();
        assert_eq!(
            store(temp.path()).resolve_actor(&reference).await.unwrap(),
            info
        );
        assert!(port
            .resolve_actor(&format!("{kind}:local:local_remote"))
            .await
            .is_err());
        assert!(port
            .resolve_actor(&format!("{kind}:local:remote"))
            .await
            .is_err());
        assert!(serde_json::to_value(&info).unwrap()["local_id"].is_null());
    }
    let restarted = store(temp.path());
    let actors = restarted.participants(&channel).await.unwrap();
    assert_eq!(actors.len(), 4);
    assert_eq!(actors.iter().filter(|a| a.local_id.is_none()).count(), 3);
    let events = restarted
        .peek(&channel, &Checkpoint::default())
        .await
        .unwrap();
    for kind in ["principal", "user", "visitor"] {
        assert!(events.iter().any(|event| matches!(event, ChannelEvent::Posted { author, .. } if author == &format!("{kind}:hub:remote"))));
    }
}

#[tokio::test]
async fn promoting_remote_contact_preserves_pair_history_and_learned_relationships() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let owner = PrincipalId::generate();
    let remote: ActorRef = "principal:hub:remote".parse().unwrap();
    port.register_actor(
        &remote,
        Some("remote"),
        Some("did:key:remote"),
        Some("remote-key"),
        Some("Remote"),
        Registration::Public,
    )
    .await
    .unwrap();
    let dm = port.ensure_dm(&owner, &remote, "/remote").await.unwrap();
    port.post(&dm, &remote, PostMsg::root("before promotion"))
        .await
        .unwrap();
    let hosted = PrincipalId::generate();
    let info = port
        .register_actor(
            &ActorRef::from(&hosted),
            Some("remote"),
            Some("did:key:remote"),
            Some("remote-key"),
            Some("Hosted"),
            Registration::Private,
        )
        .await
        .unwrap();
    assert_eq!(info.local_id.as_deref(), Some(hosted.0.as_str()));
    assert_eq!(
        port.resolve_actor("principal:hub:remote").await.unwrap(),
        info
    );
    assert_eq!(
        port.ensure_dm(&owner, &remote, "/remote").await.unwrap(),
        dm
    );
    assert_eq!(
        port.ensure_dm(&hosted, &ActorRef::from(&owner), "/owner")
            .await
            .unwrap(),
        dm
    );
    port.post(
        &dm,
        &ActorRef::from(&hosted),
        PostMsg::root("after promotion"),
    )
    .await
    .unwrap();
    let restarted = store(temp.path());
    assert_eq!(restarted.actor_info(&ActorRef::from(&hosted)).await, info);
    assert_eq!(restarted.participants(&dm).await.unwrap().len(), 2);
    let posts = restarted
        .peek(&dm, &Checkpoint::default())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e {
            ChannelEvent::Posted { author, .. } => Some(author),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(posts, vec![info.address.clone(), info.address.clone()]);
    // Refreshing the Hub profile cannot turn a hosted actor back into a remote contact.
    let refreshed = port
        .register_actor(
            &remote,
            Some("remote"),
            Some("did:key:remote"),
            Some("remote-key"),
            None,
            Registration::Private,
        )
        .await
        .unwrap();
    assert_eq!(refreshed, info);
}

#[tokio::test]
async fn invalid_or_substituted_hub_identities_are_rejected_without_registry_writes() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    for subject in [
        ActorRef::Public,
        // Text parsing refuses empty IDs; the registry must as well.
        ActorRef::User(peko_subject::Route::hub("")),
        ActorRef::Principal(peko_subject::Route::local("")),
    ] {
        assert!(port
            .register_actor(&subject, None, None, None, None, Registration::Registered)
            .await
            .is_err());
    }
    assert!(!temp.path().join("actors.json").exists());
    let remote: ActorRef = "principal:hub:remote".parse().unwrap();
    assert!(port
        .register_actor(
            &remote,
            Some("substituted"),
            None,
            None,
            None,
            Registration::Public
        )
        .await
        .is_err());
    port.register_actor(
        &remote,
        Some("remote"),
        Some("did:key:remote"),
        Some("key"),
        None,
        Registration::Public,
    )
    .await
    .unwrap();
    let before = std::fs::read(temp.path().join("actors.json")).unwrap();
    let other: ActorRef = "principal:hub:other".parse().unwrap();
    assert!(port
        .register_actor(
            &other,
            Some("other"),
            Some("did:key:remote"),
            Some("key"),
            None,
            Registration::Public
        )
        .await
        .is_err());
    assert_eq!(
        std::fs::read(temp.path().join("actors.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn unhosting_and_unlinking_preserve_history_without_public_stale_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let owner = PrincipalId::generate();
    let hosted = PrincipalId::generate();
    let hosted_subject = ActorRef::from(&hosted);
    port.register_actor(
        &hosted_subject,
        Some("hub-hosted"),
        Some("did:key:hosted"),
        Some("hosted-key"),
        Some("Hosted"),
        Registration::Private,
    )
    .await
    .unwrap();
    let dm = port
        .ensure_dm(&owner, &hosted_subject, "/hosted")
        .await
        .unwrap();
    port.post(&dm, &hosted_subject, PostMsg::root("before unhosting"))
        .await
        .unwrap();
    port.unhost_principal(&hosted).await.unwrap();
    let remote = port
        .resolve_actor("principal:hub:hub-hosted")
        .await
        .unwrap();
    assert_eq!(remote.local_id, None);
    assert_eq!(remote.address, "principal:hub:hub-hosted");
    assert!(port
        .resolve_actor(&format!("principal:local:{}", hosted.0))
        .await
        .is_err());
    assert_eq!(
        port.ensure_dm(&owner, &remote.subject(), "/hosted")
            .await
            .unwrap(),
        dm
    );
    let events = port.peek(&dm, &Checkpoint::default()).await.unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, ChannelEvent::Posted { author, .. } if author == &remote.address)));
    assert_eq!(port.actor_info(&remote.subject()).await, remote);
    // Rehosting then unlinking the Hub identity preserves earlier Hub authors.
    port.register_actor(
        &hosted_subject,
        Some("hub-hosted"),
        Some("did:key:hosted"),
        Some("hosted-key"),
        None,
        Registration::Private,
    )
    .await
    .unwrap();
    port.post(
        &dm,
        &"principal:hub:hub-hosted".parse().unwrap(),
        PostMsg::root("Hub author"),
    )
    .await
    .unwrap();
    port.register_actor(
        &hosted_subject,
        None,
        Some("did:key:hosted"),
        Some("hosted-key"),
        None,
        Registration::LocalOnly,
    )
    .await
    .unwrap();
    assert!(port
        .resolve_actor("principal:hub:hub-hosted")
        .await
        .is_err());
    assert_eq!(
        port.ensure_dm(&owner, &hosted_subject, "/hosted")
            .await
            .unwrap(),
        dm
    );
    let canonical = format!("principal:local:{}", hosted.0);
    let restarted = store(temp.path());
    assert!(restarted
        .peek(&dm, &Checkpoint::default())
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            ChannelEvent::Posted { author, .. } => Some(author),
            _ => None,
        })
        .all(|author| author == &canonical));
    assert!(restarted
        .register_actor(
            &ActorRef::from(&PrincipalId::generate()),
            Some("hub-hosted"),
            Some("did:key:other"),
            Some("other-key"),
            None,
            Registration::Public
        )
        .await
        .is_err());
}

#[tokio::test]
async fn reconciling_legacy_proxies_uses_host_inventory_and_preserves_conversations() {
    let temp = tempfile::tempdir().unwrap();
    let port = store(temp.path());
    let owner = PrincipalId::generate();
    // This proxy was previously imported and is now a hosted principal.
    let hosted = PrincipalId("local_hosted".into());
    port.register_actor(
        &ActorRef::from(&hosted),
        Some("hosted"),
        Some("did:key:hosted"),
        Some("hosted-key"),
        None,
        Registration::Private,
    )
    .await
    .unwrap();
    let remote = ActorRef::Principal("local_remote".into());
    port.register_actor(
        &remote,
        Some("remote"),
        Some("did:key:remote"),
        Some("remote-key"),
        None,
        Registration::Public,
    )
    .await
    .unwrap();
    let dm = port.ensure_dm(&owner, &remote, "/remote").await.unwrap();
    port.post(&dm, &remote, PostMsg::root("legacy history"))
        .await
        .unwrap();
    for kind in ["user", "visitor"] {
        port.register_actor(
            &format!("{kind}:local_human").parse().unwrap(),
            Some("human"),
            None,
            None,
            None,
            Registration::Registered,
        )
        .await
        .unwrap();
    }
    port.reconcile_actor_locality(&[owner.clone(), hosted.clone()])
        .await
        .unwrap();
    let actor = port.resolve_actor("principal:hub:remote").await.unwrap();
    assert!(actor.local_id.is_none());
    assert_eq!(
        port.resolve_actor("principal:hub:hosted")
            .await
            .unwrap()
            .local_id,
        Some(hosted.0.clone())
    );
    assert!(port
        .resolve_actor("principal:local:local_remote")
        .await
        .is_err());
    for kind in ["user", "visitor"] {
        let actor = port
            .resolve_actor(&format!("{kind}:hub:human"))
            .await
            .unwrap();
        assert!(actor.local_id.is_none());
        assert!(port
            .resolve_actor(&format!("{kind}:local:local_human"))
            .await
            .is_err());
    }
    assert_eq!(port.actor_info(&actor.subject()).await, actor);
    assert_eq!(
        port.ensure_dm(&owner, &actor.subject(), "/remote")
            .await
            .unwrap(),
        dm
    );
    let restarted = store(temp.path());
    assert_eq!(restarted.participants(&dm).await.unwrap().len(), 2);
    assert!(restarted
        .peek(&dm, &Checkpoint::default())
        .await
        .unwrap()
        .iter()
        .any(
            |e| matches!(e, ChannelEvent::Posted { author, .. } if author == "principal:hub:remote")
        ));
    let before = std::fs::read(temp.path().join("actors.json")).unwrap();
    port.reconcile_actor_locality(&[owner, hosted.clone()])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(temp.path().join("actors.json")).unwrap(),
        before
    );
}
