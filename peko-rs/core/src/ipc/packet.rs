//! IPC Packet Types
//!
//! Defines the request/response protocol between CLI and daemon.
//! All packets are serialized with JSON for simplicity (local IPC overhead
//! is negligible; JSON is human-debuggable with netcat/socat).
//!
//! Packet size is limited to ~60KB to stay well under UDP MTU.
//! Larger payloads are chunked at the application layer.

use crate::principal::runtime::OutputFormat;
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// Auth envelope types are `pub use` (not internal `use`) because the CLI
// crate depends on `peko_core::ipc::packet::{AuthCredential, AuthHeader}`
// paths and does not yet depend on `peko-protocol`.
// `MAX_PACKET_SIZE` is internal-use only (the in-tree `if json.len() >
// MAX_PACKET_SIZE` checks at packet serialize time).
use peko_protocol::ipc::MAX_PACKET_SIZE;
pub use peko_protocol::ipc::{AuthCredential, AuthHeader};

/// Authenticated request envelope (ADR-034).
///
/// New clients wrap their `RequestPacket` in this envelope.
/// Old clients send bare `RequestPacket`s, which deserialize with
/// `auth = AuthCredential::None` when parsed as this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticatedRequest {
    #[serde(default)]
    pub auth: AuthHeader,
    #[serde(flatten)]
    pub packet: RequestPacket,
}

/// Request sent from CLI → Daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestPacket {
    /// Execute an agent message and stream the response — retired in
    /// the principal-as-single-actor migration (audit C4). The legacy
    /// Execute path went through `StatelessAgentService` directly,
    /// bypassing `PrincipalManager` permission checks, session
    /// creation, and root-agent routing. All chat traffic is now
    /// routed through `PrincipalSend` (one-shot) or
    /// `PrincipalSendStream` (streaming) — both go through
    /// `PrincipalManager::receive` and produce principal-scoped
    /// sessions and audit trails.

    /// Spawn an async background task
    #[serde(rename = "async_spawn")]
    AsyncSpawn {
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
    },

    /// Cancel an async task
    #[serde(rename = "async_cancel")]
    AsyncCancel { request_id: u64, task_id: String },

    /// Health check / status ping
    #[serde(rename = "ping")]
    Ping { request_id: u64 },

    /// Request graceful daemon shutdown
    #[serde(rename = "shutdown")]
    Shutdown { request_id: u64, force: bool },

    /// Get system status
    #[serde(rename = "system_status")]
    SystemStatus { request_id: u64 },

    /// Run system doctor
    #[serde(rename = "system_doctor")]
    SystemDoctor { request_id: u64 },

    /// Query the in-memory audit log for events emitted this
    /// session (ADR-046). For historical events that pre-date the
    /// current daemon process, the CLI falls back to reading the
    /// JSONL file directly via `peko audit tail` — the IPC query
    /// only sees the ring buffer. The CLI's `peko audit list`
    /// subcommand is the primary user of this packet.
    #[serde(rename = "audit_query")]
    AuditQuery {
        request_id: u64,
        /// Maximum number of entries to return (newest first). The
        /// daemon's ring buffer is bounded (10k entries); requests
        /// above the cap are clipped by the ring buffer.
        limit: u32,
        /// Optional filter: only return events whose `event_type`
        /// starts with this prefix (e.g. `"cron."`).
        event_type_prefix: Option<String>,
        /// Optional filter: only return events whose `details.principal_name`
        /// matches (or whose caller is a `Subject::Principal` with
        /// this id). The `peko audit tail --principal` flag uses this.
        principal: Option<String>,
    },

    // ─── Agent CRUD ─────────────────────────────────────────────────
    // `AgentList` was retired in the principal-as-single-actor migration
    // (audit C1). Use `PrincipalList` / `PrincipalGet` below for the
    // post-migration actor surface.

    // ─── Principal CRUD (post-migration actor surface) ────────────
    /// List all loaded Principals.
    #[serde(rename = "principal_list")]
    PrincipalList { request_id: u64 },

    /// Look up a single Principal by name. Returns `ResponsePacket::PrincipalGet`
    /// on hit, or `ResponsePacket::Error` with `principal_not_found` on miss.
    #[serde(rename = "principal_get")]
    PrincipalGet { request_id: u64, name: String },

    /// Create a new Principal on disk + in-memory manager. The handler
    /// writes `agents/primary.md` before invoking `manager.create`
    /// (the manager scans `agents/` on load) and assigns ownership to
    /// the calling subject. Mirrors `peko principal create <name>` but
    /// without dropping the caller to the CLI.
    ///
    /// `model_id` is the configured model the principal pins at
    /// creation. It is required: every principal must be created with
    /// a configured model.
    #[serde(rename = "principal_create")]
    PrincipalCreate {
        request_id: u64,
        name: String,
        #[serde(default)]
        description: Option<String>,
        model_id: String,
    },

    /// Update an existing Principal's mutable config. All fields
    /// except `name` are optional; omitted fields keep their current
    /// values. Requires `ManageSettings` permission on the principal.
    #[serde(rename = "principal_update")]
    PrincipalUpdate {
        request_id: u64,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exposure: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preferred_model_id: Option<String>,
    },

    /// Remove a Principal and delete its workspace/data. Requires
    /// `ManageSettings` permission on the principal.
    #[serde(rename = "principal_remove")]
    PrincipalRemove { request_id: u64, name: String },

    /// Draft a persona for a Principal from a one-sentence
    /// description. The daemon calls a configured model via
    /// `Provider::chat_with_system` and returns the LLM's raw text
    /// in [`ResponsePacket::PersonaDrafted::content`]. No session,
    /// no memory, no streaming — this is a single-shot draft.
    ///
    /// Backed by the `peko principal persona set <name> --from "…"`
    /// CLI subcommand (see `scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md`
    /// "Top feature wish — guided persona builder"). The CLI parses
    /// the LLM JSON and writes `principal.toml` + `agents/primary.md`.
    #[serde(rename = "persona_draft")]
    PersonaDraft {
        request_id: u64,
        /// Configured model id to call. Validated server-side
        /// against the catalog before the LLM call.
        model_id: String,
        /// Free-form one-sentence description from the user
        /// (e.g. "a senior rust engineer who reviews PRs for
        /// idiomatic patterns and safety").
        from: String,
    },

    // ─── Model catalog listing ──────────────────────────────────────
    #[serde(rename = "model_list")]
    ModelList { request_id: u64 },

    /// Enumerate credentials in the vault. The optional `namespace`
    /// and `kind` filters restrict the listing; missing filters match
    /// everything. Each row is redacted (no material); see
    /// [`CredentialRow`].
    ///
    /// Replaces the pre-RP3A provider-keyed `CredentialList`. The
    /// desktop's `useCredentialList` (Tauri `credential_list`
    /// command at `peko-desktop/src-tauri/src/commands/settings.rs:301`)
    /// consumes this so Settings → Credentials can paint per-pill
    /// "Key set" indicators and the FirstRunWalkthrough can detect
    /// existing configuration. The CLI `peko credential list` path
    /// reads the vault directly and is unchanged; this handler is
    /// purely the IPC surface.
    #[serde(rename = "credential_list")]
    CredentialList {
        request_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        include_system: Option<bool>,
    },

    /// Fetch the full record for one credential (id, namespace, name,
    /// kind, metadata, timestamps). The `material` field is NOT
    /// included — use [`RequestPacket::CredentialGetMaterial`] for
    /// the secret itself (audit-logged).
    #[serde(rename = "credential_get")]
    CredentialGet { request_id: u64, id: String },

    /// Fetch the secret material for a credential. Audit-logged on
    /// the daemon side because the only legitimate caller is the
    /// "Reveal" UI affordance or the rotation-binding test path.
    #[serde(rename = "credential_get_material")]
    CredentialGetMaterial {
        request_id: u64,
        id: String,
        /// Free-form caller-supplied justification. Logged at INFO
        /// alongside the credential id so an audit trail ties the
        /// reveal back to its purpose.
        reason: String,
    },

    /// Live-ping the model identified by `id` and report whether the
    /// endpoint answered. Replaces the pre-migration `CredentialTest`:
    /// validation now happens at the model level (base URL + wire
    /// format + credential), mirroring
    /// `providers::validator::Validator::test`.
    #[serde(rename = "model_test")]
    ModelTest { request_id: u64, id: String },

    /// Insert or overwrite a credential at `(namespace, name)` with
    /// the given material. The vault assigns a fresh UUID on insert
    /// and returns it in the reply; on overwrite the existing
    /// credential at the slot is replaced (a new id is generated
    /// unless the caller specifies one — see RP3A follow-up if that
    /// path becomes necessary).
    ///
    /// `kind` is the lowercase snake_case spelling of
    /// [`crate::common::vault::CredentialKind`]. `metadata` is an
    /// optional JSON object holding per-kind extras (OAuth
    /// `refresh_token` / `expires_at`, BasicAuth `username`,
    /// PrivateKey `algorithm`).
    ///
    /// `replace_on` (PR 3 / `feature/model-first-config`): when
    /// supplied, every catalog entry that previously referenced the
    /// credential at `replace_on` is rewritten to point at the new
    /// id before the response is sent. Used by
    /// `peko credential set --replace-on <old-id>` for bulk rotation
    /// of dependents. The response's `rewired_models` field reports
    /// how many entries were rebound.
    #[serde(rename = "credential_set")]
    CredentialSet {
        request_id: u64,
        namespace: String,
        name: String,
        kind: String,
        /// Raw secret string from the caller. Wrapped in
        /// `SecretString` on the handler side before persisting.
        material: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replace_on: Option<String>,
    },

    /// Remove a credential by `id`. Powers the desktop's
    /// `credential_delete` Tauri command; the CLI's
    /// `peko credential delete <id>` writes the vault directly.
    /// Mirrors `Vault::delete_credential`.
    ///
    /// `force` (PR 3 / `feature/model-first-config`): when `false`
    /// (the default), the handler refuses to delete a credential
    /// referenced by one or more configured models and emits
    /// `ResponsePacket::Error` with code `credential_in_use` so the
    /// CLI can show a dependents list. When `true`, every catalog
    /// entry that pointed at this credential is detached (`null`)
    /// before the delete; `broken_references` on the response
    /// reports how many entries were detached. Force-deletes are
    /// audit-logged at WARN.
    #[serde(rename = "credential_delete")]
    CredentialDelete {
        request_id: u64,
        id: String,
        #[serde(default)]
        force: bool,
    },

    // ─── Rotation bindings (RP3A) ───────────────────────────────────
    /// Enumerate every rotation binding currently configured in the
    /// vault. Each binding carries its slot key (`{namespace}:{name}`),
    /// strategy, and ordered list of credential ids.
    #[serde(rename = "binding_list")]
    BindingList { request_id: u64 },

    /// Fetch one binding by slot key. Returns `None` if no binding
    /// exists for the slot.
    #[serde(rename = "binding_get")]
    BindingGet { request_id: u64, key: String },

    /// Insert or overwrite the rotation binding for a `{namespace}:{name}`
    /// slot. `strategy` is one of `round_robin` (today's only honored
    /// strategy), `last_resort`, or `random` (reserved; the resolver
    /// rejects them with a clear error if encountered). `order` is
    /// the ordered list of credential ids.
    #[serde(rename = "binding_set")]
    BindingSet {
        request_id: u64,
        key: String,
        strategy: String,
        order: Vec<String>,
    },

    /// Remove a binding by slot key. Returns `Ok(true)` if a binding
    /// was removed.
    #[serde(rename = "binding_delete")]
    BindingDelete { request_id: u64, key: String },

    /// Re-read the model catalog and the credential vault from
    /// disk. Sent by `peko model {add,remove}` and
    /// `peko credential {set,delete}` so the long-running daemon
    /// observes CLI mutations without a restart.
    #[serde(rename = "model_reload")]
    ModelReload { request_id: u64 },

    /// Enumerate the built-in model presets the runtime ships
    /// with. Sent by the desktop's "Add Model" modal so the
    /// picker can show the curated list of known presets
    /// (Anthropic, OpenAI, Groq, Ollama, …) with their default
    /// base URL, API format, and curated model list. Mirrors the
    /// CLI's `peko model presets` path, but over IPC so the
    /// desktop doesn't shell out.
    #[serde(rename = "model_templates")]
    ModelTemplates { request_id: u64 },

    /// Add a model to the catalog — either from a built-in
    /// preset (`args.template`) or fully custom
    /// (`args.custom` + `api_format` + `base_url` + `model`).
    /// Optionally stores a key in the vault in the same round-trip.
    /// Mirrors `peko model add` so the desktop modal can do
    /// the same thing without a shell-out.
    #[serde(rename = "model_add")]
    ModelAdd { request_id: u64, args: ModelAddArgs },

    /// Update an existing model catalog entry. All fields except
    /// `id` are optional; omitted fields keep their current values.
    /// The daemon persists the merged entry to `models.toml` and
    /// returns the updated catalog-summary view.
    #[serde(rename = "model_update")]
    ModelUpdate {
        request_id: u64,
        args: ModelUpdateArgs,
    },

    /// Remove a model from the catalog. Returns `removed: true` if
    /// an entry with this id existed. Idempotent — removing a missing
    /// id is not an error.
    #[serde(rename = "model_remove")]
    ModelRemove { request_id: u64, id: String },

    /// Re-read the MCP server configuration from `mcp.toml` and the
    /// credential vault from disk. Sent by `peko ext mcp {add,auth,remove}`
    /// so the long-running daemon observes CLI mutations without a restart.
    #[serde(rename = "mcp_reload")]
    McpReload { request_id: u64 },

    // ─── Quota (F18) ───────────────────────────────────────────────────
    /// Read the principal's current quota status (used + limits +
    /// window bounds). Unauthenticated: any local caller can query
    /// — the daemon's existing trust model is sufficient for F18;
    /// owner-only authz is a follow-up.
    ///
    /// F20: `is_peer` flips the resolver from `PrincipalManager`
    /// to `PeerRegistry`. The `name` field holds the principal name
    /// (default) or the peer id (when `is_peer` is `true`).
    #[serde(rename = "quota_get")]
    QuotaGet {
        request_id: u64,
        name: String,
        #[serde(default)]
        is_peer: bool,
    },

    /// Replace the principal's `QuotaConfig` (input/output/request
    /// limits + cycle). Persists to `principal.toml` and rebuilds
    /// the meter so the new limits take effect on the next call.
    ///
    /// F20: `is_peer` flips the resolver to `PeerRegistry` (writes
    /// `peer.toml` in the peer's directory).
    #[serde(rename = "quota_set")]
    QuotaSet {
        request_id: u64,
        name: String,
        #[serde(default)]
        is_peer: bool,
        config: peko_quota::QuotaConfig,
    },

    /// Force-reset the principal's quota meter to a fresh window
    /// without touching the config. Useful for ops/tests.
    ///
    /// F20: `is_peer` flips the resolver to `PeerRegistry`.
    #[serde(rename = "quota_reset")]
    QuotaReset {
        request_id: u64,
        name: String,
        #[serde(default)]
        is_peer: bool,
    },

    // ─── Extension CRUD (ADR-030 Tier 1) ────────────────────────────
    // Removed in Phase 5 (ADR-047 §2.1): the entire extension IPC
    // surface (`ExtensionList` / `ExtensionInstall` / `ExtensionUninstall`
    // / `ExtensionValidate` / `ExtensionDebug` / `ExtensionInfo` /
    // `ExtensionExport` / `ExtensionBundle`) is gone — extensions are
    // workspace-resident and have no on-disk store, no packager, no
    // runtime lifecycle. Workspace plugins are discovered by the
    // principal's catalog builder instead.

    #[serde(rename = "system_clean")]
    SystemClean {
        request_id: u64,
        scope: Option<String>,
    },

    // ── Runtime (ADR-032) ──
    #[serde(rename = "runtime_id")]
    RuntimeId { request_id: u64 },
    #[serde(rename = "runtime_info")]
    RuntimeInfo { request_id: u64 },
    #[serde(rename = "runtime_list")]
    RuntimeList { request_id: u64 },
    #[serde(rename = "runtime_register")]
    RuntimeRegister {
        request_id: u64,
        runtime_id: String,
        display_name: String,
    },
    #[serde(rename = "runtime_trust")]
    RuntimeTrust { request_id: u64, runtime_id: String },
    #[serde(rename = "runtime_remove")]
    RuntimeRemove { request_id: u64, runtime_id: String },

    // ── Tunnel (ADR-035) ──
    #[serde(rename = "tunnel_stop")]
    TunnelStop { request_id: u64 },
    #[serde(rename = "tunnel_status")]
    TunnelStatus { request_id: u64 },

    /// Comprehensive daemon status (issue #8). Returns uptime, version, and
    /// tunnel health snapshot. Used by `peko daemon status --json`.
    #[serde(rename = "status")]
    Status { request_id: u64 },

    // ── Instance status ──
    #[serde(rename = "instance_set_status")]
    InstanceSetStatus {
        request_id: u64,
        agent_name: String,
        status: String,
    },
    #[serde(rename = "instance_set_exposure")]
    InstanceSetExposure {
        request_id: u64,
        agent_name: String,
        exposure: String,
    },

    // ── Auth management (ADR-034) ──
    #[serde(rename = "auth_api_key_create")]
    AuthApiKeyCreate {
        request_id: u64,
        name: String,
        scopes: Vec<String>,
    },
    #[serde(rename = "auth_api_key_list")]
    AuthApiKeyList { request_id: u64 },
    #[serde(rename = "auth_api_key_revoke")]
    AuthApiKeyRevoke { request_id: u64, key_id: String },
    #[serde(rename = "auth_status")]
    AuthStatus { request_id: u64 },

    // ── Ownership and Permission (ADR-039) ──
    //
    // Grant/revoke packets carry a single `subject: Subject`.
    // The legacy `(subject_id, subject_type)` wire fields from ADR-033
    // were dropped in issue #30.
    // ── Principal operations ─────────────────────────────────────────
    /// Non-streaming principal send. Returns a single `PrincipalSent`
    /// response with the root agent's final answer.
    #[serde(rename = "principal_send")]
    PrincipalSend {
        request_id: u64,
        name: String,
        message: String,
        user: String,
        /// Do not treat `/`-prefixed messages as slash commands.
        #[serde(default)]
        no_slash: bool,
        /// Preferred output format for slash-command responses.
        #[serde(default)]
        output_format: OutputFormat,
        /// Per-message configured model override (e.g. `peko send --model ...`).
        #[serde(default)]
        override_model: Option<String>,
    },

    /// Streaming principal send. The daemon emits a sequence of
    /// `PrincipalSentChunk` deltas as the root agent agent's response
    /// unfolds, followed by exactly one `PrincipalSentDone` carrying
    /// the full final answer (identical content to what
    /// `PrincipalSend` would have returned). Wire-compatible with the
    /// `principal_send` request shape so the desktop Chat can opt in
    /// to streaming without changing the root agent's behavior.
    #[serde(rename = "principal_send_stream")]
    PrincipalSendStream {
        request_id: u64,
        name: String,
        message: String,
        user: String,
        /// Do not treat `/`-prefixed messages as slash commands.
        #[serde(default)]
        no_slash: bool,
        /// Preferred output format for slash-command responses.
        #[serde(default)]
        output_format: OutputFormat,
        /// Per-message configured model override (e.g. `peko send --model ...`).
        #[serde(default)]
        override_model: Option<String>,
    },

    /// Soft-stop the run bound to a (principal, peer) thread.
    ///
    /// The stop complement to `PrincipalSend`/`PrincipalSendStream`:
    /// the server resolves the peer's child session, fires the run's
    /// cancel token (the agentic loop exits at the next iteration
    /// boundary), posts a `⏹ stopped by user` marker to the peer's DM
    /// channel, and leaves a stop-context note in the session inbox
    /// for the next run. `peer: None` means the principal's owner
    /// thread. The server enforces the same privacy rule as
    /// `PrincipalLog` (`caller == peer || caller == principal.owner`).
    /// Idempotent: no in-flight run ⇒ `Done { success: false,
    /// error: "no running turn…" }` so the CLI can print a notice and
    /// exit 0.
    #[serde(rename = "principal_stop")]
    PrincipalStop {
        request_id: u64,
        name: String,
        /// None means "the principal's owner" (default thread).
        peer: Option<peko_auth::Subject>,
    },

    /// Read a peer's conversation thread with a Principal.
    ///
    /// This is the read complement to `PrincipalSend`. There is no
    /// `peko session` CLI command (ADR-042): peers only ever see their
    /// own thread, the owner sees their own by default, and any
    /// other-thread read by the owner requires `peer` to be set
    /// explicitly. The server enforces the privacy check (`caller ==
    /// peer || caller == principal.owner`) plus the principal's `Chat`
    /// grant before returning anything.
    #[serde(rename = "principal_log")]
    PrincipalLog {
        request_id: u64,
        name: String,
        /// None means "the principal's owner" (default view).
        peer: Option<peko_auth::Subject>,
        /// Cap on number of messages returned (default 50, max 1000).
        limit: Option<usize>,
        /// Only messages newer than `now() - since_secs` are returned.
        since_secs: Option<u64>,
        /// Opaque pagination cursor from a previous `PrincipalLog`
        /// response's `next_cursor`. `None` reads the latest page.
        /// Malformed cursors and cursors bound to another thread are
        /// rejected with `bad_cursor`.
        #[serde(default)]
        cursor: Option<String>,
    },

    /// Watch a peer's conversation thread with a Principal: replay
    /// `Posted` events newer than `since_cursor`, then stream new
    /// messages live.
    ///
    /// The privacy-checked sibling of `ChannelEventsWatch` (ADR-042):
    /// the server resolves `(name, peer)` to the peer's DM channel and
    /// enforces the same rule as `PrincipalLog` (`caller == peer ||
    /// caller == principal.owner`, plus the `Chat` grant). The
    /// response is a stream of `PrincipalLogAppended` packets
    /// interleaved with `Heartbeat` ticks (every
    /// `HEARTBEAT_INTERVAL_SECS`) so a quiet thread never trips the
    /// client's per-packet idle timeout. No `Done` on close — the
    /// stream ends when the client disconnects or the daemon shuts
    /// down.
    ///
    /// `since_cursor` reuses the log command's line-number cursor:
    /// only rows with line number greater than the cursor replay.
    /// `None` replays the whole thread.
    #[serde(rename = "principal_log_watch")]
    PrincipalLogWatch {
        request_id: u64,
        name: String,
        /// None means "the principal's owner" (default thread).
        peer: Option<peko_auth::Subject>,
        /// Replay seed — rows strictly newer than this line-number
        /// cursor. `None` replays from the start.
        #[serde(default)]
        since_cursor: Option<String>,
    },
    #[serde(rename = "principal_export")]
    PrincipalExport {
        request_id: u64,
        name: String,
        output: Option<String>,
        include_sessions: bool,
        /// Always `false` since Phase 5 (ADR-047): extensions are
        /// workspace-resident and ride along in the bundle. Retained
        /// for backward compat with old CLIs that still emit the
        /// field; the daemon ignores it.
        #[serde(default)]
        with_extensions: bool,
    },

    #[serde(rename = "principal_import")]
    PrincipalImport {
        request_id: u64,
        file_path: String,
        name: Option<String>,
        #[serde(default)]
        allow_unsigned: bool,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        confirmed: bool,
        /// Capabilities selected by the user during the preview flow.
        #[serde(default)]
        selected_capabilities: Vec<String>,
    },

    /// Preview a `.principal` package before importing it.
    #[serde(rename = "principal_import_preview")]
    PrincipalImportPreview {
        request_id: u64,
        file_path: String,
        name: Option<String>,
        #[serde(default)]
        allow_unsigned: bool,
        #[serde(default)]
        force: bool,
    },

    /// Preview a remote Principal package before pulling it.
    #[serde(rename = "principal_pull_preview")]
    PrincipalPullPreview {
        request_id: u64,
        registry_ref: String,
        name: Option<String>,
        #[serde(default)]
        force: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    },

    #[serde(rename = "principal_push")]
    PrincipalPush {
        request_id: u64,
        name: String,
        registry_host: Option<String>,
        registry_token: Option<String>,
    },

    #[serde(rename = "principal_pull")]
    PrincipalPull {
        request_id: u64,
        registry_ref: String,
        name: Option<String>,
        #[serde(default)]
        force: bool,
        #[serde(default)]
        confirmed: bool,
        /// Capabilities selected by the user during the preview flow.
        #[serde(default)]
        selected_capabilities: Vec<String>,
        /// Allow pulling an unsigned package.
        #[serde(default)]
        allow_unsigned: bool,
        registry_host: Option<String>,
        registry_token: Option<String>,
    },

    #[serde(rename = "principal_grant_permission")]
    PrincipalGrantPermission {
        request_id: u64,
        name: String,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    },

    #[serde(rename = "principal_revoke_permission")]
    PrincipalRevokePermission {
        request_id: u64,
        name: String,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    },

    /// Set the live status of a Principal's tunnel instance. Persisted to
    /// `principal.toml` so the change survives daemon restart. Delegates
    /// to `TunnelDispatcher::set_instance_status` to publish a
    /// `status_update` to the hub.
    #[serde(rename = "principal_set_status")]
    PrincipalSetStatus {
        request_id: u64,
        name: String,
        /// One of: "online", "offline", "busy", "error".
        status: String,
    },

    /// Set the exposure of a Principal's tunnel instance. Persisted to
    /// `principal.toml` so the change survives daemon restart. Delegates
    /// to `TunnelDispatcher::set_instance_exposure` to publish an
    /// `exposure_update` to the hub.
    #[serde(rename = "principal_set_exposure")]
    PrincipalSetExposure {
        request_id: u64,
        name: String,
        /// One of: "unexposed", "private", "public".
        exposure: String,
    },

    #[serde(rename = "principal_permissions")]
    PrincipalPermissions { request_id: u64, name: String },

    /// PR #11: mint an invite token for this principal. The caller
    /// supplies the desired `scope` (one or more `Permission` values)
    /// and the TTL in seconds. The runtime returns the encoded token
    /// plus its claims so the CLI/desktop can render the share URL.
    #[serde(rename = "principal_mint_invite")]
    PrincipalMintInvite {
        request_id: u64,
        name: String,
        scope: Vec<peko_auth::ownership::Permission>,
        ttl_secs: u64,
    },

    /// PR #11: revoke an invite token by `jti`. Idempotent — revoking
    /// an unknown `jti` is a no-op (returns success).
    #[serde(rename = "principal_revoke_invite")]
    PrincipalRevokeInvite {
        request_id: u64,
        name: String,
        jti: String,
    },

    // ─── PR-2c: channel IPC variants ─────────────────────────────
    //
    // The seven variants below mirror `peko_channel::cli_handlers`
    // (the pure-port CLI router). PR-3 may add a Shared-tier
    // `ChannelCreateShared` variant; PR-2 ships Runtime-tier only.
    //
    // Wire shape mirrors the `cli_handlers` request structs. Where
    // the handler takes a `PrincipalId`, the wire carries the
    // principal's display name (the daemon resolves it back to a
    // `PrincipalId` via `ChannelHost::principal_manager`).

    /// Create a new channel owned by `creator_name`.
    ///
    /// `passive_binding` (Phase 4, agent-session paradigm sprint) is the
    /// optional `--bind` value (session id or `/path`). Serde-defaulted
    /// so pre-Phase-4 clients decode as `None` (unbound) and older
    /// daemons silently ignore the field.
    ///
    /// `id` (ADR-049 Phase 2, D5) is the optional explicit channel id
    /// (e.g. `group:<slug>`), validated by the handler via
    /// `ChannelId::parse`. Serde-defaulted so pre-Phase-2 clients
    /// decode as `None` (the store mints a fresh `chan_<8 base36>`).
    #[serde(rename = "channel_create")]
    ChannelCreate {
        request_id: u64,
        creator_name: String,
        name: String,
        #[serde(default)]
        passive_binding: Option<String>,
        #[serde(default)]
        id: Option<String>,
    },

    /// Add `invitee_name` to `channel` (invited by `inviter_name`).
    #[serde(rename = "channel_invite")]
    ChannelInvite {
        request_id: u64,
        channel: String,
        inviter_name: String,
        invitee_name: String,
    },

    /// Post a message to `channel` from `sender_name`. `parent` is
    /// the optional task_id of the message being replied to.
    ///
    /// `sender_name` is a principal name, or (ADR-049 Phase 2) a
    /// `user:<id>` Subject wire form — the handler takes user senders
    /// verbatim and lets store-level Subject membership authorize
    /// the write.
    #[serde(rename = "channel_post")]
    ChannelPost {
        request_id: u64,
        channel: String,
        sender_name: String,
        text: String,
        parent: Option<String>,
    },

    /// List events for `channel` since `since` (None = from start).
    ///
    /// `requester` (ADR-049 Phase 2, D6) is the optional Subject wire
    /// form (`user:<id>` / `principal:<did>`) of the reader. When
    /// present, the handler refuses the read unless the requester is a
    /// channel member; a pekohub JWT caller must name themselves (and
    /// is gated even when the field is absent — Phase 4). Serde-
    /// defaulted so pre-Phase-2 clients decode as `None` (ungated for
    /// Local-trust callers — the desktop carries no identity yet).
    #[serde(rename = "channel_peek")]
    ChannelPeek {
        request_id: u64,
        channel: String,
        since: Option<String>,
        #[serde(default)]
        requester: Option<String>,
    },

    /// PR-2b: subscribe to live events for `channel`. The daemon
    /// replays events from `since` (None = from start) as a series of
    /// `ChannelEventReceived` packets, then holds the connection open
    /// and forwards new events as they arrive (signalled by the
    /// dispatcher after `append_remote_event` succeeds). The stream
    /// closes when the client disconnects or the daemon shuts down.
    ///
    /// Wire-compatible with the chat's `PrincipalSendStream` shape
    /// (request → stream of packets → `Done` on close) so the
    /// desktop Tauri backend can reuse the existing stream-forwarding
    /// path that already emits `peko-stream` events for the chat.
    ///
    /// `requester` (ADR-049 Phase 2, D6): same optional membership
    /// gate as `ChannelPeek` — see that variant's doc.
    #[serde(rename = "channel_events_watch")]
    ChannelEventsWatch {
        request_id: u64,
        channel: String,
        since: Option<String>,
        #[serde(default)]
        requester: Option<String>,
    },

    /// List members of `channel`.
    #[serde(rename = "channel_members")]
    ChannelMembers {
        request_id: u64,
        channel: String,
    },

    /// List channels where `principal_name` is a member.
    #[serde(rename = "channel_list")]
    ChannelList {
        request_id: u64,
        principal_name: String,
    },

    /// Remove `principal_name` from `channel`. PR-3a: closes the
    /// missing IPC variant — PR-1 had `handle_leave` only on the
    /// in-process path.
    #[serde(rename = "channel_leave")]
    ChannelLeave {
        request_id: u64,
        channel: String,
        principal_name: String,
    },

    /// Copy a Runtime-tier channel into the Shared tier (PR-3d).
    /// Authority gate (`channel:write_shared`) is enforced by the
    /// daemon handler; the in-process CLI fallback path performs
    /// the same check via `RuntimeAuthority`.
    #[serde(rename = "channel_pin_to_shared")]
    ChannelPinToShared {
        request_id: u64,
        channel: String,
    },
}

impl RequestPacket {
    /// Get the request ID from any variant
    #[must_use]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::AsyncSpawn { request_id, .. }
            | Self::AsyncCancel { request_id, .. }
            | Self::Ping { request_id }
            | Self::Shutdown { request_id, .. }
            | Self::PrincipalList { request_id }
            | Self::PrincipalGet { request_id, .. }
            | Self::PrincipalCreate { request_id, .. }
            | Self::PrincipalUpdate { request_id, .. }
            | Self::PrincipalRemove { request_id, .. }
            | Self::PersonaDraft { request_id, .. }
            | Self::ModelList { request_id }
            | Self::ModelReload { request_id }
            | Self::ModelTemplates { request_id }
            | Self::ModelAdd { request_id, .. }
            | Self::ModelUpdate { request_id, .. }
            | Self::ModelRemove { request_id, .. }
            | Self::ModelTest { request_id, .. }
            | Self::McpReload { request_id }
            | Self::CredentialList { request_id, .. }
            | Self::CredentialGet { request_id, .. }
            | Self::CredentialGetMaterial { request_id, .. }
            | Self::CredentialSet { request_id, .. }
            | Self::CredentialDelete { request_id, .. }
            | Self::BindingList { request_id }
            | Self::BindingGet { request_id, .. }
            | Self::BindingSet { request_id, .. }
            | Self::BindingDelete { request_id, .. }
            | Self::SystemStatus { request_id }
            | Self::SystemDoctor { request_id }
            | Self::AuditQuery { request_id, .. }
            | Self::SystemClean { request_id, .. }
            | Self::RuntimeId { request_id }
            | Self::RuntimeInfo { request_id }
            | Self::RuntimeList { request_id }
            | Self::RuntimeRegister { request_id, .. }
            | Self::RuntimeTrust { request_id, .. }
            | Self::RuntimeRemove { request_id, .. }
            | Self::AuthApiKeyCreate { request_id, .. }
            | Self::AuthApiKeyList { request_id }
            | Self::AuthApiKeyRevoke { request_id, .. }
            | Self::AuthStatus { request_id }
            | Self::TunnelStop { request_id }
            | Self::TunnelStatus { request_id }
            | Self::Status { request_id }
            | Self::InstanceSetStatus { request_id, .. }
            | Self::InstanceSetExposure { request_id, .. }
            | Self::PrincipalSend { request_id, .. }
            | Self::PrincipalSendStream { request_id, .. }
            | Self::PrincipalLog { request_id, .. }
            | Self::PrincipalLogWatch { request_id, .. }
            | Self::PrincipalExport { request_id, .. }
            | Self::PrincipalImport { request_id, .. }
            | Self::PrincipalImportPreview { request_id, .. }
            | Self::PrincipalPullPreview { request_id, .. }
            | Self::PrincipalPush { request_id, .. }
            | Self::PrincipalPull { request_id, .. }
            | Self::PrincipalGrantPermission { request_id, .. }
            | Self::PrincipalRevokePermission { request_id, .. }
            | Self::PrincipalSetStatus { request_id, .. }
            | Self::PrincipalSetExposure { request_id, .. }
            | Self::PrincipalPermissions { request_id, .. }
            | Self::PrincipalMintInvite { request_id, .. }
            | Self::PrincipalRevokeInvite { request_id, .. }
            | Self::PrincipalStop { request_id, .. }
            | Self::QuotaGet { request_id, .. }
            | Self::QuotaSet { request_id, .. }
            | Self::QuotaReset { request_id, .. }
            | Self::ChannelCreate { request_id, .. }
            | Self::ChannelInvite { request_id, .. }
            | Self::ChannelPost { request_id, .. }
            | Self::ChannelPeek { request_id, .. }
            | Self::ChannelEventsWatch { request_id, .. }
            | Self::ChannelMembers { request_id, .. }
            | Self::ChannelList { request_id, .. }
            | Self::ChannelLeave { request_id, .. }
            | Self::ChannelPinToShared { request_id, .. } => *request_id,
        }
    }

    /// Resolve the canonical `Subject` subject for a grant/revoke
    /// packet. The legacy ADR-033 wire shape was removed in issue #30;
    /// every grant/revoke packet now carries the subject inline.
    ///
    /// Only the four grant/revoke variants carry a subject. For any
    /// other variant this method returns `Ok(Subject::User(""))` so
    /// callers can use the same match arm — but in practice the server
    /// only calls this inside the grant/revoke arms.
    #[must_use]
    pub fn resolved_subject(&self) -> peko_auth::Subject {
        use peko_auth::Subject;

        match self {
            Self::PrincipalGrantPermission { subject, .. }
            | Self::PrincipalRevokePermission { subject, .. } => subject.clone(),
            // Non-grant/revoke packets have no subject. Return the
            // default sentinel so the caller doesn't have to special-case.
            _ => Subject::User(String::new()),
        }
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }

    /// Deserialize from JSON bytes
    ///
    /// # Errors
    /// Returns error if deserialization fails
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Extract the auth credential from this request.
    ///
    /// For v0.1.0, this always returns `AuthCredential::None` because
    /// `RequestPacket` variants do not carry auth directly. Use
    /// `AuthenticatedRequest::from_bytes` to parse requests that include auth.
    #[must_use]
    pub fn auth(&self) -> AuthCredential {
        AuthCredential::None
    }
}

/// Cumulative token usage for a completed run, surfaced on the
/// `ResponsePacket::RunSummary` packet. Mirrors the `AgenticEvent::Usage`
/// shape (`peko-rs/events/src/lib.rs:239-251`) — three `u32` fields,
/// no cache/reasoning tokens (those live upstream in
/// `peko_message::TokenUsage` and are intentionally lossy at the
/// `AgenticEvent` boundary; widening that is a separate concern).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUsageSummary {
    /// Prompt tokens consumed across the run.
    #[serde(default)]
    pub prompt_tokens: u32,
    /// Completion tokens generated across the run.
    #[serde(default)]
    pub completion_tokens: u32,
    /// Total tokens consumed across the run.
    #[serde(default)]
    pub total_tokens: u32,
}

/// One tool-call error observed during a run, surfaced on
/// `ResponsePacket::RunSummary`. `tool_name` is the name as it
/// appeared on the most recent `ToolStart` event for the same
/// `tool_id`; missing when no `ToolStart` was seen before the
/// failure (the runtime still records the error so users can see
/// `success: false` regardless). `error_message` is the
/// short-truncated error text the tool returned in `result`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolErrorEntry {
    #[serde(default)]
    pub tool_id: String,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub error_message: String,
}

/// Schema version carried on every `PrincipalLogMessage` row.
pub const PRINCIPAL_LOG_SCHEMA_VERSION: u8 = 1;

/// One immutable, consumer-visible text message in a `PrincipalLog`
/// page — the row type of `peko log`. Lives next to the packet that
/// carries it; the dedicated chat-log store crate was retired in
/// Phase 13 (the peer DM channels are the record now).
///
/// Wire shape is unchanged from the retired chat-log row type
/// (camelCase field names, same defaults/skips).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalLogMessage {
    pub schema_version: u8,
    pub id: String,
    pub sender: peko_auth::Subject,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl PrincipalLogMessage {
    #[must_use]
    pub fn new(
        sender: peko_auth::Subject,
        text: impl Into<String>,
        correlation_id: Option<String>,
    ) -> Self {
        Self {
            schema_version: PRINCIPAL_LOG_SCHEMA_VERSION,
            id: format!("chat_{}", uuid::Uuid::new_v4().simple()),
            sender,
            timestamp: chrono::Utc::now(),
            text: text.into(),
            correlation_id,
        }
    }
}

/// Response sent from Daemon → CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsePacket {
    /// Streaming text chunk
    #[serde(rename = "text")]
    Text {
        request_id: u64,
        /// Sequence number for ordering (per-request, monotonic)
        seq: u32,
        chunk: String,
    },

    /// Async task receipt
    #[serde(rename = "async_receipt")]
    AsyncReceipt {
        request_id: u64,
        receipt: crate::extensions::framework::async_exec::executor::AsyncTaskReceipt,
    },

    /// Final success/failure marker
    #[serde(rename = "done")]
    Done {
        request_id: u64,
        success: bool,
        error: Option<String>,
    },

    /// Error response
    #[serde(rename = "error")]
    Error { request_id: u64, message: String },

    /// Ping response
    #[serde(rename = "pong")]
    Pong {
        request_id: u64,
        uptime_secs: u64,
        version: String,
    },

    /// Heartbeat — sent during long streams so CLI can detect dead daemon
    #[serde(rename = "heartbeat")]
    Heartbeat { request_id: u64 },

    /// Shutdown acknowledgement
    #[serde(rename = "shutting_down")]
    ShuttingDown { request_id: u64 },

    /// Cron job list response
    // 2026-08-25: cron is an internal principal tool. The legacy
    // `CronList` / `CronAdded` / `CronRemoved` IPC response variants
    // were retired; principals interact with cron via `tool:Cron*`
    // grants in the agentic-loop funnel.

    // ─── PR-2c: channel IPC response variants ────────────────────
    // Mirror the seven `RequestPacket::Channel*` variants 1:1.

    /// Channel created — returns the new `ChannelId`.
    #[serde(rename = "channel_created")]
    ChannelCreated {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
    },

    /// Invite acknowledged. ADR-049 Phase 1: the invitee is
    /// `Subject`-typed (a principal or a `user:<id>` user).
    #[serde(rename = "channel_invited")]
    ChannelInvited {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        invitee: peko_subject::Subject,
    },

    /// Post acknowledged — returns the new task id.
    #[serde(rename = "channel_posted")]
    ChannelPosted {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        task_id: String,
    },

    /// Peek result — full event list since the requested cursor.
    #[serde(rename = "channel_peek_result")]
    ChannelPeekResult {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        events: Vec<peko_protocol::channel::ChannelEvent>,
    },

    /// PR-2b: one event in a `ChannelEventsWatch` stream. The daemon
    /// emits one of these per event — first replaying events from the
    /// `since` cursor, then forwarding new events as they arrive. The
    /// stream closes with `Done { request_id }` when the client
    /// disconnects or the daemon shuts down.
    #[serde(rename = "channel_event_received")]
    ChannelEventReceived {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        event: peko_protocol::channel::ChannelEvent,
    },

    /// Members list. ADR-049 Phase 1: members are `Subject`-typed
    /// (principals and users).
    #[serde(rename = "channel_members_result")]
    ChannelMembersResult {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        members: Vec<peko_subject::Subject>,
        /// P1.2 attribution: per-member runtime provenance (`runtime_id
        /// = None` for local members). Default empty so pre-PR-3b
        /// consumers that don't read this field see no change to the
        /// wire shape.
        #[serde(default)]
        member_provenance: Vec<peko_protocol::channel::MemberProvenance>,
    },

    /// List of channels where the principal is a member.
    #[serde(rename = "channel_list_result")]
    ChannelListResult {
        request_id: u64,
        principal: peko_subject::PrincipalId,
        channels: Vec<peko_protocol::channel::ChannelId>,
    },

    /// Leave acknowledged — `principal` is no longer a member of
    /// `channel`.
    #[serde(rename = "channel_left")]
    ChannelLeft {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        principal: peko_subject::PrincipalId,
    },

    /// Shared-tier pin acknowledged. `shared_path` is the absolute
    /// Shared root the channel directory was copied to. The Runtime
    /// source dir remains (COPY semantics — see PR-3d plan).
    #[serde(rename = "channel_pinned_to_shared")]
    ChannelPinnedToShared {
        request_id: u64,
        channel: peko_protocol::channel::ChannelId,
        shared_path: String,
    },

    /// Quota status snapshot (F18). Carries the principal's live
    /// `QuotaState` — used counters, configured limits (via the
    /// cycle), and the current window's start/end timestamps.
    /// Returned for `QuotaGet`, `QuotaSet`, and `QuotaReset`.
    #[serde(rename = "quota_status")]
    QuotaStatus {
        request_id: u64,
        state: peko_quota::QuotaState,
        /// The principal's effective `QuotaConfig`. Mirrors the
        /// `state.cycle` and exposes the configured limits so the
        /// CLI can render "1000 / 5000 input tokens" without a
        /// second round-trip.
        config: peko_quota::QuotaConfig,
    },

    /// Cron job run started response
    // 2026-08-25: retired along with the rest of the cron IPC surface.

    /// Audit log query response (ADR-046). Returns up to `limit`
    /// audit events (newest first) that match the optional filters
    /// passed to the corresponding `AuditQuery` request. Events
    /// come from the in-memory ring buffer; the CLI's `peko audit
    /// tail` reads the JSONL file directly for cross-session
    /// history.
    #[serde(rename = "audit_events")]
    AuditEvents {
        request_id: u64,
        /// Newest-first list of audit events matching the query.
        /// `AuditEvent` is re-exported here (not imported) so the
        /// wire shape is owned by this file and the CLI / desktop
        /// can deserialize without pulling in the observability
        /// crate directly. Field shape matches `peko-observability`'s
        /// `AuditEvent` — see `audit_event_wire_schema_v1` in
        /// the daemon test suite for the canonical contract.
        entries: Vec<peko_observability::AuditEvent>,
    },

    /// Agent list response — retired in the principal-as-single-actor
    /// migration (audit C1). Replaced by `PrincipalList` below.

    /// Principal list response — the post-migration actor surface.
    /// Replaces the legacy `AgentList` response shape; see audit C1.
    #[serde(rename = "principal_list")]
    PrincipalList {
        request_id: u64,
        principals: Vec<crate::principal::PrincipalSummary>,
    },

    /// Principal get response — single Principal summary by name.
    #[serde(rename = "principal_get")]
    PrincipalGet {
        request_id: u64,
        principal: Option<crate::principal::PrincipalSummary>,
    },

    /// Result of `PrincipalCreate`. Returns the new principal's
    /// summary so the caller can render it without a follow-up
    /// `PrincipalList`. Past-tense pairing with `PrincipalCreate`.
    #[serde(rename = "principal_created")]
    PrincipalCreated {
        request_id: u64,
        principal: crate::principal::PrincipalSummary,
    },

    /// Result of `PrincipalUpdate`. Echoes the updated principal's
    /// summary so the caller can refresh its local state without a
    /// follow-up `PrincipalGet`.
    #[serde(rename = "principal_updated")]
    PrincipalUpdated {
        request_id: u64,
        principal: crate::principal::PrincipalSummary,
    },

    /// Result of `PrincipalRemove`. `removed` is `true` when the
    /// principal existed and was deleted; idempotent removes return
    /// `false`.
    #[serde(rename = "principal_removed")]
    PrincipalRemoved {
        request_id: u64,
        name: String,
        removed: bool,
    },

    /// Reply to [`RequestPacket::PersonaDraft`]. `content` carries
    /// the LLM's raw text. The handler instructs the model to emit a
    /// JSON object matching the persona schema; the CLI parses it.
    /// If the model returned non-JSON, `parse_ok` is `false` and
    /// `content` is the raw prose for fallback rendering.
    #[serde(rename = "persona_drafted")]
    PersonaDrafted {
        request_id: u64,
        content: String,
        parse_ok: bool,
    },

    /// Result of `CredentialList`. One row per provider id that the
    /// vault knows about, regardless of whether a key is currently
    /// stored — the desktop paints "Key set" vs "No key" from
    /// `has_key`. Past-tense pairing with `CredentialList`.
    ///
    /// Field names mirror the desktop's `CredentialRow`
    /// (`peko-desktop/src-tauri/src/commands/settings.rs:287`) so
    /// the Tauri command's projection is a no-op rename.
    #[serde(rename = "credentials_listed")]
    CredentialsListed {
        request_id: u64,
        providers: Vec<CredentialRow>,
    },

    /// Reply to [`RequestPacket::ModelTest`]. Carries the
    /// structured outcome so the UI can render latency + reason
    /// without re-parsing strings. `tested_at` is an ISO-8601 UTC
    /// stamp the validator computes at response-build time so
    /// callers don't have to read the daemon's wall clock.
    #[serde(rename = "model_tested")]
    ModelTested {
        request_id: u64,
        id: String,
        ok: bool,
        message: String,
        latency_ms: u32,
        http_status: Option<u16>,
        model_used: Option<String>,
        tested_at: String,
    },

    /// Reply to [`RequestPacket::CredentialSet`]. The vault write
    /// has already succeeded (or surfaced an error via
    /// [`ResponsePacket::Error`]) by the time this is sent. The
    /// `id` echo lets the desktop update its local UI without
    /// re-issuing a `credential_list` round-trip.
    ///
    /// `rewired_models` (PR 3 / `feature/model-first-config`):
    /// count of catalog entries that were rebound from the
    /// previous credential id (passed via
    /// `CredentialSet::replace_on`) to this new id. Zero on a plain
    /// set; the CLI's `--replace-on` flow surfaces this count so
    /// the user sees "Rewired N models: …" without a follow-up
    /// `model list` round-trip.
    #[serde(rename = "credential_set_done")]
    CredentialSetDone {
        request_id: u64,
        id: String,
        #[serde(default)]
        rewired_models: u32,
    },

    /// Reply to [`RequestPacket::CredentialDelete`]. See
    /// [`ResponsePacket::CredentialSetDone`] for the same notes on
    /// the success/error split.
    ///
    /// `broken_references` (PR 3 / `feature/model-first-config`):
    /// count of catalog entries that pointed at this credential
    /// and were detached (`credential_id = null`) before the
    /// delete. Zero on a normal delete; non-zero on a `--force`
    /// delete. The CLI surfaces this count so the user sees
    /// "Removed credential. Detached N model(s)." without a
    /// follow-up `model list` round-trip.
    #[serde(rename = "credential_deleted")]
    CredentialDeleted {
        request_id: u64,
        id: String,
        #[serde(default)]
        broken_references: u32,
    },

    /// Reply to [`RequestPacket::CredentialGet`]. Carries the full
    /// record (id, namespace, name, kind, metadata, timestamps)
    /// but never the secret material.
    #[serde(rename = "credential_got")]
    CredentialGot {
        request_id: u64,
        credential: Credential,
    },

    /// Reply to [`RequestPacket::CredentialGetMaterial`]. The only
    /// IPC path that returns the secret material. Audit-logged at
    /// INFO with the caller's reason and the credential id.
    #[serde(rename = "credential_material")]
    CredentialMaterial {
        request_id: u64,
        id: String,
        material: String,
    },

    /// Reply to [`RequestPacket::BindingList`] and
    /// [`RequestPacket::BindingGet`]. Carries the binding map; for
    /// `BindingList` `bindings` is the full map, for `BindingGet`
    /// it's a one-element map or empty when no binding exists.
    #[serde(rename = "bindings_listed")]
    BindingsListed {
        request_id: u64,
        bindings: Vec<RotationBindingWire>,
    },

    /// Reply to [`RequestPacket::BindingSet`]. The vault write has
    /// already succeeded by the time this is sent.
    #[serde(rename = "binding_set_done")]
    BindingSetDone { request_id: u64, key: String },

    /// Reply to [`RequestPacket::BindingDelete`].
    #[serde(rename = "binding_deleted")]
    BindingDeleted { request_id: u64, key: String },

    /// System status response
    #[serde(rename = "system_status")]
    SystemStatus {
        request_id: u64,
        version: String,
        uptime_secs: u64,
        degraded: bool,
        instance_count: u64,
        ready: bool,
    },

    /// System doctor response
    #[serde(rename = "system_doctor")]
    SystemDoctor {
        request_id: u64,
        checks: Vec<DoctorCheck>,
        passed: u32,
        failed: u32,
        warnings: u32,
    },

    /// Model list response
    #[serde(rename = "model_list")]
    ModelList {
        request_id: u64,
        models: Vec<ModelSummary>,
    },

    /// Model catalog reload response. Reports the post-reload entry
    /// counts so the CLI can confirm what was reloaded.
    #[serde(rename = "model_reloaded")]
    ModelReloaded {
        request_id: u64,
        models_count: usize,
        keys_count: usize,
    },

    /// Result of `ModelTemplates`. One row per built-in
    /// preset. The desktop uses this to populate the
    /// "Add Model" modal's preset picker; the picker is
    /// read-only at runtime, so we ship the whole list in one
    /// round-trip rather than paginating.
    #[serde(rename = "model_templates")]
    ModelTemplates {
        request_id: u64,
        presets: Vec<ModelPresetInfo>,
    },

    /// Result of `ModelAdd`. Returns the catalog-summary view
    /// (`ModelSummary`) of the newly-inserted entry so the desktop
    /// can refresh its model list without a follow-up list call.
    #[serde(rename = "model_added")]
    ModelAdded {
        request_id: u64,
        model: ModelSummary,
    },

    /// Result of `ModelUpdate`. Returns the catalog-summary view
    /// of the merged entry so the desktop can refresh the model
    /// list and the edit modal without a follow-up call.
    #[serde(rename = "model_updated")]
    ModelUpdated {
        request_id: u64,
        model: ModelSummary,
    },

    /// Result of `ModelRemove`. `removed` is `true` when an entry
    /// was actually deleted; idempotent removes return `false`.
    #[serde(rename = "model_removed")]
    ModelRemoved {
        request_id: u64,
        id: String,
        removed: bool,
    },

    /// MCP configuration reload response. Reports the post-reload server
    /// count so the CLI can confirm the daemon picked up the change.
    #[serde(rename = "mcp_reloaded")]
    McpReloaded {
        request_id: u64,
        servers_count: usize,
    },

    /// System clean response
    #[serde(rename = "system_cleaned")]
    SystemCleaned {
        request_id: u64,
        cleaned: Vec<String>,
        bytes_freed: u64,
    },

    // ── Runtime (ADR-032) ──
    #[serde(rename = "runtime_id")]
    RuntimeId { request_id: u64, did: String },
    #[serde(rename = "runtime_info")]
    RuntimeInfo {
        request_id: u64,
        metadata: RuntimeMetadataResponse,
    },
    #[serde(rename = "runtime_list")]
    RuntimeList {
        request_id: u64,
        runtimes: Vec<KnownRuntimeResponse>,
    },

    // ── Tunnel (ADR-035) ──
    #[serde(rename = "tunnel_status")]
    TunnelStatus {
        request_id: u64,
        configured: bool,
        daemon_running: bool,
        connected: bool,
    },

    /// Comprehensive daemon status payload (issue #8). Includes tunnel
    /// health snapshot suitable for `peko daemon status --json`.
    ///
    /// `mode` is added by the engine-adoption work (ADR-043): clients
    /// like `peko-desktop`'s SidecarSupervisor use it to detect when a
    /// foreign daemon (CLI-launched or another sidecar) is already
    /// holding the IPC socket, instead of trying to spawn a competing
    /// child. `#[serde(default)]` makes the field forward+backward
    /// compatible: old clients ignore it, old daemons omit it.
    #[serde(rename = "status")]
    Status {
        request_id: u64,
        uptime_secs: u64,
        version: String,
        tunnel_state: String,
        tunnel_reconnect_attempts: u32,
        tunnel_last_error: Option<String>,
        degraded: bool,
        #[serde(default)]
        mode: Option<crate::daemon::LaunchMode>,
    },

    // ── Auth management (ADR-034) ──
    #[serde(rename = "auth_api_key_created")]
    AuthApiKeyCreated {
        request_id: u64,
        key_id: String,
        full_key: String,
    },
    #[serde(rename = "auth_api_key_list")]
    AuthApiKeyList {
        request_id: u64,
        keys: Vec<ApiKeySummary>,
    },
    #[serde(rename = "auth_api_key_revoked")]
    AuthApiKeyRevoked { request_id: u64, key_id: String },
    #[serde(rename = "auth_status")]
    AuthStatus {
        request_id: u64,
        local_trust_enabled: bool,
        pekohub_jwt_enabled: bool,
        api_key_enabled: bool,
        api_key_count: usize,
    },

    // ── Principal operations ─────────────────────────────────────────
    /// Non-streaming result of `PrincipalSend`. Single packet with the
    /// root agent's final answer.
    #[serde(rename = "principal_sent")]
    PrincipalSent { request_id: u64, content: String },

    /// Streaming chunk of a `PrincipalSendStream` response. The daemon
    /// emits zero or more of these as the root agent agent produces
    /// assistant text. The frontend appends each `delta` to the
    /// in-flight assistant message.
    #[serde(rename = "principal_sent_chunk")]
    PrincipalSentChunk { request_id: u64, delta: String },

    /// Content-free agentic-iteration boundary marker for a
    /// `PrincipalSendStream` response. Emitted once at the start of each
    /// agentic loop iteration (when the runtime observes a
    /// `Lifecycle{Running}` event). Clients use it to break assistant
    /// text into one bubble per iteration and to show a "working"
    /// indicator while awaiting the next iteration's first token
    /// (e.g. during a tool call between iterations). `iteration` is
    /// 1-based. Tool-call / thinking detail is intentionally NOT sent —
    /// it stays backend-only in the session log.
    #[serde(rename = "principal_sent_iteration")]
    PrincipalSentIteration { request_id: u64, iteration: u32 },

    /// Final packet of a `PrincipalSendStream` response. Carries the
    /// full final answer (same content the non-streaming `PrincipalSent`
    /// would have returned) so the frontend can confirm the response
    /// and persist it. Always followed by a `Done` packet.
    #[serde(rename = "principal_sent_done")]
    PrincipalSentDone { request_id: u64, content: String },

    /// Run-summary packet emitted by the daemon at the end of a
    /// principal-send run. Aggregates the run's tool-call errors and
    /// cumulative token usage so CLI `--no-stream` (and other thin
    /// consumers that don't persist the session JSONL) can surface
    /// these facts to the user.
    ///
    /// Always emitted between `PrincipalSent*` / `PrincipalSentDone` /
    /// `PrincipalSent` and the final `Done` packet on both Streaming
    /// and OneShot response kinds. Consumers that don't know about
    /// this variant should treat it as opaque (their `_ => {}`
    /// fallthrough already swallows it). All fields are
    /// `#[serde(default)]` so old CLIs tolerate a daemon that emits
    /// them and old daemons don't crash when deserializing CLI
    /// commands that include them in the future.
    ///
    /// `iterations` is the count of `Lifecycle{Running}` events
    /// observed by the daemon — i.e. the number of agentic-loop turns
    /// the LLM ran. `usage` is the cumulative token usage across all
    /// turns (peko-engine emits one `AgenticEvent::Usage` per run,
    /// immediately before `Lifecycle{End}`). `tool_errors` records
    /// every `ToolEnd { success: false }` event seen during the run,
    /// correlated against the most recent `ToolStart` for the same
    /// `tool_id` so the user sees `"<tool_name>: <error_msg>"`.
    #[serde(rename = "run_summary")]
    RunSummary {
        request_id: u64,
        #[serde(default)]
        iterations: u32,
        #[serde(default)]
        usage: Option<RunUsageSummary>,
        #[serde(default)]
        tool_errors: Vec<ToolErrorEntry>,
    },

    /// Successor packet emitted when a steering message was queued
    /// during the final-iteration drain of `PrincipalSendStream` and
    /// the predecessor's run has now completed. The runtime drained
    /// the pending steering, started a fresh run for it, and is now
    /// delivering the answer. `predecessor_request_id` is the
    /// original `request_id` of the streamed send; `request_id` is the
    /// fresh id of the successor run (returned so the client can
    /// correlate if it wants to steer the new run). Multiple
    /// `PrincipalSentSuccessor` packets may follow the predecessor's
    /// `PrincipalSentDone`; each carries the full content of one
    /// drained steering item. The original `Done { request_id:
    /// predecessor_request_id, success: true }` closes the original
    /// stream after the successors have been delivered.
    #[serde(rename = "principal_sent_successor")]
    PrincipalSentSuccessor {
        predecessor_request_id: u64,
        request_id: u64,
        content: String,
    },

    /// Response to a `PrincipalLog` request. Returns one bounded page of
    /// the peer's DM-channel conversation for `(principal_did, peer)` —
    /// `messages` ordered oldest-to-newest, `next_cursor` opaque
    /// for paging older pages, and `has_more` true when more pages
    /// exist. Pre-launch clean cutover: no `session_id`, no
    /// `HistoryEvent`s, no session-internal vocabulary. Errors emit
    /// `Error { code, message }` with `code` in
    /// `"not_found" | "forbidden" | "bad_cursor" | "internal_error"`.
    #[serde(rename = "principal_log")]
    PrincipalLog {
        request_id: u64,
        name: String,
        peer: peko_auth::Subject,
        messages: Vec<PrincipalLogMessage>,
        next_cursor: Option<String>,
        has_more: bool,
    },

    /// One message on a `PrincipalLogWatch` stream — a replayed or
    /// freshly posted row of the peer's DM channel, in the same shape
    /// `PrincipalLog` pages carry. Heartbeats ride the same stream as
    /// `Heartbeat` packets; the stream has no terminal `Done`.
    #[serde(rename = "principal_log_appended")]
    PrincipalLogAppended {
        request_id: u64,
        message: PrincipalLogMessage,
    },

    #[serde(rename = "principal_exported")]
    PrincipalExported {
        request_id: u64,
        name: String,
        output_path: String,
    },

    #[serde(rename = "principal_imported")]
    PrincipalImported {
        request_id: u64,
        name: String,
        config_path: String,
    },

    /// Result of previewing a `.principal` package before import.
    #[serde(rename = "principal_import_previewed")]
    PrincipalImportPreviewed {
        request_id: u64,
        name: String,
        version: String,
        did: String,
        description: Option<String>,
        agents: Vec<String>,
        extensions: Vec<String>,
        /// Capabilities required by the bundled extensions. Old daemons that
        /// omit this field deserialize to an empty list.
        #[serde(default)]
        required_capabilities: Vec<String>,
        signed: bool,
        validation_errors: Vec<String>,
        validation_warnings: Vec<String>,
    },

    /// Result of previewing a remote Principal package before pulling it.
    #[serde(rename = "principal_pull_previewed")]
    PrincipalPullPreviewed {
        request_id: u64,
        name: String,
        version: String,
        did: String,
        description: Option<String>,
        agents: Vec<String>,
        extensions: Vec<String>,
        /// Capabilities required by the bundled extensions.
        #[serde(default)]
        required_capabilities: Vec<String>,
        signed: bool,
        validation_errors: Vec<String>,
        validation_warnings: Vec<String>,
    },

    #[serde(rename = "principal_pushed")]
    PrincipalPushed {
        request_id: u64,
        name: String,
        digest: String,
    },

    #[serde(rename = "principal_pulled")]
    PrincipalPulled {
        request_id: u64,
        name: String,
        version: String,
        digest: String,
    },

    #[serde(rename = "principal_permission_granted")]
    PrincipalPermissionGranted {
        request_id: u64,
        name: String,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    },

    #[serde(rename = "principal_permission_revoked")]
    PrincipalPermissionRevoked {
        request_id: u64,
        name: String,
        subject: peko_auth::Subject,
        permission: peko_auth::ownership::Permission,
    },

    #[serde(rename = "principal_permissions")]
    PrincipalPermissions {
        request_id: u64,
        permissions: Vec<peko_auth::ownership::PermissionGrant>,
    },

    /// Result of `PrincipalSetStatus`. Echoes the persisted status so
    /// callers can confirm the daemon applied the change.
    #[serde(rename = "principal_status_updated")]
    PrincipalStatusUpdated {
        request_id: u64,
        name: String,
        status: String,
    },

    /// Result of `PrincipalSetExposure`. Echoes the persisted exposure.
    #[serde(rename = "principal_exposure_updated")]
    PrincipalExposureUpdated {
        request_id: u64,
        name: String,
        exposure: String,
    },

    /// PR #11: result of `PrincipalMintInvite`. Returns the wire
    /// token (claims + signature), the structured claims, and the
    /// share URL the caller can hand to the recipient. The CLI
    /// prints just the URL; the desktop renders it as a copy-paste
    /// box plus a Burn button.
    #[serde(rename = "principal_invite_minted")]
    PrincipalInviteMinted {
        request_id: u64,
        name: String,
        token: String,
        url: String,
        claims: crate::tunnel::InviteClaims,
    },

    /// PR #11: result of `PrincipalRevokeInvite`. Confirms the
    /// `jti` was added to the in-memory revocation set; the next
    /// inbound request presenting that token will be rejected.
    #[serde(rename = "principal_invite_revoked")]
    PrincipalInviteRevoked {
        request_id: u64,
        name: String,
        jti: String,
    },
    // (Session-inbox steering variants — MessageQueued, PendingMessages,
    // MessageCancelled, SteeringMessageSummary — were retired under
    // ADR-042. External steering of an in-flight session is no longer
    // reachable from the IPC surface; if a future ADR reintroduces it,
    // it must key off PrincipalMemory rather than legacy
    // SessionService.)
}

/// Summary of an extension for IPC responses
/// Catalog-summary view of one configured model entry.
///
/// This is the canonical list-row shape for the model-first catalog
/// (`models.toml`). The wire field for the API format stays the short
/// form (`"openai"` / `"anthropic"`) so the desktop's existing
/// rendering code keeps working without a coord change; the runtime
/// translates to/from the catalog's `ApiFormat`
/// (`openai_completions` / `anthropic_messages`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    pub id: String,
    pub display_name: String,
    /// Template/preset id this entry was seeded from. `None` for
    /// fully custom entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Short wire id: `"openai"` or `"anthropic"`. The desktop's
    /// existing renderer reads this field, so the on-wire value stays
    /// stable; the runtime translates to/from the catalog's
    /// `ApiFormat` (`openai_completions` / `anthropic_messages`).
    #[serde(rename = "api_format")]
    pub api_type: String,
    /// Base URL configured for this model. Empty for presets
    /// where the user must supply a deployment URL (e.g.
    /// `azure-openai`).
    pub base_url: String,
    /// Model id as it appears on the wire (e.g. `gpt-4o`,
    /// `claude-sonnet-4-5`).
    pub model_id: String,
    /// Maximum context length in tokens (input + output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Maximum output tokens for a single response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Optional extra HTTP headers (e.g. `OpenAI-Organization`).
    /// Empty for most entries; non-empty for vendors that require a
    /// tenant header.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Reference to a credential in the vault. `None` means the model
    /// does not require an API key (e.g. a local Ollama endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    pub requires_key: bool,
    /// True iff the catalog entry has `requires_key = false` (local
    /// model like Ollama). Surfaced to the desktop so it can hide
    /// the "Add Key" CTA.
    pub is_local: bool,
    /// Catalog `enabled` flag. Disabled entries still appear in the
    /// list so the desktop can render them greyed-out / at the bottom
    /// of the models panel.
    pub enabled: bool,
    /// PR 1 / `feature/model-first-config`: declarative capability
    /// descriptor (vision, audio, tools, streaming, thinking,
    /// json_mode, pricing). `None` for entries written before
    /// PR 1; the desktop falls back to `ModelSpec::default()` in
    /// that case (text-only, no tools, no thinking, streaming on).
    /// Field is skipped from JSON when absent so old daemons and
    /// old desktop builds keep working against new packets, and
    /// vice versa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<ModelSpec>,
    /// Phase 2 of `feature/multi-model-subagents`: free-text
    /// user note attached to this entry. Surfaced on the IPC
    /// `model.list` / `model.show` replies and (via the
    /// `model_list` builtin) to the parent agent so it can pick
    /// models using both `spec` flags and subjective annotations.
    /// `None` for entries written before Phase 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// PR 1 / `feature/model-first-config`: IPC mirror of
/// `peko_providers::spec::ModelSpec`. Kept in this file (rather than
/// imported from `peko_providers`) so the wire shape is owned by the
/// IPC layer and can evolve independently. All fields use
/// `#[serde(default)]` so packets emitted by a pre-PR-1 daemon
/// deserialize cleanly into the new field set.
///
/// The mirror is intentionally one-way: the daemon reads `ModelSpec`
/// from `ModelConfig::spec` and projects it onto this struct. The
/// desktop never round-trips a `ModelSpec` back through IPC — edits
/// go through the catalog file (`peko model edit` / `peko model add`)
/// rather than the IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub audio_input: bool,
    #[serde(default)]
    pub tool_support: ModelToolSupport,
    #[serde(default = "default_streaming_true")]
    pub streaming: bool,
    #[serde(default)]
    pub thinking: ModelThinkingMode,
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricingHint>,
}

fn default_streaming_true() -> bool {
    true
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self {
            image_input: false,
            audio_input: false,
            tool_support: ModelToolSupport::None,
            streaming: true,
            thinking: ModelThinkingMode::Disabled,
            json_mode: false,
            pricing: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelToolSupport {
    None,
    FunctionCalling,
    Full,
}

impl Default for ModelToolSupport {
    fn default() -> Self {
        ModelToolSupport::None
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelThinkingMode {
    Disabled,
    Optional,
    Required,
    CustomBudget,
}

impl Default for ModelThinkingMode {
    fn default() -> Self {
        ModelThinkingMode::Disabled
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricingHint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million: Option<f64>,
}

/// One model declared by a built-in model preset.
///
/// This is the IPC mirror of `providers::templates::ModelTemplate` —
/// a smaller, owned, serializable shape suitable for the desktop's
/// "Add Model" modal. The static `&'static str` slices from the
/// in-runtime template are projected into owned `String`s /
/// optional `u32`s so the struct can be sent over the wire without a
/// lifetime. `headers` from the in-runtime template are intentionally
/// omitted — the modal doesn't need them, and the catalog entry the
/// user creates from a preset starts with the preset's defaults intact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTemplateInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// One built-in model preset, projected from the in-runtime
/// `BUILT_IN_TEMPLATES` array into an owned, serializable shape for
/// the desktop's "Add Model" modal. The wire shape is intentionally
/// richer than `ModelSummary` (which is the catalog-summary view) so
/// the picker can show the curated model list and context length —
/// the choices that actually drive a one-screen decision.
///
/// Field names are snake_case to match the rest of the IPC envelope;
/// the Tauri command projects this into the camelCase TS surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPresetInfo {
    pub id: String,
    pub display_name: String,
    /// `"openai"` or `"anthropic"` — matches `ModelSummary::api_type`
    /// and the underlying `ApiFormat` enum's snake-case wire ids.
    pub api_type: String,
    /// Base URL. Empty string for presets where the user must
    /// supply a deployment URL (e.g. `azure-openai`).
    pub base_url: String,
    pub requires_key: bool,
    pub default_model: String,
    pub models: Vec<ModelTemplateInfo>,
}

/// Arguments for `RequestPacket::ModelAdd`.
///
/// This mirrors the CLI's `model add` args so the desktop
/// modal can drive exactly the same surface that
/// `peko model add` exposes. `template` and `custom` are
/// mutually exclusive; the handler refuses bare invocations the
/// same way the CLI does (per the F6/F7 symmetry rule — the
/// "either --template or --custom is required" guard stays in
/// both the CLI and the IPC so the two surfaces never disagree).
///
/// `key` is best-effort: if the user supplies it, the handler folds
/// it into the same vault write the CLI would do.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelAddArgs {
    /// Seed from a built-in preset (e.g. `"anthropic"`,
    /// `"openai"`, `"ollama"`). Mutually exclusive with `custom`.
    #[serde(default)]
    pub template: Option<String>,
    /// Override the catalog id (preset or custom). Defaults to
    /// the preset id when omitted for a preset-mode add.
    #[serde(default)]
    pub name: Option<String>,
    /// Override the catalog display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Add a fully custom (OpenAI-compatible or Anthropic-
    /// compatible) model. Mutually exclusive with `template`.
    #[serde(default)]
    pub custom: bool,
    /// API format for a custom model. One of
    /// `"openai_completions"` | `"anthropic_messages"`. Maps to
    /// `ApiFormat::from_wire`.
    #[serde(default)]
    pub api_format: Option<String>,
    /// Base URL for a custom model.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Whether the custom model requires an API key.
    /// Defaults to `true` when omitted.
    #[serde(default)]
    pub requires_key: Option<bool>,
    /// One or more wire model ids to declare. The first becomes the
    /// entry's `model_id`. The CLI accepts the same vector and uses
    /// the same defaulting rule.
    #[serde(default)]
    pub model: Vec<String>,
    /// Store an API key in the vault immediately. Equivalent to
    /// `peko credential set <id>` after the add. Ignored when
    /// the new entry does not require a key.
    #[serde(default)]
    pub key: Option<String>,
    /// Reference an existing vault credential by id instead of
    /// storing a new key. Mutually exclusive with `key`.
    #[serde(default)]
    pub credential_id: Option<String>,
}

/// Arguments for `RequestPacket::ModelUpdate`.
///
/// Every field except `id` is optional. Omitted fields leave the
/// existing catalog entry untouched; supplied fields replace the
/// current value. The daemon rewrites `models.toml` atomically.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelUpdateArgs {
    /// Catalog id of the entry to edit.
    pub id: String,
    /// Replace the display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Replace the API format. One of `"openai_completions"` |
    /// `"anthropic_messages"`; maps through `ApiFormat::from_wire`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_format: Option<String>,
    /// Replace the base URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Replace the wire model id (e.g. `gpt-4o`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Replace the context-window size (tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Replace the max output tokens for a single response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Replace the extra HTTP headers map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Replace the credential reference (vault credential id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// Replace the `requires_key` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_key: Option<bool>,
    /// Replace the `enabled` flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Structured outcome of a live model ping. Mirrors
/// `providers::validator::CredentialTestOutcome` in an owned,
/// serializable shape suitable for the `ModelTested` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_used: Option<String>,
}

/// One row of `CredentialsListed`. Redacted — never carries the
/// secret material. The full record (including metadata) is fetched
/// via `CredentialGet`; the material itself is only available via
/// `CredentialGetMaterial` (RP3A: audit-logged).
///
/// `id` is the credential's UUID. `namespace` and `name` together
/// identify the slot (`provider:openai / default`,
/// `mcp:analytics / default`, `oauth:myremote / default`, …).
/// `kind` is the lowercase snake_case spelling of
/// [`crate::common::vault::CredentialKind`] (`api_key`,
/// `bearer_token`, `oauth_token`, `basic_auth`, `private_key`,
/// `generic_secret`).
///
/// `last_tested_at` is an ISO-8601 UTC stamp from the most recent
/// `CredentialTest` against this credential; `last_tested_ok`
/// records the outcome. Both are `None` until the first test runs.
///
/// `is_referenced` / `referenced_by` (PR 3 / `feature/model-first-config`):
/// populated when the catalog has at least one `ModelConfig` whose
/// `credential_id` matches this row. `is_referenced` is the cheap
/// summary flag the desktop paints next to the credential name;
/// `referenced_by` carries the dependent `ModelSummary` rows so the
/// delete confirmation dialog can show jump links without a follow-up
/// `model list` round-trip. `#[serde(default)]` keeps the field
/// forward+backward compatible — old daemons that don't compute
/// references deserialize as `false` / `[]`, and old clients ignore
/// the fields on a new daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialRow {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub kind: String,
    pub has_key: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_ok: Option<bool>,
    #[serde(default)]
    pub system_owned: bool,
    #[serde(default)]
    pub is_referenced: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub referenced_by: Vec<ModelSummary>,
}

/// Full credential record returned by `CredentialGet`. Includes
/// metadata but NOT the secret material — use
/// [`ResponsePacket::CredentialMaterial`] for that.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub kind: String,
    #[serde(default = "serde_json::Value::default")]
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_tested_ok: Option<bool>,
    #[serde(default)]
    pub system_owned: bool,
}

/// Rotation binding wire shape. Carries the slot key (the map key
/// itself), strategy name, and ordered list of credential ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationBindingWire {
    pub key: String,
    pub strategy: String,
    pub order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub ext_type: String,
    pub version: String,
    pub source: String, // "built-in" or "installed"
    pub enabled: bool,
    pub runtime: String, // "running", "stopped", or "n/a"
    pub description: String,
    /// Capabilities this extension declares it provides (e.g. `tool:Read`).
    pub provides: Vec<String>,
    /// Capabilities this extension requires to function.
    pub requires: Vec<String>,
}

/// A single doctor check result
/// Runtime metadata response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeMetadataResponse {
    pub runtime_id: String,
    pub display_name: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub host_info: HostInfoResponse,
}

/// Host information response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfoResponse {
    pub os: String,
    pub arch: String,
    pub hostname: String,
}

/// Known runtime response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownRuntimeResponse {
    pub runtime_id: String,
    pub display_name: String,
    pub last_seen: Option<String>,
    pub connection_endpoint: Option<String>,
    pub trust_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub message: String,
    pub suggestion: Option<String>,
}

/// API key summary for IPC responses (ADR-034)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub scopes: Vec<String>,
    pub enabled: bool,
}

impl AuthenticatedRequest {
    /// Deserialize an authenticated request from JSON bytes.
    ///
    /// First tries to parse as `AuthenticatedRequest` (with auth envelope).
    /// If that fails, falls back to plain `RequestPacket` with `AuthCredential::None`.
    ///
    /// # Errors
    /// Returns error if deserialization fails for both formats.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        // Try the new format first
        if let Ok(envelope) = serde_json::from_slice::<Self>(bytes) {
            return Ok(envelope);
        }
        // Fall back to plain RequestPacket (old clients)
        let packet = serde_json::from_slice::<RequestPacket>(bytes)?;
        Ok(Self {
            auth: AuthHeader::default(),
            packet,
        })
    }

    /// Get the request ID from the inner packet
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.packet.request_id()
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }
}

impl ResponsePacket {
    /// Get the request ID from any variant
    #[must_use]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Text { request_id, .. }
            | Self::AsyncReceipt { request_id, .. }
            | Self::Done { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Pong { request_id, .. }
            | Self::Heartbeat { request_id }
            | Self::ShuttingDown { request_id }
            | Self::AuditEvents { request_id, .. }
            | Self::PrincipalList { request_id, .. }
            | Self::PrincipalGet { request_id, .. }
            | Self::PrincipalCreated { request_id, .. }
            | Self::PrincipalUpdated { request_id, .. }
            | Self::PrincipalRemoved { request_id, .. }
            | Self::PersonaDrafted { request_id, .. }
            | Self::SystemStatus { request_id, .. }
            | Self::SystemDoctor { request_id, .. }
            | Self::ModelList { request_id, .. }
            | Self::ModelReloaded { request_id, .. }
            | Self::ModelTemplates { request_id, .. }
            | Self::ModelAdded { request_id, .. }
            | Self::ModelUpdated { request_id, .. }
            | Self::ModelRemoved { request_id, .. }
            | Self::ModelTested { request_id, .. }
            | Self::McpReloaded { request_id, .. }
            | Self::CredentialsListed { request_id, .. }
            | Self::CredentialSetDone { request_id, .. }
            | Self::CredentialDeleted { request_id, .. }
            | Self::CredentialGot { request_id, .. }
            | Self::CredentialMaterial { request_id, .. }
            | Self::BindingsListed { request_id, .. }
            | Self::BindingSetDone { request_id, .. }
            | Self::BindingDeleted { request_id, .. }
            | Self::SystemCleaned { request_id, .. }
            | Self::RuntimeId { request_id, .. }
            | Self::RuntimeInfo { request_id, .. }
            | Self::RuntimeList { request_id, .. }
            | Self::AuthApiKeyCreated { request_id, .. }
            | Self::AuthApiKeyList { request_id, .. }
            | Self::AuthApiKeyRevoked { request_id, .. }
            | Self::AuthStatus { request_id, .. }
            | Self::PrincipalSent { request_id, .. }
            | Self::PrincipalSentChunk { request_id, .. }
            | Self::PrincipalSentIteration { request_id, .. }
            | Self::PrincipalSentDone { request_id, .. }
            | Self::PrincipalSentSuccessor { request_id, .. }
            | Self::RunSummary { request_id, .. }
            | Self::PrincipalLog { request_id, .. }
            | Self::PrincipalLogAppended { request_id, .. }
            | Self::PrincipalExported { request_id, .. }
            | Self::PrincipalImported { request_id, .. }
            | Self::PrincipalImportPreviewed { request_id, .. }
            | Self::PrincipalPullPreviewed { request_id, .. }
            | Self::PrincipalPushed { request_id, .. }
            | Self::PrincipalPulled { request_id, .. }
            | Self::PrincipalPermissionGranted { request_id, .. }
            | Self::PrincipalPermissionRevoked { request_id, .. }
            | Self::PrincipalPermissions { request_id, .. }
            | Self::PrincipalStatusUpdated { request_id, .. }
            | Self::PrincipalExposureUpdated { request_id, .. }
            | Self::TunnelStatus { request_id, .. }
            | Self::Status { request_id, .. }
            | Self::QuotaStatus { request_id, .. }
            | Self::PrincipalInviteMinted { request_id, .. }
            | Self::PrincipalInviteRevoked { request_id, .. }
            | Self::ChannelCreated { request_id, .. }
            | Self::ChannelInvited { request_id, .. }
            | Self::ChannelPosted { request_id, .. }
            | Self::ChannelPeekResult { request_id, .. }
            | Self::ChannelEventReceived { request_id, .. }
            | Self::ChannelMembersResult { request_id, .. }
            | Self::ChannelListResult { request_id, .. }
            | Self::ChannelLeft { request_id, .. }
            | Self::ChannelPinnedToShared { request_id, .. } => *request_id,
        }
    }

    /// Get the variant name without payload data.
    #[must_use]
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "Text",
            Self::AsyncReceipt { .. } => "AsyncReceipt",
            Self::Done { .. } => "Done",
            Self::Error { .. } => "Error",
            Self::Pong { .. } => "Pong",
            Self::Heartbeat { .. } => "Heartbeat",
            Self::ShuttingDown { .. } => "ShuttingDown",
            Self::AuditEvents { .. } => "AuditEvents",
            Self::PrincipalList { .. } => "PrincipalList",
            Self::PrincipalGet { .. } => "PrincipalGet",
            Self::PrincipalCreated { .. } => "PrincipalCreated",
            Self::PrincipalUpdated { .. } => "PrincipalUpdated",
            Self::PrincipalRemoved { .. } => "PrincipalRemoved",
            Self::PersonaDrafted { .. } => "PersonaDrafted",
            Self::SystemStatus { .. } => "SystemStatus",
            Self::SystemDoctor { .. } => "SystemDoctor",
            Self::ModelList { .. } => "ModelList",
            Self::ModelReloaded { .. } => "ModelReloaded",
            Self::ModelTemplates { .. } => "ModelTemplates",
            Self::ModelAdded { .. } => "ModelAdded",
            Self::ModelUpdated { .. } => "ModelUpdated",
            Self::ModelRemoved { .. } => "ModelRemoved",
            Self::ModelTested { .. } => "ModelTested",
            Self::McpReloaded { .. } => "McpReloaded",
            Self::CredentialsListed { .. } => "CredentialsListed",
            Self::CredentialSetDone { .. } => "CredentialSetDone",
            Self::CredentialDeleted { .. } => "CredentialDeleted",
            Self::CredentialGot { .. } => "CredentialGot",
            Self::CredentialMaterial { .. } => "CredentialMaterial",
            Self::BindingsListed { .. } => "BindingsListed",
            Self::BindingSetDone { .. } => "BindingSetDone",
            Self::BindingDeleted { .. } => "BindingDeleted",
            Self::SystemCleaned { .. } => "SystemCleaned",
            Self::RuntimeId { .. } => "RuntimeId",
            Self::RuntimeInfo { .. } => "RuntimeInfo",
            Self::RuntimeList { .. } => "RuntimeList",
            Self::AuthApiKeyCreated { .. } => "AuthApiKeyCreated",
            Self::AuthApiKeyList { .. } => "AuthApiKeyList",
            Self::AuthApiKeyRevoked { .. } => "AuthApiKeyRevoked",
            Self::AuthStatus { .. } => "AuthStatus",
            Self::PrincipalSent { .. } => "PrincipalSent",
            Self::PrincipalSentChunk { .. } => "PrincipalSentChunk",
            Self::PrincipalSentIteration { .. } => "PrincipalSentIteration",
            Self::PrincipalSentDone { .. } => "PrincipalSentDone",
            Self::PrincipalSentSuccessor { .. } => "PrincipalSentSuccessor",
            Self::RunSummary { .. } => "RunSummary",
            Self::PrincipalLog { .. } => "PrincipalLog",
            Self::PrincipalLogAppended { .. } => "PrincipalLogAppended",
            Self::PrincipalExported { .. } => "PrincipalExported",
            Self::PrincipalImported { .. } => "PrincipalImported",
            Self::PrincipalImportPreviewed { .. } => "PrincipalImportPreviewed",
            Self::PrincipalPullPreviewed { .. } => "PrincipalPullPreviewed",
            Self::PrincipalPushed { .. } => "PrincipalPushed",
            Self::PrincipalPulled { .. } => "PrincipalPulled",
            Self::PrincipalPermissionGranted { .. } => "PrincipalPermissionGranted",
            Self::PrincipalPermissionRevoked { .. } => "PrincipalPermissionRevoked",
            Self::PrincipalStatusUpdated { .. } => "PrincipalStatusUpdated",
            Self::PrincipalExposureUpdated { .. } => "PrincipalExposureUpdated",
            Self::PrincipalPermissions { .. } => "PrincipalPermissions",
            Self::TunnelStatus { .. } => "TunnelStatus",
            Self::Status { .. } => "Status",
            Self::QuotaStatus { .. } => "QuotaStatus",
            Self::PrincipalInviteMinted { .. } => "PrincipalInviteMinted",
            Self::PrincipalInviteRevoked { .. } => "PrincipalInviteRevoked",
            Self::ChannelCreated { .. } => "ChannelCreated",
            Self::ChannelInvited { .. } => "ChannelInvited",
            Self::ChannelPosted { .. } => "ChannelPosted",
            Self::ChannelPeekResult { .. } => "ChannelPeekResult",
            Self::ChannelEventReceived { .. } => "ChannelEventReceived",
            Self::ChannelMembersResult { .. } => "ChannelMembersResult",
            Self::ChannelListResult { .. } => "ChannelListResult",
            Self::ChannelLeft { .. } => "ChannelLeft",
            Self::ChannelPinnedToShared { .. } => "ChannelPinnedToShared",
        }
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }

    /// Deserialize from JSON bytes
    ///
    /// # Errors
    /// Returns error if deserialization fails
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization_roundtrip() {
        // Replaced from the retired `RequestPacket::Execute` (audit C4).
        // Round-trip coverage now uses `PrincipalSend` so the test
        // exercises a real post-migration actor-shape envelope.
        let req = RequestPacket::PrincipalSend {
            request_id: 42,
            name: "helper".to_string(),
            message: "Hello".to_string(),
            user: "alice".to_string(),
            no_slash: true,
            output_format: OutputFormat::Json,
            override_model: Some("gpt-4o".to_string()),
        };

        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();

        match decoded {
            RequestPacket::PrincipalSend {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
                override_model,
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(name, "helper");
                assert_eq!(message, "Hello");
                assert_eq!(user, "alice");
                assert!(no_slash);
                assert_eq!(output_format, OutputFormat::Json);
                assert_eq!(override_model, Some("gpt-4o".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_stop_roundtrip() {
        // `peko stop <principal>` — owner thread (peer omitted).
        let req = RequestPacket::PrincipalStop {
            request_id: 1,
            name: "scout".to_string(),
            peer: None,
        };
        let bytes = req.to_bytes().unwrap();
        // The on-wire payload must be the snake_case `principal_stop`
        // variant so a pre-launch CLI never sends an unknown variant to
        // an older daemon.
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains("\"principal_stop\""),
            "expected `principal_stop` in serialized payload, got: {json}"
        );
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalStop {
                request_id,
                name,
                peer,
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(name, "scout");
                assert!(peer.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_stop_with_peer_roundtrip() {
        // `peko stop <principal> --peer user:alice` — owner stopping
        // another peer's thread.
        let req = RequestPacket::PrincipalStop {
            request_id: 2,
            name: "scout".to_string(),
            peer: Some(peko_auth::Subject::User("alice".to_string())),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalStop {
                request_id,
                name,
                peer,
            } => {
                assert_eq!(request_id, 2);
                assert_eq!(name, "scout");
                assert_eq!(peer, Some(peko_auth::Subject::User("alice".to_string())));
            }
            _ => panic!("Wrong variant"),
        }
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains(r#""peer":{"kind":"user","id":"alice"}"#),
            "expected peer subject wire form in payload, got: {json}"
        );
    }

    #[test]
    fn test_response_serialization_roundtrip() {
        let resp = ResponsePacket::Text {
            request_id: 42,
            seq: 7,
            chunk: "hello world".to_string(),
        };

        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();

        match decoded {
            ResponsePacket::Text {
                request_id,
                seq,
                chunk,
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(seq, 7);
                assert_eq!(chunk, "hello world");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `RunSummary` round-trips with full payload: iterations + usage +
    /// tool_errors all survive serialize → deserialize. Regression for
    /// the variant's `#[serde(default)]` decorations — if a field's
    /// `default` is dropped accidentally, this catches it.
    #[test]
    fn test_run_summary_serialization_roundtrip() {
        let resp = ResponsePacket::RunSummary {
            request_id: 7,
            iterations: 3,
            usage: Some(RunUsageSummary {
                prompt_tokens: 1234,
                completion_tokens: 567,
                total_tokens: 1801,
            }),
            tool_errors: vec![ToolErrorEntry {
                tool_id: "t1".into(),
                tool_name: Some("read_file".into()),
                error_message: "ENOENT".into(),
            }],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::RunSummary {
                request_id,
                iterations,
                usage,
                tool_errors,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(iterations, 3);
                let u = usage.expect("usage must round-trip");
                assert_eq!(u.prompt_tokens, 1234);
                assert_eq!(u.completion_tokens, 567);
                assert_eq!(u.total_tokens, 1801);
                assert_eq!(tool_errors.len(), 1);
                assert_eq!(tool_errors[0].tool_name.as_deref(), Some("read_file"));
                assert_eq!(tool_errors[0].error_message, "ENOENT");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Empty `RunSummary` (no usage, no errors) still round-trips
    /// cleanly. Mirrors a successful run with zero tool calls and no
    /// usage emitted (e.g. an immediate-error path).
    #[test]
    fn test_run_summary_empty_roundtrip() {
        let resp = ResponsePacket::RunSummary {
            request_id: 1,
            iterations: 0,
            usage: None,
            tool_errors: Vec::new(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::RunSummary {
                request_id,
                iterations,
                usage,
                tool_errors,
            } => {
                assert_eq!(request_id, 1);
                assert_eq!(iterations, 0);
                assert!(usage.is_none());
                assert!(tool_errors.is_empty());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `RunSummary` request_id is extracted via the same helper that
    /// every other variant uses. This is the compile-time enforcement
    /// that we didn't forget to add an arm to the exhaustive
    /// or-pattern in `request_id()`.
    #[test]
    fn test_run_summary_request_id_extraction() {
        let resp = ResponsePacket::RunSummary {
            request_id: 99,
            iterations: 1,
            usage: None,
            tool_errors: Vec::new(),
        };
        assert_eq!(resp.request_id(), 99);
        assert_eq!(resp.variant_name(), "RunSummary");
    }

    #[test]
    fn test_request_id_extraction() {
        let req = RequestPacket::Ping { request_id: 99 };
        assert_eq!(req.request_id(), 99);

        let resp = ResponsePacket::Pong {
            request_id: 99,
            uptime_secs: 10,
            version: "0.1.0".to_string(),
        };
        assert_eq!(resp.request_id(), 99);
    }

    #[test]
    fn test_packet_size_limit() {
        // Create a packet that exceeds the limit
        let huge_chunk = "x".repeat(MAX_PACKET_SIZE + 1000);
        let resp = ResponsePacket::Text {
            request_id: 1,
            seq: 0,
            chunk: huge_chunk,
        };
        assert!(resp.to_bytes().is_err());
    }

    #[test]
    fn test_principal_list_request_roundtrip() {
        let req = RequestPacket::PrincipalList { request_id: 300 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalList { request_id } => {
                assert_eq!(request_id, 300);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_get_request_roundtrip() {
        let req = RequestPacket::PrincipalGet {
            request_id: 301,
            name: "helper".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalGet { request_id, name } => {
                assert_eq!(request_id, 301);
                assert_eq!(name, "helper");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_list_response_roundtrip() {
        let resp = ResponsePacket::PrincipalList {
            request_id: 600,
            principals: vec![crate::principal::PrincipalSummary {
                name: "helper".to_string(),
                did: peko_subject::PrincipalDID("did:peko:local:helper".to_string()),
                owner: peko_auth::Subject::User("alice".to_string()),
                description: Some("test principal".to_string()),
                exposure: peko_auth::Exposure::default(),
                status: None,
                preferred_model_id: None,
                capabilities: crate::extensions::framework::types::Capabilities::default(),
                agent_prompt_count: 0,
                workspace_path: "/tmp/helper".to_string(),
            }],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalList {
                request_id,
                principals,
            } => {
                assert_eq!(request_id, 600);
                assert_eq!(principals.len(), 1);
                assert_eq!(principals[0].name, "helper");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_get_response_roundtrip() {
        let resp = ResponsePacket::PrincipalGet {
            request_id: 601,
            principal: Some(crate::principal::PrincipalSummary {
                name: "helper".to_string(),
                did: peko_subject::PrincipalDID("did:peko:local:helper".to_string()),
                owner: peko_auth::Subject::User("alice".to_string()),
                description: None,
                exposure: peko_auth::Exposure::default(),
                status: None,
                preferred_model_id: None,
                capabilities: crate::extensions::framework::types::Capabilities::default(),
                agent_prompt_count: 2,
                workspace_path: "/tmp/helper".to_string(),
            }),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalGet {
                request_id,
                principal,
            } => {
                assert_eq!(request_id, 601);
                let p = principal.expect("principal should be present");
                assert_eq!(p.name, "helper");
                assert_eq!(p.agent_prompt_count, 2);
            }
            _ => panic!("Wrong variant"),
        }

        // And the miss case — `principal: None` round-trips cleanly.
        let miss = ResponsePacket::PrincipalGet {
            request_id: 602,
            principal: None,
        };
        let bytes = miss.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalGet {
                request_id,
                principal,
            } => {
                assert_eq!(request_id, 602);
                assert!(principal.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_templates_request_roundtrip() {
        // Pin the request wire shape for the desktop's
        // "Add Model" modal's preset picker. The bare request
        // is just `{ type, request_id }` — no payload — but we round-
        // trip the envelope anyway so a future field addition
        // surfaces as a test diff.
        let req = RequestPacket::ModelTemplates { request_id: 911 };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_templates")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(911));

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ModelTemplates { request_id } => assert_eq!(request_id, 911),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_templates_response_roundtrip() {
        // Pin the response shape — the desktop's modal
        // picks up `presets[]` with the full model list and
        // context lengths. `headers` is intentionally omitted so the
        // modal only ships the fields it actually renders.
        let resp = ResponsePacket::ModelTemplates {
            request_id: 912,
            presets: vec![ModelPresetInfo {
                id: "anthropic".to_string(),
                display_name: "Anthropic".to_string(),
                api_type: "anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                requires_key: true,
                default_model: "claude-sonnet-4-5".to_string(),
                models: vec![ModelTemplateInfo {
                    id: "claude-sonnet-4-5".to_string(),
                    display_name: Some("Claude Sonnet 4.5".to_string()),
                    context_length: Some(200_000),
                    max_output_tokens: Some(8_192),
                }],
            }],
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_templates")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(912));

        let presets = json
            .get("presets")
            .and_then(|v| v.as_array())
            .expect("response should have a presets array");
        assert_eq!(presets.len(), 1);

        // Field names must match what the desktop's Tauri command
        // projection reads.
        let p = &presets[0];
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(
            p.get("display_name").and_then(|v| v.as_str()),
            Some("Anthropic")
        );
        assert_eq!(
            p.get("api_type").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            p.get("base_url").and_then(|v| v.as_str()),
            Some("https://api.anthropic.com")
        );
        assert_eq!(p.get("requires_key").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            p.get("default_model").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );

        let models = p
            .get("models")
            .and_then(|v| v.as_array())
            .expect("models array");
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert_eq!(
            m.get("id").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            m.get("context_length").and_then(|v| v.as_u64()),
            Some(200_000)
        );
        assert_eq!(
            m.get("max_output_tokens").and_then(|v| v.as_u64()),
            Some(8_192)
        );

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ModelTemplates {
                request_id,
                presets,
            } => {
                assert_eq!(request_id, 912);
                assert_eq!(presets.len(), 1);
                assert_eq!(presets[0].id, "anthropic");
                assert_eq!(presets[0].models[0].context_length, Some(200_000));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_add_request_roundtrip() {
        // Pin the request shape for `peko model add` over
        // IPC. All fields are Option / Vec / bool with #[serde(default)]
        // so a bare request (preset mode, no overrides) round-trips
        // without losing defaulting. The handler treats a bare request
        // (no `template`, no `custom`) as an error — same guard as the
        // CLI — but the wire shape is defined either way.
        let req = RequestPacket::ModelAdd {
            request_id: 913,
            args: ModelAddArgs {
                template: Some("anthropic".to_string()),
                name: None,
                display_name: None,
                custom: false,
                api_format: None,
                base_url: None,
                requires_key: None,
                model: vec![],
                key: Some("sk-test".to_string()),
                credential_id: None,
            },
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("model_add"));
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(913));

        let args = json
            .get("args")
            .expect("response should have an args object");
        assert_eq!(
            args.get("template").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(args.get("custom").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(args.get("key").and_then(|v| v.as_str()), Some("sk-test"));

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ModelAdd { request_id, args } => {
                assert_eq!(request_id, 913);
                assert_eq!(args.template.as_deref(), Some("anthropic"));
                assert_eq!(args.key.as_deref(), Some("sk-test"));
                assert!(!args.custom);
                assert!(args.model.is_empty());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_added_response_roundtrip() {
        // Pin the response shape — the desktop uses
        // the returned `model` to refresh its model list
        // without a follow-up list call.
        let resp = ResponsePacket::ModelAdded {
            request_id: 914,
            model: ModelSummary {
                id: "anthropic".to_string(),
                display_name: "Anthropic".to_string(),
                template_id: Some("anthropic".to_string()),
                api_type: "anthropic".to_string(),
                base_url: "https://api.anthropic.com".to_string(),
                model_id: "claude-sonnet-4-5".to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(8_192),
                headers: Default::default(),
                credential_id: None,
                requires_key: true,
                is_local: false,
                enabled: true,
                spec: None,
                // Phase 2 — no annotation.
                note: None,
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_added")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(914));

        let model = json
            .get("model")
            .expect("response should have a model object");
        assert_eq!(model.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(
            model.get("display_name").and_then(|v| v.as_str()),
            Some("Anthropic")
        );
        assert_eq!(
            model.get("api_format").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            model.get("model_id").and_then(|v| v.as_str()),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            model.get("requires_key").and_then(|v| v.as_bool()),
            Some(true)
        );

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ModelAdded { request_id, model } => {
                assert_eq!(request_id, 914);
                assert_eq!(model.id, "anthropic");
                assert!(model.requires_key);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_create_request_roundtrip() {
        // All fields populated — round-trips without losing the
        // optional description field.
        let req = RequestPacket::PrincipalCreate {
            request_id: 302,
            name: "alice".to_string(),
            description: Some("personal assistant".to_string()),
            model_id: "gpt-4o".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalCreate {
                request_id,
                name,
                description,
                model_id,
            } => {
                assert_eq!(request_id, 302);
                assert_eq!(name, "alice");
                assert_eq!(description.as_deref(), Some("personal assistant"));
                assert_eq!(model_id, "gpt-4o");
            }
            _ => panic!("Wrong variant"),
        }

        // Minimal payload — name + model_id. `#[serde(default)]` lets
        // older clients omit `description` without breaking the
        // round-trip.
        let minimal = RequestPacket::PrincipalCreate {
            request_id: 303,
            name: "bob".to_string(),
            description: None,
            model_id: "claude-sonnet-4-5".to_string(),
        };
        let bytes = minimal.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalCreate {
                request_id,
                name,
                description,
                model_id,
            } => {
                assert_eq!(request_id, 303);
                assert_eq!(name, "bob");
                assert!(description.is_none());
                assert_eq!(model_id, "claude-sonnet-4-5");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_update_request_roundtrip() {
        let req = RequestPacket::PrincipalUpdate {
            request_id: 304,
            name: "alice".to_string(),
            description: Some("updated description".to_string()),
            status: Some("busy".to_string()),
            exposure: Some("public".to_string()),
            preferred_model_id: Some("claude-sonnet-4-5".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains("\"type\":\"principal_update\""),
            "expected principal_update wire tag, got: {json}"
        );
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalUpdate {
                request_id,
                name,
                description,
                status,
                exposure,
                preferred_model_id,
            } => {
                assert_eq!(request_id, 304);
                assert_eq!(name, "alice");
                assert_eq!(description.as_deref(), Some("updated description"));
                assert_eq!(status.as_deref(), Some("busy"));
                assert_eq!(exposure.as_deref(), Some("public"));
                assert_eq!(preferred_model_id.as_deref(), Some("claude-sonnet-4-5"));
            }
            _ => panic!("Wrong variant"),
        }

        // Minimal payload — only the name is required; omitted fields
        // round-trip as None.
        let minimal = RequestPacket::PrincipalUpdate {
            request_id: 305,
            name: "bob".to_string(),
            description: None,
            status: None,
            exposure: None,
            preferred_model_id: None,
        };
        let bytes = minimal.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalUpdate {
                request_id,
                name,
                description,
                status,
                exposure,
                preferred_model_id,
            } => {
                assert_eq!(request_id, 305);
                assert_eq!(name, "bob");
                assert!(description.is_none());
                assert!(status.is_none());
                assert!(exposure.is_none());
                assert!(preferred_model_id.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_remove_request_roundtrip() {
        let req = RequestPacket::PrincipalRemove {
            request_id: 306,
            name: "alice".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains("\"type\":\"principal_remove\""),
            "expected principal_remove wire tag, got: {json}"
        );
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalRemove { request_id, name } => {
                assert_eq!(request_id, 306);
                assert_eq!(name, "alice");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_updated_response_roundtrip() {
        let resp = ResponsePacket::PrincipalUpdated {
            request_id: 604,
            principal: crate::principal::PrincipalSummary {
                name: "alice".to_string(),
                did: peko_subject::PrincipalDID("did:peko:local:alice".to_string()),
                owner: peko_auth::Subject::User("alice".to_string()),
                description: Some("updated".to_string()),
                exposure: peko_auth::Exposure::Public,
                status: Some(crate::principal::config::Status::Busy),
                preferred_model_id: None,
                capabilities: crate::extensions::framework::types::Capabilities::default(),
                agent_prompt_count: 1,
                workspace_path: "/tmp/alice".to_string(),
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalUpdated {
                request_id,
                principal,
            } => {
                assert_eq!(request_id, 604);
                assert_eq!(principal.name, "alice");
                assert_eq!(principal.description.as_deref(), Some("updated"));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_removed_response_roundtrip() {
        let resp = ResponsePacket::PrincipalRemoved {
            request_id: 605,
            name: "alice".to_string(),
            removed: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalRemoved {
                request_id,
                name,
                removed,
            } => {
                assert_eq!(request_id, 605);
                assert_eq!(name, "alice");
                assert!(removed);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_created_response_roundtrip() {
        let resp = ResponsePacket::PrincipalCreated {
            request_id: 603,
            principal: crate::principal::PrincipalSummary {
                name: "alice".to_string(),
                did: peko_subject::PrincipalDID("did:peko:local:alice".to_string()),
                owner: peko_auth::Subject::User("alice".to_string()),
                description: Some("personal assistant".to_string()),
                exposure: peko_auth::Exposure::default(),
                status: None,
                preferred_model_id: None,
                capabilities: crate::extensions::framework::types::Capabilities::default(),
                agent_prompt_count: 1,
                workspace_path: "/tmp/alice".to_string(),
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalCreated {
                request_id,
                principal,
            } => {
                assert_eq!(request_id, 603);
                assert_eq!(principal.name, "alice");
                assert_eq!(principal.agent_prompt_count, 1);
                assert_eq!(principal.description.as_deref(), Some("personal assistant"));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_credential_list_request_roundtrip() {
        // RP3A: pin the widened wire shape so the desktop's
        // `credential_list` Tauri command can rely on `type`,
        // `request_id`, `namespace`, and `kind` round-trip.
        let req = RequestPacket::CredentialList {
            request_id: 901,
            namespace: Some("provider:openai".to_string()),
            kind: Some("api_key".to_string()),
            include_system: Some(true),
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_list")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(901));
        assert_eq!(
            json.get("namespace").and_then(|v| v.as_str()),
            Some("provider:openai")
        );
        assert_eq!(json.get("kind").and_then(|v| v.as_str()), Some("api_key"));

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CredentialList {
                request_id,
                namespace,
                kind,
                include_system,
            } => {
                assert_eq!(request_id, 901);
                assert_eq!(namespace.as_deref(), Some("provider:openai"));
                assert_eq!(kind.as_deref(), Some("api_key"));
                assert_eq!(include_system, Some(true));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_credentials_listed_response_roundtrip() {
        // RP3A: pin the widened response wire shape — id / namespace /
        // name / kind / has_key / last_tested_at / last_tested_ok field
        // names mirror the desktop's `CredentialRow`.
        let resp = ResponsePacket::CredentialsListed {
            request_id: 902,
            providers: vec![
                CredentialRow {
                    id: "id-minimax".to_string(),
                    namespace: "provider:minimax".to_string(),
                    name: "default".to_string(),
                    kind: "api_key".to_string(),
                    has_key: true,
                    last_tested_at: Some("2026-07-15T11:48:00Z".to_string()),
                    last_tested_ok: Some(true),
                    system_owned: false,
                    is_referenced: false,
                    referenced_by: Vec::new(),
                },
                CredentialRow {
                    id: "id-openai".to_string(),
                    namespace: "provider:openai".to_string(),
                    name: "default".to_string(),
                    kind: "api_key".to_string(),
                    has_key: false,
                    last_tested_at: None,
                    last_tested_ok: None,
                    system_owned: false,
                    is_referenced: false,
                    referenced_by: Vec::new(),
                },
            ],
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credentials_listed")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(902));

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CredentialsListed {
                request_id,
                providers,
            } => {
                assert_eq!(request_id, 902);
                assert_eq!(providers.len(), 2);
                assert_eq!(providers[0].id, "id-minimax");
                assert_eq!(providers[0].namespace, "provider:minimax");
                assert_eq!(providers[0].name, "default");
                assert_eq!(providers[0].kind, "api_key");
                assert!(providers[0].has_key);
                assert_eq!(
                    providers[0].last_tested_at,
                    Some("2026-07-15T11:48:00Z".to_string())
                );
                assert_eq!(providers[0].last_tested_ok, Some(true));
                assert_eq!(providers[1].id, "id-openai");
                assert_eq!(providers[1].namespace, "provider:openai");
                assert!(!providers[1].has_key);
                assert!(providers[1].last_tested_at.is_none());
                assert!(providers[1].last_tested_ok.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_test_request_roundtrip() {
        // Model-first: live-model-test is keyed by catalog model id.
        let req = RequestPacket::ModelTest {
            request_id: 911,
            id: "minimax".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_test")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(911));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("minimax"));

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ModelTest { request_id, id } => {
                assert_eq!(request_id, 911);
                assert_eq!(id, "minimax");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_tested_response_roundtrip() {
        let resp = ResponsePacket::ModelTested {
            request_id: 912,
            id: "anthropic".to_string(),
            ok: false,
            message: "HTTP 401: invalid api key".to_string(),
            latency_ms: 187,
            http_status: Some(401),
            model_used: Some("claude-haiku-4-5".to_string()),
            tested_at: "2026-07-15T11:48:00Z".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("model_tested")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(912));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("anthropic"));
        assert_eq!(json.get("ok").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(
            json.get("message").and_then(|v| v.as_str()),
            Some("HTTP 401: invalid api key")
        );
        assert_eq!(json.get("latency_ms").and_then(|v| v.as_u64()), Some(187));
        assert_eq!(json.get("http_status").and_then(|v| v.as_u64()), Some(401));
        assert_eq!(
            json.get("model_used").and_then(|v| v.as_str()),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            json.get("tested_at").and_then(|v| v.as_str()),
            Some("2026-07-15T11:48:00Z")
        );

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ModelTested {
                request_id,
                id,
                ok,
                message,
                latency_ms,
                http_status,
                model_used,
                tested_at,
            } => {
                assert_eq!(request_id, 912);
                assert_eq!(id, "anthropic");
                assert!(!ok);
                assert_eq!(message, "HTTP 401: invalid api key");
                assert_eq!(latency_ms, 187);
                assert_eq!(http_status, Some(401));
                assert_eq!(model_used.as_deref(), Some("claude-haiku-4-5"));
                assert_eq!(tested_at, "2026-07-15T11:48:00Z");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pin the `persona_draft` wire envelope so a future change to the
    /// JSON keys surfaces as a test failure rather than the CLI's
    /// `peko principal persona set` subcommand silently timing out.
    /// Backed by the persona builder (2026-08-01 field-test top wish).
    #[test]
    fn test_persona_draft_request_roundtrip() {
        let req = RequestPacket::PersonaDraft {
            request_id: 1301,
            model_id: "minimax-MiniMax-M3".to_string(),
            from: "a senior rust reviewer who cites the borrow checker".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("persona_draft")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(1301));
        assert_eq!(
            json.get("model_id").and_then(|v| v.as_str()),
            Some("minimax-MiniMax-M3")
        );
        assert_eq!(
            json.get("from").and_then(|v| v.as_str()),
            Some("a senior rust reviewer who cites the borrow checker")
        );

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PersonaDraft {
                request_id,
                model_id,
                from,
            } => {
                assert_eq!(request_id, 1301);
                assert_eq!(model_id, "minimax-MiniMax-M3");
                assert_eq!(from, "a senior rust reviewer who cites the borrow checker");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pin the `persona_drafted` wire envelope. `parse_ok` lets the
    /// CLI distinguish JSON success from a non-JSON LLM fallback.
    #[test]
    fn test_persona_drafted_response_roundtrip() {
        let resp = ResponsePacket::PersonaDrafted {
            request_id: 1302,
            content: r#"{"display_name":"Rust Reviewer"}"#.to_string(),
            parse_ok: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("persona_drafted")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(1302));
        assert_eq!(json.get("parse_ok").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            json.get("content").and_then(|v| v.as_str()),
            Some(r#"{"display_name":"Rust Reviewer"}"#)
        );

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PersonaDrafted {
                request_id,
                content,
                parse_ok,
            } => {
                assert_eq!(request_id, 1302);
                assert!(parse_ok);
                assert_eq!(content, r#"{"display_name":"Rust Reviewer"}"#);
            }
            _ => panic!("Wrong variant"),
        }
    }
    #[test]
    fn test_credential_set_request_roundtrip() {
        let req = RequestPacket::CredentialSet {
            request_id: 921,
            namespace: "provider:minimax".to_string(),
            name: "default".to_string(),
            kind: "api_key".to_string(),
            material: "sk-test-123".to_string(),
            metadata: Some(serde_json::json!({ "foo": "bar" })),
            replace_on: None,
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_set")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(921));
        assert_eq!(
            json.get("namespace").and_then(|v| v.as_str()),
            Some("provider:minimax")
        );
        assert_eq!(json.get("name").and_then(|v| v.as_str()), Some("default"));
        assert_eq!(json.get("kind").and_then(|v| v.as_str()), Some("api_key"));
        assert_eq!(
            json.get("material").and_then(|v| v.as_str()),
            Some("sk-test-123")
        );
        assert_eq!(
            json.get("metadata")
                .and_then(|v| v.as_object())
                .map(|m| m.len()),
            Some(1)
        );

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CredentialSet {
                request_id,
                namespace,
                name,
                kind,
                material,
                metadata,
                replace_on,
            } => {
                assert_eq!(request_id, 921);
                assert_eq!(namespace, "provider:minimax");
                assert_eq!(name, "default");
                assert_eq!(kind, "api_key");
                assert_eq!(material, "sk-test-123");
                assert_eq!(
                    metadata
                        .as_ref()
                        .and_then(|m| m.get("foo").and_then(|v| v.as_str())),
                    Some("bar")
                );
                assert!(replace_on.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pin the `credential_set_done` response wire shape. The
    /// desktop's Tauri command consumes this and updates its local
    /// UI without re-issuing a `credential_list` round-trip, so the
    /// `id` echo is part of the contract.
    #[test]
    fn test_credential_set_done_response_roundtrip() {
        let resp = ResponsePacket::CredentialSetDone {
            request_id: 922,
            id: "id-minimax".to_string(),
            rewired_models: 0,
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_set_done")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(922));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("id-minimax"));

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CredentialSetDone {
                request_id,
                id,
                rewired_models,
            } => {
                assert_eq!(request_id, 922);
                assert_eq!(id, "id-minimax");
                assert_eq!(rewired_models, 0);
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Mirror of `test_credential_set_request_roundtrip` for the
    /// delete variant. The desktop's `credential_delete` Tauri
    /// command has the same timeout pathology as `credential_set`
    /// without a registered handler.
    #[test]
    fn test_credential_delete_request_roundtrip() {
        let req = RequestPacket::CredentialDelete {
            request_id: 931,
            id: "id-minimax".to_string(),
            force: false,
        };
        let bytes = req.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_delete")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(931));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("id-minimax"));

        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CredentialDelete {
                request_id,
                id,
                force,
            } => {
                assert_eq!(request_id, 931);
                assert_eq!(id, "id-minimax");
                assert!(!force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pin the `credential_deleted` response wire shape.
    #[test]
    fn test_credential_deleted_response_roundtrip() {
        let resp = ResponsePacket::CredentialDeleted {
            request_id: 932,
            id: "id-minimax".to_string(),
            broken_references: 0,
        };
        let bytes = resp.to_bytes().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json.get("type").and_then(|v| v.as_str()),
            Some("credential_deleted")
        );
        assert_eq!(json.get("request_id").and_then(|v| v.as_u64()), Some(932));
        assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("id-minimax"));

        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CredentialDeleted {
                request_id,
                id,
                broken_references,
            } => {
                assert_eq!(request_id, 932);
                assert_eq!(id, "id-minimax");
                assert_eq!(broken_references, 0);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_import_preview_request_roundtrip() {
        let req = RequestPacket::PrincipalImportPreview {
            request_id: 303,
            file_path: "/tmp/test.principal".to_string(),
            name: Some("renamed".to_string()),
            allow_unsigned: true,
            force: false,
        };
        let bytes = req.to_bytes().unwrap();
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains("\"type\":\"principal_import_preview\""),
            "expected principal_import_preview wire tag, got: {json}"
        );
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalImportPreview {
                request_id,
                file_path,
                name,
                allow_unsigned,
                force,
            } => {
                assert_eq!(request_id, 303);
                assert_eq!(file_path, "/tmp/test.principal");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(allow_unsigned);
                assert!(!force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_import_request_confirmed_defaults_to_false() {
        // Bare deserialization of the legacy wire shape (no `confirmed`
        // field) must default to `false` so old CLI / daemon pairs don't
        // accidentally bypass the confirmation gate.
        let json = r#"{"type":"principal_import","request_id":304,"file_path":"/tmp/x.principal"}"#;
        let decoded: RequestPacket = serde_json::from_str(json).unwrap();
        match decoded {
            RequestPacket::PrincipalImport { confirmed, .. } => {
                assert!(!confirmed, "confirmed must default to false");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_import_previewed_response_roundtrip() {
        let resp = ResponsePacket::PrincipalImportPreviewed {
            request_id: 603,
            name: "preview-principal".to_string(),
            version: "1.0.0".to_string(),
            did: "did:peko:local:preview".to_string(),
            description: Some("A preview test principal".to_string()),
            agents: vec!["primary".to_string(), "researcher".to_string()],
            extensions: vec!["ext-1".to_string()],
            required_capabilities: vec!["tool:Read".to_string(), "network".to_string()],
            signed: true,
            validation_errors: vec![],
            validation_warnings: vec!["Unencrypted keys".to_string()],
        };
        let bytes = resp.to_bytes().unwrap();
        let json = std::str::from_utf8(&bytes).unwrap();
        assert!(
            json.contains("\"type\":\"principal_import_previewed\""),
            "expected principal_import_previewed wire tag, got: {json}"
        );
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalImportPreviewed {
                request_id,
                name,
                version,
                did,
                description,
                agents,
                extensions,
                required_capabilities,
                signed,
                validation_errors,
                validation_warnings,
            } => {
                assert_eq!(request_id, 603);
                assert_eq!(name, "preview-principal");
                assert_eq!(version, "1.0.0");
                assert_eq!(did, "did:peko:local:preview");
                assert_eq!(description, Some("A preview test principal".to_string()));
                assert_eq!(
                    agents,
                    vec!["primary".to_string(), "researcher".to_string()]
                );
                assert_eq!(extensions, vec!["ext-1".to_string()]);
                assert_eq!(
                    required_capabilities,
                    vec!["tool:Read".to_string(), "network".to_string()]
                );
                assert!(signed);
                assert!(validation_errors.is_empty());
                assert_eq!(validation_warnings, vec!["Unencrypted keys".to_string()]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_crud_request_ids() {
        let req_principal_list = RequestPacket::PrincipalList { request_id: 1 };
        assert_eq!(req_principal_list.request_id(), 1);

        let req_principal_get = RequestPacket::PrincipalGet {
            request_id: 2,
            name: "helper".to_string(),
        };
        assert_eq!(req_principal_get.request_id(), 2);

        let req_principal_update = RequestPacket::PrincipalUpdate {
            request_id: 3,
            name: "helper".to_string(),
            description: None,
            status: None,
            exposure: None,
            preferred_model_id: None,
        };
        assert_eq!(req_principal_update.request_id(), 3);

        let req_principal_remove = RequestPacket::PrincipalRemove {
            request_id: 4,
            name: "helper".to_string(),
        };
        assert_eq!(req_principal_remove.request_id(), 4);
    }

    #[test]
    fn test_crud_response_ids() {
        let resp_principal_list = ResponsePacket::PrincipalList {
            request_id: 10,
            principals: vec![],
        };
        assert_eq!(resp_principal_list.request_id(), 10);

        let resp_principal_get = ResponsePacket::PrincipalGet {
            request_id: 11,
            principal: None,
        };
        assert_eq!(resp_principal_get.request_id(), 11);

        let resp_principal_updated = ResponsePacket::PrincipalUpdated {
            request_id: 12,
            principal: crate::principal::PrincipalSummary {
                name: "helper".to_string(),
                did: peko_subject::PrincipalDID("did:peko:local:helper".to_string()),
                owner: peko_auth::Subject::User("alice".to_string()),
                description: None,
                exposure: peko_auth::Exposure::default(),
                status: None,
                preferred_model_id: None,
                capabilities: crate::extensions::framework::types::Capabilities::default(),
                agent_prompt_count: 0,
                workspace_path: "/tmp/helper".to_string(),
            },
        };
        assert_eq!(resp_principal_updated.request_id(), 12);

        let resp_principal_removed = ResponsePacket::PrincipalRemoved {
            request_id: 13,
            name: "helper".to_string(),
            removed: true,
        };
        assert_eq!(resp_principal_removed.request_id(), 13);
    }

    #[test]
    fn test_system_status_request_roundtrip() {
        let req = RequestPacket::SystemStatus { request_id: 900 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SystemStatus { request_id } => {
                assert_eq!(request_id, 900);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_doctor_request_roundtrip() {
        let req = RequestPacket::SystemDoctor { request_id: 901 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SystemDoctor { request_id } => {
                assert_eq!(request_id, 901);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_status_response_roundtrip() {
        let resp = ResponsePacket::SystemStatus {
            request_id: 902,
            version: "1.0.0".to_string(),
            uptime_secs: 12345,
            degraded: false,
            instance_count: 3,
            ready: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SystemStatus {
                request_id,
                version,
                uptime_secs,
                degraded,
                instance_count,
                ready,
            } => {
                assert_eq!(request_id, 902);
                assert_eq!(version, "1.0.0");
                assert_eq!(uptime_secs, 12345);
                assert!(!degraded);
                assert_eq!(instance_count, 3);
                assert!(ready);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_doctor_response_roundtrip() {
        let resp = ResponsePacket::SystemDoctor {
            request_id: 903,
            checks: vec![
                DoctorCheck {
                    name: "daemon_ready".to_string(),
                    status: "pass".to_string(),
                    message: "Daemon is ready".to_string(),
                    suggestion: None,
                },
                DoctorCheck {
                    name: "not_degraded".to_string(),
                    status: "warn".to_string(),
                    message: "Daemon is in degraded mode".to_string(),
                    suggestion: Some("Restart daemon".to_string()),
                },
            ],
            passed: 1,
            failed: 0,
            warnings: 1,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SystemDoctor {
                request_id,
                checks,
                passed,
                failed,
                warnings,
            } => {
                assert_eq!(request_id, 903);
                assert_eq!(checks.len(), 2);
                assert_eq!(checks[0].name, "daemon_ready");
                assert_eq!(checks[0].status, "pass");
                assert_eq!(checks[1].name, "not_degraded");
                assert_eq!(checks[1].status, "warn");
                assert_eq!(checks[1].suggestion, Some("Restart daemon".to_string()));
                assert_eq!(passed, 1);
                assert_eq!(failed, 0);
                assert_eq!(warnings, 1);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_request_ids() {
        let req_status = RequestPacket::SystemStatus { request_id: 1 };
        assert_eq!(req_status.request_id(), 1);

        let req_doctor = RequestPacket::SystemDoctor { request_id: 2 };
        assert_eq!(req_doctor.request_id(), 2);
    }

    #[test]
    fn test_system_response_ids() {
        let resp_status = ResponsePacket::SystemStatus {
            request_id: 10,
            version: "0.1.0".to_string(),
            uptime_secs: 0,
            degraded: false,
            instance_count: 0,
            ready: false,
        };
        assert_eq!(resp_status.request_id(), 10);

        let resp_doctor = ResponsePacket::SystemDoctor {
            request_id: 11,
            checks: vec![],
            passed: 0,
            failed: 0,
            warnings: 0,
        };
        assert_eq!(resp_doctor.request_id(), 11);
    }

    #[test]
    fn test_authenticated_request_roundtrip() {
        // Critical path: auth envelope + request packet must serialize together
        let envelope = AuthenticatedRequest {
            auth: AuthHeader {
                credential: AuthCredential::ApiKey("pkr_testkey123".to_string()),
            },
            packet: RequestPacket::Ping { request_id: 7 },
        };

        let json = serde_json::to_vec(&envelope).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 7);
        match decoded.auth.credential {
            AuthCredential::ApiKey(key) => assert_eq!(key, "pkr_testkey123"),
            other => panic!("Expected ApiKey, got: {:?}", other),
        }
    }

    #[test]
    fn test_authenticated_request_jwt_roundtrip() {
        let envelope = AuthenticatedRequest {
            auth: AuthHeader {
                credential: AuthCredential::Jwt("eyJhbGciOiJIUzI1NiJ9.test".to_string()),
            },
            packet: RequestPacket::SystemStatus { request_id: 8 },
        };

        let json = serde_json::to_vec(&envelope).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 8);
        match decoded.auth.credential {
            AuthCredential::Jwt(token) => {
                assert_eq!(token, "eyJhbGciOiJIUzI1NiJ9.test")
            }
            other => panic!("Expected Jwt, got: {:?}", other),
        }
    }

    #[test]
    fn test_authenticated_request_none_defaults() {
        // Bare RequestPacket deserialized as AuthenticatedRequest should have None auth
        let packet = RequestPacket::Ping { request_id: 9 };
        let json = serde_json::to_vec(&packet).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 9);
        match decoded.auth.credential {
            AuthCredential::None => (), // Expected
            other => panic!("Expected None credential for bare packet, got: {:?}", other),
        }
    }

    // -- issue #30: `RequestPacket::resolved_subject` --

    fn grant_pkt(subject: peko_auth::Subject) -> RequestPacket {
        RequestPacket::PrincipalGrantPermission {
            request_id: 1,
            name: "p".into(),
            subject,
            permission: peko_auth::ownership::Permission::Chat,
        }
    }

    #[test]
    fn test_resolved_subject_canonical_shape() {
        // The grant carries the subject directly (ADR-039). The
        // resolver just clones it out.
        let pkt = grant_pkt(peko_auth::Subject::Principal("helper".into()));
        assert_eq!(
            pkt.resolved_subject(),
            peko_auth::Subject::Principal("helper".into())
        );
    }

    #[test]
    fn test_resolved_subject_public_variant() {
        // Public revoke via canonical Public.
        let pkt = RequestPacket::PrincipalRevokePermission {
            request_id: 1,
            name: "p".into(),
            subject: peko_auth::Subject::Public,
            permission: peko_auth::ownership::Permission::Chat,
        };
        assert_eq!(pkt.resolved_subject(), peko_auth::Subject::Public);
    }

    #[test]
    fn test_resolved_subject_non_grant_revoke_returns_sentinel() {
        // Any non-grant/revoke variant must not panic — returns a
        // sentinel `Subject::User("")` that the caller can ignore.
        let pkt = RequestPacket::Ping { request_id: 1 };
        assert_eq!(
            pkt.resolved_subject(),
            peko_auth::Subject::User(String::new())
        );
    }

    #[test]
    fn test_grant_serialization_carries_subject_inline() {
        // After issue #30, the grant carries the `Subject` directly —
        // no legacy `subject_id` / `subject_type` fields exist on the
        // wire anymore. The wire must serialize `subject` and not the
        // dropped fields.
        let pkt = grant_pkt(peko_auth::Subject::Principal("helper".into()));
        let json = serde_json::to_string(&pkt).unwrap();
        assert!(
            json.contains("\"subject\""),
            "new-shape serialization must carry `subject`, got: {json}"
        );
        assert!(
            !json.contains("subject_id") && !json.contains("subject_type"),
            "new-shape serialization must not contain legacy fields, got: {json}"
        );
    }

    #[test]
    fn test_variant_name_does_not_leak_payload() {
        // Construct a response that contains a large binary-like payload.
        let resp = ResponsePacket::Text {
            request_id: 1,
            seq: 0,
            chunk: "sensitive-binary-payload-abc123".to_string(),
        };

        let name = resp.variant_name();
        let err = crate::ipc::unexpected_response(&resp);
        let err_msg = format!("{err}");

        assert_eq!(name, "Text");
        assert!(
            err_msg.contains("Text"),
            "error should name the variant: {err_msg}"
        );
        assert!(
            !err_msg.contains("sensitive-binary-payload"),
            "error must not leak payload: {err_msg}"
        );
        assert!(
            !err_msg.contains("chunk"),
            "error must not leak field names: {err_msg}"
        );
    }

    // ─── Principal operations tests ─────────────────────────────────

    #[test]
    fn test_principal_send_request_roundtrip() {
        let req = RequestPacket::PrincipalSend {
            request_id: 5000,
            name: "helper".to_string(),
            message: "hello".to_string(),
            user: "alice".to_string(),
            no_slash: true,
            output_format: OutputFormat::Json,
            override_model: Some("gpt-4o".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalSend {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
                override_model,
            } => {
                assert_eq!(request_id, 5000);
                assert_eq!(name, "helper");
                assert_eq!(message, "hello");
                assert_eq!(user, "alice");
                assert!(no_slash);
                assert_eq!(output_format, OutputFormat::Json);
                assert_eq!(override_model, Some("gpt-4o".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `principal_send_stream` round-trips losslessly through the
    /// JSON wire format, so the desktop and the daemon can negotiate
    /// the streaming variant without a separate codec.
    #[test]
    fn test_principal_send_stream_request_roundtrip() {
        let req = RequestPacket::PrincipalSendStream {
            request_id: 5100,
            name: "helper".to_string(),
            message: "stream please".to_string(),
            user: "alice".to_string(),
            no_slash: true,
            output_format: OutputFormat::Json,
            override_model: Some("claude-haiku-4-5".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalSendStream {
                request_id,
                name,
                message,
                user,
                no_slash,
                output_format,
                override_model,
            } => {
                assert_eq!(request_id, 5100);
                assert_eq!(name, "helper");
                assert_eq!(message, "stream please");
                assert_eq!(user, "alice");
                assert!(no_slash);
                assert_eq!(output_format, OutputFormat::Json);
                assert_eq!(override_model, Some("claude-haiku-4-5".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
        // The wire tag must match the CLI spelling so the desktop
        // can route the JSON packet to the right daemon handler.
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            raw.contains("\"type\":\"principal_send_stream\""),
            "wire tag missing: {raw}"
        );
    }

    /// Streaming chunk packets carry the request_id and a single
    /// delta string. Multiple chunks are expected on the wire before
    /// a `PrincipalSentDone` settles the run.
    #[test]
    fn test_principal_sent_chunk_roundtrip() {
        let resp = ResponsePacket::PrincipalSentChunk {
            request_id: 5100,
            delta: "Hello, ".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalSentChunk { request_id, delta } => {
                assert_eq!(request_id, 5100);
                assert_eq!(delta, "Hello, ");
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(raw.contains("\"type\":\"principal_sent_chunk\""));
    }

    /// Content-free iteration boundary marker used by clients to break
    /// assistant text into one bubble per agentic iteration.
    #[test]
    fn test_principal_sent_iteration_roundtrip() {
        let resp = ResponsePacket::PrincipalSentIteration {
            request_id: 5100,
            iteration: 3,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalSentIteration {
                request_id,
                iteration,
            } => {
                assert_eq!(request_id, 5100);
                assert_eq!(iteration, 3);
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(raw.contains("\"type\":\"principal_sent_iteration\""));
    }

    /// Final streaming packet carries the full final answer (same
    /// content the non-streaming `PrincipalSent` would have returned).
    #[test]
    fn test_principal_sent_done_roundtrip() {
        let resp = ResponsePacket::PrincipalSentDone {
            request_id: 5100,
            content: "Hello, world!".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalSentDone {
                request_id,
                content,
            } => {
                assert_eq!(request_id, 5100);
                assert_eq!(content, "Hello, world!");
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(raw.contains("\"type\":\"principal_sent_done\""));
    }

    #[test]
    fn test_principal_export_request_roundtrip() {
        let req = RequestPacket::PrincipalExport {
            request_id: 5001,
            name: "helper".to_string(),
            output: Some("/tmp/helper.principal".to_string()),
            include_sessions: true,
            with_extensions: false,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalExport {
                request_id,
                name,
                output,
                include_sessions,
                with_extensions,
            } => {
                assert_eq!(request_id, 5001);
                assert_eq!(name, "helper");
                assert_eq!(output, Some("/tmp/helper.principal".to_string()));
                assert!(include_sessions);
                assert!(!with_extensions);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_grant_permission_request_roundtrip() {
        let req = RequestPacket::PrincipalGrantPermission {
            request_id: 5002,
            name: "helper".to_string(),
            subject: peko_auth::Subject::User("bob".to_string()),
            permission: peko_auth::ownership::Permission::Chat,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalGrantPermission {
                request_id,
                name,
                subject,
                permission,
            } => {
                assert_eq!(request_id, 5002);
                assert_eq!(name, "helper");
                assert_eq!(subject, peko_auth::Subject::User("bob".to_string()));
                assert_eq!(permission, peko_auth::ownership::Permission::Chat);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_set_status_request_roundtrip() {
        let req = RequestPacket::PrincipalSetStatus {
            request_id: 5003,
            name: "helper".to_string(),
            status: "busy".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalSetStatus {
                request_id,
                name,
                status,
            } => {
                assert_eq!(request_id, 5003);
                assert_eq!(name, "helper");
                assert_eq!(status, "busy");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_set_exposure_request_roundtrip() {
        let req = RequestPacket::PrincipalSetExposure {
            request_id: 5004,
            name: "helper".to_string(),
            exposure: "private".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalSetExposure {
                request_id,
                name,
                exposure,
            } => {
                assert_eq!(request_id, 5004);
                assert_eq!(name, "helper");
                assert_eq!(exposure, "private");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_status_updated_response_roundtrip() {
        let resp = ResponsePacket::PrincipalStatusUpdated {
            request_id: 6001,
            name: "helper".to_string(),
            status: "online".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalStatusUpdated {
                request_id,
                name,
                status,
            } => {
                assert_eq!(request_id, 6001);
                assert_eq!(name, "helper");
                assert_eq!(status, "online");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_exposure_updated_response_roundtrip() {
        let resp = ResponsePacket::PrincipalExposureUpdated {
            request_id: 6002,
            name: "helper".to_string(),
            exposure: "public".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalExposureUpdated {
                request_id,
                name,
                exposure,
            } => {
                assert_eq!(request_id, 6002);
                assert_eq!(name, "helper");
                assert_eq!(exposure, "public");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_sent_response_roundtrip() {
        let resp = ResponsePacket::PrincipalSent {
            request_id: 6000,
            content: "hi there".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalSent {
                request_id,
                content,
            } => {
                assert_eq!(request_id, 6000);
                assert_eq!(content, "hi there");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_log_request_roundtrip() {
        // `peko log` IPC shape. The wire tag must match the CLI spelling
        // and round-trip must preserve `peer`, `limit`, `since_secs`,
        // and `cursor` (defaulted when omitted).
        let req = RequestPacket::PrincipalLog {
            request_id: 5200,
            name: "helper".to_string(),
            peer: Some(peko_auth::Subject::User("alice".to_string())),
            limit: Some(100),
            since_secs: Some(86_400),
            cursor: None,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalLog {
                request_id,
                name,
                peer,
                limit,
                since_secs,
                cursor,
            } => {
                assert_eq!(request_id, 5200);
                assert_eq!(name, "helper");
                assert_eq!(peer, Some(peko_auth::Subject::User("alice".to_string())));
                assert_eq!(limit, Some(100));
                assert_eq!(since_secs, Some(86_400));
                assert_eq!(cursor, None);
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            raw.contains("\"type\":\"principal_log\""),
            "wire tag missing: {raw}"
        );
    }

    #[test]
    fn principal_log_message_round_trips_with_camel_case_metadata() {
        let message = PrincipalLogMessage::new(
            peko_auth::Subject::User("local".to_string()),
            "hello",
            Some("request-1".to_string()),
        );

        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["schemaVersion"], PRINCIPAL_LOG_SCHEMA_VERSION);
        assert_eq!(value["correlationId"], "request-1");
        assert_eq!(
            serde_json::from_value::<PrincipalLogMessage>(value).unwrap(),
            message
        );
    }

    #[test]
    fn test_principal_log_response_roundtrip() {
        // Response shape: resolved peer, messages array, next_cursor,
        // has_more. Pre-launch clean cutover from session_id/events/
        // truncated.
        let resp = ResponsePacket::PrincipalLog {
            request_id: 6200,
            name: "helper".to_string(),
            peer: peko_auth::Subject::User("alice".to_string()),
            messages: vec![PrincipalLogMessage::new(
                peko_auth::Subject::User("alice".to_string()),
                "hi",
                None,
            )],
            next_cursor: Some("opaque-cursor".to_string()),
            has_more: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalLog {
                request_id,
                name,
                peer,
                messages,
                next_cursor,
                has_more,
            } => {
                assert_eq!(request_id, 6200);
                assert_eq!(name, "helper");
                assert_eq!(peer, peko_auth::Subject::User("alice".to_string()));
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].text, "hi");
                assert_eq!(next_cursor.as_deref(), Some("opaque-cursor"));
                assert!(has_more);
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            raw.contains("\"type\":\"principal_log\""),
            "wire tag missing: {raw}"
        );
    }

    #[test]
    fn test_principal_log_watch_request_roundtrip() {
        // `peko log --watch` IPC shape: snake_case wire tag, peer +
        // since_cursor preserved, both defaultable when omitted.
        let req = RequestPacket::PrincipalLogWatch {
            request_id: 5300,
            name: "helper".to_string(),
            peer: Some(peko_auth::Subject::User("alice".to_string())),
            since_cursor: Some("42".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::PrincipalLogWatch {
                request_id,
                name,
                peer,
                since_cursor,
            } => {
                assert_eq!(request_id, 5300);
                assert_eq!(name, "helper");
                assert_eq!(peer, Some(peko_auth::Subject::User("alice".to_string())));
                assert_eq!(since_cursor.as_deref(), Some("42"));
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            raw.contains("\"type\":\"principal_log_watch\""),
            "wire tag missing: {raw}"
        );

        // Omitted optional fields default to None.
        let bare = r#"{"type":"principal_log_watch","request_id":1,"name":"helper"}"#;
        match RequestPacket::from_bytes(bare.as_bytes()).unwrap() {
            RequestPacket::PrincipalLogWatch {
                peer, since_cursor, ..
            } => {
                assert!(peer.is_none());
                assert!(since_cursor.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_log_appended_response_roundtrip() {
        // One packet per chat message on the watch stream, carrying the
        // same `PrincipalLogMessage` shape the log command renders.
        let resp = ResponsePacket::PrincipalLogAppended {
            request_id: 5300,
            message: PrincipalLogMessage::new(
                peko_auth::Subject::User("alice".to_string()),
                "hi",
                None,
            ),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalLogAppended { request_id, message } => {
                assert_eq!(request_id, 5300);
                assert_eq!(message.text, "hi");
                assert_eq!(message.sender, peko_auth::Subject::User("alice".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            raw.contains("\"type\":\"principal_log_appended\""),
            "wire tag missing: {raw}"
        );
    }

    #[test]
    fn test_principal_permissions_response_roundtrip() {
        let grant = peko_auth::ownership::PermissionGrant {
            subject: peko_auth::Subject::User("bob".to_string()),
            permission: peko_auth::ownership::Permission::Chat,
            granted_at: "2026-06-01T00:00:00Z".to_string(),
            granted_by: peko_auth::Subject::User("alice".to_string()),
        };
        let resp = ResponsePacket::PrincipalPermissions {
            request_id: 6001,
            permissions: vec![grant],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::PrincipalPermissions {
                request_id,
                permissions,
            } => {
                assert_eq!(request_id, 6001);
                assert_eq!(permissions.len(), 1);
                assert_eq!(
                    permissions[0].subject,
                    peko_auth::Subject::User("bob".to_string())
                );
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_principal_request_ids() {
        let req_send = RequestPacket::PrincipalSend {
            request_id: 1,
            name: "p".to_string(),
            message: "m".to_string(),
            user: "u".to_string(),
            no_slash: false,
            output_format: OutputFormat::Human,
            override_model: None,
        };
        assert_eq!(req_send.request_id(), 1);

        let req_grant = RequestPacket::PrincipalGrantPermission {
            request_id: 2,
            name: "p".to_string(),
            subject: peko_auth::Subject::Public,
            permission: peko_auth::ownership::Permission::Chat,
        };
        assert_eq!(req_grant.request_id(), 2);

        let req_revoke = RequestPacket::PrincipalRevokePermission {
            request_id: 3,
            name: "p".to_string(),
            subject: peko_auth::Subject::Public,
            permission: peko_auth::ownership::Permission::Chat,
        };
        assert_eq!(req_revoke.request_id(), 3);
    }

    #[test]
    fn test_principal_response_ids_and_variant_names() {
        let resp_sent = ResponsePacket::PrincipalSent {
            request_id: 10,
            content: "c".to_string(),
        };
        assert_eq!(resp_sent.request_id(), 10);
        assert_eq!(resp_sent.variant_name(), "PrincipalSent");

        let resp_perms = ResponsePacket::PrincipalPermissions {
            request_id: 11,
            permissions: vec![],
        };
        assert_eq!(resp_perms.request_id(), 11);
        assert_eq!(resp_perms.variant_name(), "PrincipalPermissions");

        let resp_preview = ResponsePacket::PrincipalImportPreviewed {
            request_id: 12,
            name: "p".to_string(),
            version: "1.0.0".to_string(),
            did: "did:peko:local:p".to_string(),
            description: None,
            agents: vec![],
            extensions: vec![],
            required_capabilities: vec![],
            signed: false,
            validation_errors: vec![],
            validation_warnings: vec![],
        };
        assert_eq!(resp_preview.request_id(), 12);
        assert_eq!(resp_preview.variant_name(), "PrincipalImportPreviewed");
    }

    // ─── Interrupt means stop: Change 3 wire-shape tests ────────────
    //
    // The non-streaming `PrincipalSend` IPC variant is now internally
    // routed through the streaming machinery (see
    // `src/ipc/server.rs:run_principal_send` and the
    // `PrincipalSendResponseKind` enum). The only observable wire-level
    // difference is the success packet: one-shot emits
    // `PrincipalSent` (peko-desktop wire compat), streaming emits
    // `PrincipalSentDone`. These two tests lock down the wire shape
    // the redirect MUST preserve.

    /// The one-shot `PrincipalSent` response round-trips losslessly
    /// through the JSON wire format, with the `principal_sent` serde
    /// tag — the same shape peko-desktop's `usePrincipalSend` hook
    /// (`peko-desktop/src/hooks/usePrincipals.ts:82-88`) expects when
    /// the IPC client invokes the one-shot variant. The redirect
    /// must NOT change this packet's serde name.
    #[test]
    fn one_shot_principal_sent_preserves_wire_shape() {
        let resp = ResponsePacket::PrincipalSent {
            request_id: 42,
            content: "answer".to_string(),
        };
        let bytes = resp.to_bytes().expect("encode PrincipalSent");
        let decoded = ResponsePacket::from_bytes(&bytes).expect("decode PrincipalSent");
        match decoded {
            ResponsePacket::PrincipalSent {
                request_id,
                content,
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(content, "answer");
            }
            other => panic!(
                "decoded as wrong variant: {:?} — the redirect must keep \
                 one-shot responses as PrincipalSent",
                other.variant_name()
            ),
        }

        // Verify the JSON serde tag is exactly `principal_sent` so
        // peko-desktop's type guards still match.
        let json = String::from_utf8(bytes).expect("utf-8");
        assert!(
            json.contains("\"type\":\"principal_sent\""),
            "PrincipalSent must serialize with type tag 'principal_sent', got: {json}"
        );
    }

    /// The streaming `PrincipalSentDone` response uses the
    /// `principal_sent_done` serde tag — distinct from the one-shot
    /// `principal_sent` tag. Both shapes must coexist on the wire
    /// (the redirect adds a *third* transport behavior: a one-shot
    /// request may now emit a streamed chunk sequence, but it ends
    /// with `PrincipalSent`, never `PrincipalSentDone`).
    #[test]
    fn streaming_principal_sent_done_distinct_from_one_shot() {
        let one_shot = ResponsePacket::PrincipalSent {
            request_id: 1,
            content: "x".to_string(),
        };
        let streaming = ResponsePacket::PrincipalSentDone {
            request_id: 1,
            content: "x".to_string(),
        };

        let one_shot_json = String::from_utf8(one_shot.to_bytes().unwrap()).unwrap();
        let streaming_json = String::from_utf8(streaming.to_bytes().unwrap()).unwrap();

        assert!(
            one_shot_json.contains("\"type\":\"principal_sent\""),
            "one-shot must use 'principal_sent' tag, got: {one_shot_json}"
        );
        assert!(
            streaming_json.contains("\"type\":\"principal_sent_done\""),
            "streaming must use 'principal_sent_done' tag, got: {streaming_json}"
        );
        // The two tags are distinct, confirming wire-compat.
        assert_ne!(
            one_shot_json, streaming_json,
            "PrincipalSent and PrincipalSentDone must have distinct wire shapes"
        );
    }

    }
