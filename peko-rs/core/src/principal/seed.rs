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
    let mut doc: toml::Table =
        toml::from_str(principal_toml).context("peko.toml is not valid TOML")?;
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

/// A seed read back: the definition plus its optional inline `persona`
/// (Markdown added to the grown peko's `kb/CONVENTIONS.md`; see
/// [`add_persona`]).
#[derive(Debug, Clone)]
pub struct Seed {
    pub config: PrincipalConfig,
    pub persona: Option<String>,
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
    }
    let config: PrincipalConfig = toml::from_str(text).context("parse the seed as peko.toml")?;
    let extras: Extras = toml::from_str(text).context("parse the seed's persona")?;
    Ok(Seed {
        config,
        persona: extras.persona,
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

    #[test]
    fn a_seed_still_reads_as_a_principal_config() {
        let seed = seed_from_config(CONFIG).unwrap();
        let config: PrincipalConfig = toml::from_str(&seed).unwrap();
        assert_eq!(config.name, "ada");
        assert!(config.id.is_none() && config.did.is_none());
        assert!(config.permissions.is_empty());
    }
}
