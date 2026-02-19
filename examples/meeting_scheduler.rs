//! Meeting Scheduler Agent Example
//!
//! A consultant's automated meeting scheduler that:
//! - Integrates with Google Calendar
//! - Finds available time slots
//! - Responds to meeting requests
//! - Creates calendar events with attendees
//! - Sends confirmation messages
//!
//! ## Setup
//!
//! 1. Set up Google Calendar API credentials:
//!    - Go to https://console.cloud.google.com/
//!    - Create a project and enable Google Calendar API
//!    - Create OAuth2 credentials (Web application)
//!    - Add redirect URI: http://localhost:8080/oauth/callback
//!    - Download credentials
//!
//! 2. Get access token (one-time setup):
//!    ```bash
//!    # Visit this URL in browser:
//!    https://accounts.google.com/o/oauth2/v2/auth?
//!      client_id=YOUR_CLIENT_ID&
//!      redirect_uri=http://localhost:8080/oauth/callback&
//!      response_type=code&
//!      scope=https://www.googleapis.com/auth/calendar&
//!      access_type=offline&
//!      prompt=consent
//!    ```
//!
//! 3. Exchange code for tokens
//!
//! 4. Copy .env.example to .env and fill in calendar credentials
//!
//! 5. Run: cargo run --example meeting_scheduler

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use pekobot::agent::Agent;
use pekobot::channels::http::HttpChannel;
use pekobot::tools::calendar::{CalendarCredentials, CalendarProvider, CalendarTool};
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ModelConfig, ProviderConfig, ProviderType};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Shared application state
struct AppState {
    agent: Arc<Mutex<Agent>>,
    calendar: Arc<Mutex<CalendarTool>>,
    preferences: MeetingPreferences,
}

/// Consultant's meeting preferences
#[derive(Clone)]
struct MeetingPreferences {
    working_hours_start: u8, // 9 = 9 AM
    working_hours_end: u8,   // 17 = 5 PM
    timezone: String,
    default_duration_minutes: i64,
    buffer_minutes: i64, // Buffer between meetings
}

impl Default for MeetingPreferences {
    fn default() -> Self {
        Self {
            working_hours_start: 9,
            working_hours_end: 17,
            timezone: "America/New_York".to_string(),
            default_duration_minutes: 60,
            buffer_minutes: 15,
        }
    }
}

/// Meeting request from client
#[derive(Debug, Deserialize)]
struct MeetingRequest {
    client_email: String,
    client_name: String,
    preferred_date: String, // YYYY-MM-DD
    duration_minutes: Option<i64>,
    topic: String,
}

/// Available slots response
#[derive(Debug, Serialize)]
struct AvailableSlotsResponse {
    success: bool,
    slots: Vec<TimeSlot>,
    message: String,
}

#[derive(Debug, Serialize)]
struct TimeSlot {
    start: String,
    end: String,
    display: String,
}

/// Booking confirmation
#[derive(Debug, Deserialize)]
struct BookingRequest {
    client_email: String,
    client_name: String,
    slot_start: String, // ISO 8601
    slot_end: String,
    topic: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    info!("📅 Meeting Scheduler Agent");
    info!("=========================");
    info!("");

    // Initialize calendar tool
    let calendar = CalendarTool::from_env().map_err(|e| {
        eprintln!("❌ Failed to initialize calendar: {}", e);
        eprintln!("");
        eprintln!("Make sure you have set these environment variables:");
        eprintln!("  - CALENDAR_PROVIDER (google or outlook)");
        eprintln!("  - CALENDAR_CLIENT_ID");
        eprintln!("  - CALENDAR_CLIENT_SECRET");
        eprintln!("  - CALENDAR_ACCESS_TOKEN");
        eprintln!("  - CALENDAR_REFRESH_TOKEN (recommended)");
        eprintln!("");
        eprintln!("See .env.example for details.");
        e
    })?;

    info!("✅ Calendar connected");

    // Initialize AI agent
    let agent = create_meeting_scheduler_agent().await?;
    info!("✅ AI agent initialized with DID: {}", agent.did());

    // Create shared state
    let state = Arc::new(AppState {
        agent: Arc::new(Mutex::new(agent)),
        calendar: Arc::new(Mutex::new(calendar)),
        preferences: MeetingPreferences::default(),
    });

    // Build router
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(health_check))
        .route("/available-slots", post(get_available_slots))
        .route("/book", post(book_meeting))
        .route("/handle-request", post(handle_natural_language_request))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    info!("");
    info!("🚀 Meeting scheduler running on http://localhost:{}", port);
    info!("");
    info!("📋 Available endpoints:");
    info!("   GET  /                    - Info page");
    info!("   GET  /health              - Health check");
    info!("   POST /available-slots     - Get available time slots");
    info!("   POST /book                - Book a meeting");
    info!("   POST /handle-request      - Natural language meeting request");
    info!("");
    info!("💡 Example usage:");
    info!(
        "   curl -X POST http://localhost:{}/available-slots \\",
        port
    );
    info!("     -H 'Content-Type: application/json' \\");
    info!("     -d '{{\"date\": \"2026-02-20\", \"duration_minutes\": 60}}'");
    info!("");

    axum::serve(listener, app).await?;

    Ok(())
}

/// Index page with instructions
async fn index() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "Meeting Scheduler Agent",
        "version": "1.0.0",
        "description": "Automated meeting scheduling for consultants",
        "endpoints": {
            "GET /health": "Health check",
            "POST /available-slots": {
                "description": "Get available time slots for a date",
                "body": {
                    "date": "YYYY-MM-DD",
                    "duration_minutes": 60
                }
            },
            "POST /book": {
                "description": "Book a meeting",
                "body": {
                    "client_email": "client@example.com",
                    "client_name": "John Doe",
                    "slot_start": "2026-02-20T14:00:00Z",
                    "slot_end": "2026-02-20T15:00:00Z",
                    "topic": "Project discussion"
                }
            },
            "POST /handle-request": {
                "description": "Handle natural language meeting request",
                "body": {
                    "request": "I'd like to meet tomorrow afternoon to discuss the project"
                }
            }
        }
    }))
}

/// Health check endpoint
async fn health_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let calendar_status = {
        let cal = state.calendar.lock().await;
        "connected".to_string()
    };

    Json(serde_json::json!({
        "status": "healthy",
        "service": "meeting-scheduler",
        "calendar": calendar_status,
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

/// Get available time slots for a date
async fn get_available_slots(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    info!("📩 Available slots request: {:?}", payload);

    let date_str = payload
        .get("date")
        .and_then(|d| d.as_str())
        .unwrap_or("2026-02-20");

    let duration = payload
        .get("duration_minutes")
        .and_then(|d| d.as_i64())
        .unwrap_or(state.preferences.default_duration_minutes);

    // Parse date and create start/end times
    let date = match chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AvailableSlotsResponse {
                    success: false,
                    slots: vec![],
                    message: "Invalid date format. Use YYYY-MM-DD".to_string(),
                }),
            );
        }
    };

    let start = chrono::Utc
        .from_local_datetime(
            &date
                .and_hms_opt(state.preferences.working_hours_start as u32, 0, 0)
                .unwrap(),
        )
        .unwrap();

    let end = chrono::Utc
        .from_local_datetime(
            &date
                .and_hms_opt(state.preferences.working_hours_end as u32, 0, 0)
                .unwrap(),
        )
        .unwrap();

    // Get available slots from calendar
    let tool = state.calendar.lock().await;
    let slots_result = tool
        .execute(serde_json::json!({
            "command": "find_slots",
            "start": start.to_rfc3339(),
            "end": end.to_rfc3339(),
            "duration_minutes": duration + state.preferences.buffer_minutes,
        }))
        .await;

    drop(tool); // Release lock

    match slots_result {
        Ok(result) => {
            if let Some(slots) = result.get("slots").and_then(|s| s.as_array()) {
                let available_slots: Vec<TimeSlot> = slots
                    .iter()
                    .filter_map(|s| {
                        let start = s.get("start")?.as_str()?;
                        let end = s.get("end")?.as_str()?;

                        // Parse for display
                        let start_dt = chrono::DateTime::parse_from_rfc3339(start).ok()?;
                        let display = start_dt.format("%I:%M %p").to_string();

                        Some(TimeSlot {
                            start: start.to_string(),
                            end: end.to_string(),
                            display,
                        })
                    })
                    .collect();

                (
                    StatusCode::OK,
                    Json(AvailableSlotsResponse {
                        success: true,
                        slots: available_slots.clone(),
                        message: format!("Found {} available slots", available_slots.len()),
                    }),
                )
            } else {
                (
                    StatusCode::OK,
                    Json(AvailableSlotsResponse {
                        success: true,
                        slots: vec![],
                        message: "No available slots found".to_string(),
                    }),
                )
            }
        }
        Err(e) => {
            error!("❌ Failed to get slots: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AvailableSlotsResponse {
                    success: false,
                    slots: vec![],
                    message: format!("Error: {}", e),
                }),
            )
        }
    }
}

/// Book a meeting
async fn book_meeting(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BookingRequest>,
) -> impl IntoResponse {
    info!(
        "📅 Booking request from {} for {}",
        payload.client_name, payload.topic
    );

    let tool = state.calendar.lock().await;
    let result = tool
        .execute(serde_json::json!({
            "command": "create_event",
            "title": format!("Meeting with {} - {}", payload.client_name, payload.topic),
            "start": payload.slot_start,
            "end": payload.slot_end,
            "description": format!("Meeting with {} ({})\n\nTopic: {}",
                payload.client_name,
                payload.client_email,
                payload.topic
            ),
            "attendees": [payload.client_email],
        }))
        .await;

    drop(tool);

    match result {
        Ok(event_data) => {
            info!("✅ Meeting booked successfully");
            Json(serde_json::json!({
                "success": true,
                "message": format!("Meeting booked for {}", payload.client_name),
                "event": event_data.get("event"),
                "confirmation": format!(
                    "Your meeting with {} has been scheduled for {}. \
                     A calendar invite has been sent to {}.",
                    payload.client_name,
                    payload.slot_start,
                    payload.client_email
                )
            }))
        }
        Err(e) => {
            error!("❌ Failed to book meeting: {}", e);
            Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to book meeting: {}", e)
            }))
        }
    }
}

/// Handle natural language meeting request
async fn handle_natural_language_request(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    let request = payload
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("");

    info!("💬 Natural language request: {}", request);

    // Use AI to parse the request
    let agent = state.agent.lock().await;
    let prompt = format!(
        r#"Parse this meeting request and extract key information:
        
Request: "{}"
        
Extract:
1. Client name (if mentioned)
2. Preferred date (today, tomorrow, specific date)
3. Preferred time (morning, afternoon, specific time)
4. Meeting topic
5. Duration estimate

Respond in this exact JSON format:
{{
    "client_name": "...",
    "preferred_date": "...",
    "preferred_time": "...",
    "topic": "...",
    "duration_minutes": 60,
    "needs_clarification": false,
    "clarification_question": "..."
}}"#,
        request
    );

    let parse_result = agent.execute(&prompt).await;
    drop(agent);

    match parse_result {
        Ok(response) => {
            // Try to parse JSON from response
            let json_str = extract_json_from_response(&response);
            match serde_json::from_str::<Value>(json_str) {
                Ok(parsed) => {
                    if parsed
                        .get("needs_clarification")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let question = parsed
                            .get("clarification_question")
                            .and_then(|q| q.as_str())
                            .unwrap_or("Could you provide more details?");

                        return Json(serde_json::json!({
                            "success": true,
                            "needs_clarification": true,
                            "message": question,
                            "parsed": parsed
                        }));
                    }

                    // Return parsed info with next steps
                    Json(serde_json::json!({
                        "success": true,
                        "needs_clarification": false,
                        "parsed": parsed,
                        "message": format!(
                            "I found a meeting request for {}. \
                             Use /available-slots to see available times, \
                             then /book to confirm.",
                            parsed.get("topic").and_then(|t| t.as_str()).unwrap_or("a discussion")
                        ),
                        "next_steps": [
                            format!("GET /available-slots with date: {:?}", parsed.get("preferred_date")),
                            "POST /book with selected slot"
                        ]
                    }))
                }
                Err(_) => Json(serde_json::json!({
                    "success": false,
                    "error": "Could not parse request",
                    "raw_response": response
                })),
            }
        }
        Err(e) => {
            error!("❌ AI parsing failed: {}", e);
            Json(serde_json::json!({
                "success": false,
                "error": format!("Failed to parse request: {}", e)
            }))
        }
    }
}

/// Extract JSON from AI response (handles markdown code blocks)
fn extract_json_from_response(response: &str) -> &str {
    // Try to find JSON in code blocks
    if let Some(start) = response.find("```json") {
        if let Some(end) = response.find("```") {
            return &response[start + 7..end].trim();
        }
    }

    // Try to find JSON between braces
    if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            return &response[start..=end];
        }
    }

    response
}

/// Create and configure the meeting scheduler agent
async fn create_meeting_scheduler_agent() -> anyhow::Result<Agent> {
    let agent_config = AgentConfig {
        name: "meeting-scheduler".to_string(),
        description: Some("AI meeting scheduler for consultants".to_string()),
        capabilities: vec![AgentCapability::Text, AgentCapability::ToolUse],
        system_prompt: Some(
            r#"
You are an AI meeting scheduler for a busy consultant.

Your job:
1. Parse natural language meeting requests
2. Extract key information (who, when, what, how long)
3. Ask clarifying questions if needed
4. Help format requests for the booking system

Guidelines:
- Be professional but friendly
- If date is unclear ("tomorrow", "next week"), note it needs clarification
- Default to 60-minute meetings unless specified otherwise
- Ask for missing required info: client email, topic
- Suggest alternatives if preferred time isn't available

When parsing, extract:
- Client name (the person requesting the meeting)
- Preferred date/time
- Meeting topic
- Duration estimate
"#
            .to_string(),
        ),
        metadata: {
            let mut m = HashMap::new();
            m.insert("role".to_string(), "meeting_scheduler".to_string());
            m.insert("version".to_string(), "1.0".to_string());
            m
        },
    };

    let memory_config = MemoryConfig {
        enabled: true,
        backend: "sqlite".to_string(),
        path: Some("./meeting_scheduler_memory.db".to_string()),
    };

    let provider_config = ProviderConfig {
        provider_type: ProviderType::OpenAI,
        model: ModelConfig {
            name: "gpt-4o-mini".to_string(),
            temperature: Some(0.3), // Lower temp for consistent parsing
            max_tokens: Some(500),
        },
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        api_base: None,
    };

    let agent = Agent::new(agent_config)
        .with_memory(memory_config)
        .with_provider(provider_config)?;

    Ok(agent)
}
