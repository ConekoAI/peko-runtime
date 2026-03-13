//! Orchestration Layer
//!
//! The orchestration layer proactively invokes agents based on system events:
//! - File system changes
//! - Webhook deliveries
//! - Internal system events
//! - Timer/scheduled events
//!
//! # Architecture
//!
//! ```
//! FileWatcher ──┐
//! WebhookServer ─┼──► EventRouter ──► AgentManager
//! EventSubscriber─┘
//! ```

pub mod events;
pub mod file_watcher;
pub mod router;
pub mod webhook;

pub use events::{FileChangeType, SystemEvent};
pub use file_watcher::{FileWatcher, FileWatcherBuilder, WatchConfig};
pub use router::{AgentAction, EventRouter};
pub use webhook::{WebhookRoute, WebhookServer, WebhookServerBuilder};
