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
pub mod router;

pub use events::{FileChangeType, SystemEvent};
pub use router::{AgentAction, EventRouter};
