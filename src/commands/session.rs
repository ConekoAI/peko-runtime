//! Session Management Commands
//!
//! Thin dispatch + rendering layer. All business logic is delegated to
//! `SessionService` and `SessionCompactor`. Presentation lives in
//! `session::presentation`.

use crate::commands::GlobalPaths;
use crate::common::identifiers::parse_agent_identifier_with_override;
use crate::common::services::session_service::{HistoryQuery, SessionService};
use crate::session::presentation::{
    history_event_to_display, render_branch_success, render_compact_dry_run,
    render_compact_success, render_delete_prompt, render_delete_success,
    render_session_details, render_session_history, render_session_list,
    render_session_list_json, render_session_show_json, render_switch_success,
};
use crate::session::types::Peer;
use anyhow::Result;
use clap::Subcommand;

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
            list_sessions(&service, paths, team, agent_name, json).await
        }
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
            branch_session(&service, team, agent_name, &resolved, label, json).await
        }
        SessionCommands::Remove {
            agent,
            session_id,
            team,
            force,
        } => {
            let (team, agent_name) = parse_agent_identifier_with_override(&agent, team.as_deref())?;
            delete_session(&service, team, agent_name, &session_id, force, json).await
        }
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
            compact_session(&service, team, agent_name, &resolved, dry_run, instruction, json)
                .await
        }
    }
}

// ================================================================================
// Command Implementations (thin delegates)
// ================================================================================

async fn list_sessions(
    service: &SessionService,
    paths: &GlobalPaths,
    team: &str,
    agent: &str,
    json: bool,
) -> anyhow::Result<()> {
    let sessions = service.list_sessions_synced(agent, Some(team)).await?;

    let mut manager =
        crate::session::SessionManager::for_cli(paths.resolver.clone(), agent, Some(team), paths.user());
    let peer = Peer::User(paths.user().to_string());
    let active_session_id = manager.get_active_session_id(&peer).await.ok().flatten();

    if json {
        render_session_list_json(&sessions, team, agent, active_session_id.as_deref())?;
    } else {
        render_session_list(&sessions, team, agent, active_session_id.as_deref());
    }
    Ok(())
}

async fn show_session(
    service: &SessionService,
    team: &str,
    agent: &str,
    session_id: &str,
    show_history: bool,
    json: bool,
) -> anyhow::Result<()> {
    let Some(entry) = service.get_session_synced(agent, Some(team), session_id).await? else {
        return Err(anyhow::anyhow!(
            "Session '{session_id}' not found for agent '{agent}'"
        ));
    };

    let history_events = if show_history {
        load_session_history(service, agent, team, session_id).await.ok()
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
    Ok(result.events.into_iter().filter_map(history_event_to_display).collect())
}

async fn branch_session(
    service: &SessionService,
    team: &str,
    agent: &str,
    session_id: &str,
    label: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let result = service
        .branch_session(agent, Some(team), session_id, label.clone())
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        render_branch_success(team, agent, session_id, &result.new_session_id, label.as_deref());
    }
    Ok(())
}

async fn delete_session(
    service: &SessionService,
    team: &str,
    agent: &str,
    session_id: &str,
    force: bool,
    json: bool,
) -> anyhow::Result<()> {
    let metadata = service.get_session_synced(agent, Some(team), session_id).await?;

    if !force {
        render_delete_prompt(session_id, metadata.as_ref());
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let deleted = service.delete_session(agent, Some(team), session_id).await?;

    if json {
        let deleted_items = if deleted { vec!["jsonl", "index"] } else { vec![] };
        println!("{{\"success\": true, \"deleted\": {deleted_items:?}}}");
    } else {
        render_delete_success(session_id, deleted);
    }
    Ok(())
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

    let peer = Peer::User(user.to_string());
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

async fn compact_session(
    service: &SessionService,
    team: &str,
    agent: &str,
    session_id: &str,
    dry_run: bool,
    instruction: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let sessions_dir = service.get_sessions_dir(agent, Some(team)).await?;
    if !sessions_dir.exists() {
        return Err(anyhow::anyhow!("Agent '{agent}' not found in team '{team}'"));
    }

    let mut session = service
        .open_session(agent, Some(team), session_id, "default")
        .await?;

    let compactor = crate::compaction::cli::SessionCompactor::new();

    if dry_run {
        let report = compactor.dry_run(&session, instruction.clone()).await?;
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "dry_run": true,
                    "session_id": session_id,
                    "estimated_tokens": report.estimated_tokens,
                    "context_window": report.context_window,
                    "percent": report.percent,
                    "message_count": report.message_count,
                })
            );
        } else {
            render_compact_dry_run(session_id, &report);
        }
        return Ok(());
    }

    let result = compactor.compact(&mut session, instruction.clone()).await?;

    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "session_id": session_id,
                "messages_compacted": result.entry.messages_compacted,
                "tokens_before": result.entry.tokens_before,
                "tokens_after": result.entry.tokens_after,
                "tokens_saved": result.tokens_saved,
            })
        );
    } else {
        render_compact_success(
            session_id,
            result.entry.messages_compacted,
            result.tokens_saved,
            result.entry.tokens_before,
            result.entry.tokens_after,
            instruction.is_some(),
        );
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
}
