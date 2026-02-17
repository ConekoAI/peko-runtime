//! WhatsApp Customer Service Agent Example
//!
//! This example demonstrates how to build a customer service agent for WhatsApp.
//! 
//! ## Prerequisites
//!
//! 1. WhatsApp Business Account via Meta for Developers
//! 2. A registered phone number with WhatsApp Business API
//! 3. A public HTTPS webhook endpoint (use ngrok or Cloudflare Tunnel for local testing)
//!
//! ## Setup
//!
//! 1. Copy `.env.example` to `.env` and fill in:
//!    - WHATSAPP_ACCESS_TOKEN (from Meta Developer portal)
//!    - WHATSAPP_PHONE_NUMBER_ID (from WhatsApp Business settings)
//!    - WHATSAPP_VERIFY_TOKEN (create a random string for webhook verification)
//!
//! 2. Set up webhook endpoint:
//!    ```bash
//!    # Using ngrok for local testing
//!    ngrok http 8080
//!    # Then configure webhook URL in Meta Developer portal: https://your-ngrok.ngrok.io/webhook
//!    ```
//!
//! 3. Run the example:
//!    ```bash
//!    cargo run --example whatsapp_customer_service
//!    ```
//!
//! ## How It Works
//!
//! 1. Customer sends WhatsApp message to your business number
//! 2. Meta forwards the message to your webhook endpoint
//! 3. This example processes the message and sends an AI-generated response
//! 4. Complex issues (refunds, complaints) are escalated to humans
//!
//! ## Environment Variables
//!
//! ```bash
//! WHATSAPP_ACCESS_TOKEN=your_token_here
//! WHATSAPP_PHONE_NUMBER_ID=your_phone_id_here
//! WHATSAPP_VERIFY_TOKEN=your_verify_token_here
//! OPENAI_API_KEY=your_openai_key_here
//! ```

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use pekobot::agent::Agent;
use pekobot::channels::whatsapp::WhatsAppChannel;
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType, ModelConfig};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn, error};

/// Shared application state
struct AppState {
    whatsapp: Arc<Mutex<WhatsAppChannel>>,
    agent: Arc<Mutex<Agent>>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    info!("🏪 WhatsApp Customer Service Agent");
    info!("=================================");
    info!("");

    // Initialize WhatsApp channel from environment
    let whatsapp = WhatsAppChannel::from_env()
        .map_err(|e| {
            eprintln!("❌ Failed to initialize WhatsApp: {}", e);
            eprintln!("");
            eprintln!("Make sure you have set these environment variables:");
            eprintln!("  - WHATSAPP_ACCESS_TOKEN");
            eprintln!("  - WHATSAPP_PHONE_NUMBER_ID");
            eprintln!("  - WHATSAPP_VERIFY_TOKEN");
            eprintln!("");
            eprintln!("See .env.example for details.");
            e
        })?;

    info!("✅ WhatsApp channel initialized");

    // Initialize AI agent
    let agent = create_customer_service_agent().await?;
    info!("✅ AI agent initialized with DID: {}", agent.did());

    // Create shared state
    let state = Arc::new(AppState {
        whatsapp: Arc::new(Mutex::new(whatsapp)),
        agent: Arc::new(Mutex::new(agent)),
    });

    // Build router
    let app = Router::new()
        .route("/webhook", get(verify_webhook))  // For Meta webhook verification
        .route("/webhook", post(handle_message)) // For receiving messages
        .route("/health", get(health_check))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;

    info!("");
    info!("🚀 Server running on http://localhost:{}", port);
    info!("");
    info!("📱 To receive WhatsApp messages:");
    info!("   1. Set webhook URL in Meta Developer portal");
    info!("   2. Use ngrok for local testing: ngrok http {}", port);
    info!("   3. Configure webhook: https://your-ngrok.ngrok.io/webhook");
    info!("");
    info!("💡 Health check: http://localhost:{}/health", port);
    info!("");

    axum::serve(listener, app).await?;

    Ok(())
}

/// Webhook verification endpoint (Meta requires this)
///
/// Meta sends a verification challenge when you first configure the webhook
async fn verify_webhook(
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let mode = params.get("hub.mode").cloned().unwrap_or_default();
    let token = params.get("hub.verify_token").cloned().unwrap_or_default();
    let challenge = params.get("hub.challenge").cloned().unwrap_or_default();

    let whatsapp = state.whatsapp.lock().await;
    
    if mode == "subscribe" && token == whatsapp.verify_token() {
        info!("✅ Webhook verified by Meta");
        (StatusCode::OK, challenge)
    } else {
        warn!("❌ Webhook verification failed");
        (StatusCode::FORBIDDEN, "Verification failed".to_string())
    }
}

/// Handle incoming WhatsApp messages
async fn handle_message(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    info!("📩 Received webhook payload");

    // Parse messages from webhook payload
    let messages = {
        let mut whatsapp = state.whatsapp.lock().await;
        whatsapp.parse_webhook_payload(&payload)
    };

    for msg_text in messages {
        info!("💬 Customer message: {}", msg_text);

        // Check for escalation triggers
        let should_escalate = check_escalation(&msg_text);

        let response = if should_escalate {
            warn!("🚨 Escalation triggered for message");
            "I'm connecting you with a team member who can help with this. Please hold...".to_string()
        } else {
            // Generate AI response
            let agent = state.agent.lock().await;
            match agent.execute(&msg_text).await {
                Ok(resp) => resp,
                Err(e) => {
                    error!("❌ AI response failed: {}", e);
                    "I'm having trouble right now. Let me get a team member to help you.".to_string()
                }
            }
        };

        // Send response back to customer
        // Note: In production, you'd extract the 'from' number from the webhook payload
        // and use whatsapp.send_to_number()
        info!("🤖 Agent response: {}", response);
    }

    StatusCode::OK
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "healthy",
        "service": "whatsapp-customer-service",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}

/// Check if message should be escalated to human
fn check_escalation(message: &str) -> bool {
    let lower = message.to_lowercase();
    
    // Escalation triggers
    let triggers = [
        "refund", "money back", "chargeback",
        "lawsuit", "lawyer", "legal",
        "manager", "supervisor", "human",
        "angry", "terrible", "worst",
        "cancel subscription", "close account",
    ];

    triggers.iter().any(|t| lower.contains(t))
}

/// Create and configure the customer service agent
async fn create_customer_service_agent() -> anyhow::Result<Agent> {
    let agent_config = AgentConfig {
        name: "customer-service-bot".to_string(),
        description: Some("Friendly customer service representative".to_string()),
        capabilities: vec![AgentCapability::Text, AgentCapability::ToolUse],
        system_prompt: Some(r#"
You are a helpful customer service representative for a small retail business.

Your personality:
- Friendly, patient, and professional
- Use emojis occasionally to seem approachable 😊
- Keep responses concise (under 150 words for WhatsApp)

What you can help with:
✅ Store hours and location
✅ Product availability and pricing
✅ Order status questions
✅ General returns (under $100)
✅ Account questions

When to escalate (don't try to handle these):
🚨 Refunds over $100
🚨 Legal threats or mentions of lawyers
🚨 Very angry customers using strong language
🚨 Requests for managers or supervisors
🚨 Cancellation of subscriptions/accounts

If escalating, say: "I'm connecting you with a team member who can help with this."

Remember: It's better to escalate than give wrong information about money or legal matters.
"#.to_string()),
        metadata: {
            let mut m = HashMap::new();
            m.insert("department".to_string(), "customer_service".to_string());
            m.insert("version".to_string(), "1.0".to_string());
            m
        },
    };

    let memory_config = MemoryConfig {
        enabled: true,
        backend: "sqlite".to_string(),
        path: Some("./whatsapp_customer_service.db".to_string()),
    };

    let provider_config = ProviderConfig {
        provider_type: ProviderType::OpenAI,
        model: ModelConfig {
            name: "gpt-4o-mini".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(300),
        },
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        api_base: None,
    };

    let agent = Agent::new(agent_config)
        .with_memory(memory_config)
        .with_provider(provider_config)?;

    Ok(agent)
}
