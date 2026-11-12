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
