//! Built-in `/help` slash command renderer.

use crate::commands::GlobalPaths;
use crate::ipc::packet::{ExtensionSummary, ResponsePacket};
use crate::ipc::{DaemonClient, RequestPacket};
use crate::principal::config::{AllowedExtensions, PrincipalConfig};
use anyhow::{Context, Result};
use std::collections::BTreeMap;

/// Description shown for the built-in `/help` slash command.
pub const HELP_DESCRIPTION: &str =
    "Show built-in slash commands, enabled skills, and principal metadata";

/// Handle `/help` by loading the principal config client-side, fetching
/// the extension list from the daemon, filtering by the principal's
/// allowlist, and printing grouped output.
pub async fn handle_help(principal_name: &str, paths: &GlobalPaths, json: bool) -> Result<()> {
    let config = load_principal_config(principal_name, paths)
        .with_context(|| format!("Failed to load config for principal '{principal_name}'"))?;

    let client = DaemonClient::connect()
        .await
        .context("Failed to connect to daemon; is `peko daemon start` running?")?;
    let extensions = fetch_enabled_extensions(&client)
        .await
        .context("Failed to fetch extension list from daemon")?;

    let allowed = &config.allowed_extensions;
    let filtered: Vec<&ExtensionSummary> = extensions
        .iter()
        .filter(|ext| is_extension_allowed(ext, allowed))
        .collect();

    if json {
        render_json(principal_name, &config, allowed, &filtered)?;
    } else {
        render_human(principal_name, &config, allowed, &filtered)?;
    }

    Ok(())
}

fn load_principal_config(principal_name: &str, paths: &GlobalPaths) -> Result<PrincipalConfig> {
    let config_path = paths.principal_config(principal_name);
    let content = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read principal config at {config_path:?}"))?;
    let config: PrincipalConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse principal config at {config_path:?}"))?;
    Ok(config)
}

async fn fetch_enabled_extensions(client: &DaemonClient) -> Result<Vec<ExtensionSummary>> {
    let request_id = 0; // request_response assigns its own id internally
    let packet = RequestPacket::ExtensionList {
        request_id,
        enabled_only: true,
        ext_type: None,
    };
    match client.request_response(packet).await? {
        ResponsePacket::ExtensionList { extensions, .. } => Ok(extensions),
        other => anyhow::bail!("Unexpected response from daemon: {other:?}"),
    }
}

/// Returns true if the extension id or name matches any entry in the
/// principal's allowlist (case-insensitive). An empty allowlist is
/// treated as allow-nothing, consistent with the rest of the runtime.
fn is_extension_allowed(ext: &ExtensionSummary, allowed: &AllowedExtensions) -> bool {
    if allowed.0.is_empty() {
        return false;
    }

    let id_lower = ext.id.to_ascii_lowercase();
    let name_lower = ext.name.to_ascii_lowercase();

    allowed
        .0
        .iter()
        .any(|entry| {
            let entry_lower = entry.to_ascii_lowercase();
            entry_lower == id_lower || entry_lower == name_lower
        })
}

fn render_human(
    principal_name: &str,
    config: &PrincipalConfig,
    allowed: &AllowedExtensions,
    extensions: &[&ExtensionSummary],
) -> Result<()> {
    println!("Peko /help\n");
    println!("Principal: {}", principal_name);
    if let Some(display) = config.identity.display_name.as_deref().filter(|s| !s.is_empty()) {
        println!("Display name: {}", display);
    }
    if let Some(desc) = config.identity.description.as_deref().filter(|s| !s.is_empty()) {
        println!("Description: {}", desc);
    }

    let allowed_list = allowed
        .0
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "Allowed extensions ({}): {}",
        allowed.0.len(),
        if allowed_list.is_empty() { "(none)" } else { &allowed_list }
    );

    println!("\nBuilt-in slash commands:");
    println!("  /help    {}", HELP_DESCRIPTION);

    let grouped = group_by_ext_type(extensions);

    print_group("Enabled skills", grouped.get("skill"));
    print_group("Enabled MCP servers", grouped.get("mcp"));
    print_group("Enabled gateways", grouped.get("gateway"));
    print_group("Enabled extensions", grouped.get("tool"));

    // Any other extension types not covered above.
    for (&ext_type, items) in &grouped {
        if matches!(ext_type, "skill" | "mcp" | "gateway" | "tool") {
            continue;
        }
        let title = format!("Enabled {}", pluralize(ext_type));
        print_group(&title, Some(items));
    }

    Ok(())
}

fn render_json(
    principal_name: &str,
    config: &PrincipalConfig,
    allowed: &AllowedExtensions,
    extensions: &[&ExtensionSummary],
) -> Result<()> {
    let grouped = group_by_ext_type(extensions);

    let output = serde_json::json!({
        "principal": principal_name,
        "display_name": config.identity.display_name,
        "description": config.identity.description,
        "allowed_extensions": allowed.0,
        "built_in_slash_commands": [
            {
                "name": "help",
                "description": HELP_DESCRIPTION,
                "argument_hint": null,
            }
        ],
        "enabled_extensions": grouped
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::Array(summary_json_vec(v))))
            .collect::<serde_json::Map<String, serde_json::Value>>(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn group_by_ext_type<'a>(
    extensions: &[&'a ExtensionSummary],
) -> BTreeMap<&'a str, Vec<&'a ExtensionSummary>> {
    let mut grouped: BTreeMap<&str, Vec<&ExtensionSummary>> = BTreeMap::new();
    for ext in extensions {
        grouped
            .entry(ext.ext_type.as_str())
            .or_default()
            .push(ext);
    }
    grouped
}

fn print_group(title: &str, items: Option<&Vec<&ExtensionSummary>>) {
    println!("\n{}:", title);
    match items {
        None => println!("  (none)"),
        Some(items) if items.is_empty() => println!("  (none)"),
        Some(items) => {
            for ext in items {
                println!(
                    "  {} | {} | {} | {}",
                    ext.id, ext.ext_type, ext.name, ext.source
                );
            }
        }
    }
}

fn summary_json_vec(items: &[&ExtensionSummary]) -> Vec<serde_json::Value> {
    items
        .iter()
        .map(|ext| {
            serde_json::json!({
                "id": ext.id,
                "name": ext.name,
                "ext_type": ext.ext_type,
                "version": ext.version,
                "source": ext.source,
                "enabled": ext.enabled,
                "runtime": ext.runtime,
                "description": ext.description,
            })
        })
        .collect()
}

fn pluralize(word: &str) -> String {
    if word.ends_with('s') {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary(id: &str, name: &str, ext_type: &str) -> ExtensionSummary {
        ExtensionSummary {
            id: id.to_string(),
            name: name.to_string(),
            ext_type: ext_type.to_string(),
            version: "1.0.0".to_string(),
            source: "installed".to_string(),
            enabled: true,
            runtime: "running".to_string(),
            description: format!("The {name} extension"),
        }
    }

    #[test]
    fn is_extension_allowed_matches_id_case_insensitive() {
        let allowed = AllowedExtensions(vec!["Docker".to_string()]);
        let ext = sample_summary("docker", "Docker", "skill");
        assert!(is_extension_allowed(&ext, &allowed));
    }

    #[test]
    fn is_extension_allowed_matches_name_case_insensitive() {
        let allowed = AllowedExtensions(vec!["docker".to_string()]);
        let ext = sample_summary("pkg", "Docker", "skill");
        assert!(is_extension_allowed(&ext, &allowed));
    }

    #[test]
    fn is_extension_allowed_empty_allowlist_denies_all() {
        let allowed = AllowedExtensions::new();
        let ext = sample_summary("docker", "Docker", "skill");
        assert!(!is_extension_allowed(&ext, &allowed));
    }

    #[test]
    fn is_extension_allowed_unlisted_denied() {
        let allowed = AllowedExtensions(vec!["bash".to_string()]);
        let ext = sample_summary("docker", "Docker", "skill");
        assert!(!is_extension_allowed(&ext, &allowed));
    }
}
