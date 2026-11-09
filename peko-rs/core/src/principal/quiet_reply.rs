//! What a conversation turn's reply posts.
//!
//! A turn's final text is posted to its conversation. Pekos need a way to
//! post nothing. Otherwise they explain why they have nothing to say: the
//! 2026-11-09 collaboration benchmark found a "juno's post needs no answer
//! from me" in every run, and an "I've asked Maya to approve" after every
//! hand-up the board item already showed.
//!
//! - A reply of just [`NO_REPLY`] posts nothing. The conversation role and
//!   the approval-pending tool result tell the peko when to use it. The
//!   marker on a last line of its own is dropped, and the rest is posted.
//! - In a group, a short reply with no question that only says it needs
//!   no answer, or has nothing to add, posts nothing either. This guard
//!   catches the peko that forgets the marker.

/// The reply that posts nothing.
pub(crate) const NO_REPLY: &str = "NO_REPLY";

/// Longest reply the group guard drops. The posts it targets are two or
/// three sentences; anything longer likely carries content.
const GUARD_MAX_CHARS: usize = 300;

/// Phrases of a reply that only says the peko has nothing to say.
const NOTHING_TO_SAY: &[&str] = &[
    "nothing to add",
    "nothing for me to add",
    "need no answer",
    "needs no answer",
    "no answer needed",
    "no answer is needed",
    "doesn't need an answer",
    "does not need an answer",
    "don't need an answer",
    "do not need an answer",
    "need no reply",
    "needs no reply",
    "no reply needed",
    "no reply is needed",
    "doesn't need a reply",
    "does not need a reply",
    "no response needed",
    "no response is needed",
];

/// Words that turn "nothing to add" into "nothing to add, but …".
const CONTRAST: &[&str] = &[" but ", " however", " except", " although", " though "];

/// The text to post for a turn's `reply`, or `None` to post nothing.
/// `group` applies the group guard (see the module docs).
pub(crate) fn postable(reply: &str, group: bool) -> Option<&str> {
    let mut text = reply.trim();
    if let Some((body, last)) = text.rsplit_once('\n') {
        if is_marker(last) {
            text = body.trim_end();
        }
    }
    if text.is_empty() || is_marker(text) {
        return None;
    }
    if group && only_says_nothing(text) {
        tracing::info!(reply = %text, "conversation reply only says nothing is needed; posting nothing");
        return None;
    }
    Some(text)
}

fn is_marker(line: &str) -> bool {
    line.trim()
        .trim_matches(|c: char| matches!(c, '`' | '*' | '_' | '.' | '"' | '\'' | '(' | ')'))
        .trim()
        .eq_ignore_ascii_case(NO_REPLY)
}

fn only_says_nothing(text: &str) -> bool {
    if text.chars().count() > GUARD_MAX_CHARS || text.contains('?') {
        return false;
    }
    let lower = text.to_lowercase().replace('\u{2019}', "'");
    NOTHING_TO_SAY.iter().any(|p| lower.contains(p)) && !CONTRAST.iter().any(|c| lower.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_posts_nothing() {
        for reply in [
            "NO_REPLY",
            "  no_reply \n",
            "`NO_REPLY`",
            "**NO_REPLY**",
            "NO_REPLY.",
            "(NO_REPLY)",
        ] {
            assert_eq!(postable(reply, false), None, "{reply:?}");
            assert_eq!(postable(reply, true), None, "{reply:?}");
        }
        assert_eq!(postable("   ", true), None);
    }

    #[test]
    fn a_marker_on_its_own_last_line_is_dropped() {
        assert_eq!(
            postable("Done: tests pass.\n\nNO_REPLY", false),
            Some("Done: tests pass.")
        );
        // Inside a sentence it is just text.
        assert_eq!(
            postable("Reply NO_REPLY to stay quiet.", true),
            Some("Reply NO_REPLY to stay quiet.")
        );
    }

    /// The posts the 2026-11-09 benchmark runs made.
    #[test]
    fn a_group_reply_that_only_says_nothing_is_needed_posts_nothing() {
        let seen = [
            "Juno's fix is on branch `juno/fix-median-even` (commit e9b266e, based on main 92711ef). \
             All 4 tests pass, and it hasn't been merged. That covers Noah's request, so I have nothing to add.",
            "Juno's posts need no answer from me. Juno pushed the median fix to `juno/fix-median-even` \
             (commit 3c5a35d) and reports that all 4 tests pass. That covers what Noah asked me for, \
             so I have nothing to add.",
            "Juno\u{2019}s post doesn\u{2019}t need an answer from me. The test run was assigned to juno, who holds \
             the board item and is waiting on Maya's approval. I have no shell in this group either, \
             so I'm leaving it with juno.",
        ];
        for reply in seen {
            assert_eq!(postable(reply, true), None, "{reply}");
            // In a direct message the guard does not apply.
            assert_eq!(postable(reply, false), Some(reply));
        }
    }

    #[test]
    fn the_group_guard_keeps_questions_contrasts_and_long_posts() {
        let kept = [
            "Nothing to add from me. Should I merge it?",
            "I have nothing to add, but CI is red on main.",
            "Thanks, the fix looks right.",
        ];
        for reply in kept {
            assert_eq!(postable(reply, true), Some(reply), "{reply}");
        }
        let long = format!(
            "{} Nothing to add.",
            "The fix is on its branch and the tests pass. ".repeat(8)
        );
        assert_eq!(postable(&long, true), Some(long.as_str()));
    }
}
