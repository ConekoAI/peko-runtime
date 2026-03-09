//! Standard Capability Definitions
//!
//! Canonical capability IDs for common agent functions.
//! These are the "well-known" capabilities that agents can advertise.

/// Communication capabilities
pub mod communication {
    /// Respond to chat messages
    pub const CHAT_RESPONSE: &str = "communication.chat_response";
    
    /// Draft email content
    pub const EMAIL_DRAFT: &str = "communication.email_draft";
    
    /// Send emails
    pub const EMAIL_SEND: &str = "communication.email_send";
    
    /// Send notifications
    pub const NOTIFICATION: &str = "communication.notification";
}

/// Scheduling capabilities
pub mod scheduling {
    /// Read calendar events
    pub const CALENDAR_READ: &str = "scheduling.calendar_read";
    
    /// Create/modify calendar events
    pub const CALENDAR_WRITE: &str = "scheduling.calendar_write";
    
    /// Schedule meetings with others
    pub const SCHEDULE_MEETING: &str = "scheduling.schedule_meeting";
    
    /// Find availability across calendars
    pub const FIND_AVAILABILITY: &str = "scheduling.find_availability";
    
    /// Send meeting reminders
    pub const MEETING_REMINDER: &str = "scheduling.meeting_reminder";
}

/// Document processing capabilities
pub mod document {
    /// Extract text from documents
    pub const READ: &str = "document.read";
    
    /// Parse structured data from documents
    pub const PARSE: &str = "document.parse";
    
    /// OCR for scanned images
    pub const OCR: &str = "document.ocr";
    
    /// Generate formatted reports
    pub const GENERATE_REPORT: &str = "document.generate_report";
    
    /// Summarize document content
    pub const SUMMARIZE: &str = "document.summarize";
    
    /// Translate document content
    pub const TRANSLATE: &str = "document.translate";
}

/// Social media capabilities
pub mod social_media {
    /// Draft social media posts
    pub const DRAFT_POST: &str = "social_media.draft_post";
    
    /// Publish posts immediately
    pub const PUBLISH: &str = "social_media.publish";
    
    /// Schedule posts for later
    pub const SCHEDULE: &str = "social_media.schedule";
    
    /// Get engagement analytics
    pub const ANALYTICS: &str = "social_media.analytics";
    
    /// Monitor mentions and replies
    pub const MONITOR: &str = "social_media.monitor";
}

/// Data processing capabilities
pub mod data {
    /// Analyze datasets
    pub const ANALYSIS: &str = "data.analysis";
    
    /// Extract structured data
    pub const EXTRACTION: &str = "data.extraction";
    
    /// Transform data formats
    pub const TRANSFORMATION: &str = "data.transformation";
    
    /// Validate data quality
    pub const VALIDATION: &str = "data.validation";
    
    /// Generate data visualizations
    pub const VISUALIZE: &str = "data.visualize";
}

/// Integration capabilities
pub mod integration {
    /// Make HTTP requests
    pub const HTTP_REQUEST: &str = "integration.http_request";
    
    /// Receive webhooks
    pub const WEBHOOK_RECEIVE: &str = "integration.webhook_receive";
    
    /// Generic API integration
    pub const API_INTEGRATION: &str = "integration.api";
    
    /// Database operations
    pub const DATABASE: &str = "integration.database";
}

/// Automation capabilities
pub mod automation {
    /// Execute scheduled tasks
    pub const SCHEDULED_TASK: &str = "automation.scheduled_task";
    
    /// Trigger workflows
    pub const WORKFLOW_TRIGGER: &str = "automation.workflow_trigger";
    
    /// Conditional logic
    pub const CONDITIONAL_LOGIC: &str = "automation.conditional_logic";
}

/// All standard capabilities as a list
pub const ALL_CAPABILITIES: [&str; 26] = [
    // Communication
    communication::CHAT_RESPONSE,
    communication::EMAIL_DRAFT,
    communication::EMAIL_SEND,
    communication::NOTIFICATION,
    // Scheduling
    scheduling::CALENDAR_READ,
    scheduling::CALENDAR_WRITE,
    scheduling::SCHEDULE_MEETING,
    scheduling::FIND_AVAILABILITY,
    scheduling::MEETING_REMINDER,
    // Document
    document::READ,
    document::PARSE,
    document::OCR,
    document::GENERATE_REPORT,
    document::SUMMARIZE,
    document::TRANSLATE,
    // Social Media
    social_media::DRAFT_POST,
    social_media::PUBLISH,
    social_media::SCHEDULE,
    social_media::ANALYTICS,
    social_media::MONITOR,
    // Data
    data::ANALYSIS,
    data::EXTRACTION,
    data::TRANSFORMATION,
    // Integration
    integration::HTTP_REQUEST,
    integration::WEBHOOK_RECEIVE,
    integration::API_INTEGRATION,
    // Automation
    automation::SCHEDULED_TASK,
];
