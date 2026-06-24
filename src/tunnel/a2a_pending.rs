//! A2A response correlation registry — Slice B of issue #29.
//!
//! When the outbound a2a path sends `TunnelMessage::AgentToAgentRequest`
//! over the tunnel, it parks an `oneshot::Sender` under the request_id
//! and `await`s on the matching `oneshot::Receiver`. The tunnel
//! dispatcher, on receiving the matching `AgentToAgentResponse`, looks
//! up the sender and completes the oneshot so the outbound path
//! unblocks.
//!
//! Why a free-standing registry and not, say, an inline `Arc<Mutex<HashMap>>`
//! inside `A2aSendTool`:
//!
//!  1. The tunnel dispatcher (lives in `tunnel/dispatcher.rs`) needs to
//!     consult the registry on inbound `AgentToAgentResponse`. The
//!     dispatcher and the `A2aSendTool` are constructed independently
//!     by the daemon-state bootstrap; sharing the registry through a
//!     well-typed `Arc<PendingA2aResponses>` is cleaner than digging
//!     through the agent's tool list to find the right map.
//!
//!  2. The TTL / cleanup path (Slice B follow-up, Slice E E2E test)
//!     lives next to the registry, not next to the tool. Stale entries
//!     are a real failure mode — a target runtime that dies mid-call
//!     leaves the caller's oneshot waiting forever otherwise.
//!
//!  3. The "wait for response with timeout" surface
//!     (`PendingA2aResponses::register_and_wait`) is the natural unit
//!     of test: register, fire a response from a stub, assert delivery
//!     — no tunnel client needed.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;

/// The opaque response payload routed back to the caller. Mirrors the
/// `payload` field of `TunnelMessage::AgentToAgentResponse` — the
/// receiver of the oneshot decodes it as an IPC `ResponsePacket` (same
/// codec as `ProxiedResponse`).
pub type A2aResponsePayload = Vec<u8>;

/// In-flight a2a request registry. Maps `request_id` →
/// `oneshot::Sender<A2aResponsePayload>`. Shared between the outbound
/// `A2aSendTool` path (which registers) and the tunnel dispatcher
/// (which completes via [`PendingA2aResponses::complete`]).
///
/// Clone-friendly: holds an `Arc<Mutex<HashMap>>` internally so a
/// single registry can be shared across many call sites by
/// `Arc<PendingA2aResponses>`.
#[derive(Debug, Default)]
pub struct PendingA2aResponses {
    /// `request_id` → response sender. Synchronous `std::sync::Mutex`
    /// is correct here — every operation is a single hash lookup +
    /// insert/remove, no `.await` is ever held across the lock. A
    /// tokio mutex would serialize the entire dispatcher.
    pending: Mutex<HashMap<String, oneshot::Sender<A2aResponsePayload>>>,
}

/// Reason an a2a wait completed without delivering a response. The
/// outbound path uses these to surface structured errors to the
/// calling agent instead of a generic "remote a2a failed" string.
#[derive(Debug, thiserror::Error)]
pub enum A2aWaitError {
    /// The wait exceeded the configured timeout. The most common
    /// production-side mode when the target runtime is busy or the
    /// hub's tunnel router has a backlog.
    #[error("a2a response timed out after {0:?}")]
    Timeout(Duration),
    /// The registry entry was dropped before a response arrived. The
    /// dispatcher does this on disconnect / shutdown
    /// (`cancel_all_for_disconnect`) so callers don't hang past the
    /// tunnel lifecycle.
    #[error("a2a response oneshot was cancelled (caller disconnected or runtime shutting down)")]
    Cancelled,
}

impl PendingA2aResponses {
    /// Construct a fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a oneshot under `request_id` and immediately hand back
    /// the receiver. Slice B's outbound a2a path calls this **before**
    /// sending the `AgentToAgentRequest` over the tunnel — registering
    /// after the send opens a race window where the response arrives
    /// before the receiver is parked.
    ///
    /// Returns an error if `request_id` is already registered. The
    /// outbound path uses UUIDv4 for `request_id` so a collision means
    /// either a logic bug (re-using a stale id) or a (highly improbable)
    /// UUID collision — either way, surface it loudly.
    pub fn register(
        &self,
        request_id: impl Into<String>,
    ) -> Result<oneshot::Receiver<A2aResponsePayload>> {
        let request_id = request_id.into();
        let (tx, rx) = oneshot::channel();
        let mut guard = self
            .pending
            .lock()
            .expect("PendingA2aResponses mutex poisoned");
        if guard.contains_key(&request_id) {
            return Err(anyhow!(
                "a2a request_id `{request_id}` is already registered; \
                 the outbound path must generate a fresh UUID per call"
            ));
        }
        guard.insert(request_id, tx);
        Ok(rx)
    }

    /// Convenience: register + wait for the response with a timeout.
    /// This is what the outbound `A2aSendTool::execute_remote` path
    /// calls, factored out so unit tests can drive the same code path
    /// the production code does.
    ///
    /// # Errors
    ///
    /// - `Timeout` if no response arrives within `timeout`.
    /// - `Cancelled` if the oneshot is dropped before completing
    ///   (dispatcher disconnect; runtime shutdown).
    pub async fn register_and_wait(
        &self,
        request_id: impl Into<String>,
        timeout: Duration,
    ) -> Result<A2aResponsePayload, A2aWaitError> {
        let request_id = request_id.into();
        // `register` only fails on a logic bug (duplicate id); upgrade
        // that to a panic-equivalent so a future caller mis-using
        // this API discovers it during local dev rather than in
        // production where it'd manifest as a silent hang.
        let rx = self
            .register(&request_id)
            .expect("a2a request_id collision — outbound caller did not generate a fresh UUID");
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(_)) => {
                // Receiver dropped — remove the entry so a delayed
                // response doesn't leak. (The dispatcher cleans up on
                // delivery, but if the sender side was cancelled this
                // is the cleanup point.)
                self.discard(&request_id);
                Err(A2aWaitError::Cancelled)
            }
            Err(_) => {
                // Timeout fired. Drop the entry so a late response
                // doesn't try to complete a vanished receiver.
                self.discard(&request_id);
                Err(A2aWaitError::Timeout(timeout))
            }
        }
    }

    /// Complete the oneshot for `request_id` with `payload`. The
    /// tunnel dispatcher's `AgentToAgentResponse` arm calls this.
    ///
    /// Returns `true` if a pending entry was found and completed,
    /// `false` if no entry matched (either the caller already timed
    /// out, or the response is a spurious one from the hub —
    /// dispatcher logs the latter as a warn).
    pub fn complete(&self, request_id: &str, payload: A2aResponsePayload) -> bool {
        let mut guard = self
            .pending
            .lock()
            .expect("PendingA2aResponses mutex poisoned");
        match guard.remove(request_id) {
            Some(tx) => {
                // Send-failure means the receiver was already dropped
                // (timeout/cancel won the race). Still removing the
                // entry is the right thing — there's no point in
                // keeping a key whose sender is consumed.
                let _ = tx.send(payload);
                true
            }
            None => false,
        }
    }

    /// Drop the registry entry for `request_id` without completing
    /// it. Used internally on timeout / cancel to avoid leaks; also
    /// callable by tests to assert the post-failure shape.
    pub fn discard(&self, request_id: &str) {
        self.pending
            .lock()
            .expect("PendingA2aResponses mutex poisoned")
            .remove(request_id);
    }

    /// Cancel every in-flight a2a wait. Called by the daemon shutdown
    /// path and by the tunnel client on disconnect — both moments
    /// where the response will never come and callers need to unblock
    /// with a `Cancelled` error rather than wait out the timeout.
    pub fn cancel_all_for_disconnect(&self) {
        let mut guard = self
            .pending
            .lock()
            .expect("PendingA2aResponses mutex poisoned");
        // Dropping each `oneshot::Sender` makes the matching `Receiver`
        // resolve with `Err(_)`, which `register_and_wait` translates
        // to `A2aWaitError::Cancelled`. No need to send anything.
        guard.clear();
    }

    /// Test-only — count the currently-pending entries.
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .expect("PendingA2aResponses mutex poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Happy path: register a request, complete it, the awaiter sees
    /// the payload.
    #[tokio::test]
    async fn test_register_and_complete_delivers_payload() {
        let registry = PendingA2aResponses::new();

        let rx = registry.register("req-1").unwrap();
        assert_eq!(registry.pending_count(), 1);

        // Complete from another task; the awaiter resolves.
        let completed = registry.complete("req-1", b"hello".to_vec());
        assert!(completed, "complete must return true on a matching id");
        assert_eq!(registry.pending_count(), 0, "entry must be removed");

        let payload = rx.await.unwrap();
        assert_eq!(payload, b"hello");
    }

    /// `register_and_wait` with a generous timeout delivers the
    /// response when the dispatcher completes.
    #[tokio::test]
    async fn test_register_and_wait_returns_payload() {
        let registry = std::sync::Arc::new(PendingA2aResponses::new());
        let registry2 = registry.clone();

        // Spawn a "dispatcher" that completes after a short delay.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            registry2.complete("req-2", b"world".to_vec());
        });

        let payload = registry
            .register_and_wait("req-2", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(payload, b"world");
        assert_eq!(registry.pending_count(), 0);
    }

    /// `register_and_wait` returns `Timeout` and cleans up the entry
    /// when no response arrives in time.
    #[tokio::test]
    async fn test_register_and_wait_times_out_and_cleans_up() {
        let registry = PendingA2aResponses::new();

        let err = registry
            .register_and_wait("req-3", Duration::from_millis(10))
            .await
            .expect_err("expected timeout");
        assert!(
            matches!(err, A2aWaitError::Timeout(_)),
            "expected Timeout, got: {err:?}"
        );
        assert_eq!(registry.pending_count(), 0, "timed-out entry must not leak");
    }

    /// A spurious `complete` (no matching register) returns false and
    /// does not panic.
    #[test]
    fn test_complete_for_unknown_request_id_is_no_op() {
        let registry = PendingA2aResponses::new();
        assert!(!registry.complete("unknown-id", b"hi".to_vec()));
    }

    /// Re-registering the same `request_id` fails loudly. Production
    /// code uses UUIDv4 so collisions are vanishingly improbable —
    /// the error is a tripwire for re-use bugs.
    #[test]
    fn test_duplicate_register_errors() {
        let registry = PendingA2aResponses::new();
        let _rx = registry.register("req-dup").unwrap();
        let err = registry
            .register("req-dup")
            .expect_err("re-register must error");
        assert!(
            err.to_string().contains("already registered"),
            "error must name the condition; got: {err}"
        );
    }

    /// `cancel_all_for_disconnect` unblocks every waiter with
    /// `Cancelled`. This is what the daemon shutdown path needs so
    /// in-flight a2a callers don't hang past tunnel teardown.
    #[tokio::test]
    async fn test_cancel_all_for_disconnect_unblocks_waiters() {
        let registry = std::sync::Arc::new(PendingA2aResponses::new());
        let registry2 = registry.clone();

        let join = tokio::spawn(async move {
            registry2
                .register_and_wait("req-cancel", Duration::from_mins(1))
                .await
        });

        // Give the spawned task time to register.
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(registry.pending_count(), 1);

        registry.cancel_all_for_disconnect();
        assert_eq!(registry.pending_count(), 0);

        let result = join.await.unwrap();
        assert!(
            matches!(result, Err(A2aWaitError::Cancelled)),
            "cancel_all must propagate as Cancelled; got: {result:?}"
        );
    }

    /// `discard` removes an entry without sending anything. Useful
    /// when the outbound path errors out (e.g. signature fails to
    /// compute) between `register` and the tunnel send — we need to
    /// clean up the orphaned entry so the next request with the same
    /// id (if any) doesn't collide.
    #[test]
    fn test_discard_removes_entry_without_send() {
        let registry = PendingA2aResponses::new();
        let _rx = registry.register("req-discard").unwrap();
        assert_eq!(registry.pending_count(), 1);
        registry.discard("req-discard");
        assert_eq!(registry.pending_count(), 0);
        // Subsequent complete is a no-op.
        assert!(!registry.complete("req-discard", b"".to_vec()));
    }
}
