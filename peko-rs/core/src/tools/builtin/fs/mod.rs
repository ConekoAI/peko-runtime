//! Filesystem tools
//!
//! Granular filesystem operations for agents:
//! - `Read`: Read file contents
//! - `Write`: Write file contents
//! - `glob`: Find files matching patterns
//! - `grep`: Search file contents
//! - `Edit`: Targeted string replacement in files

pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// One entry found by [`walk`].
pub(crate) struct WalkEntry {
    pub path: std::path::PathBuf,
    /// Metadata of the entry itself (symlinks are not followed).
    pub metadata: std::fs::Metadata,
}

/// The entries under `root` in path order, the way an agent expects a
/// code search to see a project: names starting with `.` are skipped unless
/// `include_hidden`, and anything excluded by `.gitignore` / `.ignore` files
/// (or git's exclude and global ignore lists) is skipped unless
/// `include_ignored` — inside a git repository or not. `max_depth: Some(1)`
/// lists only `root`'s children; `root` itself is never returned.
pub(crate) async fn walk(
    root: std::path::PathBuf,
    max_depth: Option<usize>,
    include_hidden: bool,
    include_ignored: bool,
) -> anyhow::Result<Vec<WalkEntry>> {
    use anyhow::Context as _;
    std::fs::read_dir(&root)
        .with_context(|| format!("Failed to read directory: {}", root.display()))?;
    tokio::task::spawn_blocking(move || {
        let respect = !include_ignored;
        let walker = ignore::WalkBuilder::new(&root)
            .hidden(!include_hidden)
            .ignore(respect)
            .git_ignore(respect)
            .git_global(respect)
            .git_exclude(respect)
            .parents(respect)
            .require_git(false)
            .follow_links(false)
            .max_depth(max_depth)
            .sort_by_file_path(|a, b| a.cmp(b))
            .build();
        walker
            .filter_map(Result::ok)
            .filter(|entry| entry.depth() > 0)
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some(WalkEntry {
                    path: entry.into_path(),
                    metadata,
                })
            })
            .collect()
    })
    .await
    .context("directory walk panicked")
}
