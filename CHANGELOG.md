# Peko Changelog

All notable changes to Peko.

## [Unreleased]

### Claude Code core tool parity (in progress)

A multi-phase program to align peko's built-in tool names and schemas with
Claude Code's core tool surface, while preserving peko's daemon-first
execution, A2A protocol, and extension system.

#### Added

- `docs/architecture/builtin-tools.md` — canonical catalog of built-in tools,
  schemas, and Claude parity status.
- `tests/parity.rs` — integration-test harness for golden-transcript parity
  fixtures.
- Configuration gates `enable_async_tools` and `enable_task_tools` on
  `ToolFactoryConfig` and `BuiltinToolRegistrarConfig`.

#### Upcoming (tracked on `tool-parity-core-subset` branch)

- (none — all scoped tools are now in place)

### Added

- Planning-todo family `TaskCreate`, `TaskGet`, `TaskList`, and `TaskUpdate`.
  Todos are stored in a per-session JSONL sidecar
  (`{session_key}.todos.jsonl`) alongside the main session JSONL, using the
  same atomic-write durability strategy as session storage. `TaskCreate` takes
  `subject`, `description`, and `activeForm`; `TaskGet` and `TaskUpdate` take
  `taskId`; `TaskList` accepts an optional `status_filter`. These tools are
  registered per-agent so they resolve to the agent's own session directory.

### Changed

- **BREAKING**: Split built-in tool `task` into `AsyncSpawn`, `AsyncOutput`,
  `AsyncStop`, `AsyncStatus`, and `AsyncList`. `AsyncSpawn` runs any built-in
  tool in the background (`tool`, `params`, optional `label`); `AsyncOutput`
  reads a task's result with optional blocking (`block`, `timeout`,
  `tail_lines`); `AsyncStop` cancels a running task; `AsyncStatus` returns the
  current status; `AsyncList` lists tasks with optional filters. The previous
  `task` tool and `sub_command`-based schema are removed. Legacy
  `disabled_tools` entries `"task"` and `"async"` disable the entire Async*
  family. Update agent configs, whitelists, and prompts that referenced the old
  name.
- **BREAKING**: Renamed built-in tool `read_file` to `Read`. The tool now
  reports its canonical name as `Read` and its schema uses `file_path`
  (with `offset`, `limit`, and `pages` support). Update agent configs,
  whitelists, and prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `write_file` to `Write`. The schema now
  uses `file_path` instead of `path`; `mode` and `encoding` extensions are
  unchanged. Update agent configs, whitelists, and prompts that referenced
  the old name.
- **BREAKING**: Renamed built-in tool `str_replace_file` to `Edit`. The schema
  now uses `file_path`, `old_string`, `new_string`, and `replace_all` (default
  false); the previous `path` + `edit` object shape is removed. Update agent
  configs, whitelists, and prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `shell` to `Bash`. The schema now uses
  `command`, `description`, `cwd`, `run_in_background`, and `timeout`. Blocking
  execution returns `{ exit_code, stdout, stderr, success }`; background
  execution returns an async task receipt discoverable by the future `Async*`
  family. Update agent configs, whitelists, and prompts that referenced the old
  name.
- **BREAKING**: Split built-in tool `cron` into `CronCreate`, `CronDelete`, and
  `CronList`. `CronCreate` uses `prompt` plus one schedule source (`cron`, `at`,
  `interval_ms`, `idle_ms`, or `event_topic`) and `recurring`/`durable` flags;
  `CronDelete` takes `id`; `CronList` takes no required arguments. The previous
  `sub_command`-based schema is removed. Update agent configs, whitelists, and
  prompts that referenced the old name.
- **BREAKING**: Renamed built-in tool `agent_spawn` to `Agent`. The schema now
  uses `prompt`, `subagent_type`, `description` (renamed from `label`), and
  `model`; `isolated`, `cleanup`, and `parent_session_key` are unchanged.
  `subagent_type` resolves to `~/.peko/agents/<subagent_type>/config.toml` via
  `AgentService`. Update agent configs, whitelists, and prompts that referenced
  the old name.
- **BREAKING**: Renamed the Rust crate from `pekobot` to `peko`. Update all
  `use pekobot::...` imports to `use peko::...`.
- **BREAKING**: Renamed the public Rust type
  `peko::common::types::config::PekobotConfig` to `PekoConfig`.
- **BREAKING**: Renamed the OS keychain service namespace from
  `pekobot-runtime` to `peko-runtime`. Existing keychain entries are not
  migrated; users will need to re-onboard (negligible at 0.1.0).
- Renamed the project name in all prose, documentation, and config defaults
  from "Pekobot" to "Peko". Binary name (`peko`), package layout on disk
  (`config.toml` `app_name = "peko"`), and Python SDK defaults are unaffected.

### Notes

- The MCP example server display name was renamed from
  `pekobot-memory-server` to `peko-memory-server`.
- The Python SDK package was renamed from `pekobot-tool` / `pekobot_tool`
  to `peko-tool` / `peko_tool`.
- Historical CHANGELOG entries below are preserved verbatim.

### Provider catalog & agent decoupling (v3)

This release decouples agent configs from provider/model/API-key
wiring. Pulled registry agents now work on any host with at least one
configured provider; secrets never touch plaintext disk.

#### New: runtime-owned provider catalog (`~/.peko/providers.toml`)

- `src/providers/catalog.rs` — `ProviderCatalog`, `ProviderCatalogEntry`,
  `ModelInfo`, `ApiFormat`. Loaded on startup, shared via
  `Arc<RwLock<…>>`, persisted atomically.
- `src/providers/templates.rs` — `BUILT_IN_TEMPLATES` (15 providers:
  openai, anthropic, ollama, groq, together, fireworks, moonshot,
  deepseek, cohere, openrouter, perplexity, xai, kimi, minimax,
  azure-openai) with curated model lists and known context lengths.
- `src/providers/resolver.rs` — `LlmResolver`. Resolution precedence:
  caller override > session-pinned > agent preference > runtime
  default > first enabled catalog entry. Optional `--bootstrap-env-keys`
  for headless / CI deployments.
- CLI: `peko provider {list, templates, add, remove, set-default,
  get-default, fetch-models}`.

#### New: secure secret store

- `src/common/secret_store.rs` — `SecretStore` trait with two impls:
  - `OsKeychainSecretStore` (production) using the `keyring` crate.
    Service name `"peko"`, account = `provider_id`. Same namespace as
    `peko-desktop`'s `vault/mod.rs`, so desktop-entered keys are
    visible to the runtime after this release.
  - `InMemorySecretStore` (tests only — explicit opt-in, never used in
    production).
- CLI: `peko credential {set, delete, list, test}` with a hidden
  terminal prompt via `rpassword`.
- The legacy plaintext `~/.peko/credentials.json` is no longer
  written. The migration (`migrate_adr_provider_catalog_v3`) reads it
  once on first run, moves every entry into the keychain, and deletes
  the file.

#### New: agent ↔ provider decoupling

- `AgentConfig.version` is bumped to `"3.0"`.
- The embedded `provider: ProviderConfig` field is gone (deleted in
  v3-cleanup, PR #44). Agents carry only `preferred_provider_id` and
  `preferred_model_id` soft hints; the runtime resolves these via
  `LlmResolver` against the catalog (`~/.peko/providers.toml`) and
  the OS keychain at request time. There is no hard binding between
  an agent and a provider.
- New constructors: `Agent::new_with_resolver(config, resolver)` and
  `Agent::new_with_session_manager_and_resolver(...)`. The original
  `Agent::new(config)` continues to work — it falls back to the legacy
  `provider` field so pre-v3 fixtures still compile.

#### Adapter signature refactor (per-call `model_id`)

- `ApiAdapter::build_request` now takes `model_id: &str` as the first
  argument. The `model` field is removed from `OpenAiAdapter`,
  `AnthropicAdapter`, and `OpenAiCompatibleAdapter` — adapters are
  model-agnostic. The model id is threaded per call.
- `ApiAdapter::parse_response(model_id, response)` and
  `parse_sse_event(model_id, data)` carry the model id into the parsed
  `ChatResponse` / `StreamEvent::Start` events.
- `Provider::chat_with_tools(model_id, …)` and
  `stream_with_tools(model_id, …)` thread `model_id` into the adapter.
- `MockAdapter::new()` (no model argument) — model is set per call.

#### Migration (`src/runtime/migration_v3.rs`)

- `migrate_adr_provider_catalog_v3(resolver)`:
  - Walks `~/.peko/agents/*/config.toml`.
  - For any non-default `[provider]` block, creates a matching
    `ProviderCatalogEntry` (if absent) and seeds soft hints.
  - Moves any literal `api_key` into the OS keychain under
    `provider_id`.
  - Bumps `version` to `"3.0"` and atomic-writes the config.
  - Idempotent — already-v3 files are skipped.
- `migrate_legacy_data` now calls the v3 migration after ADR-032/033.
- Verified by `legacy_agent_config_gets_v3_and_hints` and
  `empty_state_reports_zero_migrations`.

#### Registry round-trip hardening

- `AgentConfig` no longer carries a `provider` field at all (deleted in
  v3-cleanup, PR #44), so the OCI config blob embedded by
  `agent_to_registry_manifest` cannot carry a literal `api_key`.
- Legacy `.agent` packages still in flight are stripped
  defensively: re-hydration reads the v3-clean TOML.

#### Desktop (`peko-desktop`)

- `src-tauri/src/ipc/mod.rs` gains `credential_get`, `credential_set`,
  `credential_delete`, `credential_list` IPC clients that proxy to the
  runtime's secret store.
- `src-tauri/src/commands/settings.rs` `credential_*` commands now
  route through IPC rather than reading/writing the desktop's local
  `vault/mod.rs`. The OS keychain is the single source of truth.
- `peko-desktop/src-tauri/src/vault/mod.rs` remains in place for the
  PekoHub-token callers (`pekohub.rs`, `registry.rs`) — they will
  follow in a subsequent change.

#### Mid-session model switching

- Between turns only (per the v3 plan). New CLI flag: `peko send
  --provider X --model Y` (or equivalent SDK/IPC parameter). The
  resolved pair is captured in `SessionState` and reused for every
  LLM call within that turn. In-turn provider swap remains out of
  scope (documented as a future ADR).

### Fixed (issue #26) — Add typed `Principal` caller field to `AuditEvent`

The audit event carried caller identity as a free-form `Option<String>`
(`caller_id`, added by #17), so per-user, per-key, and per-agent audit
queries had to string-parse the legacy `user:{sub}` convention with no
way to distinguish `"user:alice"` from `"apikey:foo"` from
`"agent:helper"`. This change replaces `caller_id` with
`caller: Option<Principal>` — the canonical actor type from ADR-039,
serialized as `{kind, id}` so query code can index on the kind tag.

- **`AuditEvent.caller: Option<Principal>`** replaces
  `caller_id: Option<String>`. Wire format: `{kind, id}` (or
  `{kind: "public"}` for the unit variant). `skip_serializing_if` keeps
  legacy events compact.
- **`Observability::audit_with_caller`** now takes
  `Option<&Principal>` instead of `Option<&str>`. The plain `audit(...)`
  helper is unchanged.
- **`Observability::audit_security_with_caller`** is a new sibling of
  `audit_with_caller` that stamps the caller as a typed `Principal`
  AND marks the event as `AuditSeverity::Security`. Security events
  are the ones operators query by user when investigating an incident
  — leaving them unattributed would defeat the per-user audit query
  use case this PR headlines (issue #26 review feedback).
- **`Principal::from_bridge_user(sub)`** centralizes the tunnel-bridge
  caller projection — `user:` prefix + `"anonymous" → Public`
  special-case — next to the type's other constructors, so the
  `user:` shape can't drift between the dispatcher and `CallerContext::subject()`.
- **Tunnel dispatcher** uses `Principal::from_bridge_user(&caller_user)`
  at the audit emission site (the projection logic is no longer inlined).
- **Cron engine** uses `CallerContext::local().subject()` directly at
  the two `cron.execute` / `cron.result` audit sites (matches the
  canonical `Identity::Local` projection at `caller.rs:114`; the
  intermediate helper has been removed).
- **Test coverage** — new `audit_event_caller_principal_serialization`
  asserts the canonical `{kind, id}` shape round-trips through serde
  for `User`, `Agent`, and `Public` variants, and that `None` callers
  are omitted. New `audit_security_with_caller_records_caller_and_severity`
  pins the security-side migration. New `Principal::from_bridge_user`
  tests cover the `user:` prefix and the `"anonymous" → Public`
  special-case. Existing audit + observability tests updated for the
  field rename.

`PermissionGrant.granted_by` and audit queries on PekoHub itself are
out of scope (parallel PekoHub issue to follow).

### Added (issue #28) — Per-agent persistent `agent_did` over the tunnel

`Principal::Agent(agent.name)` (from #24 / ADR-039) keys agents by the
local name — fine in a single-runtime world, but ambiguous across
runtimes and fragile when an agent is recreated with a new local name.
This change promotes the existing per-agent `Identity` (already
generated and persisted under `KeyStorage` at `peko_home/identities/`)
to a first-class `agent_did` that flows through the tunnel, the
`a2a_send` wire, and `PermissionGrant` lookups.

- **`AgentConfig.agent_did: Option<String>`** — the per-agent stable
  identifier (`did:peko:local:<keyhash>`), persisted in `config.toml`.
  Back-filled on first `Agent::new()` via a new
  `Agent::backfill_agent_did` helper. Two agents with the same `name`
  on two different `peko_home` roots now have different `agent_did`
  values; the unit test `test_two_runtimes_same_name_different_did`
  pins this invariant. New helper `AgentConfig::wire_agent_id()` is a
  thin shim over `Principal::agent_wire_id` (the single source of
  truth for the resolution, including the empty-DID defense).
- **`InstanceAnnouncePayload.agent_did: Option<String>`** — PekoHub
  now stores the per-agent DID on the instance row so it can serve
  as the canonical primary key for `Principal::Agent(...)` callers.
  Wire field is `agentDid` (camelCase) and is omitted when `None` so
  pre-#28 PekoHub instances accept the payload without modification.
- **`A2aSendTool::with_caller_did(name, did)`** — when the calling
  agent has a `agent_did`, the `a2a_send` tool now projects that DID
  into `Principal::Agent(...)` on the wire (instead of the local
  name) so the receiving agent's session is keyed by a stable,
  runtime-independent identifier. The `caller_agent` annotation stays
  as the human-readable name regardless. The legacy `with_caller(name)`
  is preserved and used as the fallback for agents predating #28.
- **`load_or_create_identity` bug fix** — the previous lookup key
  was the agent name, but `KeyStorage::store` names files after the
  DID, so a fresh keypair was being generated on **every** agent
  start. The fix looks up by `config.agent_did` first, falls back
  to the name-keyed legacy path, and only then generates a new
  identity.
- **`Principal::agent_wire_id(Option<&str>, &str) -> String`** — the
  single source of truth for the DID-or-name resolution, with the
  empty-DID guard. `A2aSendTool` and `AgentConfig::wire_agent_id`
  both route through it (review of #34 concern #3).
- **Targeted backfill, not a full overwrite** (review of #34
  concern #1) — `backfill_agent_did` reads the existing
  `config.toml`, sets just the `agent_did` key on the parsed
  `toml::Value`, and re-serializes. Preserves hand-edited comments,
  key ordering, and any fields the parser doesn't know about.
- **Loud DID rotation logging** (review of #34 concern #4) — when
  `load_or_create_identity` recovers from a missing identity file
  (e.g. a backup restore that lost `peko_home/identities/`), the
  caller logs a `warn!` naming **both** the old DID (from
  `config.agent_did`) and the new one (from the freshly generated
  `Identity`) so the operator can correlate audit / grant breakage
  to the event. Cross-runtime grants to the old DID are orphaned
  by design until the follow-up DID-rotation ADR lands.
- **Test-safety guard for subagent path** (review of #34 concern
  #5) — `new_with_shared_executor` skips the on-disk backfill when
  the resolved config path is under `std::env::temp_dir()`, so
  tests that bypass `new_for_test` don't mutate the developer's
  real `~/.peko`. The in-memory identity is still valid; the next
  production-path call does the real backfill.

### Notes for reviewers

- **Name-fallback semantics (deliberate, see issue #28 comment):**
  when an agent has no `agent_did` yet (legacy config, first run
  before the back-fill completes), `a2a_send` falls back to the
  local name. Within a single runtime, the name and DID are
  interchangeable. **Cross-runtime references require a live
  `agent_did` — the name fallback is forgeable across runtimes by
  design.** This is consistent with the `agent_did` field being
  `Option<String>` and the `agentDid` wire field being skipped when
  `None`. DID rotation / key-compromise recovery is a follow-up ADR.
- This PR lands ahead of `ConekoAI/pekohub#11` (PekoHub agent
  enforcement); PekoHub will need to thread the new `agentDid`
  field through to its `instances` row. Pre-#28 PekoHub instances
  that ignore the new field will continue to work; PekoHub upgrades
  that want to enforce per-agent matching must include the field.

### Fixed (issue #17) — Plumb hub-attested user identity through the tunnel path

Pre-#17, the tunnel dispatcher hard-coded the user attribution to the
literal string `"web"` and `MessageRequest::new` defaulted to
`"default"` — so the audit trail, the rate limiter, and per-user tool
permissions all operated on a placeholder. With this change, every
proxied request carries the resolved pekohub user identity from end to
end, with **cryptographic verification** when a JWT is present:

- **Dispatcher** — `resolve_bridge_caller()` reads
  `Authorization: Bearer <jwt>` from the bridge payload first. When a
  `JwtValidator` is configured (via `auth_config.enable_pekohub_jwt`)
  the JWT is signature-verified (RS256 / EdDSA), audience-checked
  against the runtime DID, and expiry-checked, and the validated
  `sub` claim becomes the caller. The validated sub is cross-checked
  against `x-pekohub-user-id` and a mismatch is logged as a possible
  tamper attempt. Falls back to the unverified header only when no
  JWT is present or validation fails. Returns `"anonymous"` only
  when both are absent.
- **Hook layer** — `HookInput::ToolCall` gains a `caller_id: Option<String>`
  field, plumbed through `execute_tool_via_core_with_context` →
  `ToolExecutor::execute` → `HookInput::ToolCall` so every tool
  invocation inside the agentic loop carries the resolved caller.
- **Agentic loop** — `AgenticLoop` carries a `caller_id`, set via
  `with_caller_id()` by `Agent::execute_streaming_with_session`. The
  caller is `Some(user)` for real pekohub users, `None` for local CLI
  invocations and the dispatcher's `"anonymous"` fallback.
- **Audit log** — `AuditEvent` gains a `caller_id: Option<String>` field
  (serialized with `skip_serializing_if = "Option::is_none"` to keep
  legacy events compact). New `Observability::audit_with_caller()`
  helper stamps the resolved caller on every audit event that flows
  through the request path. The tunnel dispatcher now emits a
  `tunnel_proxied_request` audit event tagged with the caller on every
  proxied request.
- **Request defaults** — `MessageRequest::new`, `ExecutionRequest::new`,
  and `SessionManager::new` no longer default `user` to `"default"`.
  The default is now `String::new()`, with a doc comment that
  production callers must set it explicitly via `.with_user()`. The
  two legacy-data fallbacks in `SessionManager::get_or_load_session`
  (peer info missing in the index) and `unified::Session::from_entries`
  (no peer provided) also drop the `"default"` literal — empty
  `sender_id` is the new fallback, distinguishable from a real resolved
  caller.
- **Agentic-loop `run` method** — `engine/agentic_loop.rs:243`'s
  hardcoded `Peer::User("default".to_string())` now uses
  `self.caller_id` (set via `with_caller_id` from the agent service),
  falling back to `Peer::User("local")` for the no-caller local-CLI
  case. The session's `sender_id` is now the resolved caller, not the
  placeholder.

**Why this matters**: unblocks per-user rate limiting
([`src/auth/rate_limit.rs`](src/auth/rate_limit.rs) is already keyed
off `CallerContext`), per-user session scoping
([`src/session/key.rs:97`](src/session/key.rs#L97) keys by `sender_id`),
per-user extension permissions
([`src/extension/core/registry.rs:194-202`](src/extension/core/registry.rs#L194)),
and any future PekoHub→runtime feature that needs to know *which*
user is asking. The JWT wiring closes the "self-asserted header"
security gap called out in issue #17.

**Test plan**:
- All 1413 lib tests pass (3 ignored, 0 failed)
- All 6 `extension_packaging` integration tests pass
- 5 new dispatcher tests for `resolve_bridge_caller` (missing / empty /
  whitespace / non-string / happy)
- 5 new JWT-wiring tests for `resolve_bridge_caller` (signed /
  tampered / no-validator / header-only / case-insensitive header)
- 2 new observability tests for `audit_with_caller`
- 1 new audit serialization test (skip_serializing_if for `None`)
- 1 new `hook_io` test for `HookInput::ToolCall::caller_id`
- `JwtValidator`'s existing 9 unit tests (positive + tampered) still pass

**Note on `src/session/key.rs:201`**: the `"web"` string there is the
*channel* segment of the session key format
(`agent:{agent}:{channel}:{sender_id}`), not user attribution. The
user's identity is keyed via `sender_id`, which is now correctly
plumbed. No change needed.

### Fixed (issue #25) — Collapse IPC `(subject_id, subject_type)` into `subject: Principal`

The IPC `RequestPacket` variants for grant/revoke
(`agent_grant_permission`, `agent_revoke_permission`,
`team_grant_permission`, `team_revoke_permission`) now carry a single
`subject: Principal` field (ADR-039). The legacy two-field shape
(`subject_id: String` + `subject_type: SubjectType`) is accepted on
the wire for one release, with a `warn!` logged once per process per
variant-kind on the legacy path so operators can monitor the
deprecation window. New CLIs only emit `subject`.

**Why this matters**: pre-#25, the `AgentRevokePermission` /
`TeamRevokePermission` packets carried only `subject_id: String` with
no `subject_type`. The server handler hardcoded
`principal_from_string_with_default_user(&subject_id)`, which always
returned `Principal::User(...)`. Since on-disk `PermissionGrant`
stores `subject: Principal` with the proper kind
(e.g. `Principal::Agent("helper")` for an Agent-issued grant), and
the service-layer revoke matches via `g.subject == *subject`,
**revoking any Agent / Team / Public grant via the IPC layer was a
silent no-op** — pinned by three regression tests in
`tests/principal_back_compat.rs`. The fix closes this hole by
collapsing the wire to the canonical `Principal` and routing both
shapes through a single `RequestPacket::resolved_subject()` helper.

- New `RequestPacket::resolved_subject()` helper in
  [`src/ipc/packet.rs`](src/ipc/packet.rs) collapses the canonical
  `subject: Principal` and the legacy `(subject_id, subject_type)`
  pair into a single `Result<Principal, Error>`. Returns an explicit
  `Error` (surfaced as `ResponsePacket::Error` with message
  "missing subject: ...") when neither field is set — strictly
  better than the previous silent no-op.
- All four grant/revoke IPC variants now carry the new `subject`
  field; legacy fields are kept as `Option<...>` with
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so new
  CLI wire bytes stay clean (no `subject_id`/`subject_type` keys
  emitted).
- Server handlers in [`src/ipc/server.rs`](src/ipc/server.rs) no
  longer call `principal_from_string_with_default_user` or
  `principal_from_wire` directly — they call
  `RequestPacket::resolved_subject()` and surface `Err` as
  `ResponsePacket::Error`.
- CLI handlers in
  [`src/commands/agent/handlers.rs`](src/commands/agent/handlers.rs)
  and [`src/commands/team.rs`](src/commands/team.rs) emit the new
  `subject: Some(principal)` shape. CLI UX is unchanged
  (still `--subject <string>` with `"public"` sentinel); the
  `--subject-kind` flag is a follow-up.
- `SubjectType` and `principal_from_wire` are marked
  `#[deprecated]`. Both are still exported for the deprecation
  window and will be removed in the next release after the warning
  logs show no legacy traffic.
- New `tests/scenarios/s6_revoke_principal_collapse_e2e.rs`
  exercises the bug repro end-to-end: an Agent-issued grant +
  revoke via IPC removes the on-disk grant; same for Team grants;
  the legacy wire shape still works; missing-subject returns a
  clean error.
- The three `test_revoke_string_form_*` regression tests in
  [`tests/principal_back_compat.rs`](tests/principal_back_compat.rs)
  are rewritten from "pin the limitation" to "pin the fix": they
  now assert that the new wire resolution correctly matches
  Agent / Team / Public grants and removes them, and that the
  cross-kind guard still holds. Two new tests
  (`test_resolved_subject_missing_subject_errors` and
  `test_resolved_subject_legacy_wire_shape_serde_round_trip`) cover
  the error path and the JSON round-trip.

### Fixed (issue #16) — `peko agent permit` / `pevoke` propagate to PekoHub within ~1s

`peko agent permit <agent> <user> chat` and `peko agent revoke <agent>
<user> chat` now push a fresh `exposure_update` to PekoHub immediately,
instead of silently waiting for the daemon to restart (or for the
agent to be re-created / the tunnel to reconnect). Previously the
grant was persisted to `~/.peko/agents/<name>/config.toml`, but
PekoHub's `canChat` ACL — and the runtime's defense-in-depth
`instance_state.allowed_users` cache — both read from the last
`instance_announce`, so a granted user was denied (or a revoked user
could keep chatting) until the daemon restarted. The revoke path
was the more dangerous half: a *security* failure disguised as a
feature.

- New `TunnelDispatcher::refresh_instance_allowed_users(agent_name)`
  in `src/tunnel/dispatcher.rs` re-derives `allowed_user_ids` from
  the live `AgentConfig.permissions` and re-announces the instance
  to PekoHub, but only if the agent's current exposure is `Private`
  (Public/Unexposed don't carry an `allowed_users` list, and we must
  not silently flip the exposure as a side effect of a permit call).
  No-op if the agent has no cached `instance_state` (tunnel not
  connected) — the next `announce_instances` after `TunnelReady` will
  pick up the latest config.
- `AgentGrantPermission` and `AgentRevokePermission` IPC handlers
  in `src/ipc/server.rs` call `refresh_instance_allowed_users`
  after a successful local config write. The call is best-effort
  and never fails the permit itself; a tunnel outage produces a
  `warn!` log and the next `TunnelReady` round-trip carries the
  new `allowed_users`.
- `TunnelDispatcher::set_instance_exposure` was refactored to
  delegate its tunnel-send step to a new private
  `send_exposure_update` helper, which `refresh_instance_allowed_users`
  also calls — the local state mutation stays in
  `set_instance_exposure` only.
- New `tests/scenarios/s5_live_permit_propagation.rs` regression
  test: starts the daemon, asserts a non-owner user is denied
  (empty `allowedUsers`), runs `peko agent permit` via subprocess,
  asserts the user is allowed within ~1s, runs `peko agent revoke`
  and asserts denial within ~1s, then re-permits and asserts
  re-allowance — all without restarting the daemon. PekoHub's
  `instance.allowedUsers` is also asserted to contain the grantee.
- `peko agent permit --help` and `peko agent revoke --help` help
  text now state the propagation behaviour explicitly.
- The "known production gap" note in
  `tests/scenarios/s4_publish_running_agent_with_permission.rs:68-82`
  is removed and replaced with a pointer to s5 + the issue.

### Fixed (issue #14) — manifest signature verification on import

**`.agent` signature is now verified on unpack.** The packager has always
signed the manifest with the agent's ed25519 DID key on write
(`Packager::sign_manifest` at `src/portable/packager.rs:492`), but the
unpackager never called any verify function on read. A tampered `.agent`
from a registry or mirror would import successfully and the runtime's
per-author trust assumption would be silently broken — the headline
"secure portable agent" claim was false. This change closes the gap.

- New `src/portable/signature.rs` module with
  `verify_manifest_signature(manifest_bytes, did_doc_bytes, allow_unsigned)`.
  Verifies the ed25519 signature in `signatures.manifest` against the
  public key embedded in the package's `identity/did.json`, using the
  same canonical byte reconstruction the packager signs
  (manifest with `signatures.manifest = ""` and `signatures.algorithm = "ed25519"`,
  re-serialized via `to_toml`).
- `Unpackager::import_from_files` now calls signature verification
  *unconditionally* — before `validate_package` — and returns the
  stable error code `[signature_verification_failed]` (with the
  `SignatureError` reason in the message) on failure.
- `--force` no longer bypasses signature verification. Signature is a
  security guarantee, not a format check, and was previously lumped in
  with `validation.is_valid()` under the same `--force` umbrella.
- New `--allow-unsigned-agent` opt-in flag (default `false`) on
  `peko agent import` and `peko agent pull` for users pulling from a
  source they don't fully trust. An *unsigned* package is permitted
  only with this flag; a *badly signed* package is always rejected.
  The flag is `allow_unsigned: bool` on `ImportOptions` /
  `TeamImportOptions` / `AgentImportOptions` and is also threaded
  through the daemon IPC `RequestPacket::AgentImport { allow_unsigned }`.
- The `InvalidSignature` and `DidResolutionFailed` variants in
  `src/portable/validation.rs` are no longer dead code paths conceptually,
  though the unpackager returns a `SignatureError` directly for richer
  error reasons rather than going through `ValidationError`.

**Surfaces two related determinism bugs** (both real, both caught
by the new tests; both fixed in the same change so the signature
gate is actually usable end-to-end):

- `packaging.checksums` was `HashMap<String, String>`. HashMap
  iteration order is randomized per instance, so the packager and
  a round-tripped manifest could serialize the checksums table in
  different orders, producing different bytes for the same manifest
  and breaking signature verification spuriously. Both
  `AgentManifest::PackagingMetadata` and `TeamPackagingMetadata`
  are now `BTreeMap<String, String>` (sorted by key) so the
  canonical signed bytes are stable across the serde round-trip.
  On-disk wire format is unchanged.

- `packaging.files` (a `Vec<String>`) was being appended to in
  insertion order by `AgentManifest::add_file` (called by
  `Packager::export_identity`, `export_config`, `export_skills`,
  `export_workspace`, `export_sessions`). On the round-trip through
  the registry, `AgentRegistry::export_package` re-builds the file
  list from the layer storage and `.sort()`s it. The two paths
  produced different bytes — the packager's signed bytes had the
  file list in insertion order, the registry's re-serialized bytes
  had it sorted — and signature verification failed after any
  push→pull cycle. `add_file` now keeps `packaging.files` sorted
  at all times via `binary_search` + `insert`, so both paths
  produce identical bytes. New regression test
  `manifest_round_trip_produces_identical_bytes` exercises the
  full serde round-trip and asserts byte equality.

- New tests in `tests/cli_agent_signature.rs` (7 tests, all passing):
  - green: signed manifest imports successfully
  - red: tampered manifest byte fails with `signature_verification_failed`
  - red: stripped signature fails (no silent fallback to "unsigned")
  - red: wrong-key signature fails (signed by A, DID doc claims B's key)
  - red: `--force` does NOT bypass signature
  - green: `--allow-unsigned-agent` permits unsigned import
  - byte-stability regression guard pinning `created_at`

### Fixed (issue #8)

**Tunnel reconnect cap and degraded-state surfacing.** Previously, when
PekoHub was permanently unreachable (DNS, network, decommissioned), the
runtime's tunnel client retried forever, producing unbounded log spam and
no operator signal that the relay was down.

- `TunnelClient` now caps consecutive reconnect attempts via
  `max_reconnect_attempts` (default `50`, ≈ 28 min with default backoff).
  After the cap, the client stops retrying and emits a one-shot
  `TunnelStatusUpdate::Degraded` callback.
- New `TunnelStatusUpdate` enum (`Connected` / `Disconnected` / `Degraded`)
  wired into `AppState::start_tunnel`, which now takes a
  `max_reconnect_attempts` parameter and tracks per-attempt state
  (`tunnel_attempts`, `tunnel_last_error`, `tunnel_degraded`).
- New `AppState::tunnel_health() -> TunnelHealth` enum with four states
  (`disabled` / `connected` / `disconnected` / `degraded`).
- New `peko daemon start --max-reconnect-attempts <N>` CLI flag (default 50).
  Pass `4294967295` (u32::MAX) to effectively disable the cap.
- New IPC `RequestPacket::Status` / `ResponsePacket::Status` packet
  returning tunnel health. `peko daemon status --json` now emits
  `tunnel: { state, reconnect_attempts, last_error, degraded }`.
  `stop_tunnel()` clears the degraded flag and per-attempt state.

## [1.0.0-rc1] - Phase 1 Completion - 2026-05-14

Phase 1 of the Pekobot runtime is complete. All P0 success criteria for the agent runtime, unified packaging, registry integration, and CLI have been implemented and verified.

### Phase 1 Summary

**Runtime Engine:**
- Turn-based agentic loop with streaming (`StreamOrchestrator`), tool execution, and session persistence
- 15+ LLM providers via metadata registry (OpenAI, Anthropic, Kimi, Minimax, Ollama, Azure, Cohere, DeepSeek, Fireworks, Groq, OpenRouter, Perplexity, Together, xAI)
- Configurable timeout per LLM request (default 60s, max 3600s)
- Max 10 iterations per turn, gracefully handles API failures and tool timeouts
- 7 integration tests covering RT-001 through RT-006 in `engine::agentic_loop::tests`

**Packaging (ADR-027):**
- Unified `.agent` format: gzip tar with TOML manifest, SHA-256 checksums, content-addressable layers
- `.team` format: checksum-validated, `team.toml` roundtrip, registry layer deduplication
- `.ext` format: extension bundles for offline distribution
- `AgentRegistry` local content-addressable storage in `~/.pekobot/registry/`

**Registry:**
- `pekobot agent push <local> <remote>` / `pekobot agent pull <registry-ref>`
- OCI-inspired protocol with bearer/basic auth, layer existence checks (HEAD), digest verification
- Python FastAPI mock registry server for integration testing

**Extension Framework:**
- 22 hook points across agent lifecycle (`PromptSystemSection` through `AgentIteration`)
- 6 extension types: builtin, skill, MCP, universal, gateway, general
- Dynamic tool registration/unregistration without restart
- Async task execution framework with event bus and queue

**MCP Integration:**
- stdio and SSE transports
- Tool discovery, schema proxying, reserved parameter injection
- Server lifecycle: start on demand, health-check, restart on failure, graceful shutdown

**Session Management:**
- JSONL storage with atomic writes (tmp + rename)
- Branching (`pekobot session branch`), recovery (`SessionRecovery`), maintenance
- Compaction with dual-threshold triggers and structured summaries

**CLI:**
- Core commands: `agent`, `team`, `ext`, `session`, `send`, `daemon`, `system`
- Top-level config CLI (`pekobot config get/set/validate/init/defaults/path`) — ADR-028
- `--json` support on major data commands
- Shell completions via `clap_complete`
- `PEKOBOT_*` environment variables for all global flags

**Security:**
- API key stripping from subprocesses (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`)
- Credential detection in config (partial enforcement)
- DID identity with ed25519 keys

**Test Coverage:**
- 1,024 unit tests passing, 0 failed, 19 ignored
- 60+ PowerShell E2E tests covering agent, session, send, tools, extensions, packaging, cron, A2A, subagent, compaction
- 0 compiler warnings, 0 clippy warnings

### Deferred to Phase 2
- `system doctor` / `system clean` (stubs remain)
- `pekobot validate` command
- `--json` on remaining commands
- MCP Streamable HTTP transport
- Performance benchmarks with baseline data
- Package signing & encryption
- Extension source references (GitHub, URL, MCP endpoint)
- OpenTelemetry export
- Public registry web UI

---

## [0.1.0] - Team Registry Layer Deduplication (Issue 023) - 2026-05-11

Team registry push/pull now uses content-addressable layers instead of a single opaque blob, enabling cross-team agent deduplication.

### Added
- **`LayerType::TeamConfig`** — New layer type for team metadata (agent index) in registry manifests
- **`TeamAgentIndex`** / **`AgentLayerRef`** — Types for the agent → layer digest mapping inside `TeamConfig` layers
- **`TeamLayerBuilder`** (`src/portable/team_layer_builder.rs`) — Decomposes `.team` archives into content-addressable layers
- **`TeamLayerReconstructor`** (`src/portable/team_layer_reconstructor.rs`) — Reconstructs agents from registry layers for direct in-memory import
- **E2E test** — `e2e_tests/packaging/team_registry_dedup.ps1` — Verifies cross-team agent deduplication on mock registry

### Changed
- **`handle_team_push`** (`src/commands/team.rs`) — Now decomposes team into `TeamConfig` + per-agent standard layers (`Config`, `Identity`, `Skills`, etc.) instead of storing a single opaque blob. Shared agents across teams are automatically deduplicated via `RegistryClient::check_existing_layers()`.
- **`handle_team_pull`** (`src/commands/team.rs`) — Now reconstructs agents directly from registry layers without creating a temporary `.team` file. Imports each agent via `Unpackager::import_from_files()`.
- **`LayerType`** — Now implements `Hash` (required for use as `HashMap` key in layer builders)

### Integration Tests
- `portable::team_layer_builder::tests` — 9 tests (basic decomposition, empty team, all layer types, shared content, digest determinism)
- `portable::team_layer_reconstructor::tests` — 6 tests (roundtrip, missing optional layers, empty index, error handling)

---

## [0.1.0] - Packaging System (Phases 1–7) - 2026-05-08

Unified packaging layer with content-addressable storage, registry push/pull, and integrity checks.

### Added
- **`src/portable/`** — Unified packaging layer (merged from `src/image/`)
  - `AgentBuilder` — Build `.agent` packages from source directories with content-addressable layers
  - `AgentRegistry` — Local content-addressable store for layers and manifests
  - `Packager` / `Unpackager` — Export/import `.agent` packages
  - `TeamPackager` / `TeamUnpackager` — Export/import `.team` packages with SHA-256 checksums
  - `ExtensionPackager` / `ExtensionUnpackager` — Export/install `.ext` packages
- **Registry client** — OCI-inspired HTTP push/pull with layer existence checks (HEAD)
- **Mock registry server** — FastAPI-based mock for integration testing ~~(`e2e_tests/mock_registry/`)~~ *(was `e2e_tests/packaging/mock_registry/main.py`; both deleted in Phase A. The Rust integration tests now exercise the real pekohub fixture server at `pekohub/backend/tests/fixtures/server.ts`.)*
- **CLI commands**
  - `pekobot agent build <path> -t <tag>` — Build `.agent` from directory
  - `pekobot agent push <tag>` — Push to registry
  - `pekobot agent pull <ref>` — Pull from registry
  - `pekobot ext export <id> -o <path>` — Export extension to `.ext`

### Changed
- **`AgentManifest` clean manifest** — Stripped of `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Packaging metadata only. `agent.toml` is the single source of truth.
- **`src/image/` deleted** — All functionality merged into `src/portable/`

### Removed
- `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` — Superseded by extension framework

### Integration Tests
- `tests/build_integration.rs` — 3 tests (valid build, missing config, layer deduplication)
- `tests/registry_integration.rs` — 4 tests (manifest roundtrip, blob roundtrip, push+pull, layer skip)
- `tests/team_integration.rs` — 4 tests (checksums, import validation, checksum mismatch, legacy warn)
- `tests/extension_packaging.rs` — 5 tests (export, manifest, install roundtrip, missing ext, checksum mismatch)
- `tests/packaging_integration.rs` — 3 tests (full pipeline, build→import roundtrip, clean manifest verification)

---

## [0.1.0] - Documentation Reorganization - 2026-04-11

Major documentation update to reflect the Unified Extension Architecture (ADR-017) implementation.

### Documentation Restructure ✅

**New Structure:**
- `docs/executive/` - Executive summaries and overviews
- `docs/architecture/` - Technical architecture (OVERVIEW.md, EXTENSION_SYSTEM.md, ADRs)
- `docs/planning/migration/` - Consolidated migration guides
- `docs/archive/` - Historical and superseded documents

**Key Updates:**
- **EXECUTIVE_SUMMARY.md** - Updated, now reflects unified extension architecture with 22 hook points
- **API_SURFACE.md** - Updated, documents new Extension Core and Extension Manager APIs
- **Architecture Overview** - New document documenting post-ADR-017 architecture
- **Extension System Guide** - New comprehensive guide for the unified extension system
- **Migration Guide** - Consolidated migration documentation

### Archived Documents ✅

Moved to `docs/archive/`:
- UNIFIED_ARCHITECTURE_SPEC.md (superseded by new architecture docs)
- ASYNC_INFRASTRUCTURE_COMPARISON.md (historical analysis)
- LEGACY_CODE_AUDIT.md
- PHASE1_ROADMAP.md (retired)

### API Changes

**New APIs:**
- `ExtensionCore` - Central hook registry with 22 hook points
- `ExtensionManager` - Unified extension lifecycle management
- `HookHandler` trait - Extension implementation interface
- `ExtensionTypeAdapter` trait - Type-specific extension adapters

**Removed APIs:**
- `MessageService` (replaced by `StatelessAgentService`)
- `AgentManager` (replaced by `StatelessAgentManager`)
- `SessionResolver` (merged into `SessionManager`)
- `AgentCreationService` (merged into `AgentService`)

---

## [0.1.0] - Phase 1 - 2026-03-18

Phase 1 establishes the **Core Runtime** including agent image/instance model, daemon with HTTP API, session management, built-in tools, team composition, and event bus.

### Milestone 1: HTTP API Server Foundation ✅

**Core infrastructure for the daemon HTTP API.**

- Created `src/api/` module with Axum-based HTTP server
- Implemented `GET /health` and `GET /info` endpoints
- Implemented `X-Pekobot-Version` and `X-Request-ID` headers
- Standard error envelope: `{error: {code, message, request_id, details}}`
- API request/response types with validation
- Graceful shutdown handling

### Milestone 2: Agent Image and Instance Model ✅

**Image/instance distinction with filesystem-first agent definition.**

- `src/image/` module for image manifest management
- `config.toml` loader with validation
- `POST /images/build` with SHA-256 digests
- `.pekobot/registry/images/` content-addressable storage
- Instance pinning to image digest at creation time
- Full instance lifecycle API (`POST /agents`, `GET /agents`, `DELETE /agents`)
- Sessions excluded from images

### Milestone 3: Session Management ✅

**Durable JSONL sessions with atomic writes.**

- Atomic JSONL writes (tmp + rename)
- All 13 event types in JSONL format
- `.index.json` sidecar generation
- `GET /agents/{id}/sessions` and history endpoints
- `POST /agents/{id}/sessions/{id}/branch`
- Session state recovery on daemon restart
- Auto-generated titles from first assistant response

### Milestone 4: Core Runtime and Agentic Loop ✅

**Turn-based agentic loop with sync/async tool calling.**

- `AgentInput` enum: UserMessage, HookTrigger, A2AMessage
- Synchronous tool execution via `TaskManager`
- Asynchronous tool execution via `UnifiedAsyncExecutor`
- Tool timeout handling (120s default)
- Tool panic isolation with `catch_unwind`
- `POST /agents/{id}/chat` with SSE streaming
- WebSocket chat endpoint `ws://localhost:11435/agents/{id}/ws`
- Watch mode (`--watch`) with file watcher
- All 4 LLM providers: Anthropic, OpenAI, Ollama, OpenAI-compatible

### Milestone 5: Built-in Tools Completion ✅

**All 13 required built-in tools with sandboxing.**

| Tool | Description |
|------|-------------|
| `filesystem` | read, write, list, exists, delete, move with path sandboxing |
| `process` | Execute commands with shell blocking, env var stripping |
| `apply_patch` | Atomic file patches with rollback |
| `agent_spawn` | Spawn subagents (sync/async) |
| `agent_spawn_status` | Check subagent status |
| `agent_spawn_list` | List spawned agents |
| `agents_list` | Team-scoped agent listing |
| `agent_info` | Get agent information |
| `sessions_send` | Send messages (with cross-team blocking) |
| `sessions_list` | List sessions |
| `sessions_history` | Get session history |
| `session_status` | Check session status |
| `cron` | 7 sub-commands: at, every, cron, idle, event, list, cancel |

- Path sandboxing enforced (filesystem, apply_patch, process cwd)
- Process env var stripping (`*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`)
- Shell blocking (sh, bash, zsh, cmd, powershell, pwsh)
- `disabled_tools` config support

### Milestone 6: Custom Tools and MCP Integration ✅

**Custom tool discovery and MCP client support.**

- `tools/` directory discovery
- Custom tool JSON protocol (stdin/stdout)
- Optional `<toolname>.json` schema sidecar
- MCP client in `src/mcp/`
- `mcp.json` parsing
- MCP tool discovery (`list_tools`)
- MCP tool call proxying
- MCP server startup failure handling
- Capability resolution order: built-in → local → MCP

### Milestone 7: Team Runtime and Event Bus ✅

**Multi-agent teams with shared services and A2A communication.**

- `team.toml` parser
- `src/team/` module for team management
- `POST /teams` (deploy from config)
- `GET /teams`, `GET /teams/{id}`, `DELETE /teams/{id}`
- In-memory event bus backend
- All 5 A2A message types: Direct, Task, TaskResult, Broadcast, Subscribe
- Shared file workspace
- Shared MCP server reference counting
- `POST /teams/{id}/scale`
- Unified runtime (no separate team runtime)

### Milestone 8: Outbound Hooks and System Events ✅

**Cron, webhook, event, and file_watch hooks.**

- Cron implementation with spec compliance
- `cron.json` persistence
- Missed job handling on restart
- Webhook server in orchestration layer
- `POST /webhooks/{instance_id}/{token}`
- Webhook token validation (constant-time comparison)
- File watcher hook
- Event-triggered hook (event bus integration)
- System event stream `ws://localhost:11435/events`
- Lifecycle events on system stream

### Milestone 9: Registry and Image Distribution ✅

**Image packaging, push/pull, and registry client.**

- OCI-inspired packaging in `src/portable/`
- Layer compression (gzip tar)
- Content-addressable layer storage
- `POST /images/pull` with streaming progress
- `POST /images/push` with streaming progress
- Registry client with bearer token auth
- Registry client with HTTP Basic auth
- Multiple registry sources in `runtime.toml`

### Milestone 10: CLI Completion and Interfaces ✅

**Complete CLI commands and Web UI.**

- CLI uses HTTP API (not direct calls)
- All commands non-interactive
- `--output json` for list/show commands
- Proper exit codes (0 success, non-zero error)
- `pekobot init ./agent/` command
- `pekobot session show <session-id>`
- Web UI embedded HTML at `/ui`
- WebSocket service endpoint
- `--debug` flag for stack traces

### Milestone 11: Security and Hardening ✅

**All security requirements and sandboxing.**

- Process tool strips sensitive env vars (`SENSITIVE_ENV_PATTERNS`)
- Credentials never appear in sessions/logs
- `config.toml` credential detection
- Filesystem path traversal rejection
- Symlink handling in sandbox
- Localhost-only default binding with warning
- Audit logging for all agent actions
- No credential leakage in API responses
- 831 tests passing including 48 security tests

### Milestone 12: Performance Optimization and Testing ✅

**Performance targets and end-to-end use cases.**

- Performance benchmarks (`benches/m12_performance_benchmarks.rs`)
- Performance measurement infrastructure (`PerformanceMetrics`, `LatencyStats`)
- `GLOBAL_METRICS` singleton
- Performance hooks in critical paths
- Metrics API endpoint (`GET /metrics/performance`)
- Use case tests for UC-001 through UC-005
- Concurrent instance stress test (50 instances)
- Comprehensive test coverage for M12 components

**Performance Targets:**
| Metric | Target | Status |
|--------|--------|--------|
| Cold Start | < 500ms | Framework Ready |
| Warm Start | < 100ms | Framework Ready |
| First Token | < 500ms | Framework Ready |
| Tool Latency | < 5ms | Framework Ready |
| Concurrent Instances | 50 stable | Framework Ready |
| Team Deploy | < 30s | Framework Ready |

### Milestone 13: Documentation and Polish ✅

**Complete documentation and Phase 1 finalization.**

- Updated Getting Started guide (`docs/getting-started/GETTING_STARTED.md`)
- Error codes reference with fix suggestions (`docs/reference/ERROR_CODES.md`)
- `--help` examples for all CLI commands
- API usage examples (`docs/api-examples.md`)
- Contributor guide (`docs/dev/CONTRIBUTOR_GUIDE.md`)
- Phase 1 CHANGELOG (this file)
- Review and documentation of [SHOULD] item deferrals

---

## Deferred Items (Phase 2/3)

The following items from the specification are explicitly deferred:

| Item | Phase | Reason |
|------|-------|--------|
| Control Plane (lifecycle policies, scheduling) | Phase 2 | Runtime foundation needed first |
| Resource enforcement (cgroups) | Phase 2 | Requires control plane |
| Capability package manager (`pekobot install`) | Phase 3 | Ecosystem maturity needed |
| Auto-install from dependencies | Phase 3 | Requires package manager |
| Redis/NATS bus backends | Phase 2 | In-memory sufficient for single-node |
| Session plugins | Phase 2 | Can use raw sessions initially |
| Package signing | Phase 2 | Verification warning mode acceptable |
| TUI (`pekobot-tui`) | Phase 2 | Web UI sufficient for Phase 1 |
| Base image inheritance | Phase 2 | Can use explicit config copying |
| Session branching UI | Phase 2 | API exists, CLI can be added later |

---

## Statistics

- **Total commits:** ~500+
- **Lines of code:** ~50,000
- **Test coverage:** 80%+
- **Documentation pages:** 15+
- **Milestones completed:** 13
- **Duration:** 21 weeks

---

## Contributors

Thank you to everyone who contributed to Phase 1!

---

## License

MIT License - See [LICENSE](../LICENSE) for details.
