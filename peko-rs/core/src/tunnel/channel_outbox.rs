//! Durable cross-runtime channel delivery: an outbox of envelopes not yet
//! confirmed by their recipient runtime, and a record of envelopes this
//! runtime has already applied.
//!
//! Channel events and invites travel source runtime → hub → recipient
//! runtime with no response channel, and the hub drops anything addressed
//! to a runtime that is not connected. So the source keeps every envelope
//! here until the recipient confirms it with a receipt
//! (`ChannelEvent::Received`, carried by the same relay), resending on a
//! backoff and whenever the tunnel reconnects. A resend reuses the
//! envelope's `request_id` but is signed afresh (signatures expire after
//! five minutes), and the recipient applies each `request_id` once
//! ([`ReceivedLog`]). The hub stays a stateless relay.
//!
//! On disk (under the channel runtime dir):
//! - `channel_delivery/outbox/<seq>-<request_id>.json` — one file per
//!   pending entry; the zero-padded sequence keeps send order across
//!   restarts.
//! - `channel_delivery/received.jsonl` — `{request_id, at}` per applied
//!   envelope, pruned of entries older than [`KEEP_FOR_DAYS`] on load.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use peko_channel::ChannelId;
use peko_protocol::channel::ChannelEvent;
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify};
use tracing::warn;

/// How long an undelivered envelope is kept, and how long an applied
/// `request_id` is remembered.
pub(crate) const KEEP_FOR_DAYS: i64 = 30;

/// Longest wait between resends of one entry.
const MAX_BACKOFF_SECS: i64 = 3600;
/// First resend delay; doubles per attempt up to [`MAX_BACKOFF_SECS`].
const BASE_BACKOFF_SECS: i64 = 30;

/// What to (re)build and sign on each attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OutboxItem {
    /// A channel event; `source` is the authoring subject's wire string
    /// (`Subject` `Display` form), re-parsed to load the author key.
    Event {
        channel: ChannelId,
        event: ChannelEvent,
        source: String,
    },
    /// A DM invite (`TunnelChannelPort::fanout_dm_invite`).
    Invite {
        channel: ChannelId,
        inviter: PrincipalId,
        creator_did: String,
        invitee: PrincipalId,
    },
}

/// One envelope awaiting its recipient's receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OutboxEntry {
    pub seq: u64,
    pub request_id: String,
    pub recipient_runtime_id: String,
    pub item: OutboxItem,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_attempt: Option<DateTime<Utc>>,
}

impl OutboxEntry {
    /// The stable id of the message this entry carries, if it is a post.
    pub(crate) fn msg_id(&self) -> Option<&str> {
        match &self.item {
            OutboxItem::Event {
                event: ChannelEvent::Posted { msg_id, .. },
                ..
            } => msg_id.as_deref(),
            _ => None,
        }
    }

    /// Whether this entry should be (re)sent now.
    fn due(&self, now: DateTime<Utc>) -> bool {
        match self.last_attempt {
            None => true,
            Some(last) => {
                let shift = self.attempts.saturating_sub(1).min(16);
                let backoff = (BASE_BACKOFF_SECS << shift).min(MAX_BACKOFF_SECS);
                now >= last + ChronoDuration::seconds(backoff)
            }
        }
    }
}

/// The pending envelopes of this runtime.
pub(crate) struct ChannelOutbox {
    dir: PathBuf,
    state: Mutex<Option<BTreeMap<u64, OutboxEntry>>>,
    /// Signalled whenever entries are confirmed.
    confirmed: Notify,
}

impl ChannelOutbox {
    /// Keep pending message identity while following a peer's new host.
    pub(crate) async fn retarget(
        &self,
        channel: &ChannelId,
        old: &str,
        new: &str,
    ) -> anyhow::Result<()> {
        let mut guard = self.state.lock().await;
        let entries = self.loaded(&mut guard).await;
        for entry in entries
            .values_mut()
            .filter(|e| e.recipient_runtime_id == old)
        {
            let target = match &entry.item {
                OutboxItem::Event { channel, .. } | OutboxItem::Invite { channel, .. } => channel,
            };
            if target != channel {
                continue;
            }
            entry.recipient_runtime_id = new.into();
            // Bootstrap the new mirror first; pending posts become due next tick.
            entry.last_attempt = Some(Utc::now());
            self.write(entry).await?;
        }
        Ok(())
    }
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: Mutex::new(None),
            confirmed: Notify::new(),
        }
    }

    fn file_for(&self, entry: &OutboxEntry) -> PathBuf {
        self.dir
            .join(format!("{:020}-{}.json", entry.seq, entry.request_id))
    }

    /// The loaded entries (reading the directory on first use), with
    /// expired ones dropped.
    async fn loaded<'a>(
        &self,
        guard: &'a mut Option<BTreeMap<u64, OutboxEntry>>,
    ) -> &'a mut BTreeMap<u64, OutboxEntry> {
        if guard.is_none() {
            *guard = Some(load_entries(&self.dir).await);
        }
        let entries = guard.as_mut().expect("just loaded");
        let cutoff = Utc::now() - ChronoDuration::days(KEEP_FOR_DAYS);
        let expired: Vec<u64> = entries
            .iter()
            .filter(|(_, e)| e.created_at < cutoff)
            .map(|(seq, _)| *seq)
            .collect();
        for seq in expired {
            if let Some(entry) = entries.remove(&seq) {
                warn!(
                    request_id = %entry.request_id,
                    recipient = %entry.recipient_runtime_id,
                    "channel outbox: giving up on an envelope undelivered for {KEEP_FOR_DAYS} days"
                );
                let _ = tokio::fs::remove_file(self.file_for(&entry)).await;
            }
        }
        entries
    }

    /// Persist a new entry for `recipient` and return it.
    pub(crate) async fn enqueue(
        &self,
        recipient_runtime_id: &str,
        item: OutboxItem,
    ) -> anyhow::Result<OutboxEntry> {
        let mut guard = self.state.lock().await;
        let entries = self.loaded(&mut guard).await;
        let seq = entries.keys().next_back().map_or(0, |last| last + 1);
        let entry = OutboxEntry {
            seq,
            request_id: uuid::Uuid::new_v4().to_string(),
            recipient_runtime_id: recipient_runtime_id.to_string(),
            item,
            created_at: Utc::now(),
            attempts: 0,
            last_attempt: None,
        };
        self.write(&entry).await?;
        entries.insert(seq, entry.clone());
        Ok(entry)
    }

    async fn write(&self, entry: &OutboxEntry) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.file_for(entry);
        let tmp = path.with_extension("json.tmp");
        tokio::fs::write(&tmp, serde_json::to_vec(entry)?).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    /// Entries due for a (re)send, oldest first. `force` ignores backoff
    /// (the tunnel just reconnected).
    pub(crate) async fn due(&self, force: bool) -> Vec<OutboxEntry> {
        let now = Utc::now();
        let mut guard = self.state.lock().await;
        self.loaded(&mut guard)
            .await
            .values()
            .filter(|e| force || e.due(now))
            .cloned()
            .collect()
    }

    /// Record a send attempt.
    pub(crate) async fn attempted(&self, seq: u64) {
        let mut guard = self.state.lock().await;
        let entries = self.loaded(&mut guard).await;
        if let Some(entry) = entries.get_mut(&seq) {
            entry.attempts = entry.attempts.saturating_add(1);
            entry.last_attempt = Some(Utc::now());
            let snapshot = entry.clone();
            drop(guard);
            if let Err(e) = self.write(&snapshot).await {
                warn!("channel outbox: could not record a send attempt: {e}");
            }
        }
    }

    /// Drop an entry that can never be sent (its envelope cannot be built).
    pub(crate) async fn discard(&self, seq: u64) {
        let mut guard = self.state.lock().await;
        if let Some(entry) = self.loaded(&mut guard).await.remove(&seq) {
            let _ = tokio::fs::remove_file(self.file_for(&entry)).await;
        }
    }

    /// `recipient` confirmed `request_ids`: forget them.
    pub(crate) async fn confirm(&self, recipient_runtime_id: &str, request_ids: &[String]) {
        let mut guard = self.state.lock().await;
        let entries = self.loaded(&mut guard).await;
        let done: Vec<u64> = entries
            .iter()
            .filter(|(_, e)| {
                e.recipient_runtime_id == recipient_runtime_id
                    && request_ids.contains(&e.request_id)
            })
            .map(|(seq, _)| *seq)
            .collect();
        for seq in &done {
            if let Some(entry) = entries.remove(seq) {
                let _ = tokio::fs::remove_file(self.file_for(&entry)).await;
            }
        }
        drop(guard);
        if !done.is_empty() {
            self.confirmed.notify_waiters();
        }
    }

    /// Whether any entry carrying message `msg_id` is still unconfirmed.
    pub(crate) async fn pending_msg(&self, msg_id: &str) -> bool {
        let mut guard = self.state.lock().await;
        self.loaded(&mut guard)
            .await
            .values()
            .any(|e| e.msg_id() == Some(msg_id))
    }

    /// Wait until every recipient confirmed message `msg_id`, or
    /// `timeout` passes. `true` when confirmed.
    pub(crate) async fn wait_confirmed(&self, msg_id: &str, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.confirmed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.pending_msg(msg_id).await {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return !self.pending_msg(msg_id).await;
            }
        }
    }
}

async fn load_entries(dir: &Path) -> BTreeMap<u64, OutboxEntry> {
    let mut out = BTreeMap::new();
    let Ok(mut read) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(item)) = read.next_entry().await {
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match tokio::fs::read(&path)
            .await
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<OutboxEntry>(&bytes)?))
        {
            Ok(entry) => {
                out.insert(entry.seq, entry);
            }
            Err(e) => {
                warn!(file = %path.display(), "channel outbox: unreadable entry skipped: {e}")
            }
        }
    }
    out
}

/// The `request_id`s of inbound envelopes this runtime has applied, so a
/// resend is confirmed again but not applied twice.
pub(crate) struct ReceivedLog {
    path: PathBuf,
    seen: Mutex<Option<HashMap<String, DateTime<Utc>>>>,
}

#[derive(Serialize, Deserialize)]
struct ReceivedLine {
    request_id: String,
    at: DateTime<Utc>,
}

impl ReceivedLog {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            seen: Mutex::new(None),
        }
    }

    async fn loaded<'a>(
        &self,
        guard: &'a mut Option<HashMap<String, DateTime<Utc>>>,
    ) -> &'a mut HashMap<String, DateTime<Utc>> {
        if guard.is_none() {
            let cutoff = Utc::now() - ChronoDuration::days(KEEP_FOR_DAYS);
            let mut seen = HashMap::new();
            if let Ok(text) = tokio::fs::read_to_string(&self.path).await {
                for line in text.lines() {
                    if let Ok(row) = serde_json::from_str::<ReceivedLine>(line) {
                        if row.at >= cutoff {
                            seen.insert(row.request_id, row.at);
                        }
                    }
                }
                // Rewrite without the pruned rows.
                let kept: String = seen
                    .iter()
                    .filter_map(|(request_id, at)| {
                        serde_json::to_string(&ReceivedLine {
                            request_id: request_id.clone(),
                            at: *at,
                        })
                        .ok()
                    })
                    .map(|line| line + "\n")
                    .collect();
                let _ = tokio::fs::write(&self.path, kept).await;
            }
            *guard = Some(seen);
        }
        guard.as_mut().expect("just loaded")
    }

    /// Whether `request_id` was already applied.
    pub(crate) async fn contains(&self, request_id: &str) -> bool {
        let mut guard = self.seen.lock().await;
        self.loaded(&mut guard).await.contains_key(request_id)
    }

    /// Record `request_id` as applied.
    pub(crate) async fn record(&self, request_id: &str) {
        let mut guard = self.seen.lock().await;
        let at = Utc::now();
        self.loaded(&mut guard)
            .await
            .insert(request_id.to_string(), at);
        drop(guard);
        let line = match serde_json::to_string(&ReceivedLine {
            request_id: request_id.to_string(),
            at,
        }) {
            Ok(line) => line + "\n",
            Err(_) => return,
        };
        if let Some(parent) = self.path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        use tokio::io::AsyncWriteExt;
        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
        {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    warn!("channel delivery: could not record a received envelope: {e}");
                }
            }
            Err(e) => warn!("channel delivery: could not record a received envelope: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn retarget_preserves_pending_message_identity_across_restart() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let outbox = ChannelOutbox::new(dir.path().join("outbox"));
        let first = outbox.enqueue("old", posted("msg_stable")).await.unwrap();
        outbox
            .retarget(&ChannelId("chan_test0001".into()), "old", "new")
            .await
            .unwrap();
        let reloaded = ChannelOutbox::new(dir.path().join("outbox"));
        let entries = reloaded.due(true).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].request_id, first.request_id);
        assert_eq!(entries[0].recipient_runtime_id, "new");
        assert_eq!(entries[0].msg_id(), Some("msg_stable"));
    }
    use super::*;

    fn posted(msg_id: &str) -> OutboxItem {
        OutboxItem::Event {
            channel: ChannelId("chan_test0001".into()),
            event: ChannelEvent::Posted {
                channel: ChannelId("chan_test0001".into()),
                author: "prin_a".into(),
                parent: None,
                text: "hi".into(),
                at: "2026-10-12T00:00:00Z".into(),
                via: None,
                msg_id: Some(msg_id.into()),
                reply_to: None,
            },
            source: "prin_a".into(),
        }
    }

    /// Entries survive a restart in send order, are due once, back off
    /// after an attempt, and leave only when their recipient confirms.
    #[tokio::test]
    async fn entries_persist_back_off_and_leave_on_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = ChannelOutbox::new(dir.path().join("outbox"));
        let first = outbox.enqueue("did:key:r1", posted("msg_1")).await.unwrap();
        let second = outbox.enqueue("did:key:r1", posted("msg_2")).await.unwrap();
        assert!(first.seq < second.seq);

        let reloaded = ChannelOutbox::new(dir.path().join("outbox"));
        let due: Vec<String> = reloaded
            .due(false)
            .await
            .into_iter()
            .map(|e| e.request_id)
            .collect();
        assert_eq!(due, [first.request_id.clone(), second.request_id.clone()]);

        reloaded.attempted(first.seq).await;
        let due: Vec<u64> = reloaded
            .due(false)
            .await
            .into_iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(due, [second.seq], "an attempted entry backs off");
        assert_eq!(
            reloaded.due(true).await.len(),
            2,
            "a reconnect resends everything"
        );

        // Another runtime's receipt for the same id changes nothing.
        reloaded
            .confirm("did:key:other", std::slice::from_ref(&first.request_id))
            .await;
        assert!(reloaded.pending_msg("msg_1").await);
        reloaded
            .confirm("did:key:r1", std::slice::from_ref(&first.request_id))
            .await;
        assert!(!reloaded.pending_msg("msg_1").await);
        assert!(reloaded.pending_msg("msg_2").await);
        let again = ChannelOutbox::new(dir.path().join("outbox"));
        assert_eq!(
            again.due(true).await.len(),
            1,
            "confirmed entries are gone on disk"
        );
    }

    #[tokio::test]
    async fn waiting_for_confirmation_ends_on_receipt_or_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = std::sync::Arc::new(ChannelOutbox::new(dir.path().join("outbox")));
        let entry = outbox.enqueue("did:key:r1", posted("msg_1")).await.unwrap();
        assert!(
            !outbox
                .wait_confirmed("msg_1", std::time::Duration::from_millis(50))
                .await
        );
        let confirmer = std::sync::Arc::clone(&outbox);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            confirmer.confirm("did:key:r1", &[entry.request_id]).await;
        });
        assert!(
            outbox
                .wait_confirmed("msg_1", std::time::Duration::from_secs(5))
                .await
        );
    }

    #[tokio::test]
    async fn the_received_log_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("received.jsonl");
        let log = ReceivedLog::new(path.clone());
        assert!(!log.contains("req-1").await);
        log.record("req-1").await;
        assert!(log.contains("req-1").await);
        assert!(ReceivedLog::new(path).contains("req-1").await);
    }
}
