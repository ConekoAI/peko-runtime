//! Built-in `/help` slash command renderer for the daemon-side slash
//! dispatcher.

use crate::extensions::framework::services::Services as ExtensionServices;
use crate::extensions::framework::store::ExtensionStore;
use crate::extensions::framework::types::{Capabilities, Capability};
use crate::ipc::packet::ExtensionSummary;
use crate::principal::config::PrincipalConfig;
use crate::principal::runtime::OutputFormat;
use crate::principal::Principal;
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Description shown for the built-in `/help` slash command.
pub const HELP_DESCRIPTION: &str =
    "Show built-in slash commands, enabled skills, and principal metadata";

/// Handle `/help` for the given principal and output format.
///
/// ADR-050 D3 ("Presence = Visibility"): the catalog surfaced here is
/// unfiltered — every enabled/installed extension appears. Capability
/// grants still gate tool *execution* through the F37 funnel
/// (`tool_registry::is_tool_enabled`), so a default principal can see
/// the catalog but cannot invoke tools they haven't been granted. The
/// "Allowed extensions (N): …" line summarizes active grants so the
/// user can tell availability from authorization.
pub async fn handle_help(
    principal: &Principal,
    extension_store: &Arc<ExtensionStore>,
    extension_services: &Arc<ExtensionServices>,
    format: OutputFormat,
) -> Result<String> {
    // Render from the manager-owned in-memory config: this is the same state
    // used to build runtime capability snapshots and avoids presenting a
    // different authorization view after an out-of-band file edit.
    let config = principal.config.read().await.clone();
    let allowed = &config.capabilities;
    let extensions = list_enabled_extensions(extension_store, extension_services).await?;
    let borrowed: Vec<&ExtensionSummary> = extensions.iter().collect();

    match format {
        OutputFormat::Human => Ok(render_human(&config.name, &config, allowed, &borrowed)),
        OutputFormat::Json => render_json(&config.name, &config, allowed, &borrowed),
    }
}

/// Query enabled extensions from the daemon's extension store and
/// built-in extension services. Mirrors the IPC `ExtensionList` handler.
async fn list_enabled_extensions(
    extension_store: &Arc<ExtensionStore>,
    extension_services: &Arc<ExtensionServices>,
) -> Result<Vec<ExtensionSummary>> {
    {
        if let Err(e) = extension_store.load_all().await {
            tracing::warn!("Failed to reload extensions for /help: {e}");
        }
    }
    let builtins = extension_services.list_builtin_extensions().await;
    let installed = extension_store.list_extensions().await;

    let mut extensions = Vec::new();

    for b in &builtins {
        let mut provides = Vec::new();
        if b.ext_type == "tool" {
            provides.push(format!("tool:{}", b.name));
        }
        extensions.push(ExtensionSummary {
            id: b.id.clone(),
            name: b.name.clone(),
            ext_type: b.ext_type.clone(),
            version: "n/a".to_string(),
            source: "built-in".to_string(),
            enabled: b.enabled,
            runtime: "n/a".to_string(),
            description: String::new(),
            provides,
            requires: Vec::new(),
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
            provides: ext.manifest.provides.clone(),
            requires: ext.manifest.requires.clone(),
        });
    }

    Ok(extensions)
}

fn render_human(
    principal_name: &str,
    config: &PrincipalConfig,
    allowed: &Capabilities,
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
        .grants
        .iter()
        .map(Capability::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "Allowed extensions ({}): {}\n",
        allowed.len(),
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
    allowed: &Capabilities,
    extensions: &[&ExtensionSummary],
) -> Result<String> {
    let grouped = group_by_ext_type(extensions);

    let output = serde_json::json!({
        "principal": principal_name,
        "display_name": config.identity.display_name,
        "description": config.identity.description,
        "capabilities": allowed.grants,
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
            provides: Vec::new(),
            requires: Vec::new(),
        }
    }

    fn allowed_caps() -> Capabilities {
        Capabilities::starter_bundle()
    }

    /// ADR-050 D3 ("Presence = Visibility"): the `/help` catalog is
    /// unfiltered. A default principal (only cross-actor grants in
    /// `Capabilities::starter_bundle()`) sees every installed
    /// extension in the rendered output. Execution gating is the F37
    /// funnel's job, not `/help`'s.
    fn minimal_config(name: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: None,
            owner: Default::default(),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: crate::principal::config::TransportPreference::Auto,
            quota: None,
            children: Default::default(),
        }
    }

    #[test]
    fn help_human_lists_all_extensions_under_starter_bundle() {
        let extensions = vec![
            sample_summary("Bash", "Bash", "tool"),
            sample_summary("Read", "Read", "tool"),
            sample_summary("docker", "Docker", "skill"),
            sample_summary("remote-mcp", "Remote MCP", "mcp"),
        ];
        let config = minimal_config("test");
        let out = render_human(
            "test",
            &config,
            &allowed_caps(),
            &extensions.iter().collect::<Vec<_>>(),
        );
        // Starter bundle grants alone (no `tool:*` / `skill:*` / `mcp:*`)
        // must not hide any extension from the catalog.
        assert!(out.contains("Bash"), "missing Bash in /help output:\n{out}");
        assert!(out.contains("Read"), "missing Read in /help output:\n{out}");
        assert!(out.contains("docker"), "missing docker in /help output:\n{out}");
        assert!(out.contains("remote-mcp"), "missing remote-mcp in /help output:\n{out}");
        // The capabilities summary line is still present.
        assert!(
            out.contains("Allowed extensions"),
            "missing capability summary:\n{out}"
        );
    }

    /// Same posture for the JSON envelope — every extension surfaces,
    /// regardless of starter_bundle.
    #[test]
    fn help_json_lists_all_extensions_under_starter_bundle() {
        let extensions = vec![
            sample_summary("Bash", "Bash", "tool"),
            sample_summary("docker", "Docker", "skill"),
        ];
        let config = minimal_config("test");
        let out = render_json(
            "test",
            &config,
            &allowed_caps(),
            &extensions.iter().collect::<Vec<_>>(),
        )
        .expect("json render");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        let tool_ids: Vec<&str> = parsed["enabled_extensions"]["tool"]
            .as_array()
            .expect("tool array")
            .iter()
            .map(|v| v["id"].as_str().expect("id str"))
            .collect();
        let skill_ids: Vec<&str> = parsed["enabled_extensions"]["skill"]
            .as_array()
            .expect("skill array")
            .iter()
            .map(|v| v["id"].as_str().expect("id str"))
            .collect();
        assert_eq!(tool_ids, vec!["Bash"]);
        assert_eq!(skill_ids, vec!["docker"]);
        // Capabilities field still carried for scripts that read it.
        assert!(parsed["capabilities"].is_array());
    }
}
