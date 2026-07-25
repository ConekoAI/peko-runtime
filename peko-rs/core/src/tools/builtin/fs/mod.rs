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
