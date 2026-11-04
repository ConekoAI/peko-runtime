//! Attachments cross runtimes (ADR-094). A child module of
//! `tunnel_channel_port` so it shares the port's store and tunnel context.
//!
//! A post that crosses runtimes names each file by its SHA-256 digest;
//! the bytes stay in the sending runtime's channel file store
//! (`<channel>/files/<digest>`). The receiving runtime pulls them, one
//! chunk at a time, from the runtime the post came from:
//!
//! - [`TunnelChannelPort::fetch_attachments`] asks for each missing file
//!   (`tunnel_attachment_request`), writes the chunks to a `.part` file,
//!   checks the whole against the digest the signed post carried, and
//!   only then names it by its digest. A file it cannot get is kept on a
//!   wanted list and asked for again when the sender is back.
//! - [`TunnelChannelPort::answer_attachment_request`] serves a chunk to a
//!   runtime that hosts a member of the channel, from the channel's own
//!   store only. Every refusal reads as "not here".
//!
//! One chunk is in flight per file, so PekoHub never relays more than a
//! chunk per transfer at once; it also refuses larger chunks and caps the
//! bytes a runtime may relay per minute.

use super::TunnelChannelPort;
use crate::principal::attachments::{self, Limits};
use crate::tunnel::{
    sign_history, AttachmentRequestPayload, TunnelHandle, TunnelMessage, ATTACHMENT_REQUEST_VERSION,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use peko_channel::ChannelId;
use peko_protocol::channel::Attachment;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tracing::{debug, warn};

/// Most bytes in one chunk. PekoHub drops a chunk whose data is larger.
pub const CHUNK_BYTES: usize = 256 * 1024;
/// How long one chunk may take to arrive.
const CHUNK_WAIT: Duration = Duration::from_secs(30);
/// Pauses before asking again for a chunk that did not come, or that the
/// sender did not have yet. One more try than pauses.
const RETRY_PAUSES: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
];
/// Files on the wanted list are given up after this long.
const WANTED_FOR: chrono::Duration = chrono::Duration::days(30);

/// A chunk request waiting for its answer.
pub(super) struct PendingChunk {
    channel: String,
    from: String,
    sha256: String,
    offset: u64,
    reply: tokio::sync::oneshot::Sender<Chunk>,
}

/// One answer: the whole file's size and this piece's bytes. Empty data
/// with size 0: the file is not there.
struct Chunk {
    size: u64,
    data: Vec<u8>,
}

/// A file this runtime could not get yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Wanted {
    channel: String,
    sha256: String,
    size: u64,
    from: String,
    since: chrono::DateTime<chrono::Utc>,
}

impl TunnelChannelPort {
    fn wanted_path(&self) -> PathBuf {
        self.local
            .config()
            .runtime_dir
            .join("channel_delivery")
            .join("wanted_files.json")
    }

    async fn load_wanted(&self) -> Vec<Wanted> {
        match tokio::fs::read(self.wanted_path()).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn save_wanted(&self, wanted: &[Wanted]) {
        let path = self.wanted_path();
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        let Ok(bytes) = serde_json::to_vec_pretty(wanted) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if tokio::fs::write(&tmp, bytes).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, &path).await;
        }
    }

    /// Add or drop one wanted entry.
    async fn mark_wanted(&self, entry: Wanted, want: bool) {
        let _guard = self.wanted_lock.lock().await;
        let mut wanted = self.load_wanted().await;
        let had = wanted.len();
        wanted.retain(|w| !(w.channel == entry.channel && w.sha256 == entry.sha256));
        if want {
            wanted.push(entry);
        } else if wanted.len() == had {
            return;
        }
        self.save_wanted(&wanted).await;
    }

    /// The receiving side (ADR-094 D3): get every file of a post that came
    /// from `from_runtime` and is not here yet. A post whose files break
    /// the remote limits is not fetched at all. Returns the attachments
    /// whose bytes are now here.
    pub(crate) async fn fetch_attachments(
        &self,
        channel: &ChannelId,
        files: &[Attachment],
        from_runtime: &str,
    ) -> Vec<Attachment> {
        let crossing: Vec<&Attachment> = files.iter().filter(|a| a.sha256.is_some()).collect();
        if crossing.is_empty() {
            return Vec::new();
        }
        let owned: Vec<Attachment> = crossing.iter().map(|a| (*a).clone()).collect();
        if let Err(e) = attachments::check(&owned, Limits::REMOTE) {
            warn!(%channel, "remote post's files are over the limits; not fetched: {e}");
            return Vec::new();
        }
        let mut here = Vec::new();
        for attachment in crossing {
            match self.fetch_one(channel, attachment, from_runtime).await {
                Ok(_) => here.push(attachment.clone()),
                Err(e) => {
                    warn!(%channel, name = %attachment.name, "attachment not received: {e}");
                }
            }
        }
        here
    }

    /// Fetch one file into the channel's store; its path there.
    async fn fetch_one(
        &self,
        channel: &ChannelId,
        attachment: &Attachment,
        from_runtime: &str,
    ) -> Result<PathBuf, String> {
        let digest = attachment
            .sha256
            .clone()
            .filter(|d| attachments::is_digest(d))
            .ok_or("the post names no valid digest")?;
        let files_dir = self
            .local
            .files_dir(channel)
            .await
            .map_err(|e| e.to_string())?;
        let dest = files_dir.join(&digest);
        if dest.is_file() {
            return Ok(dest);
        }
        let key = format!("{}/{digest}", channel.as_str());
        let lock = Arc::clone(
            self.attachment_locks
                .lock()
                .expect("attachment locks")
                .entry(key.clone())
                .or_default(),
        );
        let result = {
            let _held = lock.lock().await;
            if dest.is_file() {
                Ok(dest.clone())
            } else {
                self.download(channel, &files_dir, &digest, attachment.size, from_runtime)
                    .await
                    .map(|()| dest.clone())
            }
        };
        self.attachment_locks
            .lock()
            .expect("attachment locks")
            .remove(&key);
        let entry = Wanted {
            channel: channel.as_str().to_string(),
            sha256: digest,
            size: attachment.size,
            from: from_runtime.to_string(),
            since: chrono::Utc::now(),
        };
        self.mark_wanted(entry, result.is_err()).await;
        result
    }

    /// Pull `digest` chunk by chunk, check it, and name it by its digest.
    async fn download(
        &self,
        channel: &ChannelId,
        files_dir: &Path,
        digest: &str,
        size: u64,
        from_runtime: &str,
    ) -> Result<(), String> {
        use sha2::{Digest, Sha256};
        if size > attachments::REMOTE_MAX_FILE_BYTES {
            return Err(format!("the file is {size} bytes, over the limit"));
        }
        tokio::fs::create_dir_all(files_dir)
            .await
            .map_err(|e| format!("cannot create the file store: {e}"))?;
        let part = files_dir.join(format!("{digest}.part"));
        let mut file = tokio::fs::File::create(&part)
            .await
            .map_err(|e| format!("cannot write {}: {e}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut offset = 0u64;
        let outcome = async {
            loop {
                let chunk = self
                    .ask_chunk_with_retries(channel, digest, offset, size, from_runtime)
                    .await?;
                if chunk.size != size {
                    return Err(format!(
                        "the sender has {} bytes; the post said {size}",
                        chunk.size
                    ));
                }
                let end = offset + chunk.data.len() as u64;
                if end > size {
                    return Err("the sender sent more than the file holds".to_string());
                }
                file.write_all(&chunk.data)
                    .await
                    .map_err(|e| format!("cannot write the file: {e}"))?;
                hasher.update(&chunk.data);
                offset = end;
                if offset == size {
                    break;
                }
            }
            file.flush()
                .await
                .map_err(|e| format!("cannot write the file: {e}"))?;
            if hex::encode(hasher.finalize()) != digest {
                return Err("the bytes do not match the post's digest".to_string());
            }
            Ok(())
        }
        .await;
        drop(file);
        match outcome {
            Ok(()) => tokio::fs::rename(&part, files_dir.join(digest))
                .await
                .map_err(|e| format!("cannot store the file: {e}")),
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    /// Ask for the chunk at `offset`, again after a pause when none came
    /// or the sender did not have the file yet.
    async fn ask_chunk_with_retries(
        &self,
        channel: &ChannelId,
        digest: &str,
        offset: u64,
        size: u64,
        from_runtime: &str,
    ) -> Result<Chunk, String> {
        let mut last = String::new();
        for attempt in 0..=RETRY_PAUSES.len() {
            if attempt > 0 {
                tokio::time::sleep(RETRY_PAUSES[attempt - 1]).await;
            }
            match self.ask_chunk(channel, digest, offset, from_runtime).await {
                // An empty chunk short of the end: not there (yet).
                Ok(chunk) if chunk.data.is_empty() && offset < size => {
                    last = "the sender does not have the file".into();
                }
                Ok(chunk) => return Ok(chunk),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// One request, one answer.
    async fn ask_chunk(
        &self,
        channel: &ChannelId,
        digest: &str,
        offset: u64,
        from_runtime: &str,
    ) -> Result<Chunk, String> {
        let ctx = self
            .ctx
            .read()
            .await
            .clone()
            .ok_or("this runtime has no PekoHub connection")?;
        let handle = ctx
            .tunnel
            .read()
            .await
            .clone()
            .ok_or("not connected to PekoHub")?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = AttachmentRequestPayload {
            v: ATTACHMENT_REQUEST_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: from_runtime.to_string(),
            channel_id: channel.as_str().to_string(),
            sha256: digest.to_string(),
            offset,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.attachment_pending
            .lock()
            .expect("attachment pending")
            .insert(
                request_id.clone(),
                PendingChunk {
                    channel: payload.channel_id.clone(),
                    from: from_runtime.to_string(),
                    sha256: digest.to_string(),
                    offset,
                    reply,
                },
            );
        let forget = || {
            self.attachment_pending
                .lock()
                .expect("attachment pending")
                .remove(&request_id);
        };
        if let Err(e) = handle.send(TunnelMessage::TunnelAttachmentRequest {
            request_id: request_id.clone(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            channel_id: payload.channel_id,
            sha256: payload.sha256,
            offset,
            signature,
        }) {
            forget();
            return Err(format!("not connected to PekoHub: {e}"));
        }
        match tokio::time::timeout(CHUNK_WAIT, answer).await {
            Ok(Ok(chunk)) => Ok(chunk),
            _ => {
                forget();
                Err("the sender's computer did not answer; it may be offline".into())
            }
        }
    }

    /// A chunk arrived: accept it only as the answer to a request of
    /// ours, from the runtime asked, for the file and offset asked.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_attachment_chunk(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        channel_id: String,
        sha256: String,
        offset: u64,
        size: u64,
        data: String,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let waiting = {
            let mut pending = self.attachment_pending.lock().expect("attachment pending");
            let matches = pending.get(&request_id).is_some_and(|p| {
                p.from == source_runtime_id
                    && p.channel == channel_id
                    && p.sha256 == sha256
                    && p.offset == offset
            });
            if matches {
                pending.remove(&request_id)
            } else {
                None
            }
        };
        let Some(waiting) = waiting else {
            debug!(request_id, "unrequested attachment chunk dropped");
            return;
        };
        let Ok(bytes) = STANDARD.decode(data.as_bytes()) else {
            warn!(request_id, "attachment chunk is not base64; dropped");
            return;
        };
        if bytes.len() > CHUNK_BYTES {
            warn!(request_id, "attachment chunk over the chunk size; dropped");
            return;
        }
        let _ = waiting.reply.send(Chunk { size, data: bytes });
    }

    /// The sending side: answer a verified request with the chunk at its
    /// offset — only to a runtime that hosts a member of the channel, and
    /// only from the channel's own store. Anything else answers "not
    /// here", so a probe learns nothing.
    pub(crate) async fn answer_attachment_request(
        &self,
        handle: &TunnelHandle,
        request: &AttachmentRequestPayload,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let (size, data) = match self.read_chunk(request).await {
            Some((size, bytes)) => (size, STANDARD.encode(bytes)),
            None => (0, String::new()),
        };
        let _ = handle.send(TunnelMessage::TunnelAttachmentChunk {
            request_id: request.request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: request.source_runtime_id.clone(),
            channel_id: request.channel_id.clone(),
            sha256: request.sha256.clone(),
            offset: request.offset,
            size,
            data,
        });
    }

    /// The gate and the read behind [`Self::answer_attachment_request`].
    async fn read_chunk(&self, request: &AttachmentRequestPayload) -> Option<(u64, Vec<u8>)> {
        let channel = ChannelId::parse(&request.channel_id)?;
        let hosts_member = self
            .local
            .list_remote_members(&channel)
            .await
            .ok()?
            .iter()
            .any(|m| m.runtime_id == request.source_runtime_id);
        if !hosts_member {
            debug!(request_id = %request.request_id, "attachment request from a runtime outside the channel");
            return None;
        }
        let files_dir = self.local.files_dir(&channel).await.ok()?;
        let path = attachments::stored(&files_dir, &request.sha256)?;
        let mut file = tokio::fs::File::open(&path).await.ok()?;
        let size = file.metadata().await.ok()?.len();
        if size > attachments::REMOTE_MAX_FILE_BYTES || request.offset > size {
            return None;
        }
        file.seek(std::io::SeekFrom::Start(request.offset))
            .await
            .ok()?;
        let want = usize::try_from(size - request.offset)
            .unwrap_or(CHUNK_BYTES)
            .min(CHUNK_BYTES);
        let mut bytes = vec![0u8; want];
        file.read_exact(&mut bytes).await.ok()?;
        Some((size, bytes))
    }

    /// Ask again for the files this runtime is still waiting for — all of
    /// them, or only those `from_runtime` holds (it just came back).
    /// Entries older than [`WANTED_FOR`] are given up.
    pub(crate) async fn retry_wanted(self: &Arc<Self>, from_runtime: Option<&str>) {
        let wanted = {
            let _guard = self.wanted_lock.lock().await;
            let mut wanted = self.load_wanted().await;
            let before = wanted.len();
            let cutoff = chrono::Utc::now() - WANTED_FOR;
            wanted.retain(|w| w.since > cutoff);
            if wanted.len() != before {
                self.save_wanted(&wanted).await;
            }
            wanted
        };
        for entry in wanted {
            if from_runtime.is_some_and(|r| r != entry.from) {
                continue;
            }
            let Some(channel) = ChannelId::parse(&entry.channel) else {
                continue;
            };
            let port = Arc::clone(self);
            tokio::spawn(async move {
                let attachment = Attachment {
                    name: String::new(),
                    path: String::new(),
                    mime: String::new(),
                    size: entry.size,
                    sha256: Some(entry.sha256.clone()),
                };
                if port
                    .fetch_one(&channel, &attachment, &entry.from)
                    .await
                    .is_ok()
                {
                    debug!(%channel, sha256 = %entry.sha256, "wanted attachment received");
                }
            });
        }
    }
}

/// Per-file fetch locks (one transfer of a file at a time).
pub(super) type AttachmentLocks = std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>;

impl TunnelChannelPort {
    /// The lock inbound posts of `channel` apply under (ADR-094).
    pub(crate) fn apply_lock(&self, channel: &ChannelId) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.apply_locks
                .lock()
                .expect("apply locks")
                .entry(channel.as_str().to_string())
                .or_default(),
        )
    }

    /// Whether every file `event` names by digest is in `channel`'s store.
    pub(crate) async fn has_all_files(
        &self,
        channel: &ChannelId,
        event: &peko_protocol::channel::ChannelEvent,
    ) -> bool {
        let peko_protocol::channel::ChannelEvent::Posted { attachments, .. } = event else {
            return true;
        };
        if attachments.iter().all(|a| a.sha256.is_none()) {
            return true;
        }
        let Ok(files_dir) = self.local.files_dir(channel).await else {
            return true;
        };
        attachments.iter().all(|a| {
            a.sha256
                .as_deref()
                .and_then(|d| attachments::stored(&files_dir, d))
                .is_none_or(|p| p.is_file())
        })
    }
}
