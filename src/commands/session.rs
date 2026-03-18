//! Session Management Commands
//!
//! These commands communicate with the daemon via HTTP API.

use crate::api::client::{ApiClient, ClientError};
use crate::commands::GlobalPaths;
use clap::Subcommand;
use reqwest::StatusCode;

/// Session management subcommands
///
/// Sessions store the conversation history with an agent.
/// Each instance can have multiple sessions, but only one active session at a time.
///
/// Examples:
///   # List sessions for an instance
///   pekobot session list INSTANCE_ID
///
///   # Show session details with history
///   pekobot session show INSTANCE_ID SESSION_ID --history
///
///   # Create a branch of a session
///   pekobot session branch INSTANCE_ID SESSION_ID --label "experiment"
///
///   # Send a message to a specific session
///   pekobot session send INSTANCE_ID "Hello" --session-id SESSION_ID
///
///   # Delete a session
///   pekobot session delete INSTANCE_ID SESSION_ID
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SessionCommands {
    /// List sessions for an instance
    List {
        /// Instance ID (required - use 'pekobot agent list' to find instances)
        instance_id: String,
        /// Show all sessions including inactive
        #[arg(long)]
        all: bool,
    },

    /// Show session details and history
    Show {
        /// Instance ID
        instance_id: String,
        /// Session ID
        session_id: String,
        /// Show full message history
        #[arg(long)]
        history: bool,
    },

    /// Branch a session
    Branch {
        /// Instance ID
        instance_id: String,
        /// Session ID to branch from
        session_id: String,
        /// Optional label for the new session
        #[arg(short, long)]
        label: Option<String>,
    },

    /// Delete a session
    Delete {
        /// Instance ID
        instance_id: String,
        /// Session ID to delete
        session_id: String,
        /// Skip confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Send message to a session (via chat API)
    Send {
        /// Instance ID
        instance_id: String,
        /// Session ID (optional - uses active session if not provided)
        #[arg(short, long)]
        session_id: Option<String>,
        /// Message content
        message: String,
    },
}

/// Handle session commands
pub async fn handle_session(
    cmd: SessionCommands,
    _paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    let client = ApiClient::new()?;

    match cmd {
        SessionCommands::List {
            instance_id,
            all: _,
        } => list_sessions(&client, &instance_id, json).await,
        SessionCommands::Show {
            instance_id,
            session_id,
            history,
        } => show_session(&client, &instance_id, &session_id, history, json).await,
        SessionCommands::Branch {
            instance_id,
            session_id,
            label,
        } => branch_session(&client, &instance_id, &session_id, label, json).await,
        SessionCommands::Delete {
            instance_id,
            session_id,
            force,
        } => delete_session(&client, &instance_id, &session_id, force, json).await,
        SessionCommands::Send {
            instance_id,
            session_id,
            message,
        } => {
            println!("📤 Sending message to instance '{instance_id}': {message}");
            if let Some(sid) = session_id {
                println!("   Session: {sid}");
            }
            Ok(())
        }
    }
}

/// Check if error is a 404 not found
fn is_not_found(e: &ClientError) -> bool {
    matches!(e, ClientError::Api { status, .. } if *status == StatusCode::NOT_FOUND)
}

/// List sessions for an instance
async fn list_sessions(client: &ApiClient, instance_id: &str, json: bool) -> anyhow::Result<()> {
    match client.list_sessions(instance_id).await {
        Ok(response) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else if response.items.is_empty() {
                println!("📭 No sessions found for instance '{instance_id}'.");
                println!("   Start chatting with the agent to create sessions.");
            } else {
                println!(
                    "📋 Sessions for instance '{}' ({} found):",
                    instance_id,
                    response.items.len()
                );
                println!();

                for session in &response.items {
                    let title = session.title.as_deref().unwrap_or("(untitled)");
                    let created = format_timestamp(&session.created_at);
                    let updated = format_timestamp(&session.updated_at);

                    println!("  📁 {}", session.id);
                    println!("     Title: {}", title);
                    println!("     Turns: {}", session.turn_count);
                    println!("     Created: {}", created);
                    println!("     Updated: {}", updated);

                    if let Some(ref parent) = session.parent_session_id {
                        println!("     Parent: {}", parent);
                    }
                    println!();
                }
            }
            Ok(())
        }
        Err(ref e) if is_not_found(e) => {
            Err(anyhow::anyhow!("Instance '{}' not found", instance_id))
        }
        Err(e) => Err(anyhow::anyhow!("Failed to list sessions: {}", e)),
    }
}

/// Show session details and optionally history
async fn show_session(
    client: &ApiClient,
    instance_id: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    // Get session metadata
    let session = match client.get_session(instance_id, session_id).await {
        Ok(s) => s,
        Err(ref e) if is_not_found(e) => {
            return Err(anyhow::anyhow!(
                "Session '{}' not found for instance '{}'",
                session_id,
                instance_id
            ));
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to get session: {}", e)),
    };

    // Get history if requested
    let history = if show_history {
        match client
            .get_session_history(instance_id, session_id, true, false)
            .await
        {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("⚠️  Failed to load history: {}", e);
                None
            }
        }
    } else {
        None
    };

    if json {
        let output = serde_json::json!({
            "session": session,
            "history": history,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("📊 Session Details");
        println!("   Instance ID: {}", instance_id);
        println!("   Session ID: {}", session.id);
        if let Some(ref title) = session.title {
            println!("   Title: {}", title);
        }
        println!("   Turns: {}", session.turn_count);
        println!("   Created: {}", format_timestamp(&session.created_at));
        println!("   Updated: {}", format_timestamp(&session.updated_at));

        if let Some(ref parent) = session.parent_session_id {
            println!("   Parent Session: {}", parent);
        }

        if let Some(h) = history {
            if !h.items.is_empty() {
                println!();
                println!("📜 Message History ({} events):", h.items.len());
                println!();

                for (i, event) in h.items.iter().enumerate() {
                    print_history_event(i, event);
                }
            }
        }
    }

    Ok(())
}

/// Print a history event in human-readable format
fn print_history_event(index: usize, event: &crate::api::client::HistoryEventResponse) {
    let event_type = &event.event_type;
    let timestamp = format_timestamp(&event.created_at);

    match event_type.as_str() {
        "user.message" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 👤 User ({}):", index, timestamp);
                println!("      {}", truncate(content, 200));
            }
        }
        "assistant.message" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 🤖 Assistant ({}):", index, timestamp);
                println!("      {}", truncate(content, 200));
            }
        }
        "tool.call" => {
            if let Some(ref tool) = event.tool {
                println!("  [{}] 🔧 Tool Call: {} ({})", index, tool, timestamp);
                if let Some(ref args) = event.args {
                    println!(
                        "      Args: {}",
                        serde_json::to_string(args).unwrap_or_default()
                    );
                }
            }
        }
        "tool.result" => {
            println!("  [{}] ✅ Tool Result ({})", index, timestamp);
            if let Some(ref output) = event.output {
                println!("      Output: {}", truncate(output, 150));
            }
            if let Some(ref error) = event.error {
                println!("      Error: {}", error);
            }
        }
        "thinking" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 💭 Thinking ({}):", index, timestamp);
                println!("      {}", truncate(content, 150));
            }
        }
        "system" => {
            if let Some(ref content) = event.content {
                println!("  [{}] ⚙️  System ({}): {}", index, timestamp, content);
            }
        }
        "hook.trigger" => {
            if let Some(ref content) = event.content {
                println!("  [{}] 🪝 Hook Trigger ({}): {}", index, timestamp, content);
            }
        }
        _ => {
            println!("  [{}] {} ({})", index, event_type, timestamp);
            if let Some(ref content) = event.content {
                println!("      {}", truncate(content, 150));
            }
        }
    }
    println!();
}

/// Branch a session
async fn branch_session(
    client: &ApiClient,
    instance_id: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    match client
        .branch_session(instance_id, session_id, label.as_deref())
        .await
    {
        Ok(response) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("✅ Branched session '{}'", session_id);
                println!("   New Session ID: {}", response.session.id);
                println!("   Parent Session: {}", response.parent_session_id);
                if let Some(ref label) = label {
                    println!("   Label: {}", label);
                }
            }
            Ok(())
        }
        Err(ref e) if is_not_found(e) => Err(anyhow::anyhow!(
            "Session '{}' not found for instance '{}'",
            session_id,
            instance_id
        )),
        Err(e) => Err(anyhow::anyhow!("Failed to branch session: {}", e)),
    }
}

/// Delete a session
async fn delete_session(
    client: &ApiClient,
    instance_id: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    if !force {
        println!("⚠️  This will delete session '{}'.", session_id);
        println!("   Session history will be permanently lost.");
        print!("   Continue? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    match client.delete_session(instance_id, session_id).await {
        Ok(()) => {
            if json {
                println!("{{\"success\": true, \"session_id\": \"{}\"}}", session_id);
            } else {
                println!("✅ Deleted session '{}'", session_id);
            }
            Ok(())
        }
        Err(ref e) if is_not_found(e) => Err(anyhow::anyhow!(
            "Session '{}' not found for instance '{}'",
            session_id,
            instance_id
        )),
        Err(e) => Err(anyhow::anyhow!("Failed to delete session: {}", e)),
    }
}

/// Format an ISO timestamp for display
fn format_timestamp(ts: &str) -> String {
    // Try to parse and format nicely
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        ts.to_string()
    }
}

/// Truncate a string to max length with ellipsis
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_commands_enum() {
        // Test List command
        let cmd = SessionCommands::List {
            instance_id: "inst_123".to_string(),
            all: true,
        };
        match cmd {
            SessionCommands::List { instance_id, all } => {
                assert_eq!(instance_id, "inst_123");
                assert!(all);
            }
            _ => panic!("Expected List command"),
        }

        // Test Show command
        let cmd = SessionCommands::Show {
            instance_id: "inst_123".to_string(),
            session_id: "sess_456".to_string(),
            history: true,
        };
        match cmd {
            SessionCommands::Show {
                instance_id,
                session_id,
                history,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, "sess_456");
                assert!(history);
            }
            _ => panic!("Expected Show command"),
        }

        // Test Branch command
        let cmd = SessionCommands::Branch {
            instance_id: "inst_123".to_string(),
            session_id: "sess_456".to_string(),
            label: Some("test-branch".to_string()),
        };
        match cmd {
            SessionCommands::Branch {
                instance_id,
                session_id,
                label,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, "sess_456");
                assert_eq!(label, Some("test-branch".to_string()));
            }
            _ => panic!("Expected Branch command"),
        }

        // Test Delete command
        let cmd = SessionCommands::Delete {
            instance_id: "inst_123".to_string(),
            session_id: "sess_456".to_string(),
            force: true,
        };
        match cmd {
            SessionCommands::Delete {
                instance_id,
                session_id,
                force,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, "sess_456");
                assert!(force);
            }
            _ => panic!("Expected Delete command"),
        }

        // Test Send command
        let cmd = SessionCommands::Send {
            instance_id: "inst_123".to_string(),
            session_id: Some("sess_456".to_string()),
            message: "Hello".to_string(),
        };
        match cmd {
            SessionCommands::Send {
                instance_id,
                session_id,
                message,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(session_id, Some("sess_456".to_string()));
                assert_eq!(message, "Hello");
            }
            _ => panic!("Expected Send command"),
        }
    }

    #[test]
    fn test_format_timestamp() {
        // Test RFC3339 timestamp
        let ts = "2026-03-17T10:30:00+00:00";
        let formatted = format_timestamp(ts);
        assert_eq!(formatted, "2026-03-17 10:30:00");

        // Test invalid timestamp (should return original)
        let ts = "invalid-timestamp";
        let formatted = format_timestamp(ts);
        assert_eq!(formatted, "invalid-timestamp");
    }

    #[test]
    fn test_truncate() {
        // Test string shorter than max
        let s = "short";
        assert_eq!(truncate(s, 10), "short");

        // Test string longer than max
        let s = "this is a very long string";
        assert_eq!(truncate(s, 10), "this is a ...");

        // Test exact length
        let s = "exactlyten";
        assert_eq!(truncate(s, 10), "exactlyten");
    }

    #[test]
    fn test_is_not_found() {
        use reqwest::StatusCode;

        // Test API error with 404 status
        let err = ClientError::Api {
            status: StatusCode::NOT_FOUND,
            code: "not_found".to_string(),
            message: "Not found".to_string(),
            request_id: "req_123".to_string(),
        };
        assert!(is_not_found(&err));

        // Test API error with different status
        let err = ClientError::Api {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request".to_string(),
            message: "Bad request".to_string(),
            request_id: "req_123".to_string(),
        };
        assert!(!is_not_found(&err));

        // Test other error types
        let err = ClientError::DaemonNotRunning {
            addr: "http://localhost:11435".to_string(),
        };
        assert!(!is_not_found(&err));
    }
}
