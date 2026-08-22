//! On-disk path helpers for `ChannelStore`.
//!
//! The store persists per-channel state under
//! `<runtime_dir>/channels/<channel_id>/...`. With typed prefixes
//! (`principal:<did>` etc.) the directory name gains colons, which
//! are invalid in directory names on Windows and on classic Unix
//! filesystems. This module keeps the wire form human-readable
//! (`principal:did:peko:abc`) and normalizes only the storage path.
//!
//! The helper is intentionally one-way trivial — a literal `:` is
//! replaced with the percent-encoding-ish marker `.3A.` (matching
//! the convention used by other peko-store modules for cross-platform
//! path safety). `chan_<8 base36>` ids contain no colons, so the
//! helper is a no-op for them and the bare-form on-disk layout is
//! unchanged.

use peko_protocol::channel::{ChannelId, ChannelKind};

/// On-disk directory name for `channel`.
///
/// For typed prefixes (`principal:<did>` / `user:<id>` / `group:<slug>`),
/// every `:` is replaced with `.3A.`. For `Bare` ids (`chan_<...>`),
/// the helper is a no-op (no colons in the wire form).
///
/// Wire form and on-disk form are recoverable from one another via
/// [`channel_dir_name_inverse`].
pub fn channel_dir_name(id: &ChannelId) -> String {
    id.as_str().replace(':', ".3A.")
}

/// Inverse of [`channel_dir_name`]: replace every `.3A.` with `:` and
/// re-parse the result. Returns `None` if the reconstructed wire form
/// is not a valid `ChannelId` (corrupt directory name, or a name that
/// happens to contain `.3A.` that wasn't a marker — the prefix
/// dispatch in `ChannelId::parse` is the authoritative validator).
pub fn channel_dir_name_inverse(s: &str) -> Option<ChannelId> {
    let wire = s.replace(".3A.", ":");
    ChannelId::parse(&wire)
}

/// Is `id` one of the four wire forms that contains a colon? Used by
/// the directory walk in `ChannelStore::list_channels_for_principal`
/// to filter the listing before trying to parse each name. Mirrors
/// [`ChannelKind`] exactly.
pub fn id_needs_colon_normalization(id: &ChannelId) -> bool {
    !matches!(id.kind(), ChannelKind::Bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare-form ids (`chan_<...>`) have no colons, so the helper is
    /// a no-op. This pins the on-disk layout for pre-PR channels —
    /// no migration needed.
    #[test]
    fn bare_id_round_trips_as_identity() {
        let id = ChannelId::parse("chan_a1b2c3d4").unwrap();
        assert_eq!(channel_dir_name(&id), "chan_a1b2c3d4");
        let back = channel_dir_name_inverse("chan_a1b2c3d4").unwrap();
        assert_eq!(back, id);
    }

    /// Principal-prefix ids gain colons that need replacing. The
    /// wire form (`principal:did:key:zAlice`) has THREE colons (the
    /// prefix marker + the two DID scheme separators); all three
    /// are replaced. The marker is symmetric — `channel_dir_name` and
    /// `channel_dir_name_inverse` are inverses on any wire form.
    #[test]
    fn principal_id_replaces_all_colons() {
        let id = ChannelId::for_principal("did:key:zAlice");
        assert_eq!(
            channel_dir_name(&id),
            "principal.3A.did.3A.key.3A.zAlice"
        );
        let back = channel_dir_name_inverse("principal.3A.did.3A.key.3A.zAlice").unwrap();
        assert_eq!(back, id);
    }

    /// User-prefix ids with colons inside the user-id (uncommon but
    /// possible — no validation at the boundary). Both colons
    /// replace.
    #[test]
    fn user_id_with_embedded_colon_round_trips() {
        let id = ChannelId::parse("user:user:alice").unwrap();
        assert_eq!(channel_dir_name(&id), "user.3A.user.3A.alice");
        let back = channel_dir_name_inverse("user.3A.user.3A.alice").unwrap();
        assert_eq!(back, id);
    }

    /// Group-prefix ids with hyphens / slashes — slashes are invalid
    /// on most filesystems too, but the LLM shouldn't pick a slug
    /// with a slash (the channel tool description will warn). The
    /// helper still works for any other slug character. The prefix
    /// separator colon is normalized like every other colon.
    #[test]
    fn group_id_round_trips() {
        let id = ChannelId::for_group("eng-standup");
        assert_eq!(channel_dir_name(&id), "group.3A.eng-standup");
        let back = channel_dir_name_inverse("group.3A.eng-standup").unwrap();
        assert_eq!(back, id);
    }

    /// Inverse: a string that LOOKS like a valid wire form (rejects
    /// the percent-decoding rule) should fail the parse cleanly
    /// rather than silently producing a degenerate id. This is the
    /// defense-in-depth: the directory walk skips such entries.
    #[test]
    fn inverse_rejects_non_wire_form() {
        assert!(channel_dir_name_inverse("not-a-channel").is_none());
        // The marker-decoded form must still pass `ChannelId::parse`.
        assert!(channel_dir_name_inverse("random.3A.thing").is_none());
    }

    /// `id_needs_colon_normalization` mirrors `ChannelKind` exactly:
    /// every typed form needs it, the bare form doesn't.
    #[test]
    fn id_needs_normalization_matches_kind() {
        assert!(!id_needs_colon_normalization(
            &ChannelId::parse("chan_a1b2c3d4").unwrap()
        ));
        assert!(id_needs_colon_normalization(
            &ChannelId::for_principal("did:key:zAlice")
        ));
        assert!(id_needs_colon_normalization(
            &ChannelId::for_user("alice")
        ));
        assert!(id_needs_colon_normalization(
            &ChannelId::for_group("eng-standup")
        ));
    }
}