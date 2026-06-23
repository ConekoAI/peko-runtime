//! CLI presentation helpers for the `peko team` subcommand.
//!
//! Pure stdout formatting — no service calls, no IPC, no registry
//! I/O. Lives in its own module so `team/mod.rs` (the command
//! dispatcher) is just CLI plumbing and stays readable.

use anyhow::Result;

use crate::common::time::format_timestamp;
use crate::common::types::team::{TeamCreationResult, TeamDeletionResult, TeamInfo};

pub(super) fn render_team_created(result: &TeamCreationResult, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.metadata.name,
                "path": result.path.display().to_string(),
            })
        );
    } else {
        println!("✅ Created team '{}'", result.metadata.name);
        println!("   Path: {}", result.path.display());
        if let Some(desc) = &result.metadata.description {
            println!("   Description: {desc}");
        }
        println!();
        println!("   You can now create agents in this team:");
        println!(
            "     peko agent create {}/<agent-name>",
            result.metadata.name
        );
    }
}

pub(super) fn render_team_list(teams: &[TeamInfo], long: bool, json: bool) {
    if json {
        let teams_json: Vec<_> = teams
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "agent_count": t.agent_count,
                    "description": t.metadata.description.clone(),
                    "created_at": t.metadata.created_at.clone(),
                })
            })
            .collect();
        println!("{}", serde_json::json!({"teams": teams_json}));
    } else if teams.is_empty() {
        println!("No teams found.");
        println!("Create one with: peko team create <name>");
    } else {
        println!("📁 Teams ({} found):", teams.len());
        println!();

        for team in teams {
            let is_default = team.name == "default";
            let icon = if is_default { "⭐" } else { "📁" };

            if long {
                println!("{} {}", icon, team.name);
                if let Some(ref desc) = team.metadata.description {
                    println!("   Description: {desc}");
                }
                println!(
                    "   Created: {}",
                    format_timestamp(&team.metadata.created_at)
                );
                println!("   Agents: {}", team.agent_count);
                println!("   Path: {}", team.path.display());
                println!();
            } else {
                let desc_marker = if team.metadata.description.is_some() {
                    " •"
                } else {
                    ""
                };
                println!(
                    "{} {}{} ({} agents)",
                    icon, team.name, desc_marker, team.agent_count
                );
            }
        }
    }
}

pub(super) fn render_team_show(
    team: &TeamInfo,
    agents: &[(String, crate::agents::agent_config::AgentConfig)],
    json: bool,
) {
    if json {
        let agents_json: Vec<_> = agents
            .iter()
            .map(|(name, config)| {
                serde_json::json!({
                    "name": name,
                    "preferred_provider": config.preferred_provider_id,
                    "preferred_model": config.preferred_model_id,
                    "description": config.description,
                })
            })
            .collect();

        println!(
            "{}",
            serde_json::json!({
                "name": team.name,
                "path": team.path.display().to_string(),
                "description": team.metadata.description.clone(),
                "created_at": team.metadata.created_at.clone(),
                "agents": agents_json,
                "agent_count": team.agent_count,
            })
        );
    } else {
        println!("📁 Team: {}", team.name);
        if let Some(ref desc) = team.metadata.description {
            println!("   Description: {desc}");
        }
        println!(
            "   Created: {}",
            format_timestamp(&team.metadata.created_at)
        );
        println!("   Path: {}", team.path.display());
        println!();

        if team.agent_count == 0 {
            println!("   No agents in this team.");
            println!(
                "   Create one with: peko team create {}/<agent-name>",
                team.name
            );
        } else {
            println!("   Agents ({}):", team.agent_count);
            for (agent_name, config) in agents {
                println!("   📦 {agent_name}");
                if let Some(ref desc) = config.description {
                    println!("      {desc}");
                }
                println!(
                    "      Preferred provider: {}",
                    config.preferred_provider_id.as_deref().unwrap_or("<none>")
                );
            }
        }
    }
}

pub(super) fn render_team_deleted(result: &TeamDeletionResult, json: bool) {
    if json {
        println!(
            "{{\"success\": true, \"name\": \"{}\", \"agents_deleted\": {}}}",
            result.name, result.agents_deleted
        );
    } else {
        println!("✅ Deleted team '{}'", result.name);
        if result.agents_deleted > 0 {
            println!("   Removed {} agent(s)", result.agents_deleted);
        }
    }
}

pub(super) fn render_team_exported(
    result: &crate::common::types::team::TeamExportResult,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.name,
                "output_path": result.output_path.display().to_string(),
                "agent_count": result.agent_count,
            })
        );
    } else {
        println!("📦 Exported team '{}'", result.name);
        println!("   Agents: {}", result.agent_count);
        println!("   Output: {}", result.output_path.display());
    }
}

pub(super) fn render_team_imported(
    result: &crate::common::types::team::TeamImportResult,
    json: bool,
) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "name": result.name,
                "path": result.path.display().to_string(),
                "agents_imported": result.agents_imported,
            })
        );
    } else {
        println!("📥 Imported team '{}'", result.name);
        println!("   Agents: {}", result.agents_imported);
        println!("   Path: {}", result.path.display());
    }
}

pub(super) fn confirm_team_deletion(name: &str, agent_count: usize) -> Result<bool> {
    println!("⚠️  This will permanently delete team '{name}'.");
    if agent_count > 0 {
        println!("   It contains {agent_count} agent(s) that will also be deleted.");
    }
    println!("   This action cannot be undone.");
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

pub(super) fn confirm_team_move(
    old_name: &str,
    new_name: &str,
    agent_count: usize,
) -> Result<bool> {
    println!("⚠️  This will rename team '{old_name}' to '{new_name}'.");
    if agent_count > 0 {
        println!("   It contains {agent_count} agent(s) that will be moved.");
    }
    print!("   Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    Ok(input.trim().eq_ignore_ascii_case("y"))
}

pub(super) fn render_team_pushed(result: &crate::common::types::team::TeamPushResult, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "team_name": result.name,
                "registry_ref": result.registry_ref,
                "manifest": {
                    "name": result.manifest_name,
                    "version": result.manifest_version,
                    "digest": result.manifest_digest,
                    "kind": result.kind,
                    "layers": result.layers,
                    "total_size": result.total_size,
                }
            })
        );
    } else {
        println!("Pushed team '{}' to {}", result.name, result.registry_ref);
        println!("  Manifest: {} ({})", result.manifest_digest, result.kind);
        println!("  Layers: {}", result.layers);
    }
}

pub(super) fn render_team_pull_progress(event: &crate::registry::client::ProgressEvent) {
    match event {
        crate::registry::client::ProgressEvent::Resolving { .. } => {
            println!("  Resolving...");
        }
        crate::registry::client::ProgressEvent::Pulling {
            layer,
            bytes_received,
            bytes_total,
        } => {
            if bytes_received == bytes_total && bytes_received != &Some(0) {
                println!("  Layer {}  ✓ downloaded", &layer[..19.min(layer.len())]);
            } else if bytes_received == &Some(0) {
                println!("  Layer {}  → downloading", &layer[..19.min(layer.len())]);
            }
        }
        crate::registry::client::ProgressEvent::Verifying { layer } => {
            println!("  Verifying {layer}...");
        }
        crate::registry::client::ProgressEvent::Done { .. } => {
            println!("Done.");
        }
        crate::registry::client::ProgressEvent::Error { code, message } => {
            eprintln!("  Error: {code} - {message}");
        }
        _ => {}
    }
}

pub(super) fn render_team_pull_report(
    result: &crate::common::types::team::TeamPullResult,
    extension_results: &crate::common::types::team::TeamExtensionPullResult,
    json: bool,
) {
    if json {
        let output = serde_json::json!({
            "success": true,
            "registry_ref": result.registry_ref,
            "name": result.name,
            "path": result.path.display().to_string(),
            "agents_imported": result.agents_imported,
            "extensions": {
                "pulled": extension_results.pulled,
                "already_present": extension_results.already_present,
                "failed": extension_results.failed.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            },
            "manifest": {
                "name": result.manifest_name,
                "version": result.manifest_version,
                "digest": result.manifest_digest,
                "kind": result.manifest_kind,
                "layers": result.manifest_layers,
                "total_size": result.manifest_total_size,
            }
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_default()
        );
    } else {
        println!("📥 Imported team '{}'", result.name);
        println!("   Agents: {}", result.agents_imported);
        println!("   Path: {}", result.path.display());
        if !extension_results.pulled.is_empty() {
            println!("   Extensions pulled: {}", extension_results.pulled.len());
            for ext in &extension_results.pulled {
                println!("     ✓ {}", ext);
            }
        }
        if !extension_results.already_present.is_empty() {
            println!(
                "   Extensions already present: {}",
                extension_results.already_present.len()
            );
            for ext in &extension_results.already_present {
                println!("     ✓ {}", ext);
            }
        }
    }
}

pub(super) fn render_team_pull_extension_warnings(failed: &[(String, String)]) {
    if failed.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "Warning: {} extension(s) could not be pulled:",
        failed.len()
    );
    for (ext_id, err) in failed {
        eprintln!("  - {}: {}", ext_id, err);
    }
}

pub(super) fn render_cancelled(json: bool) {
    if json {
        println!("{{\"success\": false, \"reason\": \"cancelled\"}}");
    } else {
        println!("Cancelled.");
    }
}

pub(super) fn render_team_moved(old_name: &str, new_name: &str, agents_moved: usize, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "success": true,
                "old_name": old_name,
                "new_name": new_name,
                "agents_moved": agents_moved,
            })
        );
    } else {
        println!("Moved team '{}' -> '{}'", old_name, new_name);
    }
}

pub(super) fn render_team_transfer_success(name: &str, new_owner: &str, json: bool) {
    if json {
        println!("{{\"success\": true, \"team\": \"{name}\", \"new_owner\": \"{new_owner}\"}}");
    } else {
        println!("✅ Transferred ownership of team '{name}' to {new_owner}");
    }
}

pub(super) fn render_permission_change(team: &str, subject: &str, action: &str, json: bool) {
    if json {
        println!("{{\"success\": true, \"team\": \"{team}\", \"subject\": \"{subject}\"}}");
    } else {
        println!("✅ {action} permission on team '{team}' for {subject}");
    }
}

pub(super) fn render_team_permissions(
    name: &str,
    metadata: &crate::common::types::team::TeamMetadata,
    json: bool,
) {
    if json {
        let grants: Vec<_> = metadata
            .permissions
            .iter()
            .map(|g| {
                serde_json::json!({
                    "subject": g.subject.to_string(),
                    "permission": g.permission,
                    "granted_at": g.granted_at,
                    "granted_by": g.granted_by.to_string(),
                })
            })
            .collect();
        println!(
            "{{\"team\": \"{name}\", \"owner\": \"{}\", \"permissions\": {}}}",
            metadata.owner,
            serde_json::to_string(&grants).unwrap_or_default()
        );
    } else {
        println!("📁 Team: {}", name);
        println!("   Owner: {}", metadata.owner);
        if metadata.permissions.is_empty() {
            println!("   Permissions: none");
        } else {
            println!("   Permissions:");
            for g in &metadata.permissions {
                println!(
                    "     - {} ({}): {:?} (by {} at {})",
                    g.subject,
                    g.subject.kind(),
                    g.permission,
                    g.granted_by,
                    g.granted_at
                );
            }
        }
    }
}

pub(super) fn render_team_push_progress(event: &crate::registry::client::ProgressEvent) {
    match event {
        crate::registry::client::ProgressEvent::Resolving { .. } => {}
        crate::registry::client::ProgressEvent::Pushing {
            layer,
            bytes_sent,
            bytes_total,
        } => {
            if bytes_sent == bytes_total && bytes_sent != &Some(0) {
                println!("  Layer {}  ✓ uploaded", &layer[..19.min(layer.len())]);
            } else if bytes_sent == &Some(0) {
                println!("  Layer {}  → uploading", &layer[..19.min(layer.len())]);
            }
        }
        crate::registry::client::ProgressEvent::Done { .. } => {
            println!("  Manifest         pushed");
            println!("Done.");
        }
        crate::registry::client::ProgressEvent::Error { code, message } => {
            eprintln!("  Error: {code} - {message}");
        }
        _ => {}
    }
}
