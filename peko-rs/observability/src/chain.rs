//! Hash-chained, append-only JSONL files (ADR-079 D5).
//!
//! One file per UTC day, `<prefix>-YYYY-MM-DD.jsonl`, mode 0600, one
//! fsync'd line per record. Every line carries a monotonic `seq`, the
//! previous line's hash as `prev`, and its own `hash`, so an edited,
//! inserted, or deleted line breaks the chain. The chain runs across
//! days and across reopenings: [`ChainedJsonl::open`] resumes from the
//! last line on disk.
//!
//! The hash covers the line's exact bytes, not a re-serialization: the
//! writer serializes the record (with `seq` and `prev`), hashes that
//! text, and appends `,"hash":"sha256:<hex>"}` in place of its closing
//! brace. A verifier strips that fixed-length suffix to recover the
//! hashed bytes, so key order and number formatting cannot drift
//! between writer and verifier.
//!
//! This makes tampering *detectable*, not impossible: whoever can write
//! the files can rewrite the whole chain. Anchoring the head elsewhere
//! (checkpoints) is the caller's job.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// The `prev` of a chain's first line.
pub const GENESIS_PREV: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// `,"hash":"` + `sha256:` + 64 hex digits + `"}`.
const HASH_SUFFIX_LEN: usize = 9 + 7 + 64 + 2;

/// Where a record landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    pub seq: u64,
    pub hash: String,
}

/// An append-only, hash-chained JSONL log rooted at one directory.
pub struct ChainedJsonl {
    dir: PathBuf,
    prefix: String,
    state: Mutex<State>,
}

struct State {
    date: NaiveDate,
    file: File,
    next_seq: u64,
    head: String,
}

impl std::fmt::Debug for ChainedJsonl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainedJsonl")
            .field("dir", &self.dir)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl ChainedJsonl {
    /// Open (or create) the chain in `dir`, resuming after its last
    /// intact line. A torn final line (a crash mid-write) is closed
    /// with a newline and skipped; the chain continues from the line
    /// before it, and [`verify`] reports the torn line.
    pub fn open(dir: impl Into<PathBuf>, prefix: impl Into<String>) -> Result<Self> {
        let dir = dir.into();
        let prefix = prefix.into();
        create_private_dir(&dir)?;
        let (next_seq, head) = match last_intact(&dir, &prefix)? {
            Some((seq, hash)) => (seq + 1, hash),
            None => (0, GENESIS_PREV.to_string()),
        };
        let date = chrono::Utc::now().date_naive();
        let file = open_append(&path_for(&dir, &prefix, date))?;
        Ok(Self {
            dir,
            prefix,
            state: Mutex::new(State {
                date,
                file,
                next_seq,
                head,
            }),
        })
    }

    /// The directory holding the chain's files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `(next seq, hash of the last line)`. An empty chain reports
    /// `(0, GENESIS_PREV)`.
    #[must_use]
    pub fn head(&self) -> (u64, String) {
        let state = self.state.lock().expect("chain lock poisoned");
        (state.next_seq, state.head.clone())
    }

    /// Append `record` (a JSON object; `seq`, `prev`, and `hash` are
    /// reserved and overwritten), fsync it, and return where it landed.
    /// Blocking I/O: call from a blocking context.
    pub fn append(&self, mut record: Map<String, Value>) -> Result<Appended> {
        let mut state = self.state.lock().expect("chain lock poisoned");
        let today = chrono::Utc::now().date_naive();
        if today != state.date {
            state.file = open_append(&path_for(&self.dir, &self.prefix, today))?;
            state.date = today;
        }
        record.remove("hash");
        record.insert("seq".into(), Value::from(state.next_seq));
        record.insert("prev".into(), Value::String(state.head.clone()));
        let body = serde_json::to_string(&Value::Object(record)).context("serialize record")?;
        let hash = hash_body(&body);
        let mut line = String::with_capacity(body.len() + HASH_SUFFIX_LEN + 1);
        line.push_str(&body[..body.len() - 1]);
        line.push_str(",\"hash\":\"");
        line.push_str(&hash);
        line.push_str("\"}\n");
        state
            .file
            .write_all(line.as_bytes())
            .context("append chained line")?;
        state.file.sync_data().context("fsync chained line")?;
        let appended = Appended {
            seq: state.next_seq,
            hash: hash.clone(),
        };
        state.next_seq += 1;
        state.head = hash;
        Ok(appended)
    }
}

/// `sha256:<hex>` of a record's serialized body.
#[must_use]
pub fn hash_body(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Split a stored line into `(hashed body, stored hash)`, or `None` if
/// it does not end in a well-formed hash suffix.
#[must_use]
pub fn split_line(line: &str) -> Option<(String, &str)> {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.len() <= HASH_SUFFIX_LEN || !line.is_char_boundary(line.len() - HASH_SUFFIX_LEN) {
        return None;
    }
    let (head, suffix) = line.split_at(line.len() - HASH_SUFFIX_LEN);
    let hash = suffix.strip_prefix(",\"hash\":\"")?.strip_suffix("\"}")?;
    let hex = hash.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((format!("{head}}}"), hash))
}

/// The chain's files, oldest first.
pub fn files(dir: &Path, prefix: &str) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(date) = name
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|rest| rest.strip_suffix(".jsonl"))
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
        else {
            continue;
        };
        out.push((date, path));
    }
    out.sort();
    Ok(out)
}

/// One stored line, parsed.
#[derive(Debug, Clone)]
pub struct StoredLine {
    pub file: PathBuf,
    /// 1-based line number within `file`.
    pub line: usize,
    /// The record, or `None` for a line that is not an intact chained
    /// record (torn, hand-edited into invalid JSON, ...).
    pub record: Option<Value>,
    /// The raw text (without the newline), kept for verification.
    pub raw: String,
}

/// Every stored line of the chain, oldest first. `filter` sees each
/// file's date and may skip whole files.
pub fn read_all(
    dir: &Path,
    prefix: &str,
    mut keep_file: impl FnMut(NaiveDate) -> bool,
) -> Result<Vec<StoredLine>> {
    let mut out = Vec::new();
    for (date, path) in files(dir, prefix)? {
        if !keep_file(date) {
            continue;
        }
        let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
        for (index, raw) in BufReader::new(file).lines().enumerate() {
            let raw = raw.with_context(|| format!("read {}", path.display()))?;
            if raw.is_empty() {
                continue;
            }
            let record = split_line(&raw)
                .and_then(|_| serde_json::from_str::<Value>(&raw).ok())
                .filter(Value::is_object);
            out.push(StoredLine {
                file: path.clone(),
                line: index + 1,
                record,
                raw,
            });
        }
    }
    Ok(out)
}

/// A problem [`verify`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A line that is not an intact chained record.
    Unreadable { file: PathBuf, line: usize },
    /// A line whose bytes do not match its stored hash (edited).
    BadHash { seq: u64 },
    /// A line whose `prev` is not the hash of the line before it
    /// (inserted, deleted, or reordered lines).
    BrokenLink { seq: u64 },
    /// `seq` jumped (lines deleted).
    SeqGap { expected: u64, found: u64 },
    /// A checkpoint names a line whose hash differs.
    CheckpointMismatch { seq: u64 },
    /// A checkpoint names a line past the chain's end (tail truncated).
    Truncated {
        checkpoint_seq: u64,
        last_seq: Option<u64>,
    },
}

/// What [`verify`] saw.
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub records: u64,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub head: Option<String>,
    pub problems: Vec<Problem>,
}

impl VerifyReport {
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Walk the chain and check every link.
///
/// `starts` are the `prev` values the first remaining line may carry:
/// [`GENESIS_PREV`] plus the anchors of any pruned prefix.
/// `checkpoints` maps `seq → hash` from an independent record (the
/// global audit log); each must match, and one past the end means the
/// tail was cut.
pub fn verify(
    dir: &Path,
    prefix: &str,
    starts: &[String],
    checkpoints: &BTreeMap<u64, String>,
) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();
    let mut previous: Option<(u64, String)> = None;
    for stored in read_all(dir, prefix, |_| true)? {
        let (Some(record), Some((body, stored_hash))) = (&stored.record, split_line(&stored.raw))
        else {
            report.problems.push(Problem::Unreadable {
                file: stored.file.clone(),
                line: stored.line,
            });
            continue;
        };
        let Some(seq) = record["seq"].as_u64() else {
            report.problems.push(Problem::Unreadable {
                file: stored.file.clone(),
                line: stored.line,
            });
            continue;
        };
        let prev = record["prev"].as_str().unwrap_or_default();
        if hash_body(&body) != stored_hash {
            report.problems.push(Problem::BadHash { seq });
        }
        match &previous {
            None => {
                if !starts.iter().any(|start| start == prev) {
                    report.problems.push(Problem::BrokenLink { seq });
                }
            }
            Some((last_seq, last_hash)) => {
                if seq != last_seq + 1 {
                    report.problems.push(Problem::SeqGap {
                        expected: last_seq + 1,
                        found: seq,
                    });
                }
                if prev != last_hash {
                    report.problems.push(Problem::BrokenLink { seq });
                }
            }
        }
        if let Some(expected) = checkpoints.get(&seq) {
            if expected != stored_hash {
                report.problems.push(Problem::CheckpointMismatch { seq });
            }
        }
        report.records += 1;
        report.first_seq.get_or_insert(seq);
        report.last_seq = Some(seq);
        report.head = Some(stored_hash.to_string());
        previous = Some((seq, stored_hash.to_string()));
    }
    if let Some((&checkpoint_seq, _)) = checkpoints.last_key_value() {
        if report.last_seq.is_none_or(|last| last < checkpoint_seq) {
            report.problems.push(Problem::Truncated {
                checkpoint_seq,
                last_seq: report.last_seq,
            });
        }
    }
    Ok(report)
}

/// Delete the chain's files dated before `cutoff`, never today's.
/// Returns the `(seq, hash)` of the last deleted line — the anchor the
/// first remaining line links to — or `None` if nothing was deleted.
pub fn prune_before(dir: &Path, prefix: &str, cutoff: NaiveDate) -> Result<Option<(u64, String)>> {
    let today = chrono::Utc::now().date_naive();
    let mut anchor = None;
    for (date, path) in files(dir, prefix)? {
        if date >= cutoff || date >= today {
            break;
        }
        if let Some(last) = last_intact_in(&path)? {
            anchor = Some(last);
        }
        std::fs::remove_file(&path).with_context(|| format!("prune {}", path.display()))?;
    }
    Ok(anchor)
}

fn path_for(dir: &Path, prefix: &str, date: NaiveDate) -> PathBuf {
    dir.join(format!("{prefix}-{date}.jsonl"))
}

/// The last intact line across the chain's files, newest file first.
fn last_intact(dir: &Path, prefix: &str) -> Result<Option<(u64, String)>> {
    for (_, path) in files(dir, prefix)?.into_iter().rev() {
        close_torn_tail(&path)?;
        if let Some(last) = last_intact_in(&path)? {
            return Ok(Some(last));
        }
    }
    Ok(None)
}

fn last_intact_in(path: &Path) -> Result<Option<(u64, String)>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut last = None;
    for raw in BufReader::new(file).lines() {
        let raw = raw.with_context(|| format!("read {}", path.display()))?;
        let Some((_, hash)) = split_line(&raw) else {
            continue;
        };
        let Some(seq) = serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|record| record["seq"].as_u64())
        else {
            continue;
        };
        last = Some((seq, hash.to_string()));
    }
    Ok(last)
}

/// End a file whose last line was torn by a crash with a newline, so
/// the next append starts a fresh line.
fn close_torn_tail(path: &Path) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] != b'\n' {
        file.write_all(b"\n")?;
        file.sync_data()?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))
}

#[cfg(unix)]
fn open_append(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(not(unix))]
fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

/// Guard against a record that is not a JSON object.
pub fn as_record(value: Value) -> Result<Map<String, Value>> {
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!("a chained record must be a JSON object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(n: u64) -> Map<String, Value> {
        as_record(json!({ "kind": "test", "n": n, "text": "héllo, \"world\"" })).unwrap()
    }

    fn verify_clean(dir: &Path) -> VerifyReport {
        verify(dir, "ledger", &[GENESIS_PREV.into()], &BTreeMap::new()).unwrap()
    }

    #[test]
    fn appends_link_into_a_verifiable_chain() {
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        let first = chain.append(record(0)).unwrap();
        let second = chain.append(record(1)).unwrap();
        assert_eq!((first.seq, second.seq), (0, 1));
        assert_eq!(chain.head(), (2, second.hash.clone()));

        let lines = read_all(dir.path(), "ledger", |_| true).unwrap();
        assert_eq!(lines[0].record.as_ref().unwrap()["prev"], GENESIS_PREV);
        assert_eq!(lines[1].record.as_ref().unwrap()["prev"], first.hash);
        let report = verify_clean(dir.path());
        assert!(report.is_intact(), "{:?}", report.problems);
        assert_eq!(report.records, 2);
        assert_eq!(report.head.as_deref(), Some(second.hash.as_str()));
    }

    #[test]
    fn reopening_resumes_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let last = {
            let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
            chain.append(record(0)).unwrap();
            chain.append(record(1)).unwrap()
        };
        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        assert_eq!(chain.head(), (2, last.hash));
        chain.append(record(2)).unwrap();
        assert!(verify_clean(dir.path()).is_intact());
    }

    #[test]
    fn a_torn_tail_is_skipped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        let kept = chain.append(record(0)).unwrap();
        let (_, path) = files(dir.path(), "ledger").unwrap().pop().unwrap();
        drop(chain);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"kind\":\"torn").unwrap();
        drop(file);

        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        assert_eq!(chain.head(), (1, kept.hash));
        chain.append(record(1)).unwrap();
        let report = verify_clean(dir.path());
        assert_eq!(report.records, 2);
        assert!(matches!(
            report.problems.as_slice(),
            [Problem::Unreadable { line: 2, .. }]
        ));
    }

    fn rewrite(path: &Path, edit: impl FnOnce(&mut Vec<String>)) {
        let mut lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        edit(&mut lines);
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    fn three_line_chain() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        for n in 0..3 {
            chain.append(record(n)).unwrap();
        }
        let (_, path) = files(dir.path(), "ledger").unwrap().pop().unwrap();
        (dir, path)
    }

    #[test]
    fn an_edited_line_fails_its_hash() {
        let (dir, path) = three_line_chain();
        rewrite(&path, |lines| {
            lines[1] = lines[1].replace("\"n\":1", "\"n\":7")
        });
        let report = verify_clean(dir.path());
        assert_eq!(report.problems, vec![Problem::BadHash { seq: 1 }]);
    }

    #[test]
    fn a_deleted_line_breaks_the_link_and_the_sequence() {
        let (dir, path) = three_line_chain();
        rewrite(&path, |lines| {
            lines.remove(1);
        });
        let report = verify_clean(dir.path());
        assert!(report.problems.contains(&Problem::BrokenLink { seq: 2 }));
        assert!(report.problems.contains(&Problem::SeqGap {
            expected: 1,
            found: 2
        }));
    }

    #[test]
    fn checkpoints_catch_a_cut_tail_and_a_rewritten_history() {
        let (dir, path) = three_line_chain();
        let lines = read_all(dir.path(), "ledger", |_| true).unwrap();
        let hash_of = |i: usize| split_line(&lines[i].raw).unwrap().1.to_string();
        let checkpoints = BTreeMap::from([(2, hash_of(2))]);
        let starts = [GENESIS_PREV.to_string()];
        assert!(verify(dir.path(), "ledger", &starts, &checkpoints)
            .unwrap()
            .is_intact());

        rewrite(&path, |lines| {
            lines.pop();
        });
        let report = verify(dir.path(), "ledger", &starts, &checkpoints).unwrap();
        assert_eq!(
            report.problems,
            vec![Problem::Truncated {
                checkpoint_seq: 2,
                last_seq: Some(1)
            }]
        );

        let wrong = BTreeMap::from([(1, hash_of(0))]);
        let report = verify(dir.path(), "ledger", &starts, &wrong).unwrap();
        assert_eq!(
            report.problems,
            vec![Problem::CheckpointMismatch { seq: 1 }]
        );
    }

    #[test]
    fn pruning_returns_the_anchor_the_rest_links_to() {
        let dir = tempfile::tempdir().unwrap();
        let old = path_for(
            dir.path(),
            "ledger",
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // Build an old file by writing a chain, then renaming it back in time.
        let anchor = {
            let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
            chain.append(record(0)).unwrap()
        };
        let (_, today) = files(dir.path(), "ledger").unwrap().pop().unwrap();
        std::fs::rename(&today, &old).unwrap();
        let chain = ChainedJsonl::open(dir.path(), "ledger").unwrap();
        chain.append(record(1)).unwrap();

        let pruned = prune_before(dir.path(), "ledger", chrono::Utc::now().date_naive()).unwrap();
        assert_eq!(pruned, Some((0, anchor.hash.clone())));
        assert!(!old.exists());
        assert!(!verify_clean(dir.path()).is_intact());
        let report = verify(
            dir.path(),
            "ledger",
            &[GENESIS_PREV.into(), anchor.hash],
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(report.is_intact(), "{:?}", report.problems);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger");
        let chain = ChainedJsonl::open(&ledger, "ledger").unwrap();
        chain.append(record(0)).unwrap();
        let (_, path) = files(&ledger, "ledger").unwrap().pop().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
