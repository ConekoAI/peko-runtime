//! `channel` domain request handler (PR-2c).
//!
//! Owns the channel IPC variants: `ChannelCreate`, `ChannelInvite`,
//! `ChannelPost`, `ChannelPeek`, `ChannelMembers`, `ChannelList`,
//! `ChannelConfigGet`. The handler holds a narrow [`ChannelHost`]
//! port; the daemon-side implementation (`AppState`) is reached only
//! through the trait so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules (mirroring the rest of the F6/F7 handler family):
//! - Dependency inversion: the consumer (`ipc::handlers::channel`)
//!   defines the [`ChannelHost`] trait; the producer (`daemon::state`)
//!   implements it.
//! - This module must not import any other `ipc::handlers::*` module.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{ChannelCliRouter, ChannelId, ChannelPort};

use crate::common::paths::PathResolver;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use peko_auth::caller::CallerContext;
use peko_plan::PrincipalId;

/// Narrow port the `channel` handler uses to reach daemon state.
///
/// `AppState` is the sole production implementor. `channel_port`
/// wraps `PlanChannelAdapter::new(ChannelConfig { runtime_dir })` in
/// production; tests substitute an in-memory fixture.
///
/// `principal_manager` is optional. The default impl returns
/// `None` — when unset, every principal-bearing variant returns
/// `ResponsePacket::Error { "not loaded" }`. Production hosts that
/// have a `PrincipalManager` override the method to return `Some`.
pub(crate) trait ChannelHost: Send + Sync {
    /// Typed path resolver. The handler doesn't currently use it
    /// (PR-2 lets `ChannelCliRouter` route through the port), but the
    /// trait shape mirrors `CronHost` for forward compatibility.
    fn path_resolver(&self) -> PathResolver;

    /// Channel port for all `ChannelPort` operations.
    fn channel_port(&self) -> Arc<dyn ChannelPort>;

    /// Principal manager (optional — defaults to `None`). When
    /// `None`, principal-name resolution fails for every variant
    /// that carries one.
    fn principal_manager(&self) -> Option<&Arc<crate::principal::manager::PrincipalManager>> {
        None
    }
}

/// `channel` domain request handler. Constructed with an `Arc<dyn
/// ChannelHost>` (typically `Arc::new(app_state.clone())` from the
/// dispatcher).
pub(crate) struct ChannelHandler {
    host: Arc<dyn ChannelHost>,
}

impl ChannelHandler {
    pub(crate) fn new(host: Arc<dyn ChannelHost>) -> Self {
        Self { host }
    }

    /// Build the thin CLI router that wraps the host's port. PR-2
    /// delegates to `ChannelCliRouter` (PR-1's pure-port handler
    /// surface) — no logic duplication.
    fn router(&self) -> ChannelCliRouter {
        ChannelCliRouter::new(self.host.channel_port())
    }

    /// Helper: convert a principal name (string) into a
    /// `PrincipalId`. When the host has no `PrincipalManager`
    /// (test paths), returns `None` so the caller emits a clean
    /// `ResponsePacket::Error`. Production hosts always provide a
    /// manager.
    fn resolve_principal(&self, name: &str) -> Option<PrincipalId> {
        let pm = self.host.principal_manager()?;
        let name_owned = name.to_string();
        tokio::task::block_in_place(|| {
            let runtime = tokio::runtime::Handle::current();
            runtime.block_on(async {
                pm.get_by_name(&name_owned).await.map(|p| p.id.clone())
            })
        })
    }
}

#[async_trait]
impl RequestHandler for ChannelHandler {
    fn domain(&self) -> &'static str {
        "channel"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::ChannelCreate { .. }
                | RequestPacket::ChannelInvite { .. }
                | RequestPacket::ChannelPost { .. }
                | RequestPacket::ChannelPeek { .. }
                | RequestPacket::ChannelMembers { .. }
                | RequestPacket::ChannelList { .. }
                | RequestPacket::ChannelConfigGet { .. }
                | RequestPacket::ChannelLeave { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ChannelCreate {
                request_id,
                creator_name,
                name,
            } => {
                let Some(creator) = self.resolve_principal(&creator_name) else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Principal '{creator_name}' is not loaded"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                };
                match self.router().handle_create(&creator, &name).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelCreated {
                            request_id,
                            channel: resp.channel,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel create failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelInvite {
                request_id,
                channel,
                inviter_name,
                invitee_name,
            } => {
                let inviter = match self.resolve_principal(&inviter_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Inviter principal '{inviter_name}' is not loaded"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let invitee = match self.resolve_principal(&invitee_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Invitee principal '{invitee_name}' is not loaded"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_invite(&ch, &inviter, &invitee).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelInvited {
                            request_id,
                            channel: resp.channel,
                            invitee: resp.invitee,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel invite failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelPost {
                request_id,
                channel,
                sender_name,
                text,
                parent,
            } => {
                let sender = match self.resolve_principal(&sender_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Sender principal '{sender_name}' is not loaded"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self
                    .router()
                    .handle_post(&ch, &sender, &text, parent)
                    .await
                {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelPosted {
                            request_id,
                            channel: resp.channel,
                            task_id: resp.task_id,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel post failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelPeek {
                request_id,
                channel,
                since,
            } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_peek(&ch, since).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelPeekResult {
                            request_id,
                            channel: resp.channel,
                            events: resp.events,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel peek failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelMembers { request_id, channel } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_members(&ch).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelMembersResult {
                            request_id,
                            channel: resp.channel,
                            members: resp.members,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel members failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelList {
                request_id,
                principal_name,
            } => {
                let Some(principal) = self.resolve_principal(&principal_name) else {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: format!("Principal '{principal_name}' is not loaded"),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                };
                match self.router().handle_list(&principal).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelListResult {
                            request_id,
                            principal: resp.principal,
                            channels: resp.channels,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel list failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelLeave {
                request_id,
                channel,
                principal_name,
            } => {
                let principal = match self.resolve_principal(&principal_name) {
                    Some(p) => p,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!(
                                "Principal '{principal_name}' is not loaded"
                            ),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_leave(&ch, &principal).await {
                    Ok(resp) => {
                        let response = ResponsePacket::ChannelLeft {
                            request_id,
                            channel: resp.channel,
                            principal: resp.principal,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel leave failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            RequestPacket::ChannelConfigGet { request_id, channel } => {
                let ch = match ChannelId::parse(&channel) {
                    Some(id) => id,
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("invalid ChannelId: {channel}"),
                        };
                        send_response(sink, response).await?;
                        return Ok(());
                    }
                };
                match self.router().handle_config_get(&ch).await {
                    Ok(config) => {
                        let response = ResponsePacket::ChannelConfigResult {
                            request_id,
                            channel: ch,
                            config,
                        };
                        send_response(sink, response).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("channel config get failed: {e}"),
                        };
                        send_response(sink, response).await?;
                    }
                }
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ChannelHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::port::{CreateOpts, PostMsg};
    use peko_channel::ChannelConfig;
    use peko_channel::PlanChannelAdapter;
    use peko_channel::ConfigOnDisk;
    use peko_protocol::channel::ChannelEvent;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// TestChannelHost backed by a real `PlanChannelAdapter`. Doesn't
    /// provide a `PrincipalManager` — every principal-bearing variant
    /// returns `ResponsePacket::Error`. The PM-free tests below
    /// exercise peek / config_get paths; the PM-needed tests use
    /// direct `ChannelCliRouter` invocation.
    struct TestChannelHost {
        path_resolver: PathResolver,
        port: Arc<dyn ChannelPort>,
    }

    impl ChannelHost for TestChannelHost {
        fn path_resolver(&self) -> PathResolver {
            self.path_resolver.clone()
        }
        fn channel_port(&self) -> Arc<dyn ChannelPort> {
            self.port.clone()
        }
        // Default `principal_manager` returns None — happy paths
        // don't need it.
    }

    /// CaptureSink records every response packet for assertions.
    /// Deserializes the bytes emitted by `send_response` so we keep the
    /// same wire shape the production sink sees.
    struct CaptureSink(Arc<Mutex<Vec<ResponsePacket>>>);
    #[async_trait]
    impl crate::ipc::response_sink::ResponseSink for CaptureSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet: ResponsePacket = serde_json::from_slice(bytes)
                .map_err(|e| std::io::Error::other(format!("decode ResponsePacket: {e}")))?;
            self.0.lock().unwrap().push(packet);
            Ok(())
        }
    }

    fn test_host() -> (TempDir, Arc<TestChannelHost>) {
        let tmp = TempDir::new().expect("tempdir");
        // The host's port and `seed_channel`'s adapter must share
        // the same on-disk root — production routes through
        // `PathResolver::runtime_dir()`, so the test mirrors that.
        let runtime_dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).expect("mkdir runtime");
        let cfg = ChannelConfig {
            runtime_dir: runtime_dir.clone(),
        };
        let adapter = Arc::new(PlanChannelAdapter::new(cfg));
        let port: Arc<dyn ChannelPort> = adapter;
        let resolver = PathResolver::with_dirs(
            tmp.path().to_path_buf(),
            tmp.path().to_path_buf(),
            tmp.path().join("cache"),
        );
        let host = Arc::new(TestChannelHost {
            path_resolver: resolver,
            port,
        });
        (tmp, host)
    }

    /// Create a channel + post a message via a fresh adapter rooted
    /// in the host's runtime_dir. Returns the channel id; the seeded
    /// event is visible to the host's port (same on-disk layout).
    async fn seed_channel(host: &TestChannelHost) -> ChannelId {
        let runtime_dir = host.path_resolver.runtime_dir();
        let adapter = PlanChannelAdapter::new(ChannelConfig { runtime_dir });
        let creator = PrincipalId::generate();
        let ch = adapter
            .create(&creator, CreateOpts::runtime("seed"))
            .await
            .expect("create");
        adapter
            .post(&ch, &creator, PostMsg::root("hi"))
            .await
            .expect("post");
        ch
    }

    #[tokio::test]
    async fn handler_matches_claims_only_channel_variants() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);

        // Channel variants are claimed.
        assert!(handler.matches(&RequestPacket::ChannelCreate {
            request_id: 1,
            creator_name: "p".into(),
            name: "n".into(),
        }));
        assert!(handler.matches(&RequestPacket::ChannelPeek {
            request_id: 1,
            channel: "chan_x".into(),
            since: None,
        }));
        assert!(handler.matches(&RequestPacket::ChannelLeave {
            request_id: 1,
            channel: "chan_x".into(),
            principal_name: "p".into(),
        }));

        // Non-channel variants are NOT claimed.
        assert!(!handler.matches(&RequestPacket::Ping { request_id: 1 }));
        assert!(!handler.matches(&RequestPacket::CronList {
            request_id: 1,
            include_disabled: false,
            principal: None,
        }));
    }

    #[tokio::test]
    async fn handler_create_returns_error_when_principal_not_loaded() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);

        let req = RequestPacket::ChannelCreate {
            request_id: 7,
            creator_name: "ghost".into(),
            name: "n".into(),
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink =
            &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ResponsePacket::Error { request_id, message } => {
                assert_eq!(*request_id, 7);
                assert!(
                    message.contains("not loaded"),
                    "got {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_peek_returns_events_for_seeded_channel() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let ch = seed_channel(&host).await;

        let req = RequestPacket::ChannelPeek {
            request_id: 1,
            channel: ch.to_string(),
            since: None,
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink =
            &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1, "expected one response packet");
        match &captured[0] {
            ResponsePacket::ChannelPeekResult { events, .. } => {
                assert_eq!(
                    events.len(),
                    2,
                    "expected 2 events (created + posted), got {events:?}"
                );
                assert!(matches!(events[0], ChannelEvent::Created { .. }));
                assert!(matches!(events[1], ChannelEvent::Posted { .. }));
            }
            other => panic!("expected ChannelPeekResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handler_config_get_returns_default_for_fresh_channel() {
        let (tmp, host) = test_host();
        let _ = tmp;
        let handler = ChannelHandler::new(host.clone());
        let ch = seed_channel(&host).await;

        let req = RequestPacket::ChannelConfigGet {
            request_id: 1,
            channel: ch.to_string(),
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink =
            &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ResponsePacket::ChannelConfigResult { config, .. } => {
                assert_eq!(*config, ConfigOnDisk::default());
            }
            other => panic!("expected ChannelConfigResult, got {other:?}"),
        }
    }

    // Round-trip JSON encode/decode sanity check.
    #[test]
    fn channel_packets_round_trip_via_json() {
        let req = RequestPacket::ChannelPost {
            request_id: 42,
            channel: "chan_abcdefgh".into(),
            sender_name: "prin_alice".into(),
            text: "hello".into(),
            parent: Some("node_xyz".into()),
        };
        let json = serde_json::to_string(&req).expect("encode");
        assert!(json.contains("\"channel_post\""), "got {json}");
        let decoded: RequestPacket = serde_json::from_str(&json).expect("decode");
        assert!(matches!(
            decoded,
            RequestPacket::ChannelPost { request_id: 42, .. }
        ));

        let resp = ResponsePacket::ChannelPosted {
            request_id: 42,
            channel: ChannelId::parse("chan_abcdefgh").expect("valid ChannelId"),
            task_id: "node_qwerty".into(),
        };
        let json = serde_json::to_string(&resp).expect("encode");
        assert!(json.contains("\"channel_posted\""), "got {json}");
        let decoded: ResponsePacket = serde_json::from_str(&json).expect("decode");
        assert!(matches!(
            decoded,
            ResponsePacket::ChannelPosted { request_id: 42, .. }
        ));
    }

    // PR-3a: `ChannelLeave` IPC variant — when the principal manager
    // is missing, the arm should emit a clean `ResponsePacket::Error`
    // naming the unloaded principal rather than panicking. The
    // happy-path dispatch lives in `ChannelCliRouter::handle_leave`
    // (covered by `peko-channel` lib tests) — IPC only adds the
    // dispatcher arm.
    #[tokio::test]
    async fn handler_leave_returns_error_when_principal_not_loaded() {
        let (_tmp, host) = test_host();
        let handler = ChannelHandler::new(host);

        let req = RequestPacket::ChannelLeave {
            request_id: 11,
            channel: "chan_abcdefgh".into(),
            principal_name: "ghost".into(),
        };
        let captured = Arc::new(Mutex::new(Vec::<ResponsePacket>::new()));
        let sink: &dyn crate::ipc::response_sink::ResponseSink =
            &CaptureSink(captured.clone());
        handler
            .handle(
                req,
                &peko_auth::caller::CallerContext::local(),
                sink,
                &PeerAddr::Ip("127.0.0.1:0".parse().expect("loopback addr")),
            )
            .await
            .expect("handle");
        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        match &captured[0] {
            ResponsePacket::Error { request_id, message } => {
                assert_eq!(*request_id, 11);
                assert!(
                    message.contains("'ghost'") && message.contains("not loaded"),
                    "got {message}"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // PR-3a: `ChannelLeave` request/response round-trip.
    #[test]
    fn channel_packets_round_trip_leave_via_json() {
        let req = RequestPacket::ChannelLeave {
            request_id: 7,
            channel: "chan_abcdefgh".into(),
            principal_name: "prin_alice".into(),
        };
        let json = serde_json::to_string(&req).expect("encode");
        assert!(json.contains("\"channel_leave\""), "got {json}");
        let decoded: RequestPacket = serde_json::from_str(&json).expect("decode");
        match decoded {
            RequestPacket::ChannelLeave {
                request_id,
                channel,
                principal_name,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(channel, "chan_abcdefgh");
                assert_eq!(principal_name, "prin_alice");
            }
            other => panic!("expected ChannelLeave, got {other:?}"),
        }

        let resp = ResponsePacket::ChannelLeft {
            request_id: 7,
            channel: ChannelId::parse("chan_abcdefgh").expect("valid ChannelId"),
            principal: peko_subject::PrincipalId::generate(),
        };
        let json = serde_json::to_string(&resp).expect("encode");
        assert!(json.contains("\"channel_left\""), "got {json}");
        let decoded: ResponsePacket = serde_json::from_str(&json).expect("decode");
        assert!(matches!(
            decoded,
            ResponsePacket::ChannelLeft { request_id: 7, .. }
        ));
    }
}