use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::sync::Mutex;
use tracing::debug;

use peko_fs_persistence::{append_bytes_durable, FileLock};
use peko_subject::{PrincipalDID, Subject};

use super::cursor::{self, CursorError};
use super::types::{ChatLogMessage, ChatLogPage, ChatThreadKey, CHAT_LOG_SCHEMA_VERSION};

const CHAT_LOG_LOCK_TIMEOUT_MS: u64 = 10_000;
const REVERSE_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ChatLogRecord {
    Thread {
        schema_version: u8,
        principal: PrincipalDID,
        peer: Subject,
    },
    Message {
        #[serde(flatten)]
        message: ChatLogMessage,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ChatLogError {
    #[error("chat-log I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("chat-log serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("chat-log lock error: {0}")]
    Lock(String),
    #[error(transparent)]
    Cursor(#[from] CursorError),
    #[error("chat-log shard does not match the requested thread")]
    ThreadMismatch,
    #[error("message sender is not a participant in the requested thread")]
    InvalidSender,
    #[error("unsupported chat-log schema version {0}")]
    UnsupportedVersion(u8),
    #[error("chat-log cursor offset {offset} is invalid for a {file_len}-byte shard")]
    InvalidOffset { offset: u64, file_len: u64 },
}

/// Runtime-owned append-only storage for principal-facing chat messages.
#[derive(Debug)]
pub struct ChatLogStore {
    root: PathBuf,
    shard_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl ChatLogStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            shard_locks: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn append_message(
        &self,
        key: &ChatThreadKey,
        message: &ChatLogMessage,
    ) -> Result<(), ChatLogError> {
        if message.schema_version != CHAT_LOG_SCHEMA_VERSION {
            return Err(ChatLogError::UnsupportedVersion(message.schema_version));
        }
        self.validate_sender(key, &message.sender)?;

        let path = self.shard_path(key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let local_lock = self.shard_lock(&path).await;
        let _local_guard = local_lock.lock().await;
        let _file_lock = FileLock::acquire(&path, CHAT_LOG_LOCK_TIMEOUT_MS)
            .await
            .map_err(|e| ChatLogError::Lock(e.to_string()))?;

        let is_empty = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata.len() == 0,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
        };

        let mut bytes = Vec::new();
        if is_empty {
            let header = ChatLogRecord::Thread {
                schema_version: CHAT_LOG_SCHEMA_VERSION,
                principal: key.principal.clone(),
                peer: key.peer.clone(),
            };
            serde_json::to_writer(&mut bytes, &header)?;
            bytes.push(b'\n');
        } else {
            self.validate_header(&path, key).await?;
        }

        serde_json::to_writer(
            &mut bytes,
            &ChatLogRecord::Message {
                message: message.clone(),
            },
        )?;
        bytes.push(b'\n');
        append_bytes_durable(&path, &bytes).await?;
        Ok(())
    }

    pub async fn read_page(
        &self,
        key: &ChatThreadKey,
        cursor_value: Option<&str>,
        limit: usize,
        since: Option<DateTime<Utc>>,
    ) -> Result<ChatLogPage, ChatLogError> {
        let path = self.shard_path(key);
        if !path.exists() {
            return Ok(ChatLogPage::empty());
        }

        let local_lock = self.shard_lock(&path).await;
        let _local_guard = local_lock.lock().await;
        let _file_lock = FileLock::acquire(&path, CHAT_LOG_LOCK_TIMEOUT_MS)
            .await
            .map_err(|e| ChatLogError::Lock(e.to_string()))?;

        self.validate_header(&path, key).await?;
        let mut file = tokio::fs::File::open(&path).await?;
        let file_len = file.metadata().await?.len();
        let fingerprint = Self::thread_fingerprint(key);
        let before = match cursor_value {
            Some(value) => cursor::decode(value, &fingerprint)?,
            None => file_len,
        };
        self.validate_offset(&mut file, before, file_len).await?;

        let effective_limit = limit.clamp(1, 1000);
        let mut newest_first = self
            .read_reverse(&mut file, before, effective_limit + 1, since)
            .await?;
        let has_more = newest_first.len() > effective_limit;
        if has_more {
            newest_first.truncate(effective_limit);
        }

        let next_cursor = if has_more {
            let oldest_offset = newest_first
                .last()
                .map(|(offset, _)| *offset)
                .unwrap_or(before);
            Some(cursor::encode(&fingerprint, oldest_offset)?)
        } else {
            None
        };
        let messages = newest_first
            .into_iter()
            .rev()
            .map(|(_, message)| message)
            .collect();

        Ok(ChatLogPage {
            messages,
            next_cursor,
            has_more,
        })
    }

    /// Delete the removed principal's own chat-log views.
    pub async fn remove_principal(&self, principal: &PrincipalDID) -> Result<(), ChatLogError> {
        let path = self.root.join(Self::principal_hash(principal));
        match tokio::fs::remove_dir_all(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn validate_sender(&self, key: &ChatThreadKey, sender: &Subject) -> Result<(), ChatLogError> {
        let principal = Subject::Principal(key.principal.clone());
        if sender == &principal || sender == &key.peer {
            Ok(())
        } else {
            Err(ChatLogError::InvalidSender)
        }
    }

    async fn validate_header(
        &self,
        path: &Path,
        expected: &ChatThreadKey,
    ) -> Result<(), ChatLogError> {
        let file = tokio::fs::File::open(path).await?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        match serde_json::from_str::<ChatLogRecord>(line.trim_end())? {
            ChatLogRecord::Thread {
                schema_version,
                principal,
                peer,
            } if schema_version == CHAT_LOG_SCHEMA_VERSION
                && principal == expected.principal
                && peer == expected.peer =>
            {
                Ok(())
            }
            ChatLogRecord::Thread { schema_version, .. }
                if schema_version != CHAT_LOG_SCHEMA_VERSION =>
            {
                Err(ChatLogError::UnsupportedVersion(schema_version))
            }
            _ => Err(ChatLogError::ThreadMismatch),
        }
    }

    async fn validate_offset(
        &self,
        file: &mut tokio::fs::File,
        offset: u64,
        file_len: u64,
    ) -> Result<(), ChatLogError> {
        if offset > file_len {
            return Err(ChatLogError::InvalidOffset { offset, file_len });
        }
        if offset == 0 || offset == file_len {
            return Ok(());
        }

        file.seek(SeekFrom::Start(offset - 1)).await?;
        let mut byte = [0_u8; 1];
        file.read_exact(&mut byte).await?;
        if byte[0] == b'\n' {
            Ok(())
        } else {
            Err(ChatLogError::InvalidOffset { offset, file_len })
        }
    }

    async fn read_reverse(
        &self,
        file: &mut tokio::fs::File,
        before: u64,
        wanted: usize,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<(u64, ChatLogMessage)>, ChatLogError> {
        let mut position = before;
        let mut reversed_line = Vec::new();
        let mut messages = Vec::with_capacity(wanted);
        let mut reached_cutoff = false;

        while position > 0 && messages.len() < wanted && !reached_cutoff {
            let start = position.saturating_sub(REVERSE_READ_CHUNK_BYTES as u64);
            let chunk_len =
                usize::try_from(position - start).map_err(|_| ChatLogError::InvalidOffset {
                    offset: position,
                    file_len: before,
                })?;
            let mut chunk = vec![0_u8; chunk_len];
            file.seek(SeekFrom::Start(start)).await?;
            file.read_exact(&mut chunk).await?;

            for index in (0..chunk.len()).rev() {
                if chunk[index] == b'\n' {
                    if reversed_line.is_empty() {
                        continue;
                    }
                    reversed_line.reverse();
                    let line_start = start + index as u64 + 1;
                    reached_cutoff =
                        Self::push_message_line(&reversed_line, line_start, since, &mut messages);
                    reversed_line.clear();
                    if messages.len() >= wanted || reached_cutoff {
                        break;
                    }
                } else {
                    reversed_line.push(chunk[index]);
                }
            }
            position = start;
        }

        if position == 0 && !reversed_line.is_empty() && messages.len() < wanted && !reached_cutoff
        {
            reversed_line.reverse();
            Self::push_message_line(&reversed_line, 0, since, &mut messages);
        }

        Ok(messages)
    }

    fn push_message_line(
        line: &[u8],
        line_start: u64,
        since: Option<DateTime<Utc>>,
        messages: &mut Vec<(u64, ChatLogMessage)>,
    ) -> bool {
        let record = match serde_json::from_slice::<ChatLogRecord>(line) {
            Ok(record) => record,
            Err(error) => {
                debug!(%error, "skipping malformed chat-log line");
                return false;
            }
        };
        let ChatLogRecord::Message { message } = record else {
            return false;
        };
        if since.is_some_and(|cutoff| message.timestamp < cutoff) {
            return true;
        }
        messages.push((line_start, message));
        false
    }

    async fn shard_lock(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut locks = self.shard_locks.lock().await;
        locks
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn shard_path(&self, key: &ChatThreadKey) -> PathBuf {
        self.root
            .join(Self::principal_hash(&key.principal))
            .join(format!("{}.jsonl", Self::peer_hash(&key.peer)))
    }

    fn principal_hash(principal: &PrincipalDID) -> String {
        blake3::hash(principal.as_str().as_bytes())
            .to_hex()
            .to_string()
    }

    fn peer_hash(peer: &Subject) -> String {
        blake3::hash(peer.to_string().as_bytes())
            .to_hex()
            .to_string()
    }

    fn thread_fingerprint(key: &ChatThreadKey) -> String {
        let canonical = format!("{}\0{}", key.principal, key.peer);
        blake3::hash(canonical.as_bytes()).to_hex().to_string()
    }

    #[cfg(test)]
    fn path_for_test(&self, key: &ChatThreadKey) -> PathBuf {
        self.shard_path(key)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn thread_key(peer: &str) -> ChatThreadKey {
        ChatThreadKey::new(
            PrincipalDID::from("did:peko:principal:alice"),
            Subject::User(peer.to_string()),
        )
    }

    fn message(id: usize) -> ChatLogMessage {
        let mut message = ChatLogMessage::new(
            Subject::User("local".to_string()),
            format!("message-{id}"),
            Some(format!("request-{id}")),
        );
        message.id = format!("chat-{id}");
        message
    }

    #[tokio::test]
    async fn appends_and_pages_from_the_latest_message() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = ChatLogStore::new(temp.path().to_path_buf());
        let key = thread_key("local");
        for id in 1..=5 {
            store.append_message(&key, &message(id)).await.unwrap();
        }

        let latest = store.read_page(&key, None, 2, None).await.unwrap();
        assert_eq!(
            latest
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["message-4", "message-5"]
        );
        assert!(latest.has_more);

        let middle = store
            .read_page(&key, latest.next_cursor.as_deref(), 2, None)
            .await
            .unwrap();
        assert_eq!(
            middle
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["message-2", "message-3"]
        );
        assert!(middle.has_more);

        let oldest = store
            .read_page(&key, middle.next_cursor.as_deref(), 2, None)
            .await
            .unwrap();
        assert_eq!(
            oldest
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["message-1"]
        );
        assert!(!oldest.has_more);
        assert!(oldest.next_cursor.is_none());
    }

    #[tokio::test]
    async fn cursor_remains_stable_after_append_and_is_thread_bound() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = ChatLogStore::new(temp.path().to_path_buf());
        let key = thread_key("local");
        for id in 1..=4 {
            store.append_message(&key, &message(id)).await.unwrap();
        }
        let latest = store.read_page(&key, None, 2, None).await.unwrap();
        store.append_message(&key, &message(5)).await.unwrap();

        let older = store
            .read_page(&key, latest.next_cursor.as_deref(), 2, None)
            .await
            .unwrap();
        assert_eq!(
            older
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            vec!["message-1", "message-2"]
        );

        let wrong_key = thread_key("someone-else");
        assert!(matches!(
            store
                .read_page(&wrong_key, latest.next_cursor.as_deref(), 2, None)
                .await,
            Ok(ChatLogPage { messages, .. }) if messages.is_empty()
        ));
    }

    #[tokio::test]
    async fn skips_a_torn_final_line() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = ChatLogStore::new(temp.path().to_path_buf());
        let key = thread_key("local");
        store.append_message(&key, &message(1)).await.unwrap();

        let path = store.path_for_test(&key);
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .await
            .unwrap();
        file.write_all(br#"{"kind":"message""#).await.unwrap();
        file.sync_all().await.unwrap();

        let page = store.read_page(&key, None, 10, None).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].text, "message-1");
    }

    #[tokio::test]
    async fn concurrent_appends_do_not_interleave_or_drop_messages() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = Arc::new(ChatLogStore::new(temp.path().to_path_buf()));
        let key = thread_key("local");
        let mut tasks = Vec::new();
        for id in 0..20 {
            let store = Arc::clone(&store);
            let key = key.clone();
            tasks.push(tokio::spawn(async move {
                store.append_message(&key, &message(id)).await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let page = store.read_page(&key, None, 100, None).await.unwrap();
        assert_eq!(page.messages.len(), 20);
        let mut ids = page.messages.into_iter().map(|m| m.id).collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 20);
    }

    #[tokio::test]
    async fn rejects_non_participant_senders_and_deletes_principal_views() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = ChatLogStore::new(temp.path().to_path_buf());
        let key = thread_key("local");
        let invalid =
            ChatLogMessage::new(Subject::User("other".to_string()), "not this thread", None);
        assert!(matches!(
            store.append_message(&key, &invalid).await,
            Err(ChatLogError::InvalidSender)
        ));

        store.append_message(&key, &message(1)).await.unwrap();
        assert!(store.path_for_test(&key).exists());
        store.remove_principal(&key.principal).await.unwrap();
        assert!(!store.path_for_test(&key).exists());
    }
}
