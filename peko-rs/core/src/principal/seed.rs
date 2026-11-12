//! Exporting a seed (ADR-060): a peko's definition without its existence.
//!
//! `peko seed <name>` turns a `peko.toml` into a file PekoHub will
//! publish and `peko create <name> -s <file>` will grow. Everything that
//! belongs to this one peko — its identity, owner, grants, lifecycle,
//! deployment state, and the operator's spend policy — is removed; the
//! definition (identity text, intent, governance, routing, model) stays.
//! PekoHub refuses a seed that still carries any of [`EXISTENCE_KEYS`].

use anyhow::{Context, Result};
use peko_auth::ActorRef;

use crate::principal::config::{Exposure, PrincipalConfig};

/// Top-level keys that belong to one peko, not its definition. PekoHub
/// refuses a published seed carrying any of them.
pub const EXISTENCE_KEYS: &[&str] = &[
    "id",
    "did",
    "actor_key",
    "owner",
    "permissions",
    "boot_state",
    "status",
    "exposure",
    "children",
];

/// Also dropped on export, though a hand-written seed may carry them: the
/// operator's own budget, and a root prompt path that only exists in this
/// peko's workspace.
const LOCAL_KEYS: &[&str] = &["quota"];

/// A seed from the text of a `peko.toml`.
///
/// # Errors
/// When the text is not TOML.
pub fn seed_from_config(principal_toml: &str) -> Result<String> {
    seed_from_workspace(principal_toml, None, &[])
}

/// The persona a seed gave a peko, read back from its conventions: the
/// body of the `## Persona` section [`add_persona`] wrote, up to the
/// next `## ` heading.
#[must_use]
pub fn persona_in(conventions: &str) -> Option<String> {
    let mut lines = conventions.lines();
    lines.by_ref().find(|line| line.trim() == "## Persona")?;
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    let body = body.join("\n").trim().to_string();
    (!body.is_empty()).then_some(body)
}

/// A seed from a peko's `peko.toml`, its `kb/CONVENTIONS.md` (so the
/// persona it was grown with travels again) and the capability files to
/// carry ([`collect_files`]). `peko seed` → `peko create -s` round-trips
/// all three.
///
/// # Errors
/// When `principal_toml` is not TOML.
pub fn seed_from_workspace(
    principal_toml: &str,
    conventions: Option<&str>,
    files: &[SeedFile],
) -> Result<String> {
    let mut doc: toml::Table =
        toml::from_str(principal_toml).context("peko.toml is not valid TOML")?;
    if let Some(persona) = conventions.and_then(persona_in) {
        doc.insert("persona".into(), toml::Value::String(persona));
    }
    if !files.is_empty() {
        let table: toml::Table = files
            .iter()
            .map(|file| (file.path.clone(), toml::Value::String(file.content.clone())))
            .collect();
        doc.insert("files".into(), toml::Value::Table(table));
    }
    for key in EXISTENCE_KEYS.iter().chain(LOCAL_KEYS) {
        doc.remove(*key);
    }
    if let Some(toml::Value::Table(routing)) = doc.get_mut("routing") {
        routing.remove("root_prompt");
    }
    let body = toml::to_string_pretty(&doc).context("could not write the seed")?;
    let name = doc
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or("peko");
    Ok(format!(
        "# Seed for `{name}` — its definition, not its identity.\n\
         # Grow a peko from it: peko create <name> -s {name}.seed.toml\n\n{body}"
    ))
}

/// Where the genesis turn records how each setup step went, relative to
/// the workspace.
pub const SETUP_REPORT_PATH: &str = "kb/setup.md";

/// The setup steps as a seed may carry them: each on one line, blank
/// ones dropped. There is no limit on how many or how long: the creator
/// reads them before accepting, and can watch and stop the turn that
/// carries them out.
#[must_use]
pub fn tidy_setup(steps: &[String]) -> Vec<String> {
    steps
        .iter()
        .map(|step| step.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|step| !step.is_empty())
        .collect()
}

/// The setup steps as a numbered list, for the person who must accept
/// them before the peko is created.
#[must_use]
pub fn setup_listing(steps: &[String]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| format!("  {}. {step}", index + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The workspace folders a seed's files may go in (ADR-107): the plain
/// files a peko's capabilities are made of (ADR-050). Hooks are left
/// out: they run on every tool call.
pub const SEED_FILE_DIRS: &[&str] = &["roles", "skills", "workflows", "mcp"];

/// Most bytes of file content one seed may carry. A seed stays one
/// small text file.
pub const SEED_FILES_MAX_BYTES: usize = 64 * 1024;

/// Most files one seed may carry.
pub const SEED_FILES_MAX_COUNT: usize = 64;

/// One file a seed carries: a path inside the new peko's workspace and
/// its text (ADR-107).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedFile {
    pub path: String,
    pub content: String,
}

impl SeedFile {
    /// Whether the file names something this computer will run: an MCP
    /// server's command, or a workflow script. The person accepting the
    /// seed is shown such a file in full.
    #[must_use]
    pub fn runs_commands(&self) -> bool {
        self.path.starts_with("mcp/") || self.path.starts_with("workflows/")
    }
}

/// Why `path` may not be a seed file's path, if it may not: it must be
/// a relative path of plain names, at least two deep, under one of
/// [`SEED_FILE_DIRS`].
fn path_refusal(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    let plain = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if path.len() > 200 || parts.len() < 2 || !parts.iter().all(|part| plain(part)) {
        return Some(format!(
            "`{path}` is not a plain relative path (letters, digits, `.`, `_`, `-`; no \
             leading dots)"
        ));
    }
    if !SEED_FILE_DIRS.contains(&parts[0]) {
        return Some(format!(
            "`{path}` is outside the folders a seed may fill: {}",
            SEED_FILE_DIRS.join(", ")
        ));
    }
    None
}

/// The files of a seed's `[files]` table, checked: every key a path
/// under [`SEED_FILE_DIRS`], every value text, within the count and
/// size limits. In path order.
fn checked_files(table: Option<toml::Table>) -> Result<Vec<SeedFile>> {
    let mut files = Vec::new();
    for (path, value) in table.unwrap_or_default() {
        if let Some(refusal) = path_refusal(&path) {
            anyhow::bail!("seed file {refusal}");
        }
        let toml::Value::String(content) = value else {
            anyhow::bail!("seed file `{path}` must be text (a TOML string)");
        };
        files.push(SeedFile { path, content });
    }
    anyhow::ensure!(
        files.len() <= SEED_FILES_MAX_COUNT,
        "a seed carries at most {SEED_FILES_MAX_COUNT} files; this one has {}",
        files.len()
    );
    let bytes: usize = files.iter().map(|file| file.content.len()).sum();
    anyhow::ensure!(
        bytes <= SEED_FILES_MAX_BYTES,
        "a seed's files may hold at most {} KB together; these hold {} KB",
        SEED_FILES_MAX_BYTES / 1024,
        bytes.div_ceil(1024)
    );
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// The files a seed carries, for the person who must accept them: each
/// path and size, and the full text of the ones that name something
/// this computer will run.
#[must_use]
pub fn files_listing(files: &[SeedFile]) -> String {
    let mut lines = Vec::new();
    for file in files {
        lines.push(format!("  {} ({} bytes)", file.path, file.content.len()));
        if file.runs_commands() {
            lines.extend(
                file.content
                    .trim_end()
                    .lines()
                    .map(|line| format!("      {line}")),
            );
        }
    }
    lines.join("\n")
}

/// Write a seed's files into a new peko's workspace (ADR-107). Paths
/// were checked when the seed was parsed; they are checked again here,
/// since this is where they touch the disk.
///
/// # Errors
/// When a path is refused or a file cannot be written.
pub fn write_files(workspace: &std::path::Path, files: &[SeedFile]) -> Result<()> {
    for file in files {
        if let Some(refusal) = path_refusal(&file.path) {
            anyhow::bail!("seed file {refusal}");
        }
        let target = workspace.join(&file.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&target, &file.content)
            .with_context(|| format!("write {}", target.display()))?;
    }
    Ok(())
}

/// The capability files of a peko's workspace, for its exported seed
/// (ADR-107): everything under `roles/`, `skills/` and `workflows/`,
/// and `mcp/` too when `with_mcp`. An MCP server's file may hold
/// tokens, so it is carried only when asked for. Roles still identical
/// to the runtime's defaults are left out, as every peko gets them.
/// Files that are not UTF-8 text are skipped and named in the second
/// value.
///
/// # Errors
/// When the files exceed what a seed may carry, or cannot be read.
pub fn collect_files(
    workspace: &std::path::Path,
    with_mcp: bool,
) -> Result<(Vec<SeedFile>, Vec<String>)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(root, &path, out);
            } else if kind.is_file() {
                if let Ok(relative) = path.strip_prefix(root) {
                    out.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let defaults = crate::principal::genesis::default_role_files();
    let mut paths = Vec::new();
    for dir in SEED_FILE_DIRS {
        if *dir == "mcp" && !with_mcp {
            continue;
        }
        walk(workspace, &workspace.join(dir), &mut paths);
    }
    paths.sort();
    let (mut files, mut skipped) = (Vec::new(), Vec::new());
    for path in paths {
        if path_refusal(&path).is_some() {
            skipped.push(path);
            continue;
        }
        let Ok(content) = std::fs::read_to_string(workspace.join(&path)) else {
            skipped.push(path);
            continue;
        };
        if defaults
            .iter()
            .any(|(default_path, default)| *default_path == path && *default == content)
        {
            continue;
        }
        files.push(SeedFile { path, content });
    }
    let table: toml::Table = files
        .iter()
        .map(|file| (file.path.clone(), toml::Value::String(file.content.clone())))
        .collect();
    Ok((checked_files(Some(table))?, skipped))
}

/// A seed read back: the definition plus its optional inline `persona`
/// (Markdown added to the grown peko's `kb/CONVENTIONS.md`; see
/// [`add_persona`]) and the capability files it carries (`[files]`,
/// ADR-107).
#[derive(Debug, Clone)]
pub struct Seed {
    pub config: PrincipalConfig,
    pub persona: Option<String>,
    pub files: Vec<SeedFile>,
}

/// Parse a seed. It IS a `peko.toml` (plus a tolerated `persona`),
/// so `peko seed` output and hand-written files both read.
///
/// # Errors
/// When the text is not a valid `peko.toml`.
pub fn parse_seed(text: &str) -> Result<Seed> {
    #[derive(serde::Deserialize)]
    struct Extras {
        persona: Option<String>,
        files: Option<toml::Table>,
    }
    let mut config: PrincipalConfig =
        toml::from_str(text).context("parse the seed as peko.toml")?;
    config.intent.setup = tidy_setup(&config.intent.setup);
    let extras: Extras = toml::from_str(text).context("parse the seed's persona and files")?;
    Ok(Seed {
        config,
        persona: extras.persona,
        files: checked_files(extras.files)?,
    })
}

/// Give a peko grown from a seed its `persona`: a `## Persona` section
/// in `kb/CONVENTIONS.md`, the house rules every role follows (ADR-087:
/// who a peko is lives in `peko.toml` `[identity]` and the conventions,
/// never in a role). The kb scaffold is laid down first, so the persona
/// joins the default conventions instead of replacing them. A file that
/// already has a `## Persona` section is left alone.
///
/// # Errors
/// When the kb scaffold or `CONVENTIONS.md` cannot be written.
pub fn add_persona(workspace: &std::path::Path, persona: &str) -> Result<()> {
    let persona = persona.trim();
    if persona.is_empty() {
        return Ok(());
    }
    crate::principal::kb::seed_kb_scaffold(workspace)?;
    let path = workspace.join("kb").join("CONVENTIONS.md");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if text.lines().any(|line| line.trim() == "## Persona") {
        return Ok(());
    }
    let text = format!("{}\n\n## Persona\n\n{persona}\n", text.trim_end());
    std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
}

impl Seed {
    /// The setup steps the grown peko will carry out on its first turn
    /// (ADR-104). They run commands and download code as the owner's OS
    /// user, so every create surface shows them and has them accepted
    /// before it grows the peko.
    #[must_use]
    pub fn setup(&self) -> &[String] {
        &self.config.intent.setup
    }

    /// Whether the person creating a peko from this seed must accept
    /// something first (ADR-104 D3, ADR-107): setup steps it will carry
    /// out, or files that will be written into its workspace.
    #[must_use]
    pub fn needs_acceptance(&self) -> bool {
        !self.setup().is_empty() || !self.files.is_empty()
    }

    /// What the person accepts, as text: the numbered setup steps and
    /// the files. Empty when there is nothing to accept.
    #[must_use]
    pub fn acceptance_listing(&self) -> String {
        let mut parts = Vec::new();
        if !self.setup().is_empty() {
            parts.push(format!(
                "Setup steps it will carry out, running commands and downloading code on \
                 this computer:\n{}",
                setup_listing(self.setup())
            ));
        }
        if !self.files.is_empty() {
            parts.push(format!(
                "Files written into its folder (an MCP server or workflow file names \
                 something this computer will run):\n{}",
                files_listing(&self.files)
            ));
        }
        parts.join("\n")
    }

    /// The config of a new peko grown from this seed: named `name`, owned
    /// by `owner`, with a fresh identity and no grants (ADR-060, ADR-081
    /// D4). Lifecycle and deployment state never carry over — a seed that
    /// said `exposure = "public"` must not publish the peko grown from it.
    #[must_use]
    pub fn grown(self, name: &str, owner: ActorRef) -> PrincipalConfig {
        let mut config = self.config;
        config.name = name.to_string();
        config.id = None;
        config.did = None;
        config.actor_key = None;
        config.boot_state = None;
        config.status = None;
        config.exposure = Exposure::default();
        config.owner = owner;
        config.permissions.clear();
        config
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_persona_joins_the_conventions_every_role_follows() {
        let ws = tempfile::tempdir().unwrap();
        super::add_persona(ws.path(), "Your name is Mochi. Sign replies \"— Mochi\".\n").unwrap();
        let conventions = std::fs::read_to_string(ws.path().join("kb/CONVENTIONS.md")).unwrap();
        assert!(
            conventions.starts_with("# Conventions"),
            "the default rules stay"
        );
        assert!(
            conventions.ends_with("## Persona\n\nYour name is Mochi. Sign replies \"— Mochi\".\n")
        );

        super::add_persona(ws.path(), "Something else").unwrap();
        let again = std::fs::read_to_string(ws.path().join("kb/CONVENTIONS.md")).unwrap();
        assert_eq!(again, conventions, "an existing persona is kept");
        assert!(!ws.path().join("roles").exists(), "no role is written");
    }
    use super::*;

    /// ADR-107: a seed's `[files]` are text files under the capability
    /// folders, written into the new peko's workspace and exported again.
    #[test]
    fn a_seed_carries_capability_files() {
        let text = r#"
name = "ada"

[files]
"roles/researcher.md" = "---\nname: researcher\ndescription: Reads papers\n---\nCite sources.\n"
"mcp/echo/server.json" = '''
{ "command": "/usr/bin/true" }
'''
"skills/greet/SKILL.md" = "---\nname: greet\ndescription: Greets\n---\nHello $ARGUMENTS\n"
"#;
        let seed = parse_seed(text).unwrap();
        assert_eq!(
            seed.files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            [
                "mcp/echo/server.json",
                "roles/researcher.md",
                "skills/greet/SKILL.md"
            ]
        );
        assert!(seed.needs_acceptance());
        let listing = seed.acceptance_listing();
        assert!(listing.contains("  roles/researcher.md ("), "{listing}");
        // A file that names a command is shown in full; a prompt is not.
        assert!(
            listing.contains("      { \"command\": \"/usr/bin/true\" }"),
            "{listing}"
        );
        assert!(!listing.contains("Cite sources."), "{listing}");

        let ws = tempfile::tempdir().unwrap();
        write_files(ws.path(), &seed.files).unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.path().join("skills/greet/SKILL.md")).unwrap(),
            "---\nname: greet\ndescription: Greets\n---\nHello $ARGUMENTS\n"
        );

        // Exported again: MCP files only when asked for, and binary files
        // are skipped and named.
        std::fs::write(ws.path().join("skills/greet/logo.bin"), [0xff, 0xfe, 0x00]).unwrap();
        std::fs::create_dir_all(ws.path().join("hooks/x")).unwrap();
        std::fs::write(ws.path().join("hooks/x/hook.toml"), "x = 1").unwrap();
        let (files, skipped) = collect_files(ws.path(), false).unwrap();
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["roles/researcher.md", "skills/greet/SKILL.md"]
        );
        assert_eq!(skipped, ["skills/greet/logo.bin"]);
        let (with_mcp, _) = collect_files(ws.path(), true).unwrap();
        assert_eq!(with_mcp.len(), 3);
        let exported = seed_from_workspace("name = \"ada\"\n", None, &with_mcp).unwrap();
        assert_eq!(parse_seed(&exported).unwrap().files, seed.files);

        // Nothing to accept in a plain seed.
        let plain = parse_seed("name = \"ada\"\n").unwrap();
        assert!(!plain.needs_acceptance() && plain.acceptance_listing().is_empty());
    }

    /// ADR-107: a seed's files stay inside the capability folders.
    #[test]
    fn seed_files_outside_the_capability_folders_are_refused() {
        for (path, needle) in [
            ("peko.toml", "not a plain relative path"),
            ("POLICY.md", "not a plain relative path"),
            ("kb/MEMORY.md", "outside the folders"),
            ("hooks/x/hook.toml", "outside the folders"),
            ("roles/../peko.toml", "not a plain relative path"),
            ("/etc/passwd", "not a plain relative path"),
            ("roles/.hidden", "not a plain relative path"),
            ("roles/a b.md", "not a plain relative path"),
            ("roles", "not a plain relative path"),
        ] {
            let text = format!("name = \"ada\"\n[files]\n\"{path}\" = \"x\"\n");
            let error = format!("{:#}", parse_seed(&text).unwrap_err());
            assert!(error.contains(needle), "{path}: {error}");
            let file = SeedFile {
                path: path.to_string(),
                content: "x".into(),
            };
            let ws = tempfile::tempdir().unwrap();
            assert!(write_files(ws.path(), &[file]).is_err(), "{path}");
        }
        let not_text = "name = \"ada\"\n[files]\n\"roles/a.md\" = 3\n";
        assert!(format!("{:#}", parse_seed(not_text).unwrap_err()).contains("must be text"));
        let big = format!(
            "name = \"ada\"\n[files]\n\"roles/a.md\" = \"{}\"\n",
            "x".repeat(SEED_FILES_MAX_BYTES + 1)
        );
        assert!(format!("{:#}", parse_seed(&big).unwrap_err()).contains("at most 64 KB"));
    }

    /// The persona a seed gave a peko is in the seed exported from it.
    #[test]
    fn an_exported_seed_carries_the_persona_again() {
        let ws = tempfile::tempdir().unwrap();
        add_persona(
            ws.path(),
            "Your name is Mochi.\n\nSign replies \"— Mochi\".",
        )
        .unwrap();
        let mut conventions = std::fs::read_to_string(ws.path().join("kb/CONVENTIONS.md")).unwrap();
        assert_eq!(
            persona_in(&conventions).as_deref(),
            Some("Your name is Mochi.\n\nSign replies \"— Mochi\".")
        );
        // A later section is not part of it.
        conventions.push_str("\n## Learned\n\n- Be brief.\n");
        let seed = seed_from_workspace(CONFIG, Some(&conventions), &[]).unwrap();
        let back = parse_seed(&seed).unwrap();
        assert_eq!(
            back.persona.as_deref(),
            Some("Your name is Mochi.\n\nSign replies \"— Mochi\".")
        );
        assert_eq!(back.config.identity.display_name.as_deref(), Some("Ada"));
        // No persona section, no persona key.
        assert!(persona_in("# Conventions\n\n- Be kind.\n").is_none());
        assert!(!seed_from_config(CONFIG).unwrap().contains("persona"));
    }

    const CONFIG: &str = r#"
name = "ada"
id = "p_01"
did = "did:key:z6MkAda"
actor_key = "k_ada"
boot_state = "organized"
status = "online"
exposure = "public"
preferred_model_id = "claude-opus-5-5"

[identity]
display_name = "Ada"
description = "Reads papers and keeps notes"

[intent]
goals = ["Help with research"]

[routing]
root_prompt = "ROLE.md"
recall_top_k = 4

[quota]
daily_tokens = 1000

[[permissions]]
subject = { kind = "user", id = "bob" }
permission = "chat"
granted_at = "2026-10-01T00:00:00Z"
granted_by = { kind = "user", id = "local" }
"#;

    #[test]
    fn a_seed_keeps_the_definition_and_drops_the_existence() {
        let seed = seed_from_config(CONFIG).unwrap();
        let doc: toml::Table = toml::from_str(&seed).unwrap();
        for key in EXISTENCE_KEYS.iter().chain(LOCAL_KEYS) {
            assert!(!doc.contains_key(*key), "{key} survived the export");
        }
        assert_eq!(doc["name"].as_str(), Some("ada"));
        assert_eq!(doc["preferred_model_id"].as_str(), Some("claude-opus-5-5"));
        assert_eq!(doc["identity"]["display_name"].as_str(), Some("Ada"));
        assert_eq!(
            doc["intent"]["goals"][0].as_str(),
            Some("Help with research")
        );
        assert!(doc["routing"].get("root_prompt").is_none());
        assert_eq!(doc["routing"]["recall_top_k"].as_integer(), Some(4));
        assert!(seed.starts_with("# Seed for `ada`"));
    }

    #[test]
    fn a_grown_peko_takes_nothing_of_the_seeds_existence() {
        let grown = parse_seed(CONFIG)
            .unwrap()
            .grown("ada-2", ActorRef::User("local".into()));
        assert_eq!(grown.name, "ada-2");
        assert!(grown.id.is_none() && grown.did.is_none() && grown.actor_key.is_none());
        assert!(grown.boot_state.is_none() && grown.status.is_none());
        assert_eq!(
            grown.exposure,
            Exposure::default(),
            "a seed can't publish its grown peko"
        );
        assert!(grown.permissions.is_empty());
        assert_eq!(grown.owner, ActorRef::User("local".into()));
        assert_eq!(grown.preferred_model_id.as_deref(), Some("claude-opus-5-5"));
    }

    /// ADR-104: setup steps travel with the definition, each on one line.
    #[test]
    fn a_seed_carries_setup_steps() {
        let text = format!(
            "{CONFIG}\n[intent]\nsetup = [\"  Get the notes skill\\n from https://example.com/notes  \", \"\", \"Install ripgrep\"]\n"
        )
        .replacen("[intent]\ngoals = [\"Help with research\"]\n", "", 1);
        let seed = parse_seed(&text).unwrap();
        assert_eq!(
            seed.setup(),
            [
                "Get the notes skill from https://example.com/notes".to_string(),
                "Install ripgrep".to_string()
            ]
        );
        assert_eq!(
            setup_listing(seed.setup()),
            "  1. Get the notes skill from https://example.com/notes\n  2. Install ripgrep"
        );
        // The steps stay in the grown peko's definition and in its seed.
        let grown = seed.grown("ada-2", ActorRef::User("local".into()));
        assert_eq!(grown.intent.setup.len(), 2);
        let exported = seed_from_config(&toml::to_string(&grown).unwrap()).unwrap();
        assert_eq!(parse_seed(&exported).unwrap().setup().len(), 2);

        // No limit on how many steps or how long.
        let many = vec!["x".repeat(5_000); 40];
        assert_eq!(tidy_setup(&many).len(), 40);
        // A definition without setup writes no `setup` key.
        assert!(!seed_from_config(CONFIG).unwrap().contains("setup"));
    }

    #[test]
    fn a_seed_still_reads_as_a_principal_config() {
        let seed = seed_from_config(CONFIG).unwrap();
        let config: PrincipalConfig = toml::from_str(&seed).unwrap();
        assert_eq!(config.name, "ada");
        assert!(config.id.is_none() && config.did.is_none());
        assert!(config.permissions.is_empty());
    }
}
