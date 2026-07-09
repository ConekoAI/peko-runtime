//! Built-in `/help` slash command renderer for the daemon-side slash
//! dispatcher.

use crate::common::types::OutputFormat;
use crate::extensions::framework::manager::ExtensionManager;
use crate::extensions::framework::services::Services as ExtensionServices;
use crate::ipc::packet::ExtensionSummary;
use crate::principal::config::{AllowedExtensions, PrincipalConfig};
use crate::principal::Principal;
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Description shown for the built-in `/help` slash command.
pub const HELP_DESCRIPTION: &str =
    "Show built-in slash commands, enabled skills, and principal metadata";

/// Handle `/help` for the given principal and output format.
pub async fn handle_help(
    principal: &Principal,
    extension_manager: &Arc<RwLock<ExtensionManager>>,
    extension_services: &Arc<ExtensionServices>,
    format: OutputFormat,
) -> Result<String> {
    // Reload config from disk so /help reflects recent edits (e.g. a user
    // adding an extension to the allowlist while the daemon is running).
    // If the on-disk file is missing or corrupt, fall back to the cached
    // in-memory config rather than failing the slash command.
    let config = match reload_config(principal).await {
        Some(cfg) => cfg,
        None => principal.config.read().await.clone(),
    };
    let allowed = &config.allowed_extensions;
    let extensions = list_enabled_extensions(extension_manager, extension_services).await?;
    let filtered: Vec<&ExtensionSummary> = extensions
        .iter()
        .filter(|ext| is_extension_allowed(ext, allowed))
        .collect();

    match format {
        OutputFormat::Human => Ok(render_human(&config.name, &config, allowed, &filtered)),
        OutputFormat::Json => render_json(&config.name, &config, allowed, &filtered),
    }
}

/// Query enabled extensions from the daemon's extension manager and
/// built-in extension services. Mirrors the IPC `ExtensionList` handler.
async fn list_enabled_extensions(
    extension_manager: &Arc<RwLock<ExtensionManager>>,
    extension_services: &Arc<ExtensionServices>,
) -> Result<Vec<ExtensionSummary>> {
    {
        let mut manager = extension_manager.write().await;
        if let Err(e) = manager.load_all().await {
            tracing::warn!("Failed to reload extensions for /help: {e}");
        }
    }
    let manager = extension_manager.read().await;
    let builtins = extension_services.list_builtin_extensions().await;
    let installed = manager.list_extensions();

    let mut extensions = Vec::new();

    for b in &builtins {
        extensions.push(ExtensionSummary {
            id: b.id.clone(),
            name: b.name.clone(),
            ext_type: b.ext_type.clone(),
            version: "n/a".to_string(),
            source: "built-in".to_string(),
            enabled: b.enabled,
            runtime: "n/a".to_string(),
            description: String::new(),
        });
    }

    for ext in installed {
        extensions.push(ExtensionSummary {
            id: ext.manifest.id.0.clone(),
            name: ext.manifest.name.clone(),
            ext_type: ext.extension_type.clone(),
            version: ext.manifest.version.clone(),
            source: "installed".to_string(),
            enabled: true,
            runtime: "n/a".to_string(),
            description: ext.manifest.description.clone(),
        });
    }

    Ok(extensions)
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

    allowed.0.iter().any(|entry| {
        let entry_lower = entry.to_ascii_lowercase();
        entry_lower == id_lower || entry_lower == name_lower
    })
}

fn render_human(
    principal_name: &str,
    config: &PrincipalConfig,
    allowed: &AllowedExtensions,
    extensions: &[&ExtensionSummary],
) -> String {
    let mut out = String::new();
    out.push_str("Peko /help\n\n");
    out.push_str(&format!("Principal: {}\n", principal_name));
    if let Some(display) = config
        .identity
        .display_name
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("Display name: {}\n", display));
    }
    if let Some(desc) = config
        .identity
        .description
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("Description: {}\n", desc));
    }

    let allowed_list = allowed
        .0
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "Allowed extensions ({}): {}\n",
        allowed.0.len(),
        if allowed_list.is_empty() {
            "(none)"
        } else {
            &allowed_list
        }
    ));

    out.push_str("\nBuilt-in slash commands:\n");
    out.push_str(&format!("  /help    {}\n", HELP_DESCRIPTION));

    let grouped = group_by_ext_type(extensions);

    print_group(&mut out, "Enabled skills", grouped.get("skill"));
    print_group(&mut out, "Enabled MCP servers", grouped.get("mcp"));
    print_group(&mut out, "Enabled gateways", grouped.get("gateway"));
    print_group(&mut out, "Enabled extensions", grouped.get("tool"));

    // Any other extension types not covered above.
    for (&ext_type, items) in &grouped {
        if matches!(ext_type, "skill" | "mcp" | "gateway" | "tool") {
            continue;
        }
        let title = format!("Enabled {}", pluralize(ext_type));
        print_group(&mut out, &title, Some(items));
    }

    out
}

fn render_json(
    principal_name: &str,
    config: &PrincipalConfig,
    allowed: &AllowedExtensions,
    extensions: &[&ExtensionSummary],
) -> Result<String> {
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

    Ok(serde_json::to_string_pretty(&output)?)
}

fn group_by_ext_type<'a>(
    extensions: &[&'a ExtensionSummary],
) -> BTreeMap<&'a str, Vec<&'a ExtensionSummary>> {
    let mut grouped: BTreeMap<&str, Vec<&ExtensionSummary>> = BTreeMap::new();
    for ext in extensions {
        grouped.entry(ext.ext_type.as_str()).or_default().push(ext);
    }
    grouped
}

fn print_group(out: &mut String, title: &str, items: Option<&Vec<&ExtensionSummary>>) {
    out.push_str(&format!("\n{}:\n", title));
    match items {
        None => out.push_str("  (none)\n"),
        Some(items) if items.is_empty() => out.push_str("  (none)\n"),
        Some(items) => {
            for ext in items {
                out.push_str(&format!(
                    "  {} | {} | {} | {}\n",
                    ext.id, ext.ext_type, ext.name, ext.source
                ));
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

/// Reload the Principal's config from its on-disk `principal.toml`, if
/// possible. Returns `None` when the file cannot be read or parsed so the
/// caller can fall back to the in-memory copy.
async fn reload_config(principal: &Principal) -> Option<PrincipalConfig> {
    let path = principal.workspace_path.join("principal.toml");
    let raw = tokio::fs::read_to_string(&path).await.ok()?;
    toml::from_str::<PrincipalConfig>(&raw).ok()
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
