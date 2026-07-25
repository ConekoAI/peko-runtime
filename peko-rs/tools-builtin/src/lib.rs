//! `peko-tools-builtin` — Phase F4 partial foldback.
//!
//! After F4 lifted the bulk of built-in tool implementations into
//! `peko_core::tools::builtin::*`, this sat retains only:
//!
//! - **`cron`**: the cron tools (`CronCreateTool` / `CronDeleteTool` /
//!   `CronListTool`) + the `CronRuntime` port trait + the cron DTOs
//!   (`CronJob`, `CronJobAction`, `ScheduleKind`, `DeliveryMode`).
//!   Stays in a sat because `peko-cron` re-exports the DTOs and
//!   `peko_core::daemon::cron_runtime` implements the port trait; lifting
//!   the trait into root would reverse the leaf-crate dep direction.
//! - **`tool_search_metadata`**: pure-data helpers for the
//!   `__tool_search` stub. Engine reaches these through this sat so it
//!   doesn't have to depend on root for static metadata.

pub mod cron;
pub mod tool_search_metadata;

pub use cron::{CronCreateTool, CronDeleteTool, CronListTool, CronRuntime};
// Phase 9b.N.5b.9d: static helpers for the `__tool_search` stub. Lifted
// out of `src/tools/builtin/tool_search.rs` so `peko-engine`'s
// agentic loop can render the catalog entry without depending on
// root-only `ExtensionCore` (which the impl uses for catalog walks
// at execute time; the impl itself stays in root).
pub use tool_search_metadata::{
    synthetic_description, synthetic_parameters, TOOL_SEARCH_DEFAULT_LIMIT, TOOL_SEARCH_TOOL_NAME,
};
