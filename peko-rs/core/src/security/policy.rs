//! `POLICY.md` (ADR-078 D7): the owner's plain-language rules for what
//! the principal does for whom. The floor ([`super::floor`]) is fixed;
//! this file is where the owner's nuance lives. It is protected: only an
//! owner run writes it, and *Always allow* approvals append to it.
//!
//! Shape: optional front matter (`judge_model`, `review_replies`), then
//! default rules, then one `## <actor address>` section per actor. A
//! heading whose first word is not an actor address belongs to the
//! defaults.

use std::path::Path;

/// The policy file, at the workspace root.
pub const POLICY_FILE: &str = "POLICY.md";

/// Most policy text one run's prompt carries.
pub const POLICY_EXCERPT_MAX_BYTES: usize = 8 * 1024;

const SEED: &str = include_str!("../resources/POLICY.md");

/// A parsed `POLICY.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Catalog model id the judge runs on; `None` uses the principal's
    /// own model.
    pub judge_model: Option<String>,
    /// Also judge final replies to anyone but the owner.
    pub review_replies: bool,
    /// Rules for everyone who is not the owner.
    pub defaults: String,
    /// `(actor address, rules)` per actor section, in file order.
    pub sections: Vec<(String, String)>,
}

impl Policy {
    /// Parse `text`. Never fails: an unreadable front matter key is
    /// ignored and the rest still applies.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut policy = Self::default();
        let text = strip_comments(text);
        let text = text.as_str();
        let mut body = text;
        if let Some(rest) = text.strip_prefix("---\n") {
            if let Some(end) = rest.find("\n---") {
                for line in rest[..end].lines() {
                    let line = line.split('#').next().unwrap_or_default();
                    let Some((key, value)) = line.split_once(':') else {
                        continue;
                    };
                    let value = value.trim();
                    match key.trim() {
                        "judge_model" if !value.is_empty() => {
                            policy.judge_model = Some(value.to_string());
                        }
                        "review_replies" => policy.review_replies = value == "true",
                        _ => {}
                    }
                }
                body = rest[end + 4..].trim_start_matches('\n');
            }
        }
        let mut current: Option<String> = None;
        let mut buffer = String::new();
        let flush = |current: &Option<String>, buffer: &mut String, policy: &mut Policy| {
            let text = buffer.trim();
            if !text.is_empty() {
                match current {
                    Some(actor) => policy.sections.push((actor.clone(), text.to_string())),
                    None => {
                        if !policy.defaults.is_empty() {
                            policy.defaults.push('\n');
                        }
                        policy.defaults.push_str(text);
                    }
                }
            }
            buffer.clear();
        };
        for line in body.lines() {
            let actor = line
                .strip_prefix("## ")
                .and_then(|heading| heading.split_whitespace().next())
                .filter(|word| is_actor_address(word));
            if line.starts_with("## ") {
                flush(&current, &mut buffer, &mut policy);
                current = actor.map(str::to_string);
            }
            buffer.push_str(line);
            buffer.push('\n');
        }
        flush(&current, &mut buffer, &mut policy);
        policy
    }

    /// The rules that apply to `actor`: the defaults and the sections
    /// naming it under any alias (ADR-081 D1, identity decided by `keys`),
    /// capped at [`POLICY_EXCERPT_MAX_BYTES`]. A group channel is no actor;
    /// its section matches by its exact ID.
    #[must_use]
    pub fn for_actor(&self, actor: &str, keys: &dyn peko_subject::ActorKeys) -> String {
        let subject = actor.parse::<peko_subject::ActorRef>().ok();
        let mut text = self.defaults.clone();
        for (address, rules) in &self.sections {
            let same = address == actor
                || subject.as_ref().is_some_and(|subject| {
                    address
                        .parse::<peko_subject::ActorRef>()
                        .is_ok_and(|section| keys.same_actor(&section, subject))
                });
            if same {
                text.push_str("\n\n");
                text.push_str(rules);
            }
        }
        cap(&text, POLICY_EXCERPT_MAX_BYTES)
    }

    /// The owner's *Always allow* for `actor`, as a rule line appended to
    /// its section (created when missing).
    #[must_use]
    pub fn with_allowance(text: &str, actor: &str, rule: &str) -> String {
        let heading = format!("## {actor}");
        let line = format!("- Always allowed (owner approval {}): {rule}\n", today());
        let mut lines: Vec<&str> = text.lines().collect();
        let Some(start) = lines.iter().position(|l| {
            l.strip_prefix("## ")
                .and_then(|h| h.split_whitespace().next())
                == Some(actor)
        }) else {
            let mut out = text.trim_end().to_string();
            out.push_str(&format!("\n\n{heading}\n{line}"));
            return out;
        };
        let end = lines[start + 1..]
            .iter()
            .position(|l| l.starts_with("## "))
            .map_or(lines.len(), |offset| start + 1 + offset);
        let mut insert = end;
        while insert > start + 1 && lines[insert - 1].trim().is_empty() {
            insert -= 1;
        }
        let owned = line.trim_end().to_string();
        lines.insert(insert, &owned);
        let mut out = lines.join("\n");
        out.push('\n');
        out
    }
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// `text` without HTML comments (the seed's examples live in one).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("-->") {
            Some(end) => &rest[start + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Whether `word` reads as an ADR-073 actor address (`kind:scope:id`).
fn is_actor_address(word: &str) -> bool {
    let mut parts = word.splitn(3, ':');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("user" | "visitor" | "principal"), Some("local" | "hub"), Some(id)) if !id.is_empty()
    )
}

fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… (policy truncated; read {POLICY_FILE} for the rest)",
        &text[..end]
    )
}

/// Load the workspace's policy, if it has one.
#[must_use]
pub fn load(workspace: &Path) -> Option<Policy> {
    std::fs::read_to_string(workspace.join(POLICY_FILE))
        .ok()
        .map(|text| Policy::parse(&text))
}

/// Create the default `POLICY.md` when the workspace has none. Never
/// overwrites. Returns whether it wrote the file.
pub fn seed(workspace: &Path) -> std::io::Result<bool> {
    let path = workspace.join(POLICY_FILE);
    if path.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(workspace)?;
    std::fs::write(path, SEED)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\njudge_model: fast-judge  # cheap\nreview_replies: true\n---\n\
# Default\n- Public projects only.\n\n\
## Notes\n- Still a default.\n\n\
## user:hub:alice (Alice, trusted)\n- Full access to projects/acme/.\n\n\
## principal:hub:bob\n- Free/busy only.\n";

    #[test]
    fn parses_front_matter_defaults_and_actor_sections() {
        let policy = Policy::parse(SAMPLE);
        assert_eq!(policy.judge_model.as_deref(), Some("fast-judge"));
        assert!(policy.review_replies);
        assert!(policy.defaults.contains("Public projects only."));
        assert!(policy.defaults.contains("Still a default."));
        assert_eq!(policy.sections.len(), 2);
        assert_eq!(policy.sections[0].0, "user:hub:alice");
    }

    #[test]
    fn an_actor_sees_defaults_and_only_its_own_sections() {
        let policy = Policy::parse(SAMPLE);
        let keys = peko_subject::CanonicalKeys;
        let alice = policy.for_actor("user:hub:alice", &keys);
        assert!(alice.contains("Public projects only.") && alice.contains("acme"));
        assert!(!alice.contains("Free/busy"));
        let stranger = policy.for_actor("user:hub:zed", &keys);
        assert!(!stranger.contains("acme"));
        struct Bob;
        impl peko_subject::ActorKeys for Bob {
            fn key_of(&self, subject: &peko_subject::ActorRef) -> peko_subject::ActorKey {
                let address = subject.canonical_address();
                peko_subject::ActorKey(match address.as_str() {
                    "principal:local:prin_b" | "principal:hub:bob" => "bob".into(),
                    _ => address,
                })
            }
        }
        let alias = policy.for_actor("principal:local:prin_b", &Bob);
        assert!(alias.contains("Free/busy"));
    }

    #[test]
    fn allowances_land_in_the_actor_section() {
        let text = Policy::with_allowance(SAMPLE, "user:hub:alice", "Read docs/q3.md");
        let policy = Policy::parse(&text);
        assert!(policy.sections[0].1.contains("Read docs/q3.md"));
        let text = Policy::with_allowance(SAMPLE, "user:hub:carol", "Use ModelCall");
        let policy = Policy::parse(&text);
        assert!(policy
            .sections
            .iter()
            .any(|(a, r)| a == "user:hub:carol" && r.contains("Use ModelCall")));
    }

    #[test]
    fn the_seed_parses_and_is_create_once() {
        let policy = Policy::parse(SEED);
        assert!(!policy.defaults.is_empty());
        assert!(
            policy.sections.is_empty(),
            "the seed's example stays a comment"
        );
        assert!(!policy.defaults.contains("<uuid>"));
        assert!(!policy.review_replies);
        let dir = tempfile::tempdir().unwrap();
        assert!(seed(dir.path()).unwrap());
        std::fs::write(dir.path().join(POLICY_FILE), "mine").unwrap();
        assert!(!seed(dir.path()).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap(),
            "mine"
        );
    }
}
