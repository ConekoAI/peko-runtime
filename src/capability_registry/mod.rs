//! Capability Registry Client
//!
//! Lightweight client for querying Coneko's capability registry.
//! Pekobot stays minimal — all registry logic lives in Coneko.
//!
//! Usage:
//! ```rust
//! use pekobot::capability_registry::{RegistryClient, RegistryClientConfig};
//!
//! let client = RegistryClient::new(RegistryClientConfig {
//!     endpoint: "http://coneko.local:3000".to_string(),
//!     api_key: None,
//!     timeout_secs: 30,
//! })?;
//!
//! let agents = client.find_agents("scheduling.calendar_read", None).await?;
//! ```

mod client;
pub use client::*;

// Re-export standard capability IDs for convenience
pub mod standard_capabilities {
    /// Communication capabilities
    pub const CHAT_RESPONSE: &str = "communication.chat_response";
    pub const EMAIL_DRAFT: &str = "communication.email_draft";
    pub const EMAIL_SEND: &str = "communication.email_send";
    pub const NOTIFICATION: &str = "communication.notification";

    /// Scheduling capabilities
    pub const CALENDAR_READ: &str = "scheduling.calendar_read";
    pub const CALENDAR_WRITE: &str = "scheduling.calendar_write";
    pub const SCHEDULE_MEETING: &str = "scheduling.schedule_meeting";
    pub const FIND_AVAILABILITY: &str = "scheduling.find_availability";

    /// Document processing
    pub const DOCUMENT_READ: &str = "document.read";
    pub const DOCUMENT_PARSE: &str = "document.parse";
    pub const OCR: &str = "document.ocr";
    pub const GENERATE_REPORT: &str = "document.generate_report";

    /// Social media
    pub const SOCIAL_DRAFT: &str = "social_media.draft_post";
    pub const SOCIAL_PUBLISH: &str = "social_media.publish";
    pub const SOCIAL_SCHEDULE: &str = "social_media.schedule";
    pub const SOCIAL_ANALYTICS: &str = "social_media.analytics";

    /// Data processing
    pub const DATA_ANALYSIS: &str = "data.analysis";
    pub const DATA_EXTRACTION: &str = "data.extraction";
    pub const DATA_TRANSFORMATION: &str = "data.transformation";

    /// Integration
    pub const HTTP_REQUEST: &str = "integration.http_request";
    pub const WEBHOOK_RECEIVE: &str = "integration.webhook_receive";
}
