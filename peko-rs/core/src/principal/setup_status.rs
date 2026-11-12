//! What a peko's first turn has done so far (ADR-104 D4), read from its
//! tool ledger (ADR-079). `peko create` prints it; the daemon serves it
//! as `principal_setup_status`.

use std::path::Path;

use serde_json::Value;

use crate::security::ledger::read;

/// Longest argument shown on a line, in characters.
const DETAIL_CHARS: usize = 100;
/// Most of the setup report served over IPC, in bytes.
pub const REPORT_MAX_BYTES: usize = 16 * 1024;

/// How one setup step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The peko checked it and it works.
    Ready,
    /// The peko could not get it working; the note says why.
    Failed,
    /// Not reported: not reached yet, or the turn was stopped.
    Pending,
}

impl StepStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Pending => "pending",
        }
    }
}

/// One setup step and how it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub step: String,
    pub status: StepStatus,
    /// The peko's one line about it: how it checked, or why it failed
    /// and what the owner must do.
    pub note: Option<String>,
}

/// Where a peko's setup stands as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    /// Its definition lists no setup steps.
    None,
    /// Steps are listed and some are not reported; no setup turn is
    /// scheduled or running.
    Pending,
    /// A setup turn is scheduled or running.
    Running,
    /// Every step is reported, ready or failed.
    Done,
}

impl SetupState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
        }
    }
}

/// Each step's result, read from the peko's report (`kb/setup.md`). The
/// report has one line per step, `<n>. ready: …` or `<n>. failed: …`;
/// a step with no such line is pending. Reading is forgiving about
/// emphasis marks and the separator, since a model writes the file.
#[must_use]
pub fn parse_report(report: &str, steps: &[String]) -> Vec<StepResult> {
    let mut results: Vec<StepResult> = steps
        .iter()
        .map(|step| StepResult {
            step: step.clone(),
            status: StepStatus::Pending,
            note: None,
        })
        .collect();
    for line in report.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse::<usize>() else {
            continue;
        };
        let Some(result) = number.checked_sub(1).and_then(|i| results.get_mut(i)) else {
            continue;
        };
        let rest = line[digits.len()..]
            .trim_start_matches(['.', ')', ':', ' '])
            .trim_start_matches(['*', '_', '`']);
        let lower = rest.to_ascii_lowercase();
        let (status, word) = if lower.starts_with("ready") {
            (StepStatus::Ready, "ready")
        } else if lower.starts_with("failed") {
            (StepStatus::Failed, "failed")
        } else {
            continue;
        };
        let note = rest[word.len()..]
            .trim_start_matches(['*', '_'])
            .trim_start_matches([' ', ':', '-', '—', '–'])
            .trim();
        result.status = status;
        result.note = (!note.is_empty()).then(|| note.to_string());
    }
    results
}

/// A peko's Local-tier folder, from its sessions folder inside it.
#[must_use]
pub fn local_root(sessions_dir: &Path) -> std::path::PathBuf {
    sessions_dir
        .parent()
        .map_or_else(|| sessions_dir.to_path_buf(), Path::to_path_buf)
}

/// Where the step list the last setup turn was briefed with is kept, in
/// the peko's Local tier.
const BRIEFED_STEPS_FILE: &str = "setup_steps.json";

fn briefed_steps(local: &Path) -> Option<Vec<String>> {
    serde_json::from_str(&std::fs::read_to_string(local.join(BRIEFED_STEPS_FILE)).ok()?).ok()
}

/// Each setup step of `steps` with its result from the peko's report.
///
/// The report numbers its lines, so a line only speaks for a step if
/// the list has not changed there since the turn that wrote it. A step
/// whose text differs from what the last setup turn was briefed with
/// (the owner edited the list) is pending, whatever the report says.
#[must_use]
pub fn results(workspace: &Path, local: &Path, steps: &[String]) -> Vec<StepResult> {
    let steps = crate::principal::seed::tidy_setup(steps);
    let mut results = parse_report(&report(workspace).unwrap_or_default(), &steps);
    if let Some(briefed) = briefed_steps(local) {
        for (index, result) in results.iter_mut().enumerate() {
            if briefed.get(index) != Some(&result.step) {
                result.status = StepStatus::Pending;
                result.note = None;
            }
        }
    }
    results
}

/// Write the report as the runtime reads it: one line per step that is
/// ready or failed. Leaves a missing report alone when no step has a
/// result.
fn write_report(workspace: &Path, results: &[StepResult]) -> std::io::Result<()> {
    let lines: Vec<String> = results
        .iter()
        .enumerate()
        .filter(|(_, result)| result.status != StepStatus::Pending)
        .map(|(index, result)| {
            let note = result
                .note
                .as_deref()
                .map(|note| format!(": {note}"))
                .unwrap_or_default();
            format!("{}. {}{note}\n", index + 1, result.status.as_str())
        })
        .collect();
    let path = workspace.join(crate::principal::seed::SETUP_REPORT_PATH);
    if lines.is_empty() && !path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lines.concat())
}

/// Where setup stands: `scheduled` is whether a setup turn is scheduled
/// or running.
#[must_use]
pub fn state(results: &[StepResult], scheduled: bool) -> SetupState {
    if results.is_empty() {
        SetupState::None
    } else if scheduled {
        SetupState::Running
    } else if results.iter().all(|r| r.status != StepStatus::Pending) {
        SetupState::Done
    } else {
        SetupState::Pending
    }
}

/// Schedule a setup turn for a peko whose definition lists setup steps
/// (ADR-106). False when it lists none, or a setup turn is already
/// scheduled. The turn is told how each step went last time.
///
/// # Errors
/// When the schedule cannot be read or written.
pub fn start(
    scheduler: &peko_cron::CronScheduler,
    config: &crate::principal::config::PrincipalConfig,
    workspace: &Path,
    local: &Path,
) -> anyhow::Result<bool> {
    use crate::principal::genesis::{setup_job, SETUP_JOB_ID};
    let steps = crate::principal::seed::tidy_setup(&config.intent.setup);
    if steps.is_empty() || scheduler.get_job(SETUP_JOB_ID)?.is_some() {
        return Ok(false);
    }
    // Read against the list the last turn saw, then record this one.
    let previous = results(workspace, local, &steps);
    // The report now speaks for this list: lines about steps that were
    // edited or removed go, so a turn that is stopped before it rewrites
    // the file leaves no old result under a new step.
    if let Err(e) = write_report(workspace, &previous) {
        tracing::warn!("could not rewrite the setup report: {e}");
    }
    scheduler.add_job(&setup_job(config, chrono::Utc::now(), &previous)?)?;
    if let Err(e) = std::fs::create_dir_all(local).and_then(|()| {
        std::fs::write(
            local.join(BRIEFED_STEPS_FILE),
            serde_json::to_vec(&steps).unwrap_or_default(),
        )
    }) {
        tracing::warn!("could not record the setup steps briefed: {e}");
    }
    Ok(true)
}

/// One tool call, as shown to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub call_id: String,
    pub tool: String,
    pub line: String,
    /// `None` while the call is running.
    pub outcome: Option<String>,
}

/// One tool call as a short line: the tool, its action, and the one
/// argument that says what it is doing. `start` is a ledger `start`
/// record, so its arguments are already redacted.
#[must_use]
pub fn progress_line(start: &Value) -> String {
    let tool = start["tool"].as_str().unwrap_or("tool");
    let params = &start["params"];
    let detail = [
        "command",
        "url",
        "file_path",
        "path",
        "name",
        "server",
        "pattern",
        "title",
    ]
    .iter()
    .find_map(|key| params[*key].as_str())
    .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "));
    let mut line = tool.to_string();
    if let Some(action) = params["action"].as_str() {
        line.push(' ');
        line.push_str(action);
    }
    if let Some(detail) = detail {
        let short: String = detail.chars().take(DETAIL_CHARS).collect();
        line.push_str(": ");
        line.push_str(&short);
        if detail.chars().count() > DETAIL_CHARS {
            line.push('…');
        }
    }
    line
}

/// The peko's tool calls since yesterday, oldest first. A ledger that
/// does not exist yet, or cannot be read, has none.
#[must_use]
pub fn steps(ledger_dir: &Path) -> Vec<Step> {
    let today = chrono::Utc::now().date_naive();
    let since = today.pred_opt().unwrap_or(today);
    let Ok(calls) = read::load_calls(ledger_dir, Some(since)) else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let start = call.start.as_ref()?;
            Some(Step {
                call_id: call.call_id.clone(),
                tool: start["tool"].as_str().unwrap_or("tool").to_string(),
                line: progress_line(start),
                outcome: call.outcome().map(str::to_string),
            })
        })
        .collect()
}

/// The peko's own setup report (`kb/setup.md`), if it wrote one, cut to
/// [`REPORT_MAX_BYTES`].
#[must_use]
pub fn report(workspace: &Path) -> Option<String> {
    let text =
        std::fs::read_to_string(workspace.join(crate::principal::seed::SETUP_REPORT_PATH)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut end = text.len().min(REPORT_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_call_is_one_short_line() {
        assert_eq!(
            progress_line(&json!({"tool": "Bash", "params": {"command": "npm  install\n -g x"}})),
            "Bash: npm install -g x"
        );
        assert_eq!(
            progress_line(&json!({"tool": "MCP", "params": {"action": "status"}})),
            "MCP status"
        );
        assert_eq!(
            progress_line(&json!({"tool": "Fetch", "params": {"url": "https://example.com/a"}})),
            "Fetch: https://example.com/a"
        );
        let long = progress_line(&json!({"tool": "Bash", "params": {"command": "x".repeat(300)}}));
        assert!(long.ends_with('…') && long.chars().count() < 120, "{long}");
    }

    #[test]
    fn a_report_is_read_step_by_step() {
        let steps: Vec<String> = ["Add the notes skill", "Add Playwright", "Install ripgrep"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let report = "# Setup\n\n1. ready: `Skill` loads it by name\n\
                      - 2) **Failed** — Node is missing; run `brew install node`\n\
                      9. ready: not a step\nSome other line\n";
        let results = parse_report(report, &steps);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].status, StepStatus::Ready);
        assert_eq!(results[0].note.as_deref(), Some("`Skill` loads it by name"));
        assert_eq!(results[1].status, StepStatus::Failed);
        assert_eq!(
            results[1].note.as_deref(),
            Some("Node is missing; run `brew install node`")
        );
        assert_eq!(results[2].status, StepStatus::Pending, "not reached");
        assert_eq!(results[2].step, "Install ripgrep");

        assert_eq!(state(&results, false), SetupState::Pending);
        assert_eq!(state(&results, true), SetupState::Running);
        assert_eq!(state(&[], false), SetupState::None);
        let all = parse_report("1. ready\n2. failed: no\n3. READY: yes\n", &steps);
        assert!(all[0].note.is_none());
        assert_eq!(state(&all, false), SetupState::Done);
    }

    #[test]
    fn a_setup_turn_is_scheduled_once_and_only_for_listed_steps() {
        let dir = tempfile::tempdir().unwrap();
        let scheduler = peko_cron::CronScheduler::new(dir.path().join("cron.json")).unwrap();
        let mut config: crate::principal::config::PrincipalConfig =
            toml::from_str("name = \"ada\"\n").unwrap();
        let local = dir.path().join("local");
        assert!(
            !start(&scheduler, &config, dir.path(), &local).unwrap(),
            "no steps"
        );

        config.intent.setup = vec!["Install ripgrep".into()];
        std::fs::create_dir_all(dir.path().join("kb")).unwrap();
        std::fs::write(
            dir.path().join("kb/setup.md"),
            "1. failed: needs a password\n",
        )
        .unwrap();
        assert!(start(&scheduler, &config, dir.path(), &local).unwrap());
        assert!(
            !start(&scheduler, &config, dir.path(), &local).unwrap(),
            "already scheduled"
        );
        let job = scheduler
            .get_job(crate::principal::genesis::SETUP_JOB_ID)
            .unwrap()
            .unwrap();
        let peko_cron::CronJobAction::Send { message, .. } = &job.action else {
            panic!("a message job");
        };
        assert!(
            message.contains("(last time: failed: needs a password)"),
            "{message}"
        );
    }

    /// Found in a live run (2026-11-12): the owner edited a step, and its
    /// old result was shown against the new text. A report line speaks
    /// only for the step the turn that wrote it was briefed with.
    #[test]
    fn an_edited_step_is_pending_whatever_the_old_report_says() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local");
        let scheduler = peko_cron::CronScheduler::new(dir.path().join("cron.json")).unwrap();
        let mut config: crate::principal::config::PrincipalConfig =
            toml::from_str("name = \"ada\"\n").unwrap();
        config.intent.setup = vec!["Check git".into(), "Install htop".into()];
        assert!(start(&scheduler, &config, dir.path(), &local).unwrap());
        scheduler
            .delete_job(crate::principal::genesis::SETUP_JOB_ID)
            .unwrap();
        std::fs::create_dir_all(dir.path().join("kb")).unwrap();
        std::fs::write(
            dir.path().join("kb/setup.md"),
            "1. ready: git 2.50\n2. ready: htop 3.5\n",
        )
        .unwrap();
        let unchanged = results(dir.path(), &local, &config.intent.setup);
        assert!(unchanged.iter().all(|r| r.status == StepStatus::Ready));

        config.intent.setup[1] = "Run sudo -v".into();
        config.intent.setup.push("A new step".into());
        let edited = results(dir.path(), &local, &config.intent.setup);
        assert_eq!(edited[0].status, StepStatus::Ready, "untouched");
        assert_eq!(edited[1].status, StepStatus::Pending, "edited");
        assert!(edited[1].note.is_none());
        assert_eq!(edited[2].status, StepStatus::Pending, "added");

        // The next turn is briefed without the stale result.
        assert!(start(&scheduler, &config, dir.path(), &local).unwrap());
        let job = scheduler
            .get_job(crate::principal::genesis::SETUP_JOB_ID)
            .unwrap()
            .unwrap();
        let peko_cron::CronJobAction::Send { message, .. } = &job.action else {
            panic!("a message job");
        };
        assert!(
            message.contains("1. Check git (last time: ready: git 2.50)"),
            "{message}"
        );
        assert!(message.contains("2. Run sudo -v\n"), "{message}");

        // Also from the live run: that turn was stopped before it rewrote
        // the report. The old line about step 2 must not come back under
        // the new step, so scheduling dropped it from the file.
        scheduler
            .delete_job(crate::principal::genesis::SETUP_JOB_ID)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("kb/setup.md")).unwrap(),
            "1. ready: git 2.50\n"
        );
        let after_stop = results(dir.path(), &local, &config.intent.setup);
        assert_eq!(after_stop[0].status, StepStatus::Ready);
        assert_eq!(after_stop[1].status, StepStatus::Pending);
        assert_eq!(after_stop[2].status, StepStatus::Pending);
    }

    #[test]
    fn a_missing_ledger_or_report_shows_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(steps(&dir.path().join("ledger")).is_empty());
        assert!(report(dir.path()).is_none());
        std::fs::create_dir_all(dir.path().join("kb")).unwrap();
        std::fs::write(dir.path().join("kb/setup.md"), "  \n").unwrap();
        assert!(report(dir.path()).is_none(), "blank is no report");
        std::fs::write(dir.path().join("kb/setup.md"), "é".repeat(REPORT_MAX_BYTES)).unwrap();
        let cut = report(dir.path()).unwrap();
        assert!(cut.len() <= REPORT_MAX_BYTES && cut.chars().all(|c| c == 'é'));
    }
}
