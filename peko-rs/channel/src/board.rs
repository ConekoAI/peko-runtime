//! A group's board (ADR-101): shared work, folded from the group's own log.
//!
//! A post may carry a [`BoardOp`]. The board is the fold of every op in
//! the log, so it is signed, ordered, mirrored, served as history, and
//! followed to other computers exactly as posts are. An item's id is the
//! `msg_id` of the post that added it.
//!
//! Authority comes from the post's author, compared by immutable actor
//! key (ADR-081 D1), never from the op's contents. The same rules
//! ([`Board::check`]) refuse an op before it is posted and make the fold
//! ignore one that arrives from a copy that broke them.
//!
//! Claims are advisory. When two claims on one item cross in transit,
//! the earlier `at` wins, ties broken by `msg_id`, so every copy that
//! holds both posts folds the same claimant.
//!
//! The runtime also keeps, per channel and principal, which check-ins
//! already brought an item back (`board_check_ins.json`, beside
//! `read_marks.json`): a check-in fires once (D6).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use peko_protocol::channel::{BoardOp, BoardState, ChannelEvent};
use peko_subject::{ActorKey, ActorKeys, ActorRef};

use crate::port::{ChannelError, Result};

/// Longest item title, in characters.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest item note, in characters.
pub const MAX_NOTE_CHARS: usize = 500;
/// A check-in is at least this far ahead when it is set.
pub const MIN_CHECK_IN_MINUTES: i64 = 10;
/// A check-in is at most this far ahead when it is set.
pub const MAX_CHECK_IN_DAYS: i64 = 30;
/// A `blocked` update without a check-in gets one this far ahead.
pub const DEFAULT_BLOCKED_CHECK_IN_HOURS: i64 = 24;
/// Done items stay in the rendered board this long.
pub const DONE_VISIBLE_HOURS: i64 = 24;
/// Characters of an item id shown as its short reference.
pub const SHORT_ID_CHARS: usize = 6;

/// One piece of work on the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardItem {
    /// The `msg_id` of the post that added it.
    pub id: String,
    pub title: String,
    /// The adder's address, as written on its post.
    pub added_by: String,
    /// The member it was asked of, if any.
    #[serde(rename = "for", skip_serializing_if = "Option::is_none")]
    pub for_member: Option<String>,
    pub state: BoardState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_at: Option<DateTime<Utc>>,
    pub added_at: String,
    pub updated_at: String,
    /// The winning claim's `(at, msg_id)`, for the crossing-claims rule.
    #[serde(skip)]
    claim_stamp: Option<(String, String)>,
}

impl BoardItem {
    /// The short reference shown to models and people (`b3f9a1`).
    pub fn short_id(&self) -> String {
        short_id(&self.id)
    }

    /// Whether the item is being worked (`doing` or `blocked`).
    pub fn is_active(&self) -> bool {
        matches!(self.state, BoardState::Doing | BoardState::Blocked)
    }

    /// The check-in's key in the fired record: one item, one time.
    pub fn check_in_key(&self) -> Option<String> {
        self.check_at
            .map(|at| format!("{}@{}", self.id, at.to_rfc3339()))
    }
}

/// The short reference of item id `id`.
pub fn short_id(id: &str) -> String {
    id.trim_start_matches("msg_")
        .chars()
        .take(SHORT_ID_CHARS)
        .collect()
}

/// A group's board: items in the order they were added.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Board {
    pub items: Vec<BoardItem>,
}

/// Who may change what: the actor registry and the group's owner and
/// admins (addresses).
pub struct Authority<'a> {
    pub keys: &'a dyn ActorKeys,
    pub managers: Vec<ActorKey>,
}

impl<'a> Authority<'a> {
    /// `owner` and `admins` are addresses; unreadable ones are skipped.
    pub fn new(keys: &'a dyn ActorKeys, owner: Option<&str>, admins: &[String]) -> Self {
        let managers = owner
            .into_iter()
            .chain(admins.iter().map(String::as_str))
            .filter_map(|a| a.parse::<ActorRef>().ok())
            .map(|a| keys.key_of(&a))
            .collect();
        Self { keys, managers }
    }

    fn key(&self, address: &str) -> Option<ActorKey> {
        address
            .parse::<ActorRef>()
            .ok()
            .filter(|a| !matches!(a, ActorRef::Public))
            .map(|a| self.keys.key_of(&a))
    }

    fn same(&self, a: &str, b: &str) -> bool {
        match (self.key(a), self.key(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn manages(&self, address: &str) -> bool {
        self.key(address)
            .is_some_and(|k| self.managers.contains(&k))
    }
}

/// Whether the fold or a poster is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Before posting: the poster has seen the board, so a held item is
    /// refused.
    Post,
    /// Folding the log: a claim that crossed an earlier one in transit
    /// loses to it, whichever arrived first.
    Fold,
}

impl Board {
    /// Fold a channel's events, oldest first.
    pub fn fold<'e>(
        events: impl IntoIterator<Item = &'e ChannelEvent>,
        auth: &Authority<'_>,
    ) -> Self {
        let mut board = Self::default();
        for ev in events {
            if let ChannelEvent::Posted {
                author,
                at,
                msg_id: Some(msg_id),
                board: Some(op),
                ..
            } = ev
            {
                board.apply(author, at, msg_id, op, auth);
            }
        }
        board
    }

    /// The item `reference` names: a full id or a unique short prefix
    /// (with or without `#`).
    pub fn find(&self, reference: &str) -> std::result::Result<&BoardItem, String> {
        let r = reference.trim().trim_start_matches('#');
        if let Some(item) = self.items.iter().find(|i| i.id == r) {
            return Ok(item);
        }
        let r = r.trim_start_matches("msg_");
        if r.is_empty() {
            return Err("name an item by its id".to_string());
        }
        let matches: Vec<&BoardItem> = self
            .items
            .iter()
            .filter(|i| i.id.trim_start_matches("msg_").starts_with(r))
            .collect();
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(format!("the board has no item #{r}")),
            _ => Err(format!("#{r} names more than one item; use a longer id")),
        }
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    /// Whether `author` may make `op` now. `Err` says why not, in words
    /// a model can act on.
    pub fn check(
        &self,
        author: &str,
        op: &BoardOp,
        auth: &Authority<'_>,
    ) -> std::result::Result<(), String> {
        self.check_in(author, "", "", op, auth, Mode::Post)
    }

    fn check_in(
        &self,
        author: &str,
        at: &str,
        msg_id: &str,
        op: &BoardOp,
        auth: &Authority<'_>,
        mode: Mode,
    ) -> std::result::Result<(), String> {
        let Some(id) = op.item() else {
            return Ok(());
        };
        let item = match mode {
            Mode::Post => self.find(id)?,
            Mode::Fold => self
                .index(id)
                .map(|i| &self.items[i])
                .ok_or_else(|| format!("no item {id}"))?,
        };
        let title = &item.title;
        let claimant = item.claimant.as_deref();
        let is_claimant = claimant.is_some_and(|c| auth.same(c, author));
        let is_adder = auth.same(&item.added_by, author);
        let manages = auth.manages(author);
        match op {
            BoardOp::Add { .. } => Ok(()),
            BoardOp::Claim { .. } => {
                if item.state == BoardState::Done {
                    return Err(format!("“{title}” is done"));
                }
                match claimant {
                    None => Ok(()),
                    Some(_) if is_claimant => Ok(()),
                    Some(holder) => {
                        let earlier = item
                            .claim_stamp
                            .as_ref()
                            .is_some_and(|(t, m)| (at, msg_id) < (t.as_str(), m.as_str()));
                        if mode == Mode::Fold && earlier {
                            Ok(())
                        } else {
                            Err(format!("“{title}” is already claimed by {holder}"))
                        }
                    }
                }
            }
            BoardOp::Update { state, .. } => {
                if !(is_claimant || is_adder || manages) {
                    return Err(format!(
                        "only whoever claimed “{title}”, whoever added it, or the group's owner or an admin may update it"
                    ));
                }
                match state {
                    BoardState::Open => Err("to reopen an item, release it".to_string()),
                    BoardState::Doing | BoardState::Blocked if claimant.is_none() => {
                        Err(format!("“{title}” has no claimant; claim it first"))
                    }
                    _ => Ok(()),
                }
            }
            BoardOp::Release { .. } => {
                if claimant.is_none() {
                    return Err(format!("nobody holds “{title}”"));
                }
                if is_claimant || manages {
                    Ok(())
                } else {
                    Err(format!(
                        "only whoever claimed “{title}”, or the group's owner or an admin, may release it"
                    ))
                }
            }
            BoardOp::Drop { .. } => {
                if is_adder || manages {
                    Ok(())
                } else {
                    Err(format!(
                        "only whoever added “{title}”, or the group's owner or an admin, may remove it"
                    ))
                }
            }
        }
    }

    /// Apply one op from the log; one the rules refuse is ignored.
    fn apply(&mut self, author: &str, at: &str, msg_id: &str, op: &BoardOp, auth: &Authority<'_>) {
        if self
            .check_in(author, at, msg_id, op, auth, Mode::Fold)
            .is_err()
        {
            return;
        }
        if let BoardOp::Add { title, for_member } = op {
            if self.index(msg_id).is_none() {
                self.items.push(BoardItem {
                    id: msg_id.to_string(),
                    title: title.clone(),
                    added_by: author.to_string(),
                    for_member: for_member.clone(),
                    state: BoardState::Open,
                    claimant: None,
                    note: None,
                    check_at: None,
                    added_at: at.to_string(),
                    updated_at: at.to_string(),
                    claim_stamp: None,
                });
            }
            return;
        }
        let Some(i) = op.item().and_then(|id| self.index(id)) else {
            return;
        };
        if let BoardOp::Drop { .. } = op {
            self.items.remove(i);
            return;
        }
        let item = &mut self.items[i];
        item.updated_at = at.to_string();
        match op {
            BoardOp::Claim { check_at, .. } => {
                let same_holder = item
                    .claimant
                    .as_deref()
                    .is_some_and(|c| auth.same(c, author));
                if !same_holder {
                    item.claimant = Some(author.to_string());
                    item.claim_stamp = Some((at.to_string(), msg_id.to_string()));
                    item.note = None;
                    item.check_at = None;
                }
                item.state = BoardState::Doing;
                if let Some(when) = parse_time(check_at.as_deref()) {
                    item.check_at = Some(when);
                }
            }
            BoardOp::Update {
                state,
                note,
                check_at,
                ..
            } => {
                item.state = *state;
                if let Some(note) = note {
                    item.note = Some(note.clone());
                }
                if *state == BoardState::Done {
                    item.check_at = None;
                } else if let Some(when) = parse_time(check_at.as_deref()) {
                    item.check_at = Some(when);
                }
            }
            BoardOp::Release { .. } => {
                item.claimant = None;
                item.claim_stamp = None;
                item.state = BoardState::Open;
                item.check_at = None;
            }
            BoardOp::Add { .. } | BoardOp::Drop { .. } => {}
        }
    }

    /// Items `me` holds whose check-in is due at `now` and not in `fired`.
    pub fn due_check_ins<'b>(
        &'b self,
        me: &[&str],
        auth: &Authority<'_>,
        now: DateTime<Utc>,
        fired: &[String],
    ) -> Vec<&'b BoardItem> {
        self.items
            .iter()
            .filter(|i| i.is_active() && i.check_at.is_some_and(|at| at <= now))
            .filter(|i| {
                i.claimant
                    .as_deref()
                    .is_some_and(|c| me.iter().any(|m| auth.same(c, m)))
            })
            .filter(|i| i.check_in_key().is_some_and(|k| !fired.contains(&k)))
            .collect()
    }

    /// Whether `member` holds `item`.
    pub fn holds(item: &BoardItem, member: &str, auth: &Authority<'_>) -> bool {
        item.claimant
            .as_deref()
            .is_some_and(|c| auth.same(c, member))
    }

    /// Whether `member` is the one `item` was asked of.
    pub fn asked_of(item: &BoardItem, member: &str, auth: &Authority<'_>) -> bool {
        item.for_member
            .as_deref()
            .is_some_and(|f| auth.same(f, member))
    }

    /// Items worth showing at `now`: everything not done, and what was
    /// done in the last [`DONE_VISIBLE_HOURS`].
    pub fn visible(&self, now: DateTime<Utc>) -> impl Iterator<Item = &BoardItem> {
        let cutoff = now - Duration::hours(DONE_VISIBLE_HOURS);
        self.items.iter().filter(move |i| {
            i.state != BoardState::Done
                || parse_time(Some(&i.updated_at)).is_none_or(|at| at >= cutoff)
        })
    }
}

fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// Check an op's shape and limits before it is posted, and return it
/// trimmed. `now` bounds a check-in (D6); a `blocked` update without one
/// gets one [`DEFAULT_BLOCKED_CHECK_IN_HOURS`] ahead.
pub fn validate_op(op: &BoardOp, now: DateTime<Utc>) -> std::result::Result<BoardOp, String> {
    let check_in = |check_at: &Option<String>| -> std::result::Result<Option<String>, String> {
        let Some(raw) = check_at.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let when = parse_time(Some(raw))
            .ok_or_else(|| format!("check_at {raw:?} is not an RFC 3339 time"))?;
        if when < now + Duration::minutes(MIN_CHECK_IN_MINUTES) {
            return Err(format!(
                "a check-in must be at least {MIN_CHECK_IN_MINUTES} minutes ahead"
            ));
        }
        if when > now + Duration::days(MAX_CHECK_IN_DAYS) {
            return Err(format!(
                "a check-in must be at most {MAX_CHECK_IN_DAYS} days ahead"
            ));
        }
        Ok(Some(when.to_rfc3339()))
    };
    let item_ref = |item: &str| -> std::result::Result<String, String> {
        let item = item.trim();
        if item.is_empty() {
            Err("name the item".to_string())
        } else {
            Ok(item.to_string())
        }
    };
    Ok(match op {
        BoardOp::Add { title, for_member } => {
            let title = title.trim();
            if title.is_empty() {
                return Err("an item needs a title".to_string());
            }
            if title.contains(['\n', '\r']) {
                return Err("an item's title is one line".to_string());
            }
            if title.chars().count() > MAX_TITLE_CHARS {
                return Err(format!(
                    "an item's title is at most {MAX_TITLE_CHARS} characters"
                ));
            }
            BoardOp::Add {
                title: title.to_string(),
                for_member: for_member
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            }
        }
        BoardOp::Claim { item, check_at } => BoardOp::Claim {
            item: item_ref(item)?,
            check_at: check_in(check_at)?,
        },
        BoardOp::Update {
            item,
            state,
            note,
            check_at,
        } => {
            let note = note
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if note
                .as_deref()
                .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
            {
                return Err(format!("a note is at most {MAX_NOTE_CHARS} characters"));
            }
            let mut check_at = check_in(check_at)?;
            match state {
                BoardState::Open => return Err("to reopen an item, release it".to_string()),
                BoardState::Done => check_at = None,
                BoardState::Blocked if check_at.is_none() => {
                    check_at =
                        Some((now + Duration::hours(DEFAULT_BLOCKED_CHECK_IN_HOURS)).to_rfc3339());
                }
                _ => {}
            }
            BoardOp::Update {
                item: item_ref(item)?,
                state: *state,
                note,
                check_at,
            }
        }
        BoardOp::Release { item } => BoardOp::Release {
            item: item_ref(item)?,
        },
        BoardOp::Drop { item } => BoardOp::Drop {
            item: item_ref(item)?,
        },
    })
}

/// `op` naming `id` as its item (a short reference resolved).
pub fn with_item(op: BoardOp, id: &str) -> BoardOp {
    let id = id.to_string();
    match op {
        BoardOp::Add { .. } => op,
        BoardOp::Claim { check_at, .. } => BoardOp::Claim { item: id, check_at },
        BoardOp::Update {
            state,
            note,
            check_at,
            ..
        } => BoardOp::Update {
            item: id,
            state,
            note,
            check_at,
        },
        BoardOp::Release { .. } => BoardOp::Release { item: id },
        BoardOp::Drop { .. } => BoardOp::Drop { item: id },
    }
}

/// Whether a peer's op has a shape this runtime keeps (limits only; no
/// clock bounds, since it was posted elsewhere and earlier).
pub fn well_formed(op: &BoardOp) -> bool {
    match op {
        BoardOp::Add { title, .. } => {
            !title.trim().is_empty() && title.chars().count() <= MAX_TITLE_CHARS
        }
        BoardOp::Update { note, .. } => note
            .as_deref()
            .is_none_or(|n| n.chars().count() <= MAX_NOTE_CHARS),
        _ => true,
    }
}

/// The readable line a board post carries (ADR-101 D2). `who` is the
/// author's display name; `title` the item's, when the op names one.
pub fn describe(op: &BoardOp, who: &str, title: Option<&str>) -> String {
    let title = title.unwrap_or("an item");
    let when = |at: &Option<String>| {
        parse_time(at.as_deref())
            .map(|t| format!(" (check-in {})", t.format("%Y-%m-%d %H:%M UTC")))
            .unwrap_or_default()
    };
    match op {
        BoardOp::Add { title, for_member } => match for_member {
            Some(member) => format!("board: {who} added “{title}” for @{member}"),
            None => format!("board: {who} added “{title}”"),
        },
        BoardOp::Claim { check_at, .. } => format!("board: {who} took “{title}”{}", when(check_at)),
        BoardOp::Update {
            state,
            note,
            check_at,
            ..
        } => {
            let note = note
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default();
            format!(
                "board: {who}: “{title}” → {}{note}{}",
                state.as_str(),
                when(check_at)
            )
        }
        BoardOp::Release { .. } => format!("board: {who} let go of “{title}”"),
        BoardOp::Drop { .. } => format!("board: {who} removed “{title}”"),
    }
}

// ---------------------------------------------------------------------------
// Fired check-ins
// ---------------------------------------------------------------------------

/// Per-principal check-ins that already brought an item back, as
/// [`BoardItem::check_in_key`]s. Pruned by the writer to items still
/// due, so the file stays small.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FiredCheckIns(pub HashMap<String, Vec<String>>);

impl FiredCheckIns {
    const FILE: &'static str = "board_check_ins.json";

    pub fn path_in(chan_dir: &Path) -> PathBuf {
        chan_dir.join(Self::FILE)
    }

    pub async fn load(chan_dir: &Path) -> Result<Self> {
        match fs::read(Self::path_in(chan_dir)).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ChannelError::Adapter(format!("{}: {e}", Self::FILE))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ChannelError::Adapter(format!("{}: {e}", Self::FILE))),
        }
    }

    pub async fn save(&self, chan_dir: &Path) -> Result<()> {
        let path = Self::path_in(chan_dir);
        let tmp = chan_dir.join(format!("{}.tmp.{}", Self::FILE, std::process::id()));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| ChannelError::Adapter(format!("{}: {e}", Self::FILE)))?;
        let io = |e: std::io::Error| ChannelError::Adapter(format!("{}: {e}", Self::FILE));
        let mut f = fs::File::create(&tmp).await.map_err(io)?;
        f.write_all(&bytes).await.map_err(io)?;
        f.sync_all().await.map_err(io)?;
        drop(f);
        fs::rename(&tmp, &path).await.map_err(io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_subject::CanonicalKeys;

    fn posted(author: &str, at: &str, id: &str, op: BoardOp) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: peko_protocol::channel::ChannelId("chan_t".into()),
            author: author.into(),
            parent: None,
            text: String::new(),
            at: at.into(),
            via: None,
            msg_id: Some(id.into()),
            reply_to: None,
            attachments: Vec::new(),
            options: Vec::new(),
            model: None,
            board: Some(op),
        }
    }

    const KIT: &str = "principal:local:kit";
    const REX: &str = "principal:local:rex";
    const MAYA: &str = "user:local:maya";

    fn add(title: &str) -> BoardOp {
        BoardOp::Add {
            title: title.into(),
            for_member: None,
        }
    }
    fn claim(item: &str) -> BoardOp {
        BoardOp::Claim {
            item: item.into(),
            check_at: None,
        }
    }
    fn update(item: &str, state: BoardState) -> BoardOp {
        BoardOp::Update {
            item: item.into(),
            state,
            note: None,
            check_at: None,
        }
    }

    #[test]
    fn crossed_claims_fold_the_same_whatever_the_order() {
        let auth = Authority::new(&CanonicalKeys, Some(MAYA), &[]);
        let a = posted(
            MAYA,
            "2026-11-09T10:00:00Z",
            "msg_item",
            add("run the tests"),
        );
        let kit = posted(KIT, "2026-11-09T10:01:00Z", "msg_k", claim("msg_item"));
        let rex = posted(REX, "2026-11-09T10:01:05Z", "msg_r", claim("msg_item"));
        let rex_update = posted(
            REX,
            "2026-11-09T10:02:00Z",
            "msg_ru",
            update("msg_item", BoardState::Blocked),
        );
        let one = Board::fold([&a, &kit, &rex, &rex_update], &auth);
        let two = Board::fold([&a, &rex, &rex_update, &kit], &auth);
        assert_eq!(one.items[0].claimant.as_deref(), Some(KIT));
        assert_eq!(two.items[0].claimant.as_deref(), Some(KIT));
        assert_eq!(one.items[0].state, BoardState::Doing);
        assert_eq!(two.items[0].state, BoardState::Doing);
    }

    #[test]
    fn the_fold_ignores_ops_their_authors_may_not_make() {
        let auth = Authority::new(&CanonicalKeys, Some(MAYA), &[]);
        let a = posted(KIT, "2026-11-09T10:00:00Z", "msg_item", add("fix median"));
        let c = posted(KIT, "2026-11-09T10:01:00Z", "msg_c", claim("msg_item"));
        let steal = posted(
            REX,
            "2026-11-09T10:02:00Z",
            "msg_s",
            update("msg_item", BoardState::Done),
        );
        let release = posted(
            REX,
            "2026-11-09T10:03:00Z",
            "msg_x",
            BoardOp::Release {
                item: "msg_item".into(),
            },
        );
        let drop = posted(
            REX,
            "2026-11-09T10:04:00Z",
            "msg_d",
            BoardOp::Drop {
                item: "msg_item".into(),
            },
        );
        let board = Board::fold([&a, &c, &steal, &release, &drop], &auth);
        assert_eq!(board.items.len(), 1);
        assert_eq!(board.items[0].claimant.as_deref(), Some(KIT));
        assert_eq!(board.items[0].state, BoardState::Doing);
        // The group's owner may close or remove anything.
        let owner_done = posted(
            MAYA,
            "2026-11-09T10:05:00Z",
            "msg_o",
            update("msg_item", BoardState::Done),
        );
        let board = Board::fold([&a, &c, &owner_done], &auth);
        assert_eq!(board.items[0].state, BoardState::Done);
    }

    #[test]
    fn posting_refuses_a_held_item_and_names_the_holder() {
        let auth = Authority::new(&CanonicalKeys, None, &[]);
        let a = posted(MAYA, "2026-11-09T10:00:00Z", "msg_b3f9a1ff", add("push"));
        let c = posted(KIT, "2026-11-09T10:01:00Z", "msg_c", claim("msg_b3f9a1ff"));
        let board = Board::fold([&a, &c], &auth);
        let err = board.check(REX, &claim("b3f9a1"), &auth).unwrap_err();
        assert!(
            err.contains("already claimed by principal:local:kit"),
            "{err}"
        );
        assert!(board.check(KIT, &claim("#b3f9"), &auth).is_ok());
        assert!(board
            .check(KIT, &update("b3f9a1", BoardState::Open), &auth)
            .is_err());
    }

    #[test]
    fn a_check_in_is_due_once_for_its_claimant() {
        let auth = Authority::new(&CanonicalKeys, None, &[]);
        let a = posted(MAYA, "2026-11-09T10:00:00Z", "msg_i", add("review"));
        let c = posted(KIT, "2026-11-09T10:01:00Z", "msg_c", claim("msg_i"));
        let blocked = posted(
            KIT,
            "2026-11-09T10:02:00Z",
            "msg_b",
            BoardOp::Update {
                item: "msg_i".into(),
                state: BoardState::Blocked,
                note: Some("waiting on noah".into()),
                check_at: Some("2026-11-09T12:00:00Z".into()),
            },
        );
        let board = Board::fold([&a, &c, &blocked], &auth);
        let before: DateTime<Utc> = "2026-11-09T11:59:00Z".parse().unwrap();
        let after: DateTime<Utc> = "2026-11-09T12:00:00Z".parse().unwrap();
        assert!(board.due_check_ins(&[KIT], &auth, before, &[]).is_empty());
        let due = board.due_check_ins(&[KIT], &auth, after, &[]);
        assert_eq!(due.len(), 1);
        assert!(board.due_check_ins(&[REX], &auth, after, &[]).is_empty());
        let fired = vec![due[0].check_in_key().unwrap()];
        assert!(board.due_check_ins(&[KIT], &auth, after, &fired).is_empty());
    }

    #[test]
    fn a_blocked_update_gets_a_default_check_in_and_done_clears_it() {
        let now: DateTime<Utc> = "2026-11-09T10:00:00Z".parse().unwrap();
        let BoardOp::Update { check_at, .. } =
            validate_op(&update("i", BoardState::Blocked), now).unwrap()
        else {
            panic!()
        };
        assert_eq!(check_at.as_deref(), Some("2026-11-10T10:00:00+00:00"));
        let soon = BoardOp::Claim {
            item: "i".into(),
            check_at: Some("2026-11-09T10:05:00Z".into()),
        };
        assert!(validate_op(&soon, now).is_err());
        assert!(validate_op(&add(""), now).is_err());
        assert!(validate_op(&add(&"x".repeat(121)), now).is_err());
    }

    #[test]
    fn an_op_rides_a_post_and_old_lines_parse_unchanged() {
        let ev = posted(KIT, "2026-11-09T10:00:00Z", "msg_i", add("run the tests"));
        let line = serde_json::to_string(&ev).unwrap();
        assert!(
            line.contains(r#""board":{"op":"add","title":"run the tests"}"#),
            "{line}"
        );
        let back: ChannelEvent = serde_json::from_str(&line).unwrap();
        assert!(matches!(back, ChannelEvent::Posted { board: Some(_), .. }));
        let old =
            r#"{"kind":"posted","channel":"c","author":"a","parent":null,"text":"hi","at":"t"}"#;
        let old: ChannelEvent = serde_json::from_str(old).unwrap();
        assert!(matches!(old, ChannelEvent::Posted { board: None, .. }));
    }
}
