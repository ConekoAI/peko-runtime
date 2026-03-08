//! Session management module
//!
//! Provides session storage with OpenClaw-compatible JSONL format:
//! - File locking for concurrent access safety
//! - Session index (sessions.json) for fast lookups
//! - Session key derivation for multi-user isolation
//!
//! # Module Structure
//!
//! - `lock`: File locking with timeout and stale detection
//! - `index`: Session index (sessions.json) management
//! - `key`: Session key derivation for scoping
//! - `jsonl`: JSONL storage format (OpenClaw compatible)

pub mod index;
pub mod jsonl;
pub mod key;
pub mod lock;

// Re-export commonly used types
pub use index::{IndexEntry, MaintenanceConfig, MaintenanceMode, MaintenanceReport, SessionIndex};
pub use jsonl::{SessionEntry, SessionStorage};
pub use key::{derive_session_key, parse_session_key, ChatType, SessionContext, SessionScope};
pub use lock::FileLock;
