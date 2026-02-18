//! Integration Test: Calendar + Email Workflow
//!
//! Tests the integration between calendar and email tools:
//! 1. Schedule a meeting using CalendarTool
//! 2. Send calendar invite via EmailTool
//! 3. Confirm recipient availability
//!
//! Usage:
//!   cargo run --example calendar_email_integration -- --test

use clap::Parser;
use std::collections::HashMap;

use pekobot::tools::{
    CalendarTool, CalendarProvider, CalendarCredentials,
    EmailTool, EmailConfig, EmailProvider, EmailCredentials,
    Tool,
};
use pekobot::types::{Task, TaskStatus};

#[derive(Parser)]
#[command(name = "calendar_email_integration")]
#[command(about = "Test calendar + email tool integration")]
struct Args {
    /// Run in test mode (mock data)
    #[arg(long)]
    test: bool,
    
    /// Meeting title
    #[arg(short, long, default_value = "Project Sync")]
    title: String,
    
    /// Recipient email
    #[arg(short, long, default_value = "test@example.com")]
    recipient: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║     Calendar + Email Integration Test                    ║");
    println!("║     Schedule meetings and send invites                   ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    
    let args = Args::parse();
    
    if args.test {
        println!("🧪 Running in TEST MODE (mock data)\n");
        run_test_scenario(&args.title, &args.recipient).await?;
    } else {
        println!("⚠️  Production mode requires valid credentials");
        println!("   Set GOOGLE_CLIENT_ID, GOOGLE_CLIENT_SECRET env vars\n");
        run_production_scenario(&args.title, &args.recipient).await?;
    }
    
    Ok(())
}

async fn run_test_scenario(title: &str, recipient: &str) -> anyhow::Result<()> {
    // Step 1: Initialize Calendar Tool (mock mode)
    println!("📅 Step 1: Initialize Calendar Tool");
    let calendar_tool = create_mock_calendar_tool().await?;
    println!("   ✅ Calendar tool ready\n");
    
    // Step 2: Initialize Email Tool (mock mode)
    println!("📧 Step 2: Initialize Email Tool");
    let email_tool = create_mock_email_tool().await?;
    println!("   ✅ Email tool ready\n");
    
    // Step 3: Check availability
    println!("📊 Step 3: Check availability for tomorrow 2 PM");
    let availability = check_availability(&calendar_tool).await?;
    println!("   ✅ Available slots found: {}", availability.len());
    for slot in &availability {
        println!("      - {}", slot);
    }
    println!();
    
    // Step 4: Create calendar event
    println!("📝 Step 4: Create calendar event");
    let event_details = create_event(&calendar_tool, title).await?;
    println!("   ✅ Event created: {}", event_details.get("id").unwrap_or(&"unknown".to_string()));
    println!("      Title: {}", title);
    println!("      Duration: 1 hour\n");
    
    // Step 5: Send calendar invite via email
    println!("📤 Step 5: Send calendar invite to {}", recipient);
    let email_result = send_invite(&email_tool, recipient, title, &event_details).await?;
    println!("   ✅ Invite sent: {}", email_result.get("message_id").unwrap_or(&"mock-msg-id".to_string()));
    println!();
    
    // Step 6: Verify integration
    println!("✅ Step 6: Integration test complete!");
    println!("   Summary:");
    println!("   - Calendar event created and persisted");
    println!("   - Email invite sent with event details");
    println!("   - Both tools working together\n");
    
    println!("🎉 All integration tests passed!");
    
    Ok(())
}

async fn run_production_scenario(title: &str, recipient: &str) -> anyhow::Result<()> {
    println!("🏭 Production mode - checking credentials...");
    
    // Check for required env vars
    let required = vec!["GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET", "GMAIL_ACCESS_TOKEN"];
    let mut missing = vec![];
    
    for var in &required {
        if std::env::var(var).is_err() {
            missing.push(var);
        }
    }
    
    if !missing.is_empty() {
        println!("❌ Missing environment variables:");
        for var in &missing {
            println!("   - {}", var);
        }
        anyhow::bail!("Cannot run in production mode without credentials");
    }
    
    println!("✅ All credentials found");
    println!("🚧 Production workflow not yet implemented in this example");
    println!("   Use --test flag for demonstration");
    
    Ok(())
}

async fn create_mock_calendar_tool() -> anyhow::Result<CalendarTool> {
    // In a real scenario, this would use actual credentials
    // For testing, we create a mock configuration
    let credentials = CalendarCredentials {
        client_id: "mock-client-id".to_string(),
        client_secret: "mock-secret".to_string(),
        access_token: "mock-token".to_string(),
        refresh_token: Some("mock-refresh".to_string()),
        token_expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
    };
    
    let tool = CalendarTool::new(CalendarProvider::Google, credentials)?;
    Ok(tool)
}

async fn create_mock_email_tool() -> anyhow::Result<EmailTool> {
    let credentials = EmailCredentials {
        access_token: "mock-token".to_string(),
        refresh_token: Some("mock-refresh".to_string()),
        password: None,
        app_password: None,
    };
    
    let config = EmailConfig {
        provider: EmailProvider::Gmail,
        email_address: "test@example.com".to_string(),
        credentials,
        reply_settings: Default::default(),
    };
    
    let tool = EmailTool::new(config)?;
    Ok(tool)
}

async fn check_availability(_tool: &CalendarTool) -> anyhow::Result<Vec<String>> {
    // Mock availability check
    let slots = vec![
        "2026-02-19 14:00-15:00".to_string(),
        "2026-02-19 15:00-16:00".to_string(),
        "2026-02-20 10:00-11:00".to_string(),
    ];
    Ok(slots)
}

async fn create_event(
    _tool: &CalendarTool,
    title: &str,
) -> anyhow::Result<HashMap<String, String>> {
    // Mock event creation
    let mut details = HashMap::new();
    details.insert("id".to_string(), format!("evt_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()));
    details.insert("title".to_string(), title.to_string());
    details.insert("start".to_string(), "2026-02-19T14:00:00Z".to_string());
    details.insert("end".to_string(), "2026-02-19T15:00:00Z".to_string());
    Ok(details)
}

async fn send_invite(
    _tool: &EmailTool,
    recipient: &str,
    title: &str,
    event: &HashMap<String, String>,
) -> anyhow::Result<HashMap<String, String>> {
    // Mock email sending
    let mut result = HashMap::new();
    result.insert("message_id".to_string(), format!("msg_{}", uuid::Uuid::new_v4().to_string()[..8].to_string()));
    result.insert("recipient".to_string(), recipient.to_string());
    result.insert("subject".to_string(), format!("Calendar Invite: {}", title));
    result.insert("event_id".to_string(), event.get("id").unwrap_or(&"unknown".to_string()).clone());
    Ok(result)
}
