//! Reading the tool ledger (ADR-079 D5, D6): records grouped into calls,
//! compartment scoping, filters, and verification against the global
//! audit log.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use peko_observability::chain::{self, VerifyReport, GENESIS_PREV};
use peko_tools_core::ActingFor;
use serde_json::{json, Value};

use super::{EVENT_CHECKPOINT, EVENT_PRUNED, PREFIX};

/// One call: its `start` record and every `end` that followed (a
/// detached call has two).
#[derive(Debug, Clone)]
pub struct Call {
    pub call_id: String,
    /// `seq` of the call's first record.
    pub seq: u64,
    pub start: Option<Value>,
    pub ends: Vec<Value>,
}

impl Call {
    /// The final outcome; `None` while the call is in flight.
    #[must_use]
    pub fn outcome(&self) -> Option<&str> {
        self.ends.last().and_then(|end| end["outcome"].as_str())
    }

    #[must_use]
    pub fn stamp(&self) -> Option<ActingFor> {
        self.start
            .as_ref()
            .and_then(|start| serde_json::from_value(start["acting_for"].clone()).ok())
    }

    #[must_use]
    pub fn started_at(&self) -> Option<DateTime<Utc>> {
        self.start
            .as_ref()
            .and_then(|start| start["ts"].as_str())
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|ts| ts.with_timezone(&Utc))
    }

    fn start_str(&self, key: &str) -> Option<&str> {
        self.start.as_ref().and_then(|start| start[key].as_str())
    }

    #[must_use]
    pub fn tool(&self) -> Option<&str> {
        self.start_str("tool")
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.start_str("session_id")
    }

    #[must_use]
    pub fn parent_call_id(&self) -> Option<&str> {
        self.start
            .as_ref()
            .and_then(|start| start["parent"]["call_id"].as_str())
    }

    /// Total duration across the call's ends (a detached call's late end
    /// measures the background body).
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        let total: u64 = self
            .ends
            .iter()
            .filter_map(|end| end["duration_ms"].as_u64())
            .sum();
        (!self.ends.is_empty()).then_some(total)
    }
}

/// Every call recorded in `dir`, oldest first. `since` skips whole days
/// before it.
pub fn load_calls(dir: &Path, since: Option<NaiveDate>) -> Result<Vec<Call>> {
    let lines = chain::read_all(dir, PREFIX, |date| since.is_none_or(|since| date >= since))?;
    let mut calls: Vec<Call> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for line in lines {
        let Some(record) = line.record else { continue };
        let Some(call_id) = record["call_id"].as_str().map(str::to_string) else {
            continue;
        };
        let seq = record["seq"].as_u64().unwrap_or_default();
        let slot = *index.entry(call_id.clone()).or_insert_with(|| {
            calls.push(Call {
                call_id,
                seq,
                start: None,
                ends: Vec::new(),
            });
            calls.len() - 1
        });
        match record["kind"].as_str() {
            Some("start") => calls[slot].start = Some(record),
            Some("end") => calls[slot].ends.push(record),
            _ => {}
        }
    }
    Ok(calls)
}

/// Whether a run acting for `reader` may see `call` (ADR-078 D4): owner
/// runs see everything; any other run sees only calls made in its own
/// compartment.
#[must_use]
pub fn reachable(call: &Call, reader: &ActingFor) -> bool {
    if reader.is_owner() {
        return true;
    }
    call.stamp()
        .is_some_and(|stamp| reader.may_reach(&stamp.compartment))
}

/// Search filters; `None` matches everything.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub tool: Option<String>,
    pub action: Option<String>,
    pub sessions: Option<HashSet<String>>,
    pub actor: Option<String>,
    pub outcome: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Only calls whose first record precedes this `seq` (paging).
    pub before_seq: Option<u64>,
    pub call_id: Option<String>,
}

impl Filter {
    #[must_use]
    pub fn matches(&self, call: &Call) -> bool {
        let start = call.start.as_ref();
        let field = |key: &str| start.and_then(|s| s[key].as_str());
        if self
            .tool
            .as_deref()
            .is_some_and(|tool| field("tool") != Some(tool))
        {
            return false;
        }
        if self
            .action
            .as_deref()
            .is_some_and(|action| field("action") != Some(action))
        {
            return false;
        }
        if self
            .sessions
            .as_ref()
            .is_some_and(|ids| !call.session_id().is_some_and(|id| ids.contains(id)))
        {
            return false;
        }
        if self.actor.as_deref().is_some_and(|actor| {
            start.and_then(|s| s["acting_for"]["actor"].as_str()) != Some(actor)
        }) {
            return false;
        }
        if self.outcome.as_deref().is_some_and(|outcome| {
            // "in_flight" selects calls without an end.
            match call.outcome() {
                Some(actual) => actual != outcome,
                None => outcome != "in_flight",
            }
        }) {
            return false;
        }
        let at = call.started_at();
        if self
            .since
            .is_some_and(|since| at.is_none_or(|at| at < since))
        {
            return false;
        }
        if self
            .until
            .is_some_and(|until| at.is_none_or(|at| at > until))
        {
            return false;
        }
        if self.before_seq.is_some_and(|before| call.seq >= before) {
            return false;
        }
        if self.call_id.as_deref().is_some_and(|id| call.call_id != id) {
            return false;
        }
        true
    }
}

/// The compact search row for a call. `path_of` maps a session id to
/// its slug path, resolved at read time so moves stay accurate.
#[must_use]
pub fn row(call: &Call, path_of: &dyn Fn(&str) -> Option<String>) -> Value {
    let start = call.start.as_ref();
    let field = |key: &str| start.map_or(Value::Null, |s| s[key].clone());
    let mut row = json!({
        "call_id": call.call_id,
        "seq": call.seq,
        "ts": field("ts"),
        "tool": field("tool"),
        "outcome": call.outcome().unwrap_or("in_flight"),
    });
    let object = row.as_object_mut().expect("row is an object");
    for key in ["action", "tool_call_id"] {
        if !field(key).is_null() {
            object.insert(key.into(), field(key));
        }
    }
    if let Some(session) = call.session_id() {
        object.insert(
            "session".into(),
            json!(path_of(session).unwrap_or_else(|| session.to_string())),
        );
    }
    if let Some(stamp) = start.map(|s| &s["acting_for"]) {
        object.insert("actor".into(), stamp["actor"].clone());
        object.insert("trust".into(), stamp["trust"].clone());
    }
    if let Some(parent) = start.map(|s| &s["parent"]).filter(|p| !p.is_null()) {
        object.insert("parent".into(), parent.clone());
    }
    if let Some(ms) = call.duration_ms() {
        object.insert("duration_ms".into(), json!(ms));
    }
    if let Some(code) = call
        .ends
        .iter()
        .rev()
        .find_map(|end| end["exit_code"].as_i64())
    {
        object.insert("exit_code".into(), json!(code));
    }
    row
}

/// A principal's session tree, for showing slug paths and filtering by
/// session. Paths resolve at read time, so a moved session shows its
/// current path.
pub struct Sessions {
    metas: Vec<peko_session::SessionMetadata>,
}

impl Sessions {
    /// Load the session index in `sessions_dir` (empty if unreadable).
    pub async fn load(sessions_dir: &Path) -> Self {
        let metas = peko_session::MetadataController::new(sessions_dir)
            .list_metadata(false)
            .await
            .unwrap_or_default();
        Self { metas }
    }

    /// The slug path of session `id`, if it is in the index.
    #[must_use]
    pub fn path_of(&self, id: &str) -> Option<String> {
        let id = peko_session::SessionId(uuid::Uuid::parse_str(id).ok()?);
        self.metas
            .iter()
            .any(|meta| meta.session_id == id)
            .then(|| peko_session::path::compute_path(&self.metas, id))
    }

    /// The session ids `reference` names — a slug path or a session id —
    /// with descendants when `subtree`. Paths resolve from `anchor`'s
    /// tree, or from the trunk when there is no anchor (the owner).
    pub fn resolve(
        &self,
        anchor: Option<&str>,
        reference: &str,
        subtree: bool,
    ) -> Result<HashSet<String>> {
        use peko_session::SessionId;
        let anchor = match anchor {
            Some(id) => SessionId(
                uuid::Uuid::parse_str(id).context("the calling session id is not a session")?,
            ),
            None => self
                .metas
                .iter()
                .find(|meta| meta.parent_session_id.is_none())
                .map(|meta| meta.session_id)
                .context("this principal has no trunk session to resolve paths from")?,
        };
        let root = peko_session::path::resolve_reference(&self.metas, anchor, reference)?;
        let mut ids: HashSet<SessionId> = HashSet::from([root]);
        if subtree {
            loop {
                let before = ids.len();
                for meta in &self.metas {
                    if meta.parent_session_id.is_some_and(|p| ids.contains(&p)) {
                        ids.insert(meta.session_id);
                    }
                }
                if ids.len() == before {
                    break;
                }
            }
        }
        Ok(ids.into_iter().map(|id| id.to_string()).collect())
    }
}

/// What the global audit log independently says about one principal's
/// ledger.
#[derive(Debug, Clone, Default)]
pub struct AuditInputs {
    /// `seq → hash` from `ledger.checkpoint` events.
    pub checkpoints: BTreeMap<u64, String>,
    /// Hashes a pruned prefix ended at; the first remaining record may
    /// link to any of them.
    pub anchors: Vec<String>,
    /// `call_id → (params_digest, timestamp)` from `tool.call` events.
    pub calls: HashMap<String, (String, DateTime<Utc>)>,
}

/// Collect [`AuditInputs`] for `principal_id` from the global audit
/// directory (`audit-YYYY-MM-DD.jsonl`).
pub fn audit_inputs(audit_dir: &Path, principal_id: &str) -> Result<AuditInputs> {
    let mut inputs = AuditInputs::default();
    let mut files: Vec<PathBuf> = match std::fs::read_dir(audit_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("audit-") && n.ends_with(".jsonl"))
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(inputs),
        Err(e) => return Err(e).with_context(|| format!("read {}", audit_dir.display())),
    };
    files.sort();
    for path in files {
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for line in text.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let details = &event["details"];
            if details["principal_id"].as_str() != Some(principal_id) {
                continue;
            }
            let hash = details["hash"].as_str().map(str::to_string);
            match event["event_type"].as_str() {
                Some(EVENT_CHECKPOINT) => {
                    if let (Some(seq), Some(hash)) = (details["seq"].as_u64(), hash) {
                        inputs.checkpoints.insert(seq, hash);
                    }
                }
                Some(EVENT_PRUNED) => inputs.anchors.extend(hash),
                Some("tool.call") => {
                    let (Some(call_id), Some(digest)) = (
                        details["call_id"].as_str(),
                        details["params_digest"].as_str(),
                    ) else {
                        continue;
                    };
                    let at = event["timestamp"]
                        .as_str()
                        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                        .map_or_else(Utc::now, |ts| ts.with_timezone(&Utc));
                    inputs
                        .calls
                        .insert(call_id.to_string(), (digest.to_string(), at));
                }
                _ => {}
            }
        }
    }
    Ok(inputs)
}

/// The result of checking a ledger against itself and the global log.
#[derive(Debug, Clone, Default)]
pub struct Verification {
    pub chain: VerifyReport,
    /// Finished calls the global log never saw (`tool.call` missing).
    pub missing_from_global: Vec<String>,
    /// Calls in the global log (within the ledger's span) the ledger
    /// lacks — deleted records.
    pub missing_from_ledger: Vec<String>,
    /// Calls whose params digest differs between the two logs.
    pub digest_mismatch: Vec<String>,
}

impl Verification {
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.chain.is_intact()
            && self.missing_from_global.is_empty()
            && self.missing_from_ledger.is_empty()
            && self.digest_mismatch.is_empty()
    }
}

/// Verify `principal_id`'s ledger in `ledger_dir` against the global
/// audit log in `audit_dir`.
pub fn verify(ledger_dir: &Path, audit_dir: &Path, principal_id: &str) -> Result<Verification> {
    let inputs = audit_inputs(audit_dir, principal_id)?;
    let mut starts = vec![GENESIS_PREV.to_string()];
    starts.extend(inputs.anchors.iter().cloned());
    let chain = chain::verify(ledger_dir, PREFIX, &starts, &inputs.checkpoints)?;

    let calls = load_calls(ledger_dir, None)?;
    let span_start = calls.iter().find_map(Call::started_at);
    let mut seen = HashSet::new();
    let mut verification = Verification {
        chain,
        ..Verification::default()
    };
    for call in &calls {
        seen.insert(call.call_id.as_str());
        let Some(start) = &call.start else { continue };
        match inputs.calls.get(&call.call_id) {
            Some((digest, _)) => {
                if start["params_digest"].as_str() != Some(digest.as_str()) {
                    verification.digest_mismatch.push(call.call_id.clone());
                }
            }
            // In-flight calls are audited globally only when they end.
            None if !call.ends.is_empty() => {
                verification.missing_from_global.push(call.call_id.clone());
            }
            None => {}
        }
    }
    let mut missing: Vec<(DateTime<Utc>, String)> = inputs
        .calls
        .iter()
        .filter(|(id, (_, at))| {
            !seen.contains(id.as_str()) && span_start.is_some_and(|start| *at >= start)
        })
        .map(|(id, (_, at))| (*at, id.clone()))
        .collect();
    missing.sort();
    verification.missing_from_ledger = missing.into_iter().map(|(_, id)| id).collect();
    Ok(verification)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ledger::{end_record, End, Ledger, Outcome};
    use peko_observability::chain::as_record;
    use peko_tools_core::{Compartment, Origin, Trust};

    fn stamp(trust: Trust, compartment: Compartment) -> ActingFor {
        ActingFor {
            actor: "user:hub:x".into(),
            name: None,
            trust,
            compartment,
            origin: Origin::Runtime,
        }
    }

    async fn write_call(ledger: &Ledger, id: &str, tool: &str, who: &ActingFor, outcome: Outcome) {
        ledger
            .append(
                as_record(json!({
                    "kind": "start", "ts": crate::security::ledger::now(), "call_id": id,
                    "tool": tool, "session_id": "s1", "acting_for": who, "params_digest": "d",
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        ledger
            .append(end_record(
                id,
                &End {
                    outcome,
                    duration_ms: Some(5),
                    exit_code: None,
                    task_id: None,
                    result: None,
                    late: false,
                },
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn calls_group_scope_and_filter() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path(), "p").unwrap();
        let mine = Compartment::Conversation {
            channel: "chan_a".into(),
        };
        let theirs = Compartment::Conversation {
            channel: "chan_b".into(),
        };
        write_call(
            &ledger,
            "c1",
            "Bash",
            &stamp(Trust::Trusted, mine.clone()),
            Outcome::Ok,
        )
        .await;
        write_call(
            &ledger,
            "c2",
            "Read",
            &stamp(Trust::Untrusted, theirs),
            Outcome::FloorDenied,
        )
        .await;
        write_call(
            &ledger,
            "c3",
            "Read",
            &stamp(Trust::Owner, Compartment::Internal),
            Outcome::Ok,
        )
        .await;

        let calls = load_calls(dir.path(), None).unwrap();
        assert_eq!(calls.len(), 3, "genesis is not a call");
        assert_eq!(calls[1].outcome(), Some("floor_denied"));

        let guest = stamp(Trust::Untrusted, mine);
        let visible: Vec<_> = calls
            .iter()
            .filter(|c| reachable(c, &guest))
            .map(|c| c.call_id.as_str())
            .collect();
        assert_eq!(visible, ["c1"]);
        let unattributed = ActingFor::unattributed();
        assert!(calls.iter().all(|c| !reachable(c, &unattributed)));
        let owner = stamp(Trust::Owner, Compartment::Internal);
        assert!(calls.iter().all(|c| reachable(c, &owner)));

        let reads = Filter {
            tool: Some("Read".into()),
            outcome: Some("ok".into()),
            ..Filter::default()
        };
        let hits: Vec<_> = calls.iter().filter(|c| reads.matches(c)).collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].call_id, "c3");
        let row = row(hits[0], &|id| (id == "s1").then(|| "/work".into()));
        assert_eq!(row["session"], "/work");
        assert_eq!(row["duration_ms"], 5);
    }

    #[tokio::test]
    async fn verify_cross_checks_the_global_log() {
        let root = tempfile::tempdir().unwrap();
        let (ledger_dir, audit_dir) = (root.path().join("ledger"), root.path().join("audit"));
        let ledger = Ledger::open(&ledger_dir, "p").unwrap();
        let owner = stamp(Trust::Owner, Compartment::Internal);
        write_call(&ledger, "c1", "Read", &owner, Outcome::Ok).await;
        write_call(&ledger, "c2", "Read", &owner, Outcome::Ok).await;
        let (seq, hash) = ledger.take_checkpoint(true).unwrap();

        let observability =
            peko_observability::Observability::with_audit_dir("t", audit_dir.clone()).unwrap();
        let tool_call = |id: &str, digest: &str| json!({ "principal_id": "p", "call_id": id, "params_digest": digest });
        observability
            .audit("tool.call", None, tool_call("c1", "d"))
            .await
            .unwrap();
        observability
            .audit("tool.call", None, tool_call("c2", "other"))
            .await
            .unwrap();
        observability
            .audit("tool.call", None, tool_call("c9", "d"))
            .await
            .unwrap();
        observability
            .audit(
                EVENT_CHECKPOINT,
                None,
                json!({ "principal_id": "p", "seq": seq, "hash": hash }),
            )
            .await
            .unwrap();

        let report = verify(&ledger_dir, &audit_dir, "p").unwrap();
        assert!(report.chain.is_intact(), "{:?}", report.chain.problems);
        assert_eq!(report.digest_mismatch, ["c2"]);
        assert_eq!(report.missing_from_ledger, ["c9"]);
        assert!(report.missing_from_global.is_empty());
        assert!(!report.is_intact());
    }
}
