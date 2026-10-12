//! Replies go to the session that asked.
//!
//! A post a principal makes carries `via` — the session that made it —
//! and a stable `msg_id`. When someone answers with a post threaded onto
//! it (`reply_to` = that id), the answer is a reply to that session, not
//! a new conversation: [`ReplyRouter`] — wrapped around every channel
//! subscriber's responder — hands it to the asking session's inbox and
//! wakes the session (an idle session gets a turn; a running
//! one reads it at its next iteration — the path background-task
//! completions take). New root posts keep going to the channel's own
//! responder, so every post has exactly one consumer: no duplicates.
//!
//! This is what lets `ChannelSend` post and return: nothing waits in
//! memory for an answer that may take days, and a reply read after a
//! daemon restart (subscriber cursors persist) is routed the same way.
//! Delivering a reply is not answering it — the asker answers only by
//! calling `ChannelSend` again — so two agents cannot auto-reply in a
//! loop.
//!
//! The asker is whichever principal on THIS runtime wrote the asked
//! post, not necessarily the subscriber that reads the reply: a
//! principal on the same runtime is reached through the TARGET's DM
//! channel (where its responder runs; the asker has no subscriber
//! there), so the target's subscriber routes the reply back to the
//! asker. The request is also recorded, under the same `msg_id`, on the
//! asker's own DM channel; the reply is mirrored there (so `peko log`
//! reads the full exchange). A shared claim set routes each reply once,
//! however many subscribers see it or its mirror.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peko_channel::{ChannelEvent, ChannelId, ChannelPort, ChannelResponder, PostMsg, RespondCtx};
use peko_subject::{PrincipalId, Subject};
use tracing::warn;

use crate::principal::manager::PrincipalManager;

/// How many characters of the original message the delivered reply
/// quotes, so the asker can tell which question it answers.
const QUOTE_CHARS: usize = 120;

/// Where routed replies go. Trait seam so the router's decisions are
/// testable without a principal manager.
#[async_trait]
pub(crate) trait ReplyDelivery: Send + Sync + 'static {
    /// Whether `author` (a post's author string) is a principal loaded
    /// on this runtime — one whose sessions can receive a reply.
    async fn is_local_principal(&self, author: &str) -> bool;

    /// Record that `principal` is handling reply `reply_id`. `false` if
    /// it already was (another subscriber saw it, or this is the mirror
    /// of a reply already routed).
    fn claim(&self, principal: &PrincipalId, reply_id: &str) -> bool;

    /// Hand `text` to the session at slug path `via` in `principal`'s
    /// tree and wake it.
    async fn deliver(
        &self,
        principal: &PrincipalId,
        via: &str,
        reply_id: &str,
        text: &str,
    ) -> anyhow::Result<()>;

    /// `principal`'s own DM channel with the post author `author`, when
    /// `author` is another principal on this runtime.
    async fn own_dm_channel_with(&self, principal: &PrincipalId, author: &str)
        -> Option<ChannelId>;

    /// The address the asker can answer `author` at (`principal:<did>`,
    /// `user:<id>`), when known.
    async fn address_of(&self, author: &str) -> Option<String>;
}

/// Wraps a channel's responder: replies to a post by a principal on this
/// runtime are routed to the asking session; everything else goes to
/// `inner`.
pub(crate) struct ReplyRouter {
    channel: ChannelId,
    port: Arc<dyn ChannelPort>,
    delivery: Arc<dyn ReplyDelivery>,
    inner: Arc<dyn ChannelResponder>,
}

impl ReplyRouter {
    pub(crate) fn new(
        channel: ChannelId,
        port: Arc<dyn ChannelPort>,
        delivery: Arc<dyn ReplyDelivery>,
        inner: Arc<dyn ChannelResponder>,
    ) -> Self {
        Self {
            channel,
            port,
            delivery,
            inner,
        }
    }

    /// Route `event` if it replies to a post by a principal on this
    /// runtime. `true` when it did (or the reply was already routed).
    async fn route(&self, event: &ChannelEvent) -> bool {
        let ChannelEvent::Posted {
            author,
            text,
            msg_id: Some(reply_id),
            reply_to: Some(asked_id),
            ..
        } = event
        else {
            return false;
        };
        let asked = match self.port.find_post(&self.channel, asked_id).await {
            Ok(Some((_, post))) => post,
            Ok(None) => return false,
            Err(e) => {
                warn!(channel = %self.channel, "reply routing: lookup failed: {e}");
                return false;
            }
        };
        let ChannelEvent::Posted {
            author: asked_author,
            text: asked_text,
            via,
            ..
        } = &asked
        else {
            return false;
        };
        if asked_author == author || !self.delivery.is_local_principal(asked_author).await {
            return false;
        }
        let asker = PrincipalId(asked_author.clone());
        if !self.delivery.claim(&asker, reply_id) {
            return true;
        }

        // A local cross-principal reply lands on the target's channel:
        // mirror it onto the asker's own channel under the same id.
        if let Some(own) = self
            .delivery
            .own_dm_channel_with(&asker, author)
            .await
            .filter(|own| *own != self.channel)
        {
            self.mirror(&own, &asker, author, asked_id, reply_id, text)
                .await;
        }

        let Some(via) = via else {
            // No asking session recorded (a CLI post): the reply stays
            // in the channel record.
            return true;
        };
        let who = self
            .delivery
            .address_of(author)
            .await
            .unwrap_or_else(|| author.clone());
        let message = format!(
            "[peko] Reply from {who} on channel {channel} to your message \"{quote}\":\n\n{text}\n\n\
             To answer, use ChannelSend to {who}.",
            channel = self.channel,
            quote = quote(asked_text),
        );
        if let Err(e) = self.delivery.deliver(&asker, via, reply_id, &message).await {
            warn!(
                channel = %self.channel,
                via = %via,
                "reply routing: could not deliver to the asking session: {e:#}"
            );
        }
        true
    }

    async fn mirror(
        &self,
        own: &ChannelId,
        asker: &PrincipalId,
        author: &str,
        asked_id: &str,
        reply_id: &str,
        text: &str,
    ) {
        let parent = match self.port.find_post(own, asked_id).await {
            Ok(Some((line, _))) => line,
            Ok(None) => return,
            Err(e) => {
                warn!(channel = %own, "reply mirror: lookup failed: {e}");
                return;
            }
        };
        let msg = PostMsg::reply(parent, text).with_msg_id(reply_id);
        if let Err(e) = self
            .port
            .post_attributed(own, &Subject::from(asker), author, msg)
            .await
        {
            warn!(channel = %own, "reply mirror: post failed: {e}");
        }
    }
}

fn quote(text: &str) -> String {
    let mut quoted: String = text.chars().take(QUOTE_CHARS).collect();
    if text.chars().count() > QUOTE_CHARS {
        quoted.push('…');
    }
    quoted.replace('\n', " ")
}

#[async_trait]
impl ChannelResponder for ReplyRouter {
    async fn consider_response(&self, ctx: RespondCtx) -> peko_channel::Result<()> {
        if self.route(&ctx.event).await {
            return Ok(());
        }
        self.inner.consider_response(ctx).await
    }
}

/// Production [`ReplyDelivery`] over the principal manager: resolves the
/// asking session in the principal's tree, queues the reply in the
/// daemon-shared inbox, and fires the completion wake.
pub(crate) struct SessionReplyDelivery {
    principal_manager: Arc<PrincipalManager>,
    port: Arc<dyn ChannelPort>,
    /// `(principal, reply id)` pairs already routed this daemon lifetime
    /// (a restart re-reads nothing: subscriber cursors persist).
    routed: Mutex<HashSet<(PrincipalId, String)>>,
}

impl SessionReplyDelivery {
    pub(crate) fn new(
        principal_manager: Arc<PrincipalManager>,
        port: Arc<dyn ChannelPort>,
    ) -> Self {
        Self {
            principal_manager,
            port,
            routed: Mutex::new(HashSet::new()),
        }
    }
}

#[async_trait]
impl ReplyDelivery for SessionReplyDelivery {
    async fn is_local_principal(&self, author: &str) -> bool {
        self.principal_manager
            .get(PrincipalId(author.to_string()))
            .await
            .is_some()
    }

    fn claim(&self, principal: &PrincipalId, reply_id: &str) -> bool {
        self.routed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((principal.clone(), reply_id.to_string()))
    }

    async fn deliver(
        &self,
        principal: &PrincipalId,
        via: &str,
        reply_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let pm = &self.principal_manager;
        let loaded = pm
            .get(principal.clone())
            .await
            .ok_or_else(|| anyhow::anyhow!("principal {principal} is not loaded"))?;
        let owner = loaded.config.read().await.owner.clone();
        let sessions = crate::principal::child_turns::peer_child_session_manager(
            &loaded,
            &crate::principal::routers::root::default_root_prompt().name,
            &owner,
        );
        let metas = sessions.write().await.list_all_sessions(false).await?;
        // `via` was rendered by `compute_path`; match it the same way, so
        // the lookup works while the trunk session does not exist yet.
        let wanted = format!(
            "{}{}",
            peko_session::path::SCHEME_PREFIX,
            peko_session::path::strip_scheme_prefix(via)
        );
        let session = metas
            .iter()
            .find(|m| peko_session::path::compute_path(&metas, m.session_id) == wanted)
            .map(|m| m.session_id.to_string())
            .ok_or_else(|| anyhow::anyhow!("no session at {via}"))?;
        pm.shared_inbox_registry()
            .get_or_create(&session)
            .await
            .push(peko_session::SteeringMessage::new(text.to_string()).into())
            .await;
        crate::async_exec::executor::wake::notify_completion_wake(
            crate::async_exec::executor::wake::CompletionWakeNotice {
                session_key: session,
                task_id: reply_id.to_string(),
                tool_name: "ChannelSend".to_string(),
                principal_id: Some(principal.to_string()),
                via_steering: true,
            },
        );
        Ok(())
    }

    async fn own_dm_channel_with(
        &self,
        principal: &PrincipalId,
        author: &str,
    ) -> Option<ChannelId> {
        let peer = self
            .principal_manager
            .get(PrincipalId(author.to_string()))
            .await?;
        let slug =
            crate::principal::peer_children::peer_child_slug(&Subject::Principal(peer.did().await))
                .ok()?;
        let channel = crate::principal::peer_dm::find_peer_dm_channel(
            &self.port,
            principal,
            &format!("/{slug}"),
        )
        .await
        .ok()??;
        let creator = self.port.membership(&channel).await.ok()?.creator;
        (creator == principal.to_string()).then_some(channel)
    }

    async fn address_of(&self, author: &str) -> Option<String> {
        if author.starts_with("user:") || author.starts_with("principal:") {
            return Some(author.to_string());
        }
        let peer = self
            .principal_manager
            .get(PrincipalId(author.to_string()))
            .await?;
        Some(format!("principal:{}", peer.did().await.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, CreateOpts};

    #[derive(Default)]
    struct RecordingDelivery {
        routed: Mutex<HashSet<String>>,
        delivered: Mutex<Vec<(String, String)>>,
        own: Mutex<Option<ChannelId>>,
    }

    #[async_trait]
    impl ReplyDelivery for RecordingDelivery {
        async fn is_local_principal(&self, author: &str) -> bool {
            author == "prin_me" || author == "prin_peer"
        }
        fn claim(&self, principal: &PrincipalId, reply_id: &str) -> bool {
            self.routed
                .lock()
                .unwrap()
                .insert(format!("{principal}/{reply_id}"))
        }
        async fn deliver(
            &self,
            principal: &PrincipalId,
            via: &str,
            _reply_id: &str,
            text: &str,
        ) -> anyhow::Result<()> {
            self.delivered
                .lock()
                .unwrap()
                .push((format!("{principal} {via}"), text.to_string()));
            Ok(())
        }
        async fn own_dm_channel_with(
            &self,
            _principal: &PrincipalId,
            _author: &str,
        ) -> Option<ChannelId> {
            self.own.lock().unwrap().clone()
        }
        async fn address_of(&self, author: &str) -> Option<String> {
            Some(format!("principal:did:{author}"))
        }
    }

    #[derive(Default)]
    struct CountingResponder(Mutex<usize>);

    #[async_trait]
    impl ChannelResponder for CountingResponder {
        async fn consider_response(&self, _ctx: RespondCtx) -> peko_channel::Result<()> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        store: Arc<ChannelStore>,
        me: PrincipalId,
        peer: PrincipalId,
        delivery: Arc<RecordingDelivery>,
        inner: Arc<CountingResponder>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ChannelStore::new(ChannelConfig {
                runtime_dir: dir.path().to_path_buf(),
                shared_dir: None,
            }));
            Self {
                _dir: dir,
                store,
                me: PrincipalId("prin_me".into()),
                peer: PrincipalId("prin_peer".into()),
                delivery: Arc::default(),
                inner: Arc::default(),
            }
        }

        async fn channel(&self, creator: &PrincipalId) -> ChannelId {
            let channel = self
                .store
                .create(creator, CreateOpts::runtime("dm"))
                .await
                .unwrap();
            let other = if *creator == self.me {
                &self.peer
            } else {
                &self.me
            };
            self.store
                .invite(&channel, creator, &Subject::from(other))
                .await
                .unwrap();
            channel
        }

        fn router(&self, channel: &ChannelId) -> ReplyRouter {
            ReplyRouter::new(
                channel.clone(),
                Arc::clone(&self.store) as Arc<dyn ChannelPort>,
                Arc::clone(&self.delivery) as Arc<dyn ReplyDelivery>,
                Arc::clone(&self.inner) as Arc<dyn ChannelResponder>,
            )
        }

        async fn post(
            &self,
            channel: &ChannelId,
            by: &PrincipalId,
            msg: PostMsg,
        ) -> (String, ChannelEvent) {
            self.store
                .post_with_event(channel, &Subject::from(by), msg)
                .await
                .unwrap()
        }

        async fn consider(
            &self,
            router: &ReplyRouter,
            channel: &ChannelId,
            line: String,
            event: ChannelEvent,
        ) {
            router
                .consider_response(RespondCtx {
                    channel: channel.clone(),
                    principal: self.me.clone(),
                    event,
                    event_id: line,
                    now: std::time::SystemTime::now(),
                })
                .await
                .unwrap();
        }
    }

    /// A reply to my post goes to the session that posted it, quoting
    /// the question and naming who to answer; it never reaches the
    /// channel's responder.
    #[tokio::test]
    async fn a_reply_to_my_post_goes_to_the_asking_session() {
        let fx = Fixture::new();
        let channel = fx.channel(&fx.me).await;
        let (asked, _) = fx
            .post(
                &channel,
                &fx.me,
                PostMsg::root("what is 2+2?").with_via("sess:/user-a/math"),
            )
            .await;
        let (line, reply) = fx
            .post(&channel, &fx.peer, PostMsg::reply(asked, "4"))
            .await;
        fx.consider(&fx.router(&channel), &channel, line, reply)
            .await;

        let delivered = fx.delivery.delivered.lock().unwrap().clone();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, "prin_me sess:/user-a/math");
        let text = &delivered[0].1;
        assert!(
            text.contains("Reply from principal:did:prin_peer"),
            "{text}"
        );
        assert!(text.contains("\"what is 2+2?\""), "{text}");
        assert!(text.contains("\n\n4\n\n"), "{text}");
        assert_eq!(
            *fx.inner.0.lock().unwrap(),
            0,
            "the responder never sees it"
        );
    }

    /// Root posts, a reply to one's own post, and replies to posts by
    /// principals not on this runtime all pass through to the channel's
    /// responder untouched.
    #[tokio::test]
    async fn everything_else_reaches_the_responder() {
        let fx = Fixture::new();
        let channel = fx.channel(&fx.me).await;
        let router = fx.router(&channel);
        let (theirs, _) = fx.post(&channel, &fx.peer, PostMsg::root("hello")).await;
        let (line, ev) = fx
            .post(&channel, &fx.peer, PostMsg::root("a new topic"))
            .await;
        fx.consider(&router, &channel, line, ev).await;
        let (line, ev) = fx
            .post(
                &channel,
                &fx.peer,
                PostMsg::reply(theirs.clone(), "self-thread"),
            )
            .await;
        fx.consider(&router, &channel, line, ev).await;
        let stranger = PrincipalId("prin_remote".into());
        fx.store
            .invite(&channel, &fx.me, &Subject::from(&stranger))
            .await
            .unwrap();
        let (remote_post, _) = fx
            .post(&channel, &stranger, PostMsg::root("from afar"))
            .await;
        let (line, ev) = fx
            .post(&channel, &fx.peer, PostMsg::reply(remote_post, "answer"))
            .await;
        fx.consider(&router, &channel, line, ev).await;
        assert_eq!(*fx.inner.0.lock().unwrap(), 3);
        assert!(fx.delivery.delivered.lock().unwrap().is_empty());
    }

    /// A local cross-principal reply: the request lives on the peer's
    /// channel and, under the same id, on mine. The PEER's subscriber
    /// (the only one on its channel) routes the reply back to me and
    /// mirrors it onto my channel; my subscriber then sees the mirror and
    /// does not route it again.
    #[tokio::test]
    async fn a_reply_on_the_peers_channel_is_mirrored_and_routed_once() {
        let fx = Fixture::new();
        let mine = fx.channel(&fx.me).await;
        let theirs = fx.channel(&fx.peer).await;
        *fx.delivery.own.lock().unwrap() = Some(mine.clone());
        fx.post(
            &mine,
            &fx.me,
            PostMsg::root("ping")
                .with_via("sess:/user-a")
                .with_msg_id("msg_req"),
        )
        .await;
        let (asked, _) = fx
            .post(
                &theirs,
                &fx.me,
                PostMsg::root("ping")
                    .with_via("sess:/user-a")
                    .with_msg_id("msg_req"),
            )
            .await;
        let (line, reply) = fx
            .post(&theirs, &fx.peer, PostMsg::reply(asked, "pong"))
            .await;
        fx.consider(&fx.router(&theirs), &theirs, line, reply).await;
        assert_eq!(
            fx.delivery.delivered.lock().unwrap()[0].0,
            "prin_me sess:/user-a",
            "delivered to the asker, not the subscriber"
        );

        // Mirrored onto my channel, threaded onto my record of the request.
        let events = fx
            .store
            .peek_with_ids(&mine, &peko_channel::Checkpoint::default())
            .await
            .unwrap();
        let (mirror_line, mirror) = events.last().cloned().unwrap();
        let ChannelEvent::Posted {
            author,
            text,
            reply_to,
            ..
        } = &mirror
        else {
            panic!("{mirror:?}");
        };
        assert_eq!(author, "prin_peer");
        assert_eq!(text, "pong");
        assert_eq!(reply_to.as_deref(), Some("msg_req"));

        // The subscriber on my channel then sees the mirror: no second delivery.
        fx.consider(&fx.router(&mine), &mine, mirror_line, mirror)
            .await;
        assert_eq!(fx.delivery.delivered.lock().unwrap().len(), 1);
    }
}
