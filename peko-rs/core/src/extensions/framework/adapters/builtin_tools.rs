//! Catalog of built-in tool names registered by the framework.
//!
//! Lives in the extension framework because both sides of the contract
//! read it: the framework's own `ExtensionStore::resolve_tool_name`
//! (builtin-name short-circuit) and the principal layer's
//! `PrincipalCatalog::build` (which computes `enabled` flags for the
//! catalog entries — reached via the
//! `principal::runtime::builtin_tools` re-export). The host crate does
//! not own these names — they are the canonical contract between the
//! framework's `ToolRuntime::register_builtins` call (in
//! `peko-rs/core/src/engine/tool_runtime.rs`) and the principal layer's
//! view of which built-ins are available.

/// Tools registered once at daemon startup by `ToolRuntime::register_builtins`.
pub const GLOBAL_TOOL_NAMES: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Glob",
    "Grep",
    "Edit",
    "session",
    "CronCreate",
    "CronDelete",
    "CronList",
    "CronUpdate",
    "CronTrigger",
    "CronHistory",
    "AsyncStatus",
    "AsyncList",
    "AsyncStop",
    "Skill",
    // PR-4a — channel reading as a tool. The principal's agentic
    // loop calls this on demand; principal boundary preserved
    // because the principal invokes the tool itself.
    "ChannelRead",
    // ADR-061 phase 2a (D3) — one-shot completion / judgment-model
    // dispatch. Unlike the rest of this list it is NOT registered by
    // `ToolRuntime::register_builtins`: it needs the daemon-global
    // `PrincipalManager` (meter + default-model resolution), which is
    // built after the tool runtime, so `daemon::state` registers it on
    // the shared core once the manager exists. It is listed here so the
    // principal catalog gates it by `tool:ModelCall` like any built-in.
    "ModelCall",
    // ADR-061 phase 2b (D7) — workflow runner. Same registration
    // reason as ModelCall: it needs the `PrincipalManager` (caller
    // resolution) and the daemon's `RunTokenRegistry` (spawn tokens),
    // so `daemon::state` registers it. Gated by `tool:Workflow`.
    "Workflow",
    // Sprint 4: `ChannelSend` is per-agent (see
    // `AGENT_SPECIFIC_TOOL_NAMES` below) because the tool needs the
    // caller's principal DID bound at construction — global
    // registration can't supply that. The pre-sprint 4 `ChannelSend`
    // bare-post path is the global one; the principal / user / group
    // branches need the bound DID and live per-agent.
];

/// Tools registered per-agent in `Agent::init_builtins_async()`.
pub const AGENT_SPECIFIC_TOOL_NAMES: &[&str] = &[
    "Agent",
    // Sprint 4: unified channel send (was `send_peer` + the bare-post
    // `ChannelSend`). The dispatch branch is selected by the wire
    // form of the LLM-supplied `channel` parameter (`chan_*` /
    // `principal:<did>` / `user:<id>` / `group:<slug>`).
    "ChannelSend",
    "AsyncSpawn",
    "AsyncOutput",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
    // F35 — synthetic deferred-tool discovery stub. Registered
    // per-agent when `AgentConfig.enable_tool_search` is true.
    "__tool_search",
    // Phase 2 of `feature/multi-model-subagents` — `model_list`
    // builtin. Registered per-agent when both
    // `AgentConfig.enable_model_list` and a bound `ModelCatalog`
    // are present.
    "model_list",
];

/// Concatenation of [`GLOBAL_TOOL_NAMES`] and [`AGENT_SPECIFIC_TOOL_NAMES`].
#[must_use]
pub fn all_tool_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = GLOBAL_TOOL_NAMES.to_vec();
    names.extend_from_slice(AGENT_SPECIFIC_TOOL_NAMES);
    names
}

/// True iff `name` (case-insensitive) is in [`all_tool_names`].
#[must_use]
pub fn is_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    all_tool_names().iter().any(|&n| n.to_lowercase() == lower)
}

/// True iff `name` (case-insensitive) is in [`AGENT_SPECIFIC_TOOL_NAMES`].
#[must_use]
pub fn is_agent_specific_builtin_tool(name: &str) -> bool {
    let lower = name.to_lowercase();
    AGENT_SPECIFIC_TOOL_NAMES
        .iter()
        .any(|&n| n.to_lowercase() == lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_names_includes_both_lists() {
        let names = all_tool_names();
        assert!(names.contains(&"Bash"));
        assert!(names.contains(&"Agent"));
    }

    #[test]
    fn is_builtin_tool_is_case_insensitive() {
        assert!(is_builtin_tool("Bash"));
        assert!(is_builtin_tool("bash"));
        assert!(!is_builtin_tool("nope"));
    }
}
