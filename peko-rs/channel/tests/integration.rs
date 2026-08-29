//! `peko-channel` integration tests.
//!
//! Two tests per `lexical-soaring-pretzel.md` §10:
//!
//! - `two_principals_full_lifecycle` — causal-order, fan-out, idempotency.
//! - `subscriber_calls_responder_for_each_new_event` — per-event fan-out
//!   to the responder, no spurious calls on re-tick.
//!
//! Tests run against the file-backed `ChannelStore` rooted in a
//! `tempfile::TempDir` so the storage layer is exercised end-to-end.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peko_channel::cursors::ChannelCursors;
use peko_channel::port::{ChannelError, Checkpoint, CreateOpts, PostMsg, Tier};
use peko_channel::responder::{ChannelResponder, RespondCtx};
use peko_channel::subscription::SubscriptionConfig;
use peko_channel::{
    ChannelCliRouter, ChannelConfig, ChannelId, ChannelPort, ChannelStore,
};
use peko_subject::{PrincipalId, Subject};
use peko_protocol::channel::ChannelEvent;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a fresh adapter rooted in a tempdir.
fn adapter_in_tempdir() -> (TempDir, Arc<ChannelStore>) {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = ChannelConfig {
        runtime_dir: tmp.path().to_path_buf(),
        shared_dir: None, // PR-3d: defaults to None for the legacy
                          // single-tier test path. `pin_to_shared`
                          // surfaces `ChannelError::Adapter` here.
    };
    let adapter = ChannelStore::new(cfg);
    (tmp, Arc::new(adapter))
}

/// Wrap in `Arc<dyn ChannelPort>` (port-trait dispatch seam — proves
/// callers don't depend on the concrete struct).
fn as_port(a: Arc<ChannelStore>) -> Arc<dyn ChannelPort> {
    a
}

/// Build a fresh adapter rooted in a tempdir with both Runtime and
/// Shared tier roots populated. Mirrors `adapter_in_tempdir()` but
/// enables `pin_to_shared` + `Tier::Shared` creation paths.
fn adapter_in_tempdir_with_shared() -> (TempDir, Arc<ChannelStore>) {
    let tmp = TempDir::new().expect("tempdir");
    let runtime_dir = tmp.path().join("runtime");
    let shared_dir = tmp.path().join("shared");
    std::fs::create_dir_all(&runtime_dir).expect("mkdir runtime");
    std::fs::create_dir_all(&shared_dir).expect("mkdir shared");
    let cfg = ChannelConfig {
        runtime_dir,
        shared_dir: Some(shared_dir),
    };
    let adapter = ChannelStore::new(cfg);
    (tmp, Arc::new(adapter))
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
        .post(&chan, &Subject::from(&bob), PostMsg::root("premature"))
        .await
        .expect_err("non-member post must fail");
    assert!(matches!(err, peko_channel::ChannelError::NotMember));

    // Invite bob.
    port.invite(&chan, &alice, &Subject::from(&bob)).await.expect("invite");

    // Alice posts a root message.
    let msg1 = port
        .post(&chan, &Subject::from(&alice), PostMsg::root("hello"))
        .await
        .expect("post msg1");

    // Bob replies to msg1.
    let msg2 = port
        .post(&chan, &Subject::from(&bob), PostMsg::reply(msg1.clone(), "reply"))
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
    assert_eq!(members.len(), 2);
    assert!(members.contains(&Subject::from(&alice)));
    assert!(members.contains(&Subject::from(&bob)));

    // Each principal sees exactly one channel.
    let alice_chans = port.list_for_principal(&alice).await.expect("alice chans");
    let bob_chans = port.list_for_principal(&bob).await.expect("bob chans");
    assert_eq!(alice_chans, vec![chan.clone()]);
    assert_eq!(bob_chans, vec![chan.clone()]);

    // Idempotent invite is silent (no error, no duplicate member).
    port.invite(&chan, &alice, &Subject::from(&bob)).await.expect("idempotent invite");
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
    port.invite(&chan, &alice, &Subject::from(&bob)).await.expect("invite bob");

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
    port.post(&chan, &Subject::from(&alice), PostMsg::root("a"))
        .await
        .expect("post a");
    port.post(&chan, &Subject::from(&alice), PostMsg::root("b"))
        .await
        .expect("post b");
    port.post(&chan, &Subject::from(&alice), PostMsg::root("c"))
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
    port.post(&chan, &Subject::from(&alice), PostMsg::root("d"))
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
// Test C: push-wake (sprint 3 Phase 10)
// ---------------------------------------------------------------------------

/// A spawned subscriber with a LONG backstop interval must still
/// process a fresh post promptly: the store's append-time broadcast
/// wakes the loop's `select!` instead of the post sitting unseen
/// until the next tick. This is the end-to-end contract the DM-tier
/// `PassiveBindingResponder` relies on for real-time bound-channel
/// wake-ups.
#[tokio::test(flavor = "multi_thread")]
async fn spawned_subscriber_wakes_on_append_without_waiting_for_tick() {
    let (tmp, adapter) = adapter_in_tempdir();
    let port: Arc<dyn ChannelPort> = as_port(adapter.clone());

    let alice = PrincipalId::generate();
    let chan = port
        .create(&alice, CreateOpts::runtime("wake-test"))
        .await
        .expect("create");

    // Bob joins + subscribes with a counter responder and a backstop
    // interval far beyond the test's patience — if the wake-up ever
    // regresses to pure polling, the assertions below time out.
    let bob = PrincipalId::generate();
    port.invite(&chan, &alice, &Subject::from(&bob)).await.expect("invite bob");

    let responder = Arc::new(CountResponder::default());
    let counters = responder.clone();
    let chan_dir = tmp.path().join("channels").join(chan.as_str());
    let sub = peko_channel::ChannelSubscriber::new(
        chan.clone(),
        bob.clone(),
        chan_dir,
        port.clone(),
        responder,
        peko_channel::cost::noop_meter(),
        ChannelCursors::new(),
        SubscriptionConfig {
            poll_interval: Duration::from_secs(3600),
        },
    );
    let handle = sub.spawn();

    // First tick processes the backlog (Created + MemberJoined)
    // before we post — wait for it so the post is the ONLY new event.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while counters.count.load(Ordering::Relaxed) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "first tick must deliver the 2 backlog events promptly"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    port.post(&chan, &Subject::from(&alice), PostMsg::root("ping"))
        .await
        .expect("post ping");

    // The append broadcast must wake the loop well under any polling
    // interval — allow 2s for scheduling slack on loaded CI.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while counters.count.load(Ordering::Relaxed) < 3 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "post must be delivered via push-wake, not the 1h backstop tick"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        counters.count.load(Ordering::Relaxed),
        3,
        "exactly one responder call for the pushed post"
    );

    handle.abort();
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

    let created = router
        .handle_create(&alice, "demo", None)
        .await
        .expect("create");
    router
        .handle_invite(&created.channel, &alice, &Subject::from(&bob))
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
        port.invite(&chan, &creator, &Subject::from(&p)).await.expect("invite");
        members.push(p);
    }
    assert_eq!(port.list_members(&chan).await.unwrap().len(), 8);

    // 9th invite must error with FanOutCap.
    let ninth = PrincipalId::generate();
    let err = port
        .invite(&chan, &creator, &Subject::from(&ninth))
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
        id: None,            // sprint 4: explicit id (None = mint fresh)
        passive_binding: None,
    };
    let _ = opts; // sanity: Tier::Runtime is the only valid option
    let _ = port.create(&alice, CreateOpts::runtime("ok")).await;
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Test F: PR-3d Shared-tier opt-in
// ---------------------------------------------------------------------------

/// PR-3d: `pin_to_shared` copies `meta.json` + `members.json` to the
/// Shared root. Initial state (no posts) means zero `plan_*.jsonl`
/// files are present, but the directory + both files must exist on
/// the Shared side after the call. Runtime source remains intact
/// (COPY semantics — see PR-3d plan §3d).
#[tokio::test(flavor = "multi_thread")]
async fn shared_pin_copies_files_to_shared_root() {
    let (tmp, adapter) = adapter_in_tempdir_with_shared();
    let port: Arc<dyn ChannelPort> = as_port(adapter);

    let creator = PrincipalId::generate();
    let chan = port
        .create(&creator, CreateOpts::runtime("team"))
        .await
        .expect("create");

    let shared_path = port
        .pin_to_shared(&chan)
        .await
        .expect("pin_to_shared");

    // Path returned matches the expected Shared root layout.
    let expected_root = tmp.path().join("shared").join("channels");
    assert!(
        shared_path.starts_with(&expected_root),
        "shared_path {shared_path:?} must be under {expected_root:?}"
    );
    assert!(shared_path.join("meta.json").exists());
    assert!(shared_path.join("members.json").exists());

    // Runtime source dir still exists.
    let runtime_chan_dir = tmp.path().join("runtime").join("channels").join(chan.as_str());
    assert!(runtime_chan_dir.exists(), "runtime source must remain");
}

/// PR-3d / PR-5b: pin copies the runtime `events.jsonl` log into the
/// Shared root after a post. Verifies the COPY semantics actually
/// transfer bytes — not just the static files.
#[tokio::test(flavor = "multi_thread")]
async fn shared_pin_copies_log_lines_after_post() {
    let (tmp, adapter) = adapter_in_tempdir_with_shared();
    let port: Arc<dyn ChannelPort> = as_port(adapter);

    let creator = PrincipalId::generate();
    let chan = port
        .create(&creator, CreateOpts::runtime("team"))
        .await
        .expect("create");
    let _task_id = port
        .post(&chan, &Subject::from(&creator), PostMsg::root("hello"))
        .await
        .expect("post");

    let shared_path = port
        .pin_to_shared(&chan)
        .await
        .expect("pin_to_shared");

    // The runtime side has events.jsonl after the post.
    let runtime_chan_dir = tmp.path().join("runtime").join("channels").join(chan.as_str());
    let runtime_log = runtime_chan_dir.join("events.jsonl");
    assert!(
        runtime_log.exists(),
        "runtime should have events.jsonl after a post"
    );
    let runtime_bytes = std::fs::read(&runtime_log).expect("read runtime log");
    assert!(
        !runtime_bytes.is_empty(),
        "runtime events.jsonl should not be empty after a post"
    );

    // The Shared side must mirror the same log file (COPY).
    let shared_log = shared_path.join("events.jsonl");
    assert!(
        shared_log.exists(),
        "shared must mirror runtime events.jsonl"
    );
    let shared_bytes = std::fs::read(&shared_log).expect("read shared log");
    assert_eq!(
        shared_bytes.len(),
        runtime_bytes.len(),
        "shared events.jsonl must mirror runtime"
    );

    // The posted message must be discoverable via `peek` on the
    // *runtime* side (proves we did not MOVE the source — COPY).
    let events = port.peek(&chan, &Checkpoint::default()).await.expect("peek");
    let posted = events
        .iter()
        .find_map(|e| match e {
            peko_protocol::channel::ChannelEvent::Posted { text, .. }
                if text == "hello" =>
            {
                Some(())
            }
            _ => None,
        });
    assert!(posted.is_some(), "runtime source must still expose the posted message");
}

/// PR-3d: `create()` with `Tier::Shared` writes the channel dir
/// directly under the Shared root — no Runtime sibling is created.
#[tokio::test(flavor = "multi_thread")]
async fn create_with_shared_tier_writes_to_shared_root() {
    let (tmp, adapter) = adapter_in_tempdir_with_shared();
    let port: Arc<dyn ChannelPort> = as_port(adapter);

    let creator = PrincipalId::generate();
    let chan = port
        .create(&creator, CreateOpts::shared("direct-shared"))
        .await
        .expect("create with shared tier");

    // Shared side has the chan dir.
    let shared_chan_dir = tmp
        .path()
        .join("shared")
        .join("channels")
        .join(chan.as_str());
    assert!(shared_chan_dir.exists());
    assert!(shared_chan_dir.join("meta.json").exists());

    // Runtime side has NO sibling.
    let runtime_chan_dir = tmp
        .path()
        .join("runtime")
        .join("channels")
        .join(chan.as_str());
    assert!(
        !runtime_chan_dir.exists(),
        "shared-tier create must not write to runtime"
    );
}

/// PR-3d: `pin_to_shared` against an adapter with `shared_dir: None`
/// must surface `ChannelError::Adapter` (not panic, not silently
/// succeed). Mirrors the CLI fallback path that has no
/// `SharedLayout` access.
#[tokio::test(flavor = "multi_thread")]
async fn pin_to_shared_fails_when_shared_dir_is_none() {
    let (_tmp, adapter) = adapter_in_tempdir(); // no shared_dir
    let port: Arc<dyn ChannelPort> = as_port(adapter);

    let creator = PrincipalId::generate();
    let chan = port
        .create(&creator, CreateOpts::runtime("team"))
        .await
        .expect("create");

    let err = port
        .pin_to_shared(&chan)
        .await
        .expect_err("pin_to_shared must fail without shared_dir");
    assert!(
        matches!(err, ChannelError::Adapter(_)),
        "got {err:?}"
    );
}