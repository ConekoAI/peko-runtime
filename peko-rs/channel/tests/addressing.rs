use peko_channel::{
    dm_channel_id, ChannelConfig, ChannelEvent, ChannelId, ChannelPort, ChannelStore, Checkpoint,
    CreateOpts, PostMsg,
};
use peko_subject::{PrincipalId, Subject};

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
    let user = Subject::User("local_account".into());
    port.register_actor(
        &Subject::from(&principal),
        None,
        Some("did:key:signer"),
        Some("actor-key"),
        Some("Alice"),
        "local_only",
    )
    .await
    .unwrap();
    port.register_actor(
        &user,
        Some("account"),
        None,
        None,
        Some("Human"),
        "registered",
    )
    .await
    .unwrap();
    let before = port.ensure_dm(&principal, &user, "/human").await.unwrap();
    port.post(&before, &user, PostMsg::root("hello"))
        .await
        .unwrap();
    port.register_actor(
        &Subject::from(&principal),
        Some("hub-a"),
        Some("did:key:signer"),
        Some("actor-key"),
        Some("Alice"),
        "private",
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
    assert!(events.iter().any(|ev| matches!(ev, ChannelEvent::Posted { author,.. } if author == "user:local:local_account")));
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
        &Subject::from(&principal),
        None,
        None,
        Some("actor-key"),
        Some("Alice"),
        "local_only",
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
    let a = Subject::Principal("prin_a".into());
    port.register_actor(
        &a,
        Some("hub-a"),
        Some("did:key:a"),
        Some("key-a"),
        None,
        "public",
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
            "public"
        )
        .await
        .is_err());
    assert!(port
        .register_actor(
            &Subject::Principal("prin_b".into()),
            Some("hub-a"),
            Some("did:key:b"),
            Some("key-b"),
            None,
            "public"
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
            "registered",
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
        &Subject::from(&aid),
        None,
        None,
        Some("key-a"),
        Some("Alice"),
        "local_only",
    )
    .await
    .unwrap();
    b.register_actor(
        &Subject::from(&bid),
        None,
        None,
        Some("key-b"),
        Some("Bob"),
        "local_only",
    )
    .await
    .unwrap();
    let actor_a = Subject::from(&aid);
    let actor_b = Subject::from(&bid);
    let (ab, ba) = tokio::join!(
        a.ensure_dm(&aid, &actor_b, "/bob"),
        b.ensure_dm(&bid, &actor_a, "/alice")
    );
    let channel = ab.unwrap();
    assert_eq!(channel, ba.unwrap());
    assert_eq!(
        a.binding_for(&channel, &aid).await.unwrap().as_deref(),
        Some("/bob")
    );
    assert_eq!(
        b.binding_for(&channel, &bid).await.unwrap().as_deref(),
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
    let visitor = Subject::Principal("prin_guest".into());
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
    let remote = Subject::Principal("local_remote".into());
    port.register_actor(
        &remote,
        Some("remote-hub"),
        Some("did:key:remote"),
        Some("remote-key"),
        None,
        "public",
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
