//! Suggested replies staged for a conversation's reply (ADR-093 D3).
//!
//! A conversation's reply is the run's final text, posted after the run
//! by [`super::peer_dm::post_peer_dm_reply`] on every reply path. The
//! `Suggest` tool cannot post it, so it stages its options here, keyed
//! by the conversation channel and the principal; the reply post takes
//! them. Only the conversation's own session may stage (the tool checks
//! the binding), so one principal has at most one reply pending per
//! channel.
//!
//! Staged options carry the channel's newest line when staged. They go
//! on the reply only if nobody else has posted since: a run that ends
//! without a reply (an error, an empty answer) cannot hand its options
//! to a later reply that answers something else.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use peko_channel::{ChannelEvent, ChannelId, ChannelPort};
use peko_subject::{ActorRef, PrincipalId};

/// How far back the reply looks for other posts since staging. A
/// conversation busier than this between a `Suggest` call and the reply
/// drops the options rather than reading further.
const LOOKBACK: usize = 50;

struct Staged {
    options: Vec<String>,
    /// The channel's newest line when staged; `None` for an empty log.
    after: Option<u64>,
}

static STAGED: LazyLock<Mutex<HashMap<(String, String), Staged>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn key(channel: &ChannelId, principal: &PrincipalId) -> (String, String) {
    (channel.as_str().to_string(), principal.0.clone())
}

/// The channel's newest line id, if it has one.
async fn newest_line(port: &dyn ChannelPort, channel: &ChannelId) -> Option<u64> {
    let page = port.peek_tail(channel, 1, None).await.ok()?;
    page.events.last().and_then(|(line, _)| line.parse().ok())
}

/// Stage `options` for `principal`'s next reply on `channel`, replacing
/// any staged before. Empty `options` clears.
pub(crate) async fn stage(
    port: &dyn ChannelPort,
    channel: &ChannelId,
    principal: &PrincipalId,
    options: Vec<String>,
) {
    if options.is_empty() {
        clear(channel, principal);
        return;
    }
    let after = newest_line(port, channel).await;
    STAGED
        .lock()
        .expect("reply options mutex")
        .insert(key(channel, principal), Staged { options, after });
}

/// Drop what is staged for `principal`'s reply on `channel` (a withheld
/// reply takes its options with it).
pub(crate) fn clear(channel: &ChannelId, principal: &PrincipalId) {
    STAGED
        .lock()
        .expect("reply options mutex")
        .remove(&key(channel, principal));
}

/// Take what is staged for `principal`'s reply on `channel`: the options
/// when nobody else has posted since they were staged, else none. The
/// slot is cleared either way.
pub(crate) async fn take(
    port: &dyn ChannelPort,
    channel: &ChannelId,
    principal: &PrincipalId,
) -> Vec<String> {
    let Some(staged) = STAGED
        .lock()
        .expect("reply options mutex")
        .remove(&key(channel, principal))
    else {
        return Vec::new();
    };
    let Ok(page) = port.peek_tail(channel, LOOKBACK, None).await else {
        return Vec::new();
    };
    let since: Vec<&ChannelEvent> = page
        .events
        .iter()
        .filter(|(line, _)| match (line.parse::<u64>(), staged.after) {
            (Ok(line), Some(after)) => line > after,
            (Ok(_), None) => true,
            (Err(_), _) => false,
        })
        .map(|(_, event)| event)
        .collect();
    // The whole page is newer than the staging point and older lines
    // exist: the conversation moved on further than we read.
    let first_is_newer = page
        .events
        .first()
        .and_then(|(line, _)| line.parse::<u64>().ok())
        .is_some_and(|first| staged.after.is_none_or(|after| first > after + 1));
    if page.has_more && first_is_newer {
        return Vec::new();
    }
    let me = ActorRef::from(principal);
    let my_address = port.actor_info(&me).await.address;
    let others_posted = since.iter().any(|event| match event {
        ChannelEvent::Posted { author, .. } => *author != principal.0 && *author != my_address,
        _ => false,
    });
    if others_posted {
        Vec::new()
    } else {
        staged.options
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, CreateOpts, PostMsg};
    use std::sync::Arc;

    async fn fixture() -> (tempfile::TempDir, Arc<ChannelStore>, PrincipalId, ChannelId) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().to_path_buf(),
            shared_dir: None,
        }));
        let peko = PrincipalId::generate();
        let channel = store.create(&peko, CreateOpts::default()).await.unwrap();
        (dir, store, peko, channel)
    }

    fn two() -> Vec<String> {
        vec!["Yes".into(), "Not yet".into()]
    }

    #[tokio::test]
    async fn staged_options_are_taken_once() {
        let (_dir, store, peko, channel) = fixture().await;
        stage(store.as_ref(), &channel, &peko, two()).await;
        // The peko's own posts (an Attach, a ChannelSend) keep them.
        store
            .post(&channel, &ActorRef::from(&peko), PostMsg::root("chart"))
            .await
            .unwrap();
        assert_eq!(take(store.as_ref(), &channel, &peko).await, two());
        assert!(take(store.as_ref(), &channel, &peko).await.is_empty());
    }

    #[tokio::test]
    async fn a_later_call_replaces_and_empty_clears() {
        let (_dir, store, peko, channel) = fixture().await;
        stage(
            store.as_ref(),
            &channel,
            &peko,
            vec!["A".into(), "B".into()],
        )
        .await;
        stage(store.as_ref(), &channel, &peko, two()).await;
        assert_eq!(take(store.as_ref(), &channel, &peko).await, two());
        stage(store.as_ref(), &channel, &peko, two()).await;
        stage(store.as_ref(), &channel, &peko, Vec::new()).await;
        assert!(take(store.as_ref(), &channel, &peko).await.is_empty());
    }

    /// A run that staged options and never replied cannot hand them to
    /// the reply to someone's next message.
    #[tokio::test]
    async fn someone_else_posting_drops_them() {
        let (_dir, store, peko, channel) = fixture().await;
        let bob = PrincipalId::generate();
        store
            .invite(&channel, &peko, &ActorRef::from(&bob))
            .await
            .unwrap();
        stage(store.as_ref(), &channel, &peko, two()).await;
        store
            .post(
                &channel,
                &ActorRef::from(&bob),
                PostMsg::root("something else"),
            )
            .await
            .unwrap();
        assert!(take(store.as_ref(), &channel, &peko).await.is_empty());
    }

    #[tokio::test]
    async fn slots_are_per_channel_and_principal() {
        let (_dir, store, peko, channel) = fixture().await;
        let other = store.create(&peko, CreateOpts::default()).await.unwrap();
        stage(store.as_ref(), &channel, &peko, two()).await;
        assert!(take(store.as_ref(), &other, &peko).await.is_empty());
        assert!(take(store.as_ref(), &channel, &PrincipalId::generate())
            .await
            .is_empty());
        assert_eq!(take(store.as_ref(), &channel, &peko).await, two());
    }
}
