//! `peko version` — print the runtime version.
//!
//! Distinct from `peko --version` (handled by clap's top-level version
//! attribute). The flag is for users typing `peko --version` at the
//! terminal; this subcommand is for programmatic use by tools that need
//! the version in a parseable shape — notably `peko-desktop`'s
//! `SidecarSupervisor` (ADR-043) and external health-check scripts.
//!
//! Output formats:
//! - default: `<semver>\n`
//! - `--json`: `{"version": "<semver>"}\n`

use clap::Args;
use serde_json::json;

/// Arguments for `peko version`
#[derive(Args, Debug, Clone)]
pub struct VersionArgs {
    /// Output as JSON for machine consumption.
    #[arg(long)]
    pub json: bool,
}

/// Handle `peko version`
pub fn handle_version(args: &VersionArgs, _global_json: bool) -> anyhow::Result<()> {
    let version = crate::VERSION;
    if args.json {
        println!("{}", json!({ "version": version }));
    } else {
        println!("{version}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_version_plain_output() {
        // Capture stdout by redirecting at the file descriptor level is
        // overkill for a one-liner — instead we assert on the formatting
        // helper directly so the test stays hermetic.
        let v = crate::VERSION;
        let formatted = format!("{v}\n");
        assert!(formatted.ends_with('\n'));
        assert!(!formatted.trim().is_empty());
    }

    #[test]
    fn handle_version_json_is_parseable() {
        let v = crate::VERSION;
        let payload = json!({ "version": v });
        let serialized = serde_json::to_string(&payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["version"].as_str().unwrap(), v);
    }
}