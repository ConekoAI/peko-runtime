//! Concrete capability intersection for channels.
//!
//! **Hard-coded for channels. NOT a generic abstraction.** Per
//! `prefer-concrete-over-speculative-abstraction.md`: generalize on
//! second use; channels are the first use.
//!
//! ## What this does
//!
//! A channel needs an "intersected cap set" for two callers:
//!
//! 1. **The channel itself** (`Channel.caps`) — the union of all members'
//!    cap sets, restricted to whatever the channel *needs* to do (post,
//!    read, manage membership).
//! 2. **A given member's effective view** (`intersect_member_caps`) —
//!    used when the responder (PR-2) decides whether a principal has
//!    authority to take an action *as that member* in this channel.
//!
//! PR-1 ships the second function only. The first is deferred to PR-2
//! because computing the "channel union" requires knowing all members
//! in O(N) and PR-1's fan-out cap is 8 — at that scale, callers can
//! iterate inline if they need to. We avoid carrying an unused
//! abstraction.
//!
//! ## Caps representation (PR-1 placeholder)
//!
//! We don't yet know the exact cap shape the engine will use —
//! `peko-rs/principal/src/capability_evaluator*` is referenced from
//! `phase-c-write-side-gate.md` and the `caps` field doesn't have a
//! stable wire type yet. PR-1 ships the function on a *placeholder*
//! cap set so we can lock the public signature. PR-2 swaps the
//! placeholder for the real type once the engine-side contracts land.
//!
//! Per the rule on `phase-c-write-side-gate.md` ("don't extend
//! `peko-protocol`'s dep list beyond `serde + serde_json`"), the
//! placeholder is a `BTreeMap<String, Vec<String>>` — every principal
//! domain has the same shape: `domain → actions`.

use std::collections::BTreeMap;

use peko_plan::PrincipalId;

// ---------------------------------------------------------------------------
// Placeholder cap type
// ---------------------------------------------------------------------------

/// Placeholder for the real cap set type. Used in PR-1 to lock the
/// signature; replaced in PR-2 by the principal-domain `Caps` type
/// (likely `peko_principal::Caps` after Phase 14.c.1).
///
/// Convention: a member "has" an action iff the action string appears
/// in the action list for the domain. Trust levels (`min`, `high`, etc.)
/// are out of scope here — PR-1 assumes principal caps are action-only;
/// trust grading lands alongside the responder in PR-2.
pub type Caps = BTreeMap<String, Vec<String>>;

/// The channel-side cap set. For PR-1, this is the union of all
/// members' `Caps` keyed by domain. Empty domains (no member has the
/// action) are stripped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ChannelCaps(pub Caps);

impl ChannelCaps {
    /// Construct an empty channel cap set. Useful for tests.
    pub fn empty() -> Self {
        Self(Default::default())
    }
}

// ---------------------------------------------------------------------------
// Intersection
// ---------------------------------------------------------------------------

/// Compute the actions a member is allowed to take in a channel.
///
/// PR-1's intersection is **additive only**: the member's action set
/// in each domain is the *intersection* of the channel's domain
/// actions and the member's domain actions. Domain keys not present
/// in *both* the channel and the member are dropped (no implicit
/// "allow all" for missing domains).
///
/// `channel` — the channel's effective cap set (already intersected
///             across members; the caller computes the union first if
///             they don't have it precomputed).
/// `member`  — the principal's own cap set.
pub fn intersect_member_caps(channel: &Caps, member: &Caps) -> Caps {
    let mut out: Caps = Default::default();
    for (domain, allowed) in channel {
        let Some(member_actions) = member.get(domain) else {
            // Member has no caps in this domain → drop it entirely.
            continue;
        };
        // Intersection: keep actions present in both lists. Preserve
        // channel-side ordering (caller-curated) so the wire shape is
        // deterministic; avoids accidental churn for IPC consumers.
        let inter: Vec<String> = allowed
            .iter()
            .filter(|a| member_actions.iter().any(|m| m == *a))
            .cloned()
            .collect();
        if !inter.is_empty() {
            out.insert(domain.clone(), inter);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Channel-cursor step (placeholder for PR-3)
// ---------------------------------------------------------------------------

/// PR-3 will need a "did this principal act in this channel recently?"
/// helper to drive attribution. PR-1 ships the function stub returning
/// `None` so signature is locked; PR-2/3 replace the body once cursor
/// files exist (per `lexical-soaring-pretzel.md` cursor module).
pub fn last_action_by(_principal: &PrincipalId) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with(domain: &str, actions: &[&str]) -> Caps {
        let mut c = Caps::new();
        c.insert(
            domain.to_string(),
            actions.iter().map(|s| s.to_string()).collect(),
        );
        c
    }

    #[test]
    fn intersection_keeps_only_actions_in_both() {
        let mut channel = Caps::new();
        channel.insert(
            "channel.post".into(),
            vec!["create".into(), "reply".into(), "delete".into()],
        );
        channel.insert(
            "channel.member".into(),
            vec!["invite".into(), "kick".into()],
        );

        let mut member = Caps::new();
        member.insert("channel.post".into(), vec!["create".into(), "reply".into()]);
        member.insert("channel.member".into(), vec!["invite".into()]);

        let out = intersect_member_caps(&channel, &member);
        assert_eq!(
            out.get("channel.post").unwrap(),
            &vec!["create".to_string(), "reply".to_string()]
        );
        assert_eq!(
            out.get("channel.member").unwrap(),
            &vec!["invite".to_string()]
        );
    }

    #[test]
    fn intersection_drops_domains_member_lacks() {
        let channel = caps_with("channel.post", &["create"]);
        let member = caps_with("channel.member", &["invite"]);

        let out = intersect_member_caps(&channel, &member);
        assert!(out.is_empty(), "expected empty intersection, got {out:?}");
    }

    #[test]
    fn intersection_drops_domains_channel_lacks() {
        let channel = caps_with("channel.post", &["create"]);
        let mut member = Caps::new();
        member.insert("channel.post".into(), vec!["create".into()]);
        member.insert("totally.unrelated".into(), vec!["whatever".into()]);

        let out = intersect_member_caps(&channel, &member);
        assert!(!out.contains_key("totally.unrelated"));
        assert!(out.contains_key("channel.post"));
    }

    #[test]
    fn intersection_drops_empty_action_lists() {
        let channel = caps_with("channel.post", &["create"]);
        let member = caps_with("channel.post", &[]);

        let out = intersect_member_caps(&channel, &member);
        assert!(out.is_empty());
    }
}