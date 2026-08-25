//! Workspace-resident universal tool scanner (ADR-047 §2.1, §2.4).
//!
//! Phase 2 PR 3 deletes the framework-coupled
//! [`crate::extensions::universal::adapter::UniversalToolAdapter`]
//! (the `ExtensionTypeAdapter` impl that wrapped every universal
//! tool in 4 framework hooks). Each tool is now a file inside the
//! principal's workspace — `<workspace>/tools/<id>/manifest.yaml` —
//! and the scanner below reads each manifest, constructs the
//! canonical [`crate::extensions::universal::protocol::UniversalToolAdapter`]
//! (which already implements `peko_tools_core::Tool`), and
//! registers it via
//! [`crate::extensions::builtin::BuiltinToolAdapter::register_tool`]
//! — no framework hook layer between the tool and the dispatcher.
//!
//! Per ADR-047 §2.1 the directory convention is `<workspace>/tools/`,
//! matching the published `~/.peko/principal/<name>/tools/` layout.
//! The legacy flat layout (a tool file directly under the directory)
//! is rejected: ADR-024 unified manifests have always lived in
//! per-tool subdirectories, and the framework
//! `register_adapter(UniversalToolAdapter::new())` only ever scanned
//! subdirectories.

use crate::extensions::builtin::BuiltinToolAdapter;
use crate::extensions::framework::adapters::parsing::find_executable;
use crate::extensions::universal::protocol::Manifest;
use crate::extensions::universal::protocol::UniversalToolAdapter;
use peko_subject::PrincipalId;
use peko_tools_core::Tool;
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, warn};

/// Scan `<workspace>/tools/<id>/manifest.yaml`, registering each
/// tool with the global `BuiltinToolAdapter`. Returns the number of
/// successfully-registered tools.
///
/// Tools whose `name` matches a tool already present on the
/// `BuiltinToolAdapter`'s underlying `ExtensionCore` are skipped
/// (the registry dedups by tool name).
///
/// Mirrors [`crate::extensions::mcp::workspace::load_workspace_mcp_servers`]:
/// the workspace scanner is the single canonical path for principal
/// tool discovery.
pub async fn load_workspace_universal_tools(
    tools_dir: &Path,
    core: &crate::extensions::framework::core::ExtensionCore,
    principal_id: &PrincipalId,
) -> anyhow::Result<usize> {
    if !tools_dir.exists() {
        debug!(
            "Tools workspace dir does not exist, skipping: {}",
            tools_dir.display()
        );
        return Ok(0);
    }

    let mut entries = match tokio::fs::read_dir(tools_dir).await {
        Ok(entries) => entries,
        Err(e) => {
            warn!(
                "Failed to read tools workspace dir {}: {e}",
                tools_dir.display()
            );
            return Ok(0);
        }
    };

    let mut dir_entries = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        dir_entries.push(entry);
    }

    let mut loaded = 0usize;
    for entry in dir_entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }

        let tool_path = entry.path();
        if !tool_path.is_dir() {
            debug!(
                path = %tool_path.display(),
                "Skipping non-directory entry in tools workspace"
            );
            continue;
        }

        let manifest_path = tool_path.join("manifest.yaml");
        if !manifest_path.exists() {
            debug!(
                path = %tool_path.display(),
                "No manifest.yaml in tool dir, skipping"
            );
            continue;
        }

        let manifest = match Manifest::from_file(&manifest_path).await {
            Ok(m) => m,
            Err(e) => {
                warn!(
                    path = %manifest_path.display(),
                    error = %e,
                    "Failed to parse universal tool manifest"
                );
                continue;
            }
        };

        let tool_name = manifest.name.clone();
        let executable = match find_executable(&tool_path, &tool_name).await {
            Some(p) => p,
            None => {
                warn!(
                    path = %tool_path.display(),
                    tool = %tool_name,
                    "Universal tool manifest parsed but no executable found"
                );
                continue;
            }
        };

        let adapter = match UniversalToolAdapter::from_manifest(&manifest_path, &executable).await {
            Ok(a) => Arc::new(a) as Arc<dyn Tool>,
            Err(e) => {
                warn!(
                    tool = %tool_name,
                    error = %e,
                    "Failed to construct universal tool adapter"
                );
                continue;
            }
        };

        if let Err(e) =
            BuiltinToolAdapter::register_tool(core, adapter, principal_id).await
        {
            warn!(
                tool = %tool_name,
                error = %e,
                "Failed to register universal tool via BuiltinToolAdapter"
            );
            continue;
        }

        loaded += 1;
        debug!(tool = %tool_name, "Registered universal tool");
    }

    Ok(loaded)
}

/// Parse-only walk used by the manifest validator (`peko ext validate`).
/// Mirrors [`load_workspace_universal_tools`] but does **not** require
/// a live `ExtensionCore`; it just enumerates the tools visible at
/// `dir` and returns their parsed manifests so the validator can
/// report what exists. Each entry is `(tool_name, manifest_path)`.
pub async fn discover_workspace_universal_tools(
    dir: &Path,
) -> anyhow::Result<Vec<(String, std::path::PathBuf)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = tokio::fs::read_dir(dir).await?;
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.yaml");
        if !manifest_path.exists() {
            continue;
        }
        match Manifest::from_file(&manifest_path).await {
            Ok(m) => out.push((m.name, manifest_path)),
            Err(_) => out.push((manifest_path.display().to_string(), manifest_path)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::core::ExtensionCore;
    use tempfile::TempDir;

    fn write_tool(dir: &Path, name: &str, description: &str) -> std::path::PathBuf {
        let tool_dir = dir.join(name);
        std::fs::create_dir(&tool_dir).unwrap();
        let manifest = format!(
            "name: {name}\nextension_type: universal-tool\ndescription: {description}\nversion: \"1.0.0\"\nparameters:\n  type: object\n  properties:\n    input:\n      type: string\n"
        );
        let manifest_path = tool_dir.join("manifest.yaml");
        std::fs::write(&manifest_path, manifest).unwrap();
        let script_path = tool_dir.join(format!("{name}.sh"));
        std::fs::write(&script_path, "#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        tool_dir
    }

    #[tokio::test]
    async fn load_skips_missing_dir() {
        let core = ExtensionCore::new();
        let n = load_workspace_universal_tools(
            std::path::Path::new("/nonexistent/tools"),
            &core,
            &PrincipalId::system(),
        )
        .await
        .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn load_reads_per_tool_subdirs() {
        let tmp = TempDir::new().unwrap();
        write_tool(tmp.path(), "tool1", "First");
        write_tool(tmp.path(), "tool2", "Second");

        let core = ExtensionCore::new();
        let n = load_workspace_universal_tools(
            tmp.path(),
            &core,
            &PrincipalId::system(),
        )
        .await
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(core.tool_count(PrincipalId::system()).await, 2);
    }
}