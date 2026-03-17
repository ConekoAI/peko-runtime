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
//! ```text
//! FileWatcher ──┐
//! WebhookServer ─┼──► EventRouter ──► AgentManager
//! EventSubscriber─┘
//! ```

pub mod config;
pub mod events;
pub mod external_ingress;
pub mod file_watcher;
pub mod router;
pub mod subscriber;
pub mod webhook;

pub use config::{
    FileWatchConfig, FileWatcherConfig, OrchestrationConfig, OrchestrationConfigBuilder,
    RouterConfig, WebhookConfig, WebhookRouteConfig,
};
pub use events::{FileChangeType, SystemEvent};
pub use external_ingress::{
    ExternalIngress, ExternalIngressBuilder, ExternalIngressConfig, ExternalSource,
    SourceDetection, VerificationConfig,
};
pub use file_watcher::{FileWatcher, FileWatcherBuilder, WatchConfig};
pub use router::{AgentAction, EventRouter};
pub use subscriber::{EventSubscriber, EventSubscriberBuilder};
pub use webhook::{WebhookRoute, WebhookServer, WebhookServerBuilder};
