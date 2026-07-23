//! Runtime-owned, append-only chat log storage for principal-facing
//! conversations. Internal agent/session working memory lives in
//! `peko-session`; this crate is the immutable, externally visible
//! record of what participants said to each other.
//!
//! Phase 5 of the post-migration cleanup. Replaces the root
//! `src/chat_log/` directory; historical `peko::chat_log::*` paths
//! are intentionally broken.

mod cursor;
mod store;
mod types;

pub use cursor::{decode as decode_cursor, encode as encode_cursor, CursorError};
pub use store::{ChatLogError, ChatLogStore};
pub use types::{ChatLogMessage, ChatLogPage, ChatThreadKey, CHAT_LOG_SCHEMA_VERSION};
