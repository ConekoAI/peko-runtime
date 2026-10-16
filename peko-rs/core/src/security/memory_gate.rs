//! What a scheduled owner run may add to the hot memory file (ADR-080 D4).
//!
//! A cron turn acts for the owner, yet it reads other people's
//! conversations: the daily memory pass reviews every channel, and the
//! keepalive checks them. Whatever it writes to `kb/MEMORY.md` reaches
//! every agent's prompt as the owner's own memory. So when such a run
//! writes that file through `Write` or `Edit`, each line it adds must name
//! its source — `(source: /local-user)` or `(source: chan_x#<event>)` at
//! the end of the line — and every source must be a conversation only the
//! owner speaks in. Other lines are held back and filed for the owner in
//! `kb/inbox/`, under the conversation they came from; the rest of the
//! write is applied.
//!
//! The memory run itself gets no `Bash`, so the file tools are its only
//! way to the file. A line citing an owner source it does not come from
//! still passes; checking claims against their sources is a later step.

use std::collections::{HashMap, HashSet};

use peko_tools_core::{ActingFor, Compartment, Origin};

/// The hot memory file, relative to the workspace.
pub const HOT_MEMORY: &str = "kb/MEMORY.md";

/// Job id of the runtime-seeded daily memory pass.
pub const MEMORY_JOB_ID: &str = "memory";

/// Whether `stamp` is an owner run a cron job started: no person is
/// driving it, and it may have read anyone's conversation.
#[must_use]
pub fn is_scheduled_owner(stamp: &ActingFor) -> bool {
    stamp.is_owner() && matches!(stamp.origin, Origin::Cron { .. })
}

/// Whether `stamp` is the daily memory pass, or an agent it spawned.
#[must_use]
pub fn is_memory_run(stamp: &ActingFor) -> bool {
    stamp.is_owner() && matches!(&stamp.origin, Origin::Cron { job_id } if job_id == MEMORY_JOB_ID)
}

/// Where a cited source stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceClass {
    /// A session or channel only the owner speaks in.
    Owner,
    /// A conversation someone else speaks in.
    Other(Compartment),
    /// Nothing this principal holds.
    Unknown,
}

/// Why a line was held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldReason {
    /// The line names no source.
    Uncited,
    /// A cited source is nothing this principal holds.
    UnknownSource(String),
    /// A cited source is a conversation someone else speaks in.
    OtherSource {
        source: String,
        compartment: Compartment,
    },
}

/// One line held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub line: String,
    pub reason: HeldReason,
}

impl Held {
    /// The compartment whose inbox the line is filed under: the
    /// conversation it came from, else the run's own.
    #[must_use]
    pub fn inbox_compartment<'a>(&'a self, own: &'a Compartment) -> &'a Compartment {
        match &self.reason {
            HeldReason::OtherSource { compartment, .. } => compartment,
            _ => own,
        }
    }

    /// The reason, for the owner and the run.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.reason {
            HeldReason::Uncited => "names no source".into(),
            HeldReason::UnknownSource(source) => format!("cites `{source}`, which was not found"),
            HeldReason::OtherSource {
                source,
                compartment,
            } => format!(
                "cites `{source}`, a conversation with others in it ({})",
                compartment.channel().unwrap_or("internal")
            ),
        }
    }
}

/// The sources `line` cites in a trailing `(source: a, b)` group.
#[must_use]
pub fn sources(line: &str) -> Vec<String> {
    let trimmed = line.trim_end();
    let Some(body) = trimmed.strip_suffix(')') else {
        return Vec::new();
    };
    let Some(open) = body.rfind('(') else {
        return Vec::new();
    };
    let inner = body[open + 1..].trim_start();
    let lower = inner.to_ascii_lowercase();
    let rest = ["sources:", "source:"]
        .iter()
        .find_map(|prefix| lower.starts_with(prefix).then(|| &inner[prefix.len()..]));
    rest.map(|rest| {
        rest.split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Whether a line carries content that needs a source: not blank, not a
/// heading, a rule, or a code fence.
fn needs_source(line: &str) -> bool {
    let t = line.trim();
    !(t.is_empty()
        || t.starts_with('#')
        || t.starts_with("```")
        || t.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')))
}

/// The lines of `candidate` that need a source and are not already in
/// `existing` (compared trimmed, so moving or re-indenting a line adds
/// nothing).
fn added<'a>(existing: &str, candidate: &'a str) -> Vec<(usize, &'a str)> {
    let old: HashSet<&str> = existing.lines().map(str::trim).collect();
    candidate
        .split('\n')
        .enumerate()
        .filter(|(_, line)| needs_source(line) && !old.contains(line.trim()))
        .collect()
}

/// The sources the added lines of `candidate` cite, so the caller can
/// classify each once.
#[must_use]
pub fn cited(existing: &str, candidate: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (_, line) in added(existing, candidate) {
        for source in sources(line) {
            if !out.contains(&source) {
                out.push(source);
            }
        }
    }
    out
}

/// `candidate` with every added line that may not pass removed, and the
/// lines removed. A line passes when it cites at least one source and
/// every source it cites is [`SourceClass::Owner`]; a source missing
/// from `classes` counts as unknown.
#[must_use]
pub fn screen(
    existing: &str,
    candidate: &str,
    classes: &HashMap<String, SourceClass>,
) -> (String, Vec<Held>) {
    let mut held_at = HashSet::new();
    let mut held = Vec::new();
    for (index, line) in added(existing, candidate) {
        let cited = sources(line);
        let reason = if cited.is_empty() {
            Some(HeldReason::Uncited)
        } else {
            cited
                .into_iter()
                .find_map(|source| match classes.get(&source) {
                    Some(SourceClass::Owner) => None,
                    Some(SourceClass::Other(compartment)) => Some(HeldReason::OtherSource {
                        source,
                        compartment: compartment.clone(),
                    }),
                    Some(SourceClass::Unknown) | None => Some(HeldReason::UnknownSource(source)),
                })
        };
        if let Some(reason) = reason {
            held_at.insert(index);
            held.push(Held {
                line: line.trim_end().to_string(),
                reason,
            });
        }
    }
    let kept = candidate
        .split('\n')
        .enumerate()
        .filter(|(index, _)| !held_at.contains(index))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    (kept, held)
}

/// The channel a source names, when it names one: `chan_…` or `dm_…`,
/// with any `#event` suffix dropped.
#[must_use]
pub fn channel_of(source: &str) -> Option<&str> {
    let channel = source.split('#').next().unwrap_or(source).trim();
    (channel.starts_with("chan_") || channel.starts_with("dm_")).then_some(channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guest() -> Compartment {
        Compartment::Conversation {
            channel: "dm_guest".into(),
        }
    }

    #[test]
    fn sources_are_read_from_a_trailing_group() {
        assert_eq!(
            sources("- likes tea (source: /local-user, chan_a#e1)"),
            vec!["/local-user", "chan_a#e1"]
        );
        assert_eq!(sources("- x (Sources: /a; /b)  "), vec!["/a", "/b"]);
        assert!(sources("- likes tea (green)").is_empty());
        assert!(sources("- (source: /a) but more").is_empty());
        assert_eq!(channel_of("chan_a#e1"), Some("chan_a"));
        assert_eq!(channel_of("dm_ff"), Some("dm_ff"));
        assert_eq!(channel_of("/local-user"), None);
    }

    #[test]
    fn only_owner_sourced_added_lines_pass() {
        let existing = "# Memory\n\n- old fact\n";
        let candidate = "# Memory\n\n- old fact\n- owner fact (source: /local-user)\n\
                         - guest fact (source: /user-x)\n- bare fact\n\
                         - mixed (source: /local-user, /user-x)\n- ghost (source: /nope)\n\n## New\n";
        let classes = HashMap::from([
            ("/local-user".to_string(), SourceClass::Owner),
            ("/user-x".to_string(), SourceClass::Other(guest())),
            ("/nope".to_string(), SourceClass::Unknown),
        ]);
        assert_eq!(
            cited(existing, candidate),
            vec!["/local-user", "/user-x", "/nope"]
        );
        let (kept, held) = screen(existing, candidate, &classes);
        assert_eq!(
            kept,
            "# Memory\n\n- old fact\n- owner fact (source: /local-user)\n\n## New\n"
        );
        let reasons: Vec<_> = held.iter().map(|h| h.reason.clone()).collect();
        assert_eq!(
            reasons,
            vec![
                HeldReason::OtherSource {
                    source: "/user-x".into(),
                    compartment: guest()
                },
                HeldReason::Uncited,
                HeldReason::OtherSource {
                    source: "/user-x".into(),
                    compartment: guest()
                },
                HeldReason::UnknownSource("/nope".into()),
            ]
        );
        assert_eq!(held[0].inbox_compartment(&Compartment::Internal), &guest());
        assert_eq!(
            held[1].inbox_compartment(&Compartment::Internal),
            &Compartment::Internal
        );
    }

    #[test]
    fn moved_and_reindented_lines_are_not_new() {
        let existing = "- a\n- b\n";
        let (kept, held) = screen(existing, "  - b\n- a", &HashMap::new());
        assert!(held.is_empty());
        assert_eq!(kept, "  - b\n- a");
    }

    #[test]
    fn only_cron_owner_runs_are_scheduled() {
        let mut stamp = ActingFor::runtime();
        assert!(!is_scheduled_owner(&stamp));
        stamp.origin = Origin::Cron {
            job_id: "keepalive".into(),
        };
        assert!(is_scheduled_owner(&stamp) && !is_memory_run(&stamp));
        stamp.origin = Origin::Cron {
            job_id: MEMORY_JOB_ID.into(),
        };
        assert!(is_memory_run(&stamp));
        let guest = stamp.capped(peko_tools_core::Trust::Trusted);
        assert!(!is_scheduled_owner(&guest) && !is_memory_run(&guest));
    }
}
