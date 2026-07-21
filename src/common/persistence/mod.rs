//! Shared persistence primitives for append-only runtime data.

pub mod durable;
pub mod file_lock;

pub use durable::append_bytes_durable;
pub use file_lock::FileLock;
