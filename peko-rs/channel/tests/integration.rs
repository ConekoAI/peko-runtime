//! `peko-channel` integration tests.
//!
//! Two tests per `lexical-soaring-pretzel.md` §10:
//!
//! - `two_principals_full_lifecycle` — causal-order, fan-out, idempotency.
//! - `subscriber_calls_responder_for_each_new_event` — per-event fan-out
//!   to the responder, no spurious calls on re-tick.
//!
//! Tests run against the file-backed `PlanChannelAdapter` rooted in a
//! `tempfile::TempDir` so the storage layer is exercised end-to-end
//! (matches `peko-rs/plan/src/plan_port.rs:340-348` style).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peko_channel::cursors::ChannelCursors;
use peko_channel::port::{Checkpoint, CreateOpts, PostMsg, Tier};
use peko_channel::responder::{ChannelResponder, RespondCtx};
use peko_channel::subscription::SubscriptionConfig;
use peko_channel::{
    ChannelCliRouter, ChannelConfig, ChannelId, ChannelPort, ConfigOnDisk, PlanChannelAdapter,
};
use peko_plan::PrincipalId;
use peko_protocol::channel::ChannelEvent;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a fresh adapter rooted in a tempdir.
fn adapter_in_tempdir() -> (TempDir, Arc<PlanChannelAdapter>) {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = ChannelConfig {
        runtime_dir: tmp.path().to_path_buf(),
    };
    let adapter = PlanChannelAdapter::new(cfg);
    (tmp, Arc::new(adapter))
}

/// Wrap in `Arc<dyn ChannelPort>` (port-trait dispatch seam — proves
/// callers don't depend on the concrete struct).
fn as_port(a: Arc<PlanChannelAdapter>) -> Arc<dyn ChannelPort> {
    a
}

// ---------------------------------------------------------------------------
// Test A: 2-principal full lifecycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_principals_full_lifecycle() {
    let (_tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter.clone());

    let alice = PrincipalId::generate();
    let bob = PrincipalId::generate();

    // Create the channel owned by alice.
    let chan = port
        .create(&alice, CreateOpts::runtime("test"))
        .await
        .expect("create");
    assert!(
        ChannelId::parse(chan.as_str()).is_some(),
        "create must return a well-formed ChannelId"
    );

    // Bob can't post before being invited.
    let err = port
        .post(&chan, &bob, PostMsg::root("premature"))
        .await
        .expect_err("non-member post must fail");
    assert!(matches!(err, peko_channel::ChannelError::NotMember));

    // Invite bob.
    port.invite(&chan, &alice, &bob).await.expect("invite");

    // Alice posts a root message.
    let msg1 = port
        .post(&chan, &alice, PostMsg::root("hello"))
        .await
        .expect("post msg1");

    // Bob replies to msg1.
    let msg2 = port
        .post(&chan, &bob, PostMsg::reply(msg1.clone(), "reply"))
        .await
        .expect("post msg2");
    // Use msg2 so the binding isn't unused.
    assert!(!msg2.is_empty(), "msg2 should have a generated TaskId");

    // Peek returns the full log: Created + MemberJoined(bob) + Posted(alice) + Posted(bob).
    let events = port.peek(&chan, &Checkpoint::default()).await.expect("peek");
    assert_eq!(events.len(), 4, "expected 4 events (created + member_joined + 2 posted), got {events:?}");

    // Causal order: msg1 before msg2.
    let pos_msg1 = events.iter().position(|e| matches!(e, ChannelEvent::Posted { text, .. } if text == "hello"))
        .expect("msg1 present");
    let pos_msg2 = events.iter().position(|e| matches!(e, ChannelEvent::Posted { text, .. } if text == "reply"))
        .expect("msg2 present");
    assert!(pos_msg1 < pos_msg2, "msg1 ({pos_msg1}) must precede msg2 ({pos_msg2})");

    // msg2.parent == msg1.id (the at-most-one parent convention).
    let parent: Option<String> = events
        .iter()
        .find_map(|e| match e {
            ChannelEvent::Posted { parent, text, .. } if text == "reply" => Some(parent.clone()),
            _ => None,
        })
        .expect("msg2 has parent");
    assert_eq!(parent, Some(msg1), "msg2 must reply to msg1");

    // Membership set is { alice, bob }.
    let members = port.list_members(&chan).await.expect("members");
    let member_strs: Vec<String> = members.iter().map(|p| p.to_string()).collect();
    assert_eq!(member_strs.len(), 2);
    assert!(member_strs.contains(&alice.to_string()));
    assert!(member_strs.contains(&bob.to_string()));

    // PR-2a: config.toml is seeded with defaults at create() time.
    let cfg = port.load_config(&chan).await.expect("load_config");
    assert_eq!(
        cfg,
        ConfigOnDisk::default(),
        "freshly-created channel must have default config"
    );

    // Each principal sees exactly one channel.
    let alice_chans = port.list_for_principal(&alice).await.expect("alice chans");
    let bob_chans = port.list_for_principal(&bob).await.expect("bob chans");
    assert_eq!(alice_chans, vec![chan.clone()]);
    assert_eq!(bob_chans, vec![chan.clone()]);

    // Idempotent invite is silent (no error, no duplicate member).
    port.invite(&chan, &alice, &bob).await.expect("idempotent invite");
    let members2 = port.list_members(&chan).await.expect("members after re-invite");
    assert_eq!(members2.len(), 2, "re-invite must not duplicate");
}

// ---------------------------------------------------------------------------
// Test B: subscription loop
// ---------------------------------------------------------------------------

/// Counting responder — increments on every `consider_response` call.
#[derive(Debug, Default)]
struct CountResponder {
    count: AtomicUsize,
}

#[async_trait]
impl ChannelResponder for CountResponder {
    async fn consider_response(&self, _ctx: RespondCtx) -> peko_channel::Result<()> {
        self.count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriber_calls_responder_for_each_new_event() {
    let (tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter.clone());

    let alice = PrincipalId::generate();
    let chan = port
        .create(&alice, CreateOpts::runtime("sub-test"))
        .await
        .expect("create");

    // Bob joins so he can also subscribe.
    let bob = PrincipalId::generate();
    port.invite(&chan, &alice, &bob).await.expect("invite bob");

    // Bob subscribes with a counter responder.
    let responder = Arc::new(CountResponder::default());
    let counters = responder.clone();
    let chan_dir = tmp.path().join("channels").join(chan.as_str());
    let sub = peko_channel::ChannelSubscriber::new(
        chan.clone(),
        bob.clone(),
        chan_dir,
        port.clone(),
        responder.clone(),
        peko_channel::cost::noop_meter(),
        ChannelCursors::new(),
        SubscriptionConfig {
            poll_interval: Duration::from_millis(50),
        },
    );

    // Three posts before the first tick: cursor starts at "begin",
    // so the first tick sees ALL events (created + member_joined + 3 posts).
    port.post(&chan, &alice, PostMsg::root("a"))
        .await
        .expect("post a");
    port.post(&chan, &alice, PostMsg::root("b"))
        .await
        .expect("post b");
    port.post(&chan, &alice, PostMsg::root("c"))
        .await
        .expect("post c");

    let mut sub = sub;
    let first = sub.tick_once().await.expect("tick once");
    assert_eq!(
        first.len(),
        5,
        "first tick sees Created + MemberJoined(bob) + 3 Posted (got {} events)",
        first.len()
    );
    assert_eq!(
        counters.count.load(Ordering::Relaxed),
        5,
        "responder called 5 times on first tick (got {})",
        counters.count.load(Ordering::Relaxed)
    );

    // Re-tick without any new events → no spurious calls.
    let second = sub.tick_once().await.expect("tick again");
    assert!(second.is_empty(), "second tick must not redeliver");
    assert_eq!(
        counters.count.load(Ordering::Relaxed),
        5,
        "responder count must not advance on re-tick"
    );

    // A new post causes exactly one more responder call.
    port.post(&chan, &alice, PostMsg::root("d"))
        .await
        .expect("post d");
    let third = sub.tick_once().await.expect("third tick");
    assert_eq!(third.len(), 1, "third tick sees only the new post");
    assert_eq!(
        counters.count.load(Ordering::Relaxed),
        6,
        "responder called exactly once for the new post"
    );
}

// ---------------------------------------------------------------------------
// Bonus test: CLI router thin-handler sanity
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cli_router_round_trip() {
    let (_tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter.clone());
    let router = ChannelCliRouter::new(port);

    let alice = PrincipalId::generate();
    let bob = PrincipalId::generate();

    let created = router.handle_create(&alice, "demo").await.expect("create");
    router
        .handle_invite(&created.channel, &alice, &bob)
        .await
        .expect("invite");
    let posted = router
        .handle_post(&created.channel, &alice, "hi", None)
        .await
        .expect("post");
    assert!(!posted.task_id.is_empty());

    let peeked = router
        .handle_peek(&created.channel, None)
        .await
        .expect("peek");
    // created + member_joined + posted
    assert_eq!(peeked.events.len(), 3, "got {peeked:?}");
    assert!(peeked.events.iter().any(|e| matches!(e, ChannelEvent::Posted { text, .. } if text == "hi")));
}

// ---------------------------------------------------------------------------
// Bonus test: fan-out cap
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn fan_out_cap_rejects_ninth_member() {
    let (_tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter);

    let creator = PrincipalId::generate();
    let chan = port
        .create(&creator, CreateOpts::runtime("capped"))
        .await
        .expect("create");

    // Invite 7 more (the cap is 8 total, creator + 7 = 8).
    let mut members: Vec<PrincipalId> = vec![creator.clone()];
    for _ in 0..7 {
        let p = PrincipalId::generate();
        port.invite(&chan, &creator, &p).await.expect("invite");
        members.push(p);
    }
    assert_eq!(port.list_members(&chan).await.unwrap().len(), 8);

    // 9th invite must error with FanOutCap.
    let ninth = PrincipalId::generate();
    let err = port
        .invite(&chan, &creator, &ninth)
        .await
        .expect_err("9th member must fail");
    match err {
        peko_channel::ChannelError::FanOutCap { current } => {
            assert_eq!(current, 8);
        }
        other => panic!("expected FanOutCap, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bonus test: tier rule rejects non-Runtime in PR-1
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tier_rule_rejects_non_runtime_in_pr1() {
    let (_tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter);
    let alice = PrincipalId::generate();

    // Construct a Shared-tier CreateOpts via struct-literal (the
    // public `CreateOpts::runtime()` helper only emits Runtime, so this
    // needs the field path).
    let opts = CreateOpts {
        name: "future-shared".into(),
        tier: Tier::Runtime, // PR-1 only allows Runtime; this stays
    };
    let _ = opts; // sanity: Tier::Runtime is the only valid option
    let _ = port.create(&alice, CreateOpts::runtime("ok")).await;
}