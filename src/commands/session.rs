//! Session Management Commands
//!
//! Thin dispatch + rendering layer. All business logic is delegated to
//! `SessionService` and `SessionCompactor`. Presentation lives in
//! `session::presentation`.

use crate::auth::principal::Principal;
use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::session_service::{HistoryQuery, SessionService};
use crate::session::presentation::{
    history_event_to_display, render_branch_success, render_compact_dry_run,
    render_compact_success, render_delete_prompt, render_delete_success, render_session_details,
    render_session_history, render_session_list, render_session_list_json,
    render_session_show_json, render_switch_success,
};
use anyhow::Result;
use clap::Subcommand;

/// Helper: connect to daemon and send a request/response packet
async fn ipc_request(
    packet: crate::ipc::RequestPacket,
) -> anyhow::Result<crate::ipc::ResponsePacket> {
    let client = crate::ipc::DaemonClient::connect().await?;
    client.request_response(packet).await
}

/// Build the JSON object emitted by `peko session compact --dry-run --json`.
///
/// Extracted from the match arm so the wire format is unit-testable
/// without needing a daemon. Field names are part of the public CLI
/// contract and any rename here is a breaking change for callers that
/// parse this JSON.
fn render_dry_run_json(
    session_id: &str,
    estimated_tokens: usize,
    context_window: usize,
    percent: usize,
    message_count: usize,
    messages_to_compact: usize,
) -> serde_json::Value {
    serde_json::json!({
        "success": true,
        "dry_run": true,
        "session_id": session_id,
        "estimated_tokens": estimated_tokens,
        "context_window": context_window,
        "percent": percent,
        "message_count": message_count,
        "messages_to_compact": messages_to_compact,
    })
}

/// Session management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum SessionCommands {
    /// List sessions for an agent (offline)
    List {
        agent: String,
        #[arg(short, long)]
        team: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show session details and history (offline)
    Show {
        agent: String,
        #[arg(short, long)]
        session_id: Option<String>,
        #[arg(short, long)]
        team: Option<String>,
        #[arg(long)]
        history: bool,
    },
    /// Branch a session (offline - copies session files)
    Branch {
        agent: String,
        #[arg(short, long)]
        session_id: Option<String>,
        #[arg(short, long)]
        team: Option<String>,
        #[arg(short, long)]
        label: Option<String>,
    },
    /// Remove a session (offline - removes session files)
    Remove {
        agent: String,
        session_id: String,
        #[arg(short, long)]
        team: Option<String>,
        #[arg(short, long)]
        force: bool,
    },
    /// Switch active session (offline - updates preference file)
    Switch {
        agent: String,
        session_id: String,
        #[arg(short, long)]
        team: Option<String>,
    },
    /// Compact a session (offline - summarizes old messages)
    Compact {
        agent: String,
        #[arg(short, long)]
        session_id: Option<String>,
        #[arg(short, long)]
        team: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(short, long)]
        instruction: Option<String>,
    },
}

/// Handle session commands
pub async fn handle_session(
    cmd: SessionCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    let service = SessionService::new(paths.resolver().clone());

    match cmd {
        SessionCommands::List { agent, team, .. } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            let packet = crate::ipc::RequestPacket::SessionList {
                request_id: 1,
                agent: Some(agent_name.to_string()),
                team: Some(team.to_string()),
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SessionList { sessions, .. } => {
                    let mut manager = crate::session::SessionManager::for_cli(
                        paths.resolver.clone(),
                        agent_name,
                        Some(team),
                        paths.user(),
                    );
                    let peer = Principal::User(paths.user().to_string());
                    let active_session_id =
                        manager.get_active_session_id(&peer).await.ok().flatten();

                    if json {
                        render_session_list_json(
                            &sessions,
                            team,
                            agent_name,
                            active_session_id.as_deref(),
                        )?;
                    } else {
                        render_session_list(
                            &sessions,
                            team,
                            agent_name,
                            active_session_id.as_deref(),
                        );
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        // SessionShow and SessionSwitch remain as direct file I/O for now
        // because they need more complex IPC packets (session history streaming,
        // active session preference file updates).
        SessionCommands::Show {
            agent,
            session_id,
            team,
            history,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            let resolved = service
                .resolve_session_id(agent_name, Some(team), paths.user(), session_id)
                .await?;
            if !json {
                println!("📋 Using active session: {resolved}");
            }
            show_session(&service, team, agent_name, &resolved, history, json).await
        }
        SessionCommands::Branch {
            agent,
            session_id,
            team,
            label,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            let resolved = service
                .resolve_session_id(agent_name, Some(team), paths.user(), session_id)
                .await?;
            if !json {
                println!("🌿 Branching from active session: {resolved}");
            }
            let packet = crate::ipc::RequestPacket::SessionBranch {
                request_id: 1,
                agent: agent_name.to_string(),
                team: Some(team.to_string()),
                session_id: resolved.clone(),
                label: label.clone(),
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SessionBranched {
                    new_session_id,
                    parent_session_id,
                    ..
                } => {
                    if json {
                        let result = crate::common::services::session_service::BranchResult {
                            new_session_id,
                            parent_session_id,
                            label: label.clone(),
                        };
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    } else {
                        render_branch_success(
                            team,
                            agent_name,
                            &resolved,
                            &new_session_id,
                            label.as_deref(),
                        );
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        SessionCommands::Remove {
            agent,
            session_id,
            team,
            force,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;

            // Confirmation prompt (CLI-specific)
            if !force {
                render_delete_prompt(&session_id, None);
                std::io::Write::flush(&mut std::io::stdout())?;

                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;

                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled.");
                    return Ok(());
                }
            }

            let packet = crate::ipc::RequestPacket::SessionRemove {
                request_id: 1,
                agent: agent_name.to_string(),
                team: Some(team.to_string()),
                session_id: session_id.clone(),
                force,
            };
            let response = ipc_request(packet).await?;
            match response {
                crate::ipc::ResponsePacket::SessionRemoved { deleted, .. } => {
                    if json {
                        let deleted_items = if deleted {
                            vec!["jsonl", "index"]
                        } else {
                            vec![]
                        };
                        println!("{{\"success\": true, \"deleted\": {:?}}}", deleted_items);
                    } else {
                        render_delete_success(&session_id, deleted);
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        // SessionSwitch remains as direct file I/O — it updates the local
        // active-session preference file, which is simpler to do locally.
        SessionCommands::Switch {
            agent,
            session_id,
            team,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            switch_session(paths, team, agent_name, &session_id, paths.user(), json).await
        }
        SessionCommands::Compact {
            agent,
            session_id,
            team,
            dry_run,
            instruction,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            let resolved = service
                .resolve_session_id(agent_name, Some(team), paths.user(), session_id)
                .await?;
            if !json {
                println!("📦 Using active session: {resolved}");
            }
            let packet = crate::ipc::RequestPacket::SessionCompact {
                request_id: 1,
                agent: agent_name.to_string(),
                team: Some(team.to_string()),
                session_id: resolved.clone(),
                dry_run,
                instruction: instruction.clone(),
            };
            let response = ipc_request(packet).await?;
            match response {
                // Dry-run: the daemon sends a dedicated response that
                // carries the full DryRunReport fields. We don't reuse
                // SessionCompacted here because that variant's
                // `messages_compacted` means "messages folded into the
                // summary", which is meaningless for a no-op preview.
                crate::ipc::ResponsePacket::SessionCompactDryRun {
                    session_id: dry_session_id,
                    estimated_tokens,
                    context_window,
                    percent,
                    message_count,
                    messages_to_compact,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            render_dry_run_json(
                                &dry_session_id,
                                estimated_tokens,
                                context_window,
                                percent,
                                message_count,
                                messages_to_compact,
                            )
                        );
                    } else {
                        let report = crate::session::compaction::cli::DryRunReport {
                            estimated_tokens,
                            context_window,
                            percent,
                            message_count,
                            messages_to_compact,
                        };
                        render_compact_dry_run(&dry_session_id, &report);
                    }
                    Ok(())
                }
                crate::ipc::ResponsePacket::SessionCompacted {
                    messages_compacted,
                    tokens_saved,
                    tokens_before,
                    tokens_after,
                    ..
                } => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "success": true,
                                "session_id": resolved,
                                "messages_compacted": messages_compacted,
                                "tokens_before": tokens_before,
                                "tokens_after": tokens_after,
                                "tokens_saved": tokens_saved,
                            })
                        );
                    } else {
                        render_compact_success(
                            &resolved,
                            messages_compacted,
                            tokens_saved,
                            tokens_before,
                            tokens_after,
                            instruction.is_some(),
                        );
                    }
                    Ok(())
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}

// ================================================================================
// Command Implementations (thin delegates)
// ================================================================================

async fn show_session(
    service: &SessionService,
    team: &str,
    agent: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(entry) = service
        .get_session_synced(agent, Some(team), session_id)
        .await?
    else {
        return Err(anyhow::anyhow!(
            "Session '{session_id}' not found for agent '{agent}'"
        ));
    };

    let history_events = if show_history {
        load_session_history(service, agent, team, session_id)
            .await
            .ok()
    } else {
        None
    };

    if json {
        let events_slice = history_events.as_deref();
        render_session_show_json(&entry, events_slice)?;
    } else {
        render_session_details(&entry, team, agent);
        if let Some(mut events) = history_events {
            events.reverse();
            render_session_history(&events);
        } else if show_history {
            println!();
            println!("   ⚠️  Failed to load history from JSONL file");
        } else {
            println!();
            println!("   💡 Use --history to see full message history");
        }
    }
    Ok(())
}

async fn load_session_history(
    service: &SessionService,
    agent: &str,
    team: &str,
    session_id: &str,
) -> Result<Vec<crate::session::presentation::HistoryDisplayEntry>> {
    let result = service
        .get_history(agent, Some(team), session_id, HistoryQuery::default())
        .await?;
    Ok(result
        .events
        .into_iter()
        .filter_map(history_event_to_display)
        .collect())
}

async fn switch_session(
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    session_id: &str,
    user: &str,
    json: bool,
) -> anyhow::Result<()> {
    let sessions_dir = paths.resolver().agent_sessions_dir(agent, Some(team));
    tokio::fs::create_dir_all(&sessions_dir).await?;

    let mut manager =
        crate::session::SessionManager::for_cli(paths.resolver.clone(), agent, Some(team), user);

    let _ = manager
        .get_session_metadata(session_id)
        .await
        .map_err(|_| anyhow::anyhow!("Session '{session_id}' not found for agent '{agent}'"))?;

    let peer = Principal::User(user.to_string());
    manager.switch_session(&peer, session_id).await?;

    if json {
        println!(
            "{{\"success\": true, \"session_id\": \"{session_id}\", \"agent\": \"{agent}\", \"team\": \"{team}\"}}"
        );
    } else {
        render_switch_success(team, agent, session_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_commands_compact_variant_exists() {
        let cmd = SessionCommands::Compact {
            agent: "test-agent".to_string(),
            session_id: Some("sess_123".to_string()),
            team: Some("test-team".to_string()),
            dry_run: true,
            instruction: Some("preserve API decisions".to_string()),
        };

        match cmd {
            SessionCommands::Compact {
                agent,
                session_id,
                team,
                dry_run,
                instruction,
            } => {
                assert_eq!(agent, "test-agent");
                assert_eq!(session_id, Some("sess_123".to_string()));
                assert_eq!(team, Some("test-team".to_string()));
                assert!(dry_run);
                assert_eq!(instruction, Some("preserve API decisions".to_string()));
            }
            _ => panic!("Expected Compact variant"),
        }
    }

    #[test]
    fn test_session_commands_compact_defaults() {
        let cmd = SessionCommands::Compact {
            agent: "myagent".to_string(),
            session_id: None,
            team: None,
            dry_run: false,
            instruction: None,
        };

        match cmd {
            SessionCommands::Compact {
                agent,
                session_id,
                dry_run,
                instruction,
                ..
            } => {
                assert_eq!(agent, "myagent");
                assert!(session_id.is_none());
                assert!(!dry_run);
                assert!(instruction.is_none());
            }
            _ => panic!("Expected Compact variant"),
        }
    }

    // Regression for issue 030: `message_count` and
    // `messages_to_compact` must come from a dedicated dry-run
    // response, not be overloaded from the real-compaction
    // `messages_compacted` field (which means "messages folded into
    // the summary" and is meaningless for a no-op preview).

    #[test]
    fn test_render_dry_run_json_preserves_message_counts() {
        let json = render_dry_run_json(
            "sess-abc", /* estimated_tokens */ 622, /* context_window  */ 128_000,
            /* percent         */ 0, /* message_count   */ 12,
            /* messages_to_compact */ 10,
        );
        assert_eq!(json["success"], serde_json::Value::Bool(true));
        assert_eq!(json["dry_run"], serde_json::Value::Bool(true));
        assert_eq!(json["session_id"], "sess-abc");
        assert_eq!(json["estimated_tokens"], 622);
        assert_eq!(json["context_window"], 128_000);
        assert_eq!(json["percent"], 0);
        assert_eq!(json["message_count"], 12);
        assert_eq!(json["messages_to_compact"], 10);
    }

    #[test]
    fn test_render_dry_run_json_separates_message_count_from_messages_to_compact() {
        // When there's only a system message + one user/assistant
        // pair, message_count = 3 but messages_to_compact = 1
        // (kept = 2, compacted = len() - 2). The two fields are
        // semantically distinct and must round-trip independently.
        let json = render_dry_run_json("s", 0, 128_000, 0, 3, 1);
        assert_ne!(
            json["message_count"], json["messages_to_compact"],
            "message_count and messages_to_compact must remain distinct fields",
        );
    }

    #[test]
    fn test_dry_run_json_no_messages_compacted_field() {
        // The pre-fix bug over-emitted the real-compaction field name
        // `messages_compacted`. The dry-run JSON shape must not
        // contain it; callers parsing dry-run output should not see a
        // field that means "messages folded into the summary".
        let json = render_dry_run_json("s", 0, 128_000, 0, 0, 0);
        assert!(
            json.get("messages_compacted").is_none(),
            "dry-run JSON must not emit the real-compaction `messages_compacted` field",
        );
    }
}
