//! `peko audit ledger` / `peko audit verify` — a principal's tool ledger
//! (ADR-079), read straight from its Local tier. The CLI is the owner,
//! so every call is visible and session paths resolve from the trunk.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use peko_core::principal::config::PrincipalConfig;
use peko_core::security::ledger::read::{self, Call, Filter, Sessions};
use serde_json::Value;

use crate::commands::GlobalPaths;

/// `peko audit ledger` options.
pub struct Query {
    pub since: Option<String>,
    pub tool: Option<String>,
    pub session: Option<String>,
    pub actor: Option<String>,
    pub outcome: Option<String>,
    pub call: Option<String>,
    pub limit: usize,
    pub follow: bool,
    pub json: bool,
}

/// Where a peko's ledger and sessions live, and its stable id.
struct Target {
    principal_id: String,
    ledger_dir: PathBuf,
    sessions_dir: PathBuf,
}

fn target(paths: &GlobalPaths, peko: &str) -> Result<Target> {
    let layout = paths.principal_layout(peko);
    let config_file = &layout.shared.config_file;
    let text = std::fs::read_to_string(config_file)
        .with_context(|| format!("no peko named '{peko}' ({})", config_file.display()))?;
    let config: PrincipalConfig =
        toml::from_str(&text).with_context(|| format!("parse {}", config_file.display()))?;
    let principal_id = config
        .id
        .map(|id| id.0)
        .with_context(|| format!("'{peko}' has no principal id yet"))?;
    Ok(Target {
        principal_id,
        ledger_dir: layout.local.ledger_dir,
        sessions_dir: layout.local.sessions_dir,
    })
}

pub async fn show(paths: &GlobalPaths, peko: &str, query: Query) -> Result<()> {
    let target = target(paths, peko)?;
    if !target.ledger_dir.exists() {
        eprintln!(
            "📒 '{peko}' has no tool ledger yet ({}). The daemon writes it on the first tool call.",
            target.ledger_dir.display()
        );
        return Ok(());
    }
    let sessions = Sessions::load(&target.sessions_dir).await;
    let calls = read::load_calls(&target.ledger_dir, None)?;

    if let Some(call_id) = &query.call {
        return print_call(&calls, call_id, &sessions, query.json);
    }

    let filter = Filter {
        tool: query.tool.clone(),
        sessions: query
            .session
            .as_deref()
            .map(|reference| sessions.resolve(None, reference, false))
            .transpose()?,
        actor: query.actor.clone(),
        outcome: query.outcome.clone(),
        since: query.since.as_deref().map(super::parse_since).transpose()?,
        ..Filter::default()
    };
    let mut shown: Vec<&Call> = calls
        .iter()
        .rev()
        .filter(|call| filter.matches(call))
        .take(query.limit)
        .collect();
    shown.reverse();
    for call in &shown {
        print_row(call, &sessions, query.json);
    }
    if shown.is_empty() && !query.follow {
        eprintln!("(no matching calls)");
    }
    if query.follow {
        follow(
            &target,
            &filter,
            &sessions,
            query.json,
            calls.last().map(|c| c.seq),
        )?;
    }
    Ok(())
}

fn print_row(call: &Call, sessions: &Sessions, json: bool) {
    let row = read::row(call, &|id| sessions.path_of(id));
    if json {
        println!("{row}");
        return;
    }
    let text = |key: &str| row[key].as_str().unwrap_or("-").to_string();
    let duration = row["duration_ms"]
        .as_u64()
        .map_or_else(|| "-".to_string(), |ms| format!("{ms}ms"));
    let exit = row["exit_code"]
        .as_i64()
        .map(|code| format!(" exit={code}"))
        .unwrap_or_default();
    let action = row["action"]
        .as_str()
        .map(|a| format!(" {a}"))
        .unwrap_or_default();
    println!(
        "{} {}{} {}{} {} {} {} ({}) {}",
        text("ts"),
        text("tool"),
        action,
        text("outcome"),
        exit,
        duration,
        text("session"),
        text("actor"),
        text("trust"),
        call.call_id,
    );
}

fn print_call(calls: &[Call], call_id: &str, sessions: &Sessions, json: bool) -> Result<()> {
    let Some(call) = calls.iter().find(|call| call.call_id == call_id) else {
        bail!("no call {call_id} in this ledger");
    };
    let path_of = |id: &str| sessions.path_of(id);
    let parent = call
        .parent_call_id()
        .and_then(|id| calls.iter().find(|c| c.call_id == id))
        .map(|parent| read::row(parent, &path_of));
    let children: Vec<Value> = calls
        .iter()
        .filter(|c| c.parent_call_id() == Some(call_id))
        .map(|c| read::row(c, &path_of))
        .collect();
    let report = serde_json::json!({
        "call_id": call.call_id,
        "session": call.session_id().map(|id| path_of(id).unwrap_or_else(|| id.to_string())),
        "start": call.start,
        "ends": call.ends,
        "parent": parent,
        "children": children,
    });
    if json {
        println!("{report}");
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

/// Print calls as new records arrive, until interrupted.
fn follow(
    target: &Target,
    filter: &Filter,
    sessions: &Sessions,
    json: bool,
    mut last_seq: Option<u64>,
) -> Result<()> {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let today = chrono::Utc::now().date_naive();
        for call in read::load_calls(&target.ledger_dir, Some(today))? {
            let newest = call
                .ends
                .iter()
                .filter_map(|end| end["seq"].as_u64())
                .chain([call.seq])
                .max()
                .unwrap_or(call.seq);
            if last_seq.is_some_and(|last| newest <= last) {
                continue;
            }
            last_seq = Some(last_seq.map_or(newest, |last| last.max(newest)));
            if filter.matches(&call) {
                print_row(&call, sessions, json);
            }
        }
    }
}

pub fn verify(paths: &GlobalPaths, peko: &str, json: bool) -> Result<()> {
    let target = target(paths, peko)?;
    let report = read::verify(&target.ledger_dir, &paths.audit_dir(), &target.principal_id)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "intact": report.is_intact(),
                "records": report.chain.records,
                "first_seq": report.chain.first_seq,
                "last_seq": report.chain.last_seq,
                "head": report.chain.head,
                "chain_problems": report
                    .chain
                    .problems
                    .iter()
                    .map(|p| format!("{p:?}"))
                    .collect::<Vec<_>>(),
                "missing_from_global": report.missing_from_global,
                "missing_from_ledger": report.missing_from_ledger,
                "digest_mismatch": report.digest_mismatch,
            })
        );
    } else {
        println!(
            "{} {peko}: {} records (seq {}–{})",
            if report.is_intact() { "✅" } else { "❌" },
            report.chain.records,
            report.chain.first_seq.map_or("-".into(), |s| s.to_string()),
            report.chain.last_seq.map_or("-".into(), |s| s.to_string()),
        );
        for problem in &report.chain.problems {
            println!("   chain: {problem:?}");
        }
        for id in &report.missing_from_global {
            println!("   in the ledger but not the global log: {id}");
        }
        for id in &report.missing_from_ledger {
            println!("   in the global log but not the ledger: {id}");
        }
        for id in &report.digest_mismatch {
            println!("   params differ between the logs: {id}");
        }
        println!(
            "   Tamper evidence only: anyone who can write both logs can rewrite both (ADR-079 D5)."
        );
    }
    if !report.is_intact() {
        std::process::exit(1);
    }
    Ok(())
}
