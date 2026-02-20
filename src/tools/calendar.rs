//! Calendar tool for scheduling and managing events
//!
//! Supports Google Calendar and Outlook/Exchange integration.
//! Handles `OAuth2` authentication and token refresh.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::Tool;

/// Calendar provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarProvider {
    Google,
    Outlook,
}

impl std::str::FromStr for CalendarProvider {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "google" | "gcal" => Ok(CalendarProvider::Google),
            "outlook" | "exchange" | "microsoft" => Ok(CalendarProvider::Outlook),
            _ => Err(anyhow::anyhow!("Unknown calendar provider: {s}")),
        }
    }
}

/// `OAuth2` credentials for calendar access
#[derive(Debug, Clone)]
pub struct CalendarCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Calendar event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: chrono::DateTime<chrono::Utc>,
    pub location: Option<String>,
    pub attendees: Vec<String>,
    pub status: String, // confirmed, tentative, cancelled
}

/// Time slot for availability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSlot {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Calendar tool for scheduling
pub struct CalendarTool {
    provider: CalendarProvider,
    credentials: CalendarCredentials,
    client: reqwest::Client,
    calendar_id: String, // "primary" for default
}

impl CalendarTool {
    /// Create new calendar tool
    #[must_use]
    pub fn new(provider: CalendarProvider, credentials: CalendarCredentials) -> Self {
        Self {
            provider,
            credentials,
            client: reqwest::Client::new(),
            calendar_id: "primary".to_string(),
        }
    }

    /// Create from environment variables
    pub fn from_env() -> anyhow::Result<Self> {
        let provider = std::env::var("CALENDAR_PROVIDER")
            .unwrap_or_else(|_| "google".to_string())
            .parse::<CalendarProvider>()?;

        let credentials = CalendarCredentials {
            client_id: std::env::var("CALENDAR_CLIENT_ID")
                .map_err(|_| anyhow::anyhow!("CALENDAR_CLIENT_ID not set"))?,
            client_secret: std::env::var("CALENDAR_CLIENT_SECRET")
                .map_err(|_| anyhow::anyhow!("CALENDAR_CLIENT_SECRET not set"))?,
            access_token: std::env::var("CALENDAR_ACCESS_TOKEN")
                .map_err(|_| anyhow::anyhow!("CALENDAR_ACCESS_TOKEN not set"))?,
            refresh_token: std::env::var("CALENDAR_REFRESH_TOKEN").ok(),
            token_expires_at: None,
        };

        Ok(Self::new(provider, credentials))
    }

    /// Set calendar ID (default is "primary")
    #[must_use]
    pub fn with_calendar_id(mut self, id: String) -> Self {
        self.calendar_id = id;
        self
    }

    /// Refresh access token if expired
    async fn refresh_token_if_needed(&mut self) -> anyhow::Result<()> {
        // Check if token is expired or about to expire (within 5 minutes)
        let should_refresh = if let Some(expires_at) = self.credentials.token_expires_at {
            let now = chrono::Utc::now();
            let buffer = chrono::Duration::minutes(5);
            now + buffer >= expires_at
        } else {
            false
        };

        if should_refresh {
            if let Some(refresh_token) = self.credentials.refresh_token.clone() {
                self.refresh_token(&refresh_token).await?;
            }
        }

        Ok(())
    }

    /// Refresh `OAuth2` token
    async fn refresh_token(&mut self, refresh_token: &str) -> anyhow::Result<()> {
        match self.provider {
            CalendarProvider::Google => {
                let url = "https://oauth2.googleapis.com/token";
                let params = [
                    ("client_id", self.credentials.client_id.as_str()),
                    ("client_secret", self.credentials.client_secret.as_str()),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                ];

                let response = self.client.post(url).form(&params).send().await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Token refresh failed: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                if let Some(token) = data.get("access_token").and_then(|t| t.as_str()) {
                    self.credentials.access_token = token.to_string();
                }

                if let Some(expires_in) = data.get("expires_in").and_then(serde_json::Value::as_i64)
                {
                    self.credentials.token_expires_at =
                        Some(chrono::Utc::now() + chrono::Duration::seconds(expires_in));
                }
            }
            CalendarProvider::Outlook => {
                // Outlook token refresh implementation
                let url = "https://login.microsoftonline.com/common/oauth2/v2.0/token";
                let params = [
                    ("client_id", self.credentials.client_id.as_str()),
                    ("client_secret", self.credentials.client_secret.as_str()),
                    ("refresh_token", refresh_token),
                    ("grant_type", "refresh_token"),
                    ("scope", "https://graph.microsoft.com/Calendars.ReadWrite"),
                ];

                let response = self.client.post(url).form(&params).send().await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Token refresh failed: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                if let Some(token) = data.get("access_token").and_then(|t| t.as_str()) {
                    self.credentials.access_token = token.to_string();
                }
            }
        }

        Ok(())
    }

    /// List events in a time range
    async fn list_events(
        &mut self,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<Vec<CalendarEvent>> {
        self.refresh_token_if_needed().await?;

        match self.provider {
            CalendarProvider::Google => {
                let url = format!(
                    "https://www.googleapis.com/calendar/v3/calendars/{}/events",
                    self.calendar_id
                );

                let response = self
                    .client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.credentials.access_token),
                    )
                    .query(&[
                        ("timeMin", start.to_rfc3339()),
                        ("timeMax", end.to_rfc3339()),
                        ("singleEvents", "true".to_string()),
                    ])
                    .send()
                    .await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Google Calendar API error: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                let mut events = Vec::new();

                if let Some(items) = data.get("items").and_then(|i| i.as_array()) {
                    for item in items {
                        if let Some(event) = self.parse_google_event(item) {
                            events.push(event);
                        }
                    }
                }

                Ok(events)
            }
            CalendarProvider::Outlook => {
                // Outlook implementation
                let url = format!(
                    "https://graph.microsoft.com/v1.0/me/calendars/{}/calendarView",
                    self.calendar_id
                );

                let response = self
                    .client
                    .get(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.credentials.access_token),
                    )
                    .query(&[
                        ("startDateTime", start.to_rfc3339()),
                        ("endDateTime", end.to_rfc3339()),
                    ])
                    .send()
                    .await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Outlook API error: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                let mut events = Vec::new();

                if let Some(items) = data.get("value").and_then(|v| v.as_array()) {
                    for item in items {
                        if let Some(event) = self.parse_outlook_event(item) {
                            events.push(event);
                        }
                    }
                }

                Ok(events)
            }
        }
    }

    /// Find available time slots
    async fn find_available_slots(
        &mut self,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        duration_minutes: i64,
    ) -> anyhow::Result<Vec<TimeSlot>> {
        let events = self.list_events(start, end).await?;
        let duration = chrono::Duration::minutes(duration_minutes);

        // Sort events by start time
        let mut sorted_events = events;
        sorted_events.sort_by(|a, b| a.start_time.cmp(&b.start_time));

        let mut slots = Vec::new();
        let mut current_time = start;

        for event in sorted_events {
            // Check if there's a gap before this event
            if event.start_time > current_time + duration {
                slots.push(TimeSlot {
                    start: current_time,
                    end: event.start_time,
                });
            }
            // Move current time to after this event
            if event.end_time > current_time {
                current_time = event.end_time;
            }
        }

        // Check for slot after last event
        if end > current_time + duration {
            slots.push(TimeSlot {
                start: current_time,
                end,
            });
        }

        Ok(slots)
    }

    /// Create a new event
    async fn create_event(
        &mut self,
        title: &str,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        description: Option<&str>,
        attendees: Vec<&str>,
    ) -> anyhow::Result<CalendarEvent> {
        self.refresh_token_if_needed().await?;

        match self.provider {
            CalendarProvider::Google => {
                let url = format!(
                    "https://www.googleapis.com/calendar/v3/calendars/{}/events",
                    self.calendar_id
                );

                let mut body = serde_json::json!({
                    "summary": title,
                    "start": {
                        "dateTime": start.to_rfc3339(),
                        "timeZone": "UTC"
                    },
                    "end": {
                        "dateTime": end.to_rfc3339(),
                        "timeZone": "UTC"
                    },
                });

                if let Some(desc) = description {
                    body["description"] = json!(desc);
                }

                if !attendees.is_empty() {
                    body["attendees"] = json!(attendees
                        .iter()
                        .map(|email| { json!({ "email": email }) })
                        .collect::<Vec<_>>());
                }

                let response = self
                    .client
                    .post(&url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.credentials.access_token),
                    )
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Failed to create event: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                self.parse_google_event(&data)
                    .ok_or_else(|| anyhow::anyhow!("Failed to parse created event"))
            }
            CalendarProvider::Outlook => {
                let url = "https://graph.microsoft.com/v1.0/me/events";

                let mut body = serde_json::json!({
                    "subject": title,
                    "start": {
                        "dateTime": start.to_rfc3339(),
                        "timeZone": "UTC"
                    },
                    "end": {
                        "dateTime": end.to_rfc3339(),
                        "timeZone": "UTC"
                    },
                });

                if let Some(desc) = description {
                    body["body"] = json!({
                        "contentType": "text",
                        "content": desc
                    });
                }

                if !attendees.is_empty() {
                    body["attendees"] = json!(attendees
                        .iter()
                        .map(|email| {
                            json!({
                                "emailAddress": {
                                    "address": email
                                },
                                "type": "required"
                            })
                        })
                        .collect::<Vec<_>>());
                }

                let response = self
                    .client
                    .post(url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", self.credentials.access_token),
                    )
                    .header("Content-Type", "application/json")
                    .json(&body)
                    .send()
                    .await?;

                if !response.status().is_success() {
                    let error = response.text().await?;
                    return Err(anyhow::anyhow!("Failed to create event: {error}"));
                }

                let data: serde_json::Value = response.json().await?;
                self.parse_outlook_event(&data)
                    .ok_or_else(|| anyhow::anyhow!("Failed to parse created event"))
            }
        }
    }

    /// Parse Google Calendar event JSON
    fn parse_google_event(&self, data: &serde_json::Value) -> Option<CalendarEvent> {
        let id = data.get("id")?.as_str()?.to_string();
        let title = data.get("summary")?.as_str()?.to_string();
        let description = data
            .get("description")
            .and_then(|d| d.as_str())
            .map(std::string::ToString::to_string);
        let status = data.get("status")?.as_str()?.to_string();

        let start_time = data
            .get("start")?
            .get("dateTime")?
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))?;

        let end_time = data
            .get("end")?
            .get("dateTime")?
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))?;

        let location = data
            .get("location")
            .and_then(|l| l.as_str())
            .map(std::string::ToString::to_string);

        let attendees: Vec<String> = data
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        a.get("email")
                            .and_then(|e| e.as_str())
                            .map(std::string::ToString::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(CalendarEvent {
            id,
            title,
            description,
            start_time,
            end_time,
            location,
            attendees,
            status,
        })
    }

    /// Parse Outlook event JSON
    fn parse_outlook_event(&self, data: &serde_json::Value) -> Option<CalendarEvent> {
        let id = data.get("id")?.as_str()?.to_string();
        let title = data.get("subject")?.as_str()?.to_string();
        let status = data
            .get("showAs")?
            .as_str()
            .map_or("busy", |s| s)
            .to_string();

        let start_time = data
            .get("start")?
            .get("dateTime")?
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))?;

        let end_time = data
            .get("end")?
            .get("dateTime")?
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))?;

        let description = data
            .get("body")
            .and_then(|b| b.get("content"))
            .and_then(|c| c.as_str())
            .map(std::string::ToString::to_string);

        let location = data
            .get("location")
            .and_then(|l| l.get("displayName"))
            .and_then(|d| d.as_str())
            .map(std::string::ToString::to_string);

        let attendees: Vec<String> = data
            .get("attendees")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        a.get("emailAddress")
                            .and_then(|e| e.get("address"))
                            .and_then(|a| a.as_str())
                            .map(std::string::ToString::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(CalendarEvent {
            id,
            title,
            description,
            start_time,
            end_time,
            location,
            attendees,
            status,
        })
    }
}

#[async_trait]
impl Tool for CalendarTool {
    fn name(&self) -> &'static str {
        "calendar"
    }

    fn description(&self) -> &'static str {
        r#"Calendar tool for scheduling meetings and checking availability.

Supports Google Calendar and Outlook/Exchange.

Commands:
- list_events: List calendar events in a time range
- find_slots: Find available time slots for meetings
- create_event: Create a new calendar event with optional attendees

Examples:
TOOL_CALL: {"name": "calendar", "parameters": {"command": "list_events", "start": "2026-02-17T09:00:00Z", "end": "2026-02-17T17:00:00Z"}}
TOOL_CALL: {"name": "calendar", "parameters": {"command": "find_slots", "start": "2026-02-17T09:00:00Z", "end": "2026-02-17T17:00:00Z", "duration_minutes": 60}}
TOOL_CALL: {"name": "calendar", "parameters": {"command": "create_event", "title": "Client Meeting", "start": "2026-02-17T14:00:00Z", "end": "2026-02-17T15:00:00Z", "description": "Discuss project requirements", "attendees": ["client@example.com"]}}

Environment Variables Required:
- CALENDAR_PROVIDER (google or outlook)
- CALENDAR_CLIENT_ID
- CALENDAR_CLIENT_SECRET
- CALENDAR_ACCESS_TOKEN
- CALENDAR_REFRESH_TOKEN (optional but recommended)"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let command = params
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("list_events");

        // Clone self to allow mutation for token refresh
        let mut tool = CalendarTool {
            provider: self.provider,
            credentials: self.credentials.clone(),
            client: self.client.clone(),
            calendar_id: self.calendar_id.clone(),
        };

        match command {
            "list_events" => {
                let start = params
                    .get("start")
                    .and_then(|s| s.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Missing or invalid 'start' parameter (ISO 8601 format required)"
                        )
                    })?;

                let end = params
                    .get("end")
                    .and_then(|e| e.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Missing or invalid 'end' parameter (ISO 8601 format required)"
                        )
                    })?;

                let events = tool.list_events(start, end).await?;

                Ok(json!({
                    "success": true,
                    "events": events.iter().map(|e| json!({
                        "id": e.id,
                        "title": e.title,
                        "description": e.description,
                        "start_time": e.start_time.to_rfc3339(),
                        "end_time": e.end_time.to_rfc3339(),
                        "location": e.location,
                        "attendees": e.attendees,
                        "status": e.status
                    })).collect::<Vec<_>>(),
                    "count": events.len()
                }))
            }

            "find_slots" => {
                let start = params
                    .get("start")
                    .and_then(|s| s.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'start' parameter"))?;

                let end = params
                    .get("end")
                    .and_then(|e| e.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'end' parameter"))?;

                let duration = params
                    .get("duration_minutes")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(60);

                let slots = tool.find_available_slots(start, end, duration).await?;

                Ok(json!({
                    "success": true,
                    "slots": slots.iter().map(|s| json!({
                        "start": s.start.to_rfc3339(),
                        "end": s.end.to_rfc3339()
                    })).collect::<Vec<_>>(),
                    "count": slots.len(),
                    "duration_minutes": duration
                }))
            }

            "create_event" => {
                let title = params
                    .get("title")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'title' parameter"))?;

                let start = params
                    .get("start")
                    .and_then(|s| s.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'start' parameter"))?;

                let end = params
                    .get("end")
                    .and_then(|e| e.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'end' parameter"))?;

                let description = params.get("description").and_then(|d| d.as_str());

                let attendees: Vec<&str> = params
                    .get("attendees")
                    .and_then(|a| a.as_array())
                    .map(|arr| arr.iter().filter_map(|a| a.as_str()).collect())
                    .unwrap_or_default();

                let event = tool
                    .create_event(title, start, end, description, attendees)
                    .await?;

                Ok(json!({
                    "success": true,
                    "event": {
                        "id": event.id,
                        "title": event.title,
                        "description": event.description,
                        "start_time": event.start_time.to_rfc3339(),
                        "end_time": event.end_time.to_rfc3339(),
                        "location": event.location,
                        "attendees": event.attendees,
                        "status": event.status
                    },
                    "message": format!("Event '{}' created successfully", event.title)
                }))
            }

            _ => Err(anyhow::anyhow!(
                "Unknown command: {command}. Use 'list_events', 'find_slots', or 'create_event'"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_provider_parse() {
        assert!(matches!(
            "google".parse::<CalendarProvider>().unwrap(),
            CalendarProvider::Google
        ));
        assert!(matches!(
            "outlook".parse::<CalendarProvider>().unwrap(),
            CalendarProvider::Outlook
        ));
    }

    #[tokio::test]
    async fn test_find_slots_logic() {
        // This would require mock calendar data
        // Skipping for now as it requires API credentials
    }
}
