//! Email Management Tool
//!
//! AI-powered email assistant with inbox summarization, smart replies,
//! and scheduled sending. Supports Gmail and Outlook/Exchange.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Email provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    /// Email provider
    pub provider: EmailProvider,
    /// User email address
    pub email_address: String,
    /// OAuth2 credentials
    pub credentials: EmailCredentials,
    /// Default reply settings
    pub reply_settings: ReplySettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailProvider {
    Gmail,
    Outlook,
    Exchange,
    Imap, // Generic IMAP for other providers
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailCredentials {
    /// Access token (OAuth2)
    pub access_token: String,
    /// Refresh token
    pub refresh_token: Option<String>,
    /// IMAP/SMTP password (for non-OAuth providers)
    pub password: Option<String>,
    /// App-specific password
    pub app_password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplySettings {
    /// Default tone for replies
    pub default_tone: ReplyTone,
    /// Auto-include signature
    pub include_signature: bool,
    /// Signature text
    pub signature: Option<String>,
    /// Auto-draft replies for flagged emails
    pub auto_draft_flagged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyTone {
    Professional,
    Friendly,
    Formal,
    Brief,
}

/// Email message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Email {
    pub id: String,
    pub thread_id: String,
    pub subject: String,
    pub from: EmailAddress,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    pub bcc: Vec<EmailAddress>,
    pub body_text: String,
    pub body_html: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub labels: Vec<String>,
    pub is_read: bool,
    pub is_starred: bool,
    pub attachments: Vec<Attachment>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailAddress {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub content_id: Option<String>,
}

/// Inbox summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSummary {
    pub total_unread: u32,
    pub total_threads: u32,
    pub categories: Vec<EmailCategory>,
    pub urgent_emails: Vec<EmailPreview>,
    pub requires_reply: Vec<EmailPreview>,
    pub newsletters: Vec<EmailPreview>,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailCategory {
    pub name: String,
    pub count: u32,
    pub priority: Priority,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailPreview {
    pub id: String,
    pub subject: String,
    pub from: EmailAddress,
    pub preview_text: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub urgency_score: f32, // 0.0 - 1.0
    pub suggested_action: SuggestedAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
    Ignore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedAction {
    ReplyImmediately,
    ReplyToday,
    ReplyThisWeek,
    Archive,
    Unsubscribe,
    Review,
}

/// Smart reply draft
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartReply {
    pub original_email_id: String,
    pub suggested_subject: String,
    pub draft_body: String,
    pub tone: ReplyTone,
    pub confidence: f32,
    pub key_points_addressed: Vec<String>,
    pub suggested_attachments: Vec<String>,
}

/// Scheduled email
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledEmail {
    pub id: String,
    pub email: Email,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub status: ScheduledStatus,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledStatus {
    Pending,
    Sent,
    Failed,
    Cancelled,
}

/// Email filter/rule
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailFilter {
    pub id: String,
    pub name: String,
    pub conditions: Vec<FilterCondition>,
    pub actions: Vec<FilterAction>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCondition {
    pub field: String, // "from", "subject", "body", "has_attachment"
    pub operator: String, // "contains", "equals", "starts_with"
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterAction {
    Label(String),
    Archive,
    Delete,
    MarkRead,
    Forward(String),
    AutoReply(String),
}

/// Email management tool
pub struct EmailTool {
    config: EmailConfig,
    http_client: reqwest::Client,
}

impl EmailTool {
    /// Create new email tool
    pub fn new(config: EmailConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Get inbox summary
    pub async fn get_inbox_summary(&self) -> anyhow::Result<InboxSummary> {
        // Fetch recent emails
        let emails = self.list_emails(50).await?;
        
        let mut categories: HashMap<String, u32> = HashMap::new();
        let mut urgent = Vec::new();
        let mut requires_reply = Vec::new();
        let mut newsletters = Vec::new();
        let mut unread_count = 0;

        for email in &emails {
            if !email.is_read {
                unread_count += 1;
            }

            // Categorize email
            let category = self.categorize_email(email);
            *categories.entry(category.clone()).or_insert(0) += 1;

            // Check urgency
            let urgency = self.calculate_urgency(email);
            
            let preview = EmailPreview {
                id: email.id.clone(),
                subject: email.subject.clone(),
                from: email.from.clone(),
                preview_text: email.body_text.chars().take(100).collect(),
                timestamp: email.timestamp,
                urgency_score: urgency,
                suggested_action: self.suggest_action(email, urgency),
            };

            if urgency > 0.8 {
                urgent.push(preview.clone());
            }

            if self.requires_reply(email) {
                requires_reply.push(preview.clone());
            }

            if category == "newsletter" || category == "promotional" {
                newsletters.push(preview);
            }
        }

        // Sort by urgency
        urgent.sort_by(|a, b| b.urgency_score.partial_cmp(&a.urgency_score).unwrap());
        requires_reply.sort_by(|a, b| b.urgency_score.partial_cmp(&a.urgency_score).unwrap());

        let category_list: Vec<EmailCategory> = categories
            .into_iter()
            .map(|(name, count)| EmailCategory {
                name,
                count,
                priority: Priority::Medium, // Would determine from content
            })
            .collect();

        Ok(InboxSummary {
            total_unread: unread_count,
            total_threads: emails.len() as u32,
            categories: category_list,
            urgent_emails: urgent.into_iter().take(5).collect(),
            requires_reply: requires_reply.into_iter().take(10).collect(),
            newsletters: newsletters.into_iter().take(5).collect(),
            generated_at: chrono::Utc::now(),
        })
    }

    /// List recent emails
    pub async fn list_emails(&self, max_results: u32) -> anyhow::Result<Vec<Email>> {
        match self.config.provider {
            EmailProvider::Gmail => self.list_gmail_emails(max_results).await,
            EmailProvider::Outlook | EmailProvider::Exchange => {
                self.list_outlook_emails(max_results).await
            }
            EmailProvider::Imap => {
                anyhow::bail!("IMAP provider not yet implemented")
            }
        }
    }

    /// Get single email by ID
    pub async fn get_email(&self, email_id: &str) -> anyhow::Result<Option<Email>> {
        match self.config.provider {
            EmailProvider::Gmail => self.get_gmail_email(email_id).await,
            EmailProvider::Outlook | EmailProvider::Exchange => {
                self.get_outlook_email(email_id).await
            }
            EmailProvider::Imap => Ok(None),
        }
    }

    /// Generate smart reply draft
    pub async fn draft_reply(
        &self,
        email_id: &str,
        tone: Option<ReplyTone>,
    ) -> anyhow::Result<SmartReply> {
        let email = self.get_email(email_id).await?;
        
        if email.is_none() {
            anyhow::bail!("Email not found");
        }

        let original = email.unwrap();
        let tone = tone.unwrap_or(self.config.reply_settings.default_tone.clone());

        // Analyze email content
        let key_points = self.extract_key_points(&original);
        
        // Generate draft based on tone and content
        let draft_body = self.generate_draft_body(&original, &tone, &key_points);
        let subject = if original.subject.to_lowercase().starts_with("re:") {
            original.subject.clone()
        } else {
            format!("Re: {}", original.subject)
        };

        // Add signature if configured
        let final_body = if self.config.reply_settings.include_signature {
            if let Some(ref sig) = self.config.reply_settings.signature {
                format!("{}\n\n--\n{}", draft_body, sig)
            } else {
                draft_body
            }
        } else {
            draft_body
        };

        Ok(SmartReply {
            original_email_id: email_id.to_string(),
            suggested_subject: subject,
            draft_body: final_body,
            tone,
            confidence: 0.85,
            key_points_addressed: key_points,
            suggested_attachments: vec![],
        })
    }

    /// Send email immediately
    pub async fn send_email(&self, email: &Email) -> anyhow::Result<String> {
        match self.config.provider {
            EmailProvider::Gmail => self.send_gmail_email(email).await,
            EmailProvider::Outlook | EmailProvider::Exchange => {
                self.send_outlook_email(email).await
            }
            EmailProvider::Imap => {
                anyhow::bail!("IMAP send not yet implemented")
            }
        }
    }

    /// Schedule email for later delivery
    pub async fn schedule_email(
        &self,
        email: Email,
        send_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<ScheduledEmail> {
        // In production, would store in database and have cron job process
        let scheduled = ScheduledEmail {
            id: format!("sch_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()),
            email,
            scheduled_at: send_at,
            status: ScheduledStatus::Pending,
            timezone: "UTC".to_string(),
        };

        Ok(scheduled)
    }

    /// Cancel scheduled email
    pub async fn cancel_scheduled(&self, scheduled_id: &str) -> anyhow::Result<bool> {
        // In production, would update database
        Ok(true)
    }

    /// Mark email as read
    pub async fn mark_read(&self, email_id: &str) -> anyhow::Result<()> {
        match self.config.provider {
            EmailProvider::Gmail => self.modify_gmail_label(email_id, "UNREAD", false).await,
            _ => Ok(()),
        }
    }

    /// Archive email
    pub async fn archive_email(&self, email_id: &str) -> anyhow::Result<()> {
        match self.config.provider {
            EmailProvider::Gmail => {
                self.modify_gmail_label(email_id, "INBOX", false).await
            }
            _ => Ok(()),
        }
    }

    /// Create email filter
    pub async fn create_filter(&self, filter: EmailFilter) -> anyhow::Result<()> {
        // In production, would create via provider API
        Ok(())
    }

    // Helper methods

    fn categorize_email(&self, email: &Email) -> String {
        let subject_lower = email.subject.to_lowercase();
        let from_lower = email.from.email.to_lowercase();

        if subject_lower.contains("unsubscribe") 
            || from_lower.contains("newsletter")
            || from_lower.contains("noreply")
            || subject_lower.contains("digest") {
            return "newsletter".to_string();
        }

        if subject_lower.contains("re:") || subject_lower.contains("fw:") {
            return "conversation".to_string();
        }

        if from_lower.contains("support") 
            || from_lower.contains("help")
            || subject_lower.contains("ticket") {
            return "support".to_string();
        }

        if subject_lower.contains("invoice")
            || subject_lower.contains("payment")
            || subject_lower.contains("receipt") {
            return "financial".to_string();
        }

        if subject_lower.contains("meeting")
            || subject_lower.contains("calendar")
            || subject_lower.contains("invitation") {
            return "calendar".to_string();
        }

        "general".to_string()
    }

    fn calculate_urgency(&self, email: &Email) -> f32 {
        let mut score = 0.0;

        // Check subject for urgency markers
        let subject_lower = email.subject.to_lowercase();
        if subject_lower.contains("urgent") || subject_lower.contains("asap") {
            score += 0.4;
        }
        if subject_lower.contains("deadline") || subject_lower.contains("due") {
            score += 0.3;
        }

        // Check for action words
        if subject_lower.contains("action required") || subject_lower.contains("needs your") {
            score += 0.3;
        }

        // Starred emails are likely important
        if email.is_starred {
            score += 0.2;
        }

        // Recent emails more urgent
        let hours_old = (chrono::Utc::now() - email.timestamp).num_hours();
        if hours_old < 2 {
            score += 0.2;
        } else if hours_old < 24 {
            score += 0.1;
        }

        score.min(1.0)
    }

    fn suggest_action(&self, email: &Email, urgency: f32) -> SuggestedAction {
        let category = self.categorize_email(email);

        if category == "newsletter" || category == "promotional" {
            return SuggestedAction::Archive;
        }

        if urgency > 0.8 {
            SuggestedAction::ReplyImmediately
        } else if urgency > 0.5 {
            SuggestedAction::ReplyToday
        } else if self.requires_reply(email) {
            SuggestedAction::ReplyThisWeek
        } else {
            SuggestedAction::Review
        }
    }

    fn requires_reply(&self, email: &Email) -> bool {
        let subject_lower = email.subject.to_lowercase();
        
        // Check if email asks questions
        if email.body_text.contains("?") {
            return true;
        }

        // Check for request words
        let request_words = ["please", "could you", "would you", "can you", "need you to"];
        let body_lower = email.body_text.to_lowercase();
        for word in &request_words {
            if body_lower.contains(word) {
                return true;
            }
        }

        // Check subject for reply indicators
        if subject_lower.contains("?") 
            || subject_lower.contains("request")
            || subject_lower.contains("approval") {
            return true;
        }

        false
    }

    fn extract_key_points(&self, email: &Email) -> Vec<String> {
        let mut points = Vec::new();
        let body_lower = email.body_text.to_lowercase();

        // Extract questions
        for sentence in email.body_text.split(['.', '!', '?']) {
            if sentence.contains('?') {
                points.push(format!("Question: {}", sentence.trim()));
            }
        }

        // Extract deadlines
        if body_lower.contains("by ") || body_lower.contains("deadline") {
            points.push("Has deadline mentioned".to_string());
        }

        // Extract action items
        if body_lower.contains("please") || body_lower.contains("need you") {
            points.push("Contains request/action item".to_string());
        }

        points
    }

    fn generate_draft_body(
        &self,
        original: &Email,
        tone: &ReplyTone,
        key_points: &[String],
    ) -> String {
        let greeting = if let Some(ref name) = original.from.name {
            format!("Hi {},", name.split_whitespace().next().unwrap_or(name))
        } else {
            "Hi,".to_string()
        };

        let closing = match tone {
            ReplyTone::Formal => "Best regards,",
            ReplyTone::Professional => "Best,",
            ReplyTone::Friendly => "Thanks!",
            ReplyTone::Brief => "-",
        };

        // Simple response template
        let body = match tone {
            ReplyTone::Brief => {
                "Got it, thanks for letting me know.".to_string()
            }
            _ => {
                if key_points.iter().any(|p| p.contains("Question")) {
                    format!(
                        "{}\n\nThanks for your email. I've reviewed your questions and will get back to you with answers shortly.\n\n{}",
                        greeting, closing
                    )
                } else if key_points.iter().any(|p| p.contains("request")) {
                    format!(
                        "{}\n\nThanks for reaching out. I'll take care of this and update you soon.\n\n{}",
                        greeting, closing
                    )
                } else {
                    format!(
                        "{}\n\nThanks for your email. I've received it and will respond if needed.\n\n{}",
                        greeting, closing
                    )
                }
            }
        };

        body
    }

    // Gmail API implementations
    async fn list_gmail_emails(&self, max_results: u32) -> anyhow::Result<Vec<Email>> {
        let url = format!(
            "https://www.googleapis.com/gmail/v1/users/me/messages?maxResults={}",
            max_results
        );

        let response = self.http_client
            .get(&url)
            .bearer_auth(&self.config.credentials.access_token)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Gmail API error: {}", response.status());
        }

        // Mock data for now
        Ok(vec![
            self.create_mock_email("urgent", "Project deadline moved up"),
            self.create_mock_email("meeting", "Team sync tomorrow at 10am"),
            self.create_mock_email("newsletter", "Weekly Tech Digest"),
            self.create_mock_email("financial", "Invoice #12345 Payment Received"),
        ])
    }

    async fn get_gmail_email(&self, email_id: &str) -> anyhow::Result<Option<Email>> {
        let url = format!(
            "https://www.googleapis.com/gmail/v1/users/me/messages/{}",
            email_id
        );

        let response = self.http_client
            .get(&url)
            .bearer_auth(&self.config.credentials.access_token)
            .send()
            .await?;

        if response.status() == 404 {
            return Ok(None);
        }

        if !response.status().is_success() {
            anyhow::bail!("Gmail API error: {}", response.status());
        }

        // Return mock for now
        Ok(Some(self.create_mock_email("general", "Test Email")))
    }

    async fn send_gmail_email(&self, _email: &Email) -> anyhow::Result<String> {
        // Would call Gmail API send endpoint
        Ok(format!("msg_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()))
    }

    async fn modify_gmail_label(
        &self,
        _email_id: &str,
        _label: &str,
        _add: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    // Outlook API implementations
    async fn list_outlook_emails(&self, _max_results: u32) -> anyhow::Result<Vec<Email>> {
        // Would call Microsoft Graph API
        Ok(vec![
            self.create_mock_email("urgent", "Action required: Budget approval"),
            self.create_mock_email("calendar", "Meeting invitation: Q4 Review"),
        ])
    }

    async fn get_outlook_email(&self, _email_id: &str) -> anyhow::Result<Option<Email>> {
        Ok(Some(self.create_mock_email("general", "Outlook Test Email")))
    }

    async fn send_outlook_email(&self, _email: &Email) -> anyhow::Result<String> {
        Ok(format!("msg_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()))
    }

    // Helper to create mock emails
    fn create_mock_email(&self, category: &str, subject: &str) -> Email {
        let urgency_keywords = vec!["urgent", "deadline", "action required", "asap"];
        let is_urgent = urgency_keywords.iter().any(|k| subject.to_lowercase().contains(k));

        Email {
            id: format!("msg_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()),
            thread_id: format!("thread_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()),
            subject: subject.to_string(),
            from: EmailAddress {
                name: Some("John Doe".to_string()),
                email: "john@example.com".to_string(),
            },
            to: vec![EmailAddress {
                name: Some(self.config.email_address.clone()),
                email: self.config.email_address.clone(),
            }],
            cc: vec![],
            bcc: vec![],
            body_text: format!("This is a sample {} email body for testing purposes.", category),
            body_html: None,
            timestamp: chrono::Utc::now() - chrono::Duration::hours(if is_urgent { 1 } else { 24 }),
            labels: vec![category.to_string(), "inbox".to_string()],
            is_read: !is_urgent,
            is_starred: is_urgent,
            attachments: vec![],
            in_reply_to: None,
            references: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_email_tool_creation() {
        let config = EmailConfig {
            provider: EmailProvider::Gmail,
            email_address: "test@example.com".to_string(),
            credentials: EmailCredentials {
                access_token: "test_token".to_string(),
                refresh_token: None,
                password: None,
                app_password: None,
            },
            reply_settings: ReplySettings {
                default_tone: ReplyTone::Professional,
                include_signature: true,
                signature: Some("Test Signature".to_string()),
                auto_draft_flagged: true,
            },
        };

        let tool = EmailTool::new(config);
        assert!(tool.is_ok());
    }
}
