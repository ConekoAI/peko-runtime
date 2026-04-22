//! Generic registry infrastructure
//!
//! Provides reusable, composable registry patterns to eliminate duplication
//! of `Arc<RwLock<HashMap<K, V>>>` and hand-rolled `HashMap<K, V>` wrappers
//! across the codebase.
//!
//! ## When to use what
//!
//! | Type | Use when | Thread-safe? |
//! |------|----------|-------------|
//! | [`SimpleRegistry`] | Owned by a single struct, `&mut self` access | No |
//! | [`SharedRegistry`] | Shared across tasks/threads, `&self` access | Yes |
//!
//! ## Naming Convention
//!
//! | Suffix | Meaning | Example |
//! |--------|---------|---------|
//! | `Registry` | Read-heavy lookup collection | `ToolRegistry` |
//! | `Manager` | Lifecycle + stateful coordination | `SessionManager` |
//! | `Service` | Business logic / use-case orchestration | `SessionService` |
//! | `Client` | External API consumer | `RegistryClient` |
//! | `Cache` | In-memory temporary storage | `SessionCache` |
//! | `Registrar` | One-time registration helper | `BuiltinToolRegistrar` |
//!
//! ## Migration Status
//!
//! - [x] `SubagentRegistry` → `SimpleRegistry`
//! - [x] `AsyncTaskRegistry` → `SimpleRegistry`
//! - [x] `AsyncResultQueueManager` → `SimpleRegistry`
//! - [x] `ToolRegistry` → `SharedRegistry`
//! - [x] `LocalRegistry` → `SimpleRegistry` (primary storage)
//! - [x] `SessionCache` (was `InMemorySessionRegistry`) → `SimpleRegistry`
//! - [x] `BuiltinRegistry` → renamed to `BuiltinToolRegistrar`

pub mod core;

pub use core::{SharedRegistry, SimpleRegistry};
