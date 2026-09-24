//! `ModelCall` builtin (ADR-061 D3/D4) — one-shot inference primitive:
//! **no session persistence, no tool-calling loop, no streaming**.
//!
//! Two modes (exactly one per call):
//!
//! - **Completion** (`{model?, prompt, system?, max_tokens?, temperature?}`):
//!   a single non-streaming chat completion built with `tools: None`,
//!   returning `{text, model, usage}`.
//! - **Judgment** (`{model?, state, questions}`): POSTs
//!   `{model, state, questions}` to the resolved catalog entry's
//!   `{base_url}/v1/evaluate` (Vercel AI Gateway / TypeSafe-compatible
//!   decision API) and returns the typed answers verbatim. Only valid
//!   against entries whose `ModelSpec` declares `decisions: true` (D4);
//!   the chat-shaped `ApiAdapter` contract is deliberately not involved.
//!
//! ## Attribution + metering
//!
//! The tool executes with the caller's `ToolContext` and resolves the
//! calling principal **server-side** from `ctx.principal_name` (never
//! from tool params), then:
//!
//! - resolves the model through the daemon-global `LlmResolver`
//!   (`model` param = explicit override, else the principal's
//!   `preferred_model_id`) and its credential through the same vault
//!   chain `LlmResolver::build_provider` uses,
//! - refuses the call when there is no attributable principal (an
//!   unmetered LLM call is never allowed),
//! - completion mode applies the subagent-style `cost_per_call_max`
//!   pre-flight ([`estimate_spawn_cost_usd`]) against the entry's
//!   `PricingHint` before any traffic,
//! - charges the principal's `QuotaMeter` **after** the call from
//!   provider-reported usage — never from request params — folding USD
//!   cost via [`compute_cost_usd`] so `budget_per_cycle` applies.
//!
//! Charging is a direct `QuotaMeter::charge_with_cost`, not a
//! `QuotaScope` + `StackedMeteredProvider` wrap: inside an agentic turn
//! the loop already opened a scope holding this same meter, and
//! `from_current_scope` would charge it twice.
//!
//! ## Registration
//!
//! Registered once on the daemon-global `ExtensionCore` under the
//! system scope by `daemon::state` (it needs the `PrincipalManager`
//! handle, which does not exist yet when
//! `ToolRuntime::register_builtins` runs). Both the agentic loop and
//! the ADR-061 `ExecuteTool` IPC path reach it through the F37 funnel;
//! capability-gated by `tool:ModelCall` like any other built-in.

use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde_json::{json, Value};

use peko_engine::compute_cost_usd;
use peko_message::{ContentBlock, TokenUsage};
use peko_provider_api::ChatOptions;
use peko_providers::catalog::ModelConfig;
use peko_providers::resolver::{LlmResolver, ResolveRequest, ResolvedChoice};
use peko_quota::QuotaMeter;
use peko_tools_core::{Tool, ToolContext, ToolError};

use crate::agents::subagent_executor::estimate_spawn_cost_usd;
use crate::extensions::framework::types::ToolExposure;
use crate::principal::manager::PrincipalManager;
use crate::principal::Principal;

/// Synthetic tool name surfaced to the LLM. Single source of truth so
/// registration sites (daemon) and tests don't drift.
pub const MODEL_CALL_TOOL_NAME: &str = "ModelCall";

/// Judgment-mode endpoint path, appended to the catalog entry's
/// `base_url` (ADR-061 D4; Vercel AI Gateway `/v1/evaluate` shape).
const JUDGMENT_PATH: &str = "/v1/evaluate";

/// Judgment calls are fast (tens of ms per ADR-061 §1.2); the timeout
/// only guards against a wedged endpoint.
const JUDGMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// `ModelCall` builtin — one-shot completion or structured judgment.
///
/// Holds a `Weak<PrincipalManager>` (not `Arc`) so the tool does not
/// extend the manager's lifetime past the daemon. All per-principal
/// state (quota meter, `preferred_model_id`) and the daemon-global
/// `LlmResolver` are reached through it at execute time.
pub struct ModelCallTool {
    principals: Weak<PrincipalManager>,
    /// Client for the judgment-mode POST. Built once (connection pool
    /// reused); only judgment mode touches it — completion goes
    /// through the resolver-built `Provider`.
    http: reqwest::Client,
}

impl ModelCallTool {
    /// Construct with a weak handle to the daemon's principal manager.
    #[must_use]
    pub fn new(principals: Weak<PrincipalManager>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(JUDGMENT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { principals, http }
    }

    /// Resolve the calling principal and the daemon-global resolver.
    ///
    /// Fail-closed: without an attributable principal there is no
    /// meter to charge, and ModelCall never runs unmetered.
    async fn resolve_caller(
        &self,
        ctx: &ToolContext,
    ) -> anyhow::Result<(Arc<Principal>, Arc<LlmResolver>)> {
        let name = ctx
            .principal_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "ModelCall requires a calling-principal context \
                     (ToolContext.principal_name is unset); refusing to run an \
                     unmetered LLM call"
                )
            })?;
        let manager = self.principals.upgrade().ok_or_else(|| {
            anyhow!(
                "ModelCall is not wired to a PrincipalManager on this runtime; \
                 the tool is only available in daemon mode"
            )
        })?;
        let principal = manager.get_by_name(name).await.ok_or_else(|| {
            anyhow!("ModelCall: unknown principal '{name}'; refusing an unattributed LLM call")
        })?;
        let resolver = manager.llm_resolver().ok_or_else(|| {
            anyhow!("ModelCall: no LlmResolver is bound to this runtime; cannot resolve models")
        })?;
        Ok((principal, resolver))
    }

    /// Resolve the target model: explicit `model` param wins, else the
    /// principal's `preferred_model_id` (the runtime's standard
    /// precedence, minus the session tier — ModelCall is sessionless).
    async fn resolve_model(
        resolver: &LlmResolver,
        principal: &Principal,
        model: Option<&str>,
    ) -> anyhow::Result<ResolvedChoice> {
        let preferred = principal.config.read().await.preferred_model_id.clone();
        resolver
            .resolve(ResolveRequest {
                override_model: model,
                agent_model: preferred.as_deref(),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("ModelCall: model resolution failed: {e}"))
    }

    /// Completion mode: one non-streaming chat completion with
    /// `tools: None`, metered from provider-reported usage.
    async fn run_completion(
        &self,
        resolver: &LlmResolver,
        meter: &QuotaMeter,
        choice: &ResolvedChoice,
        prompt: &str,
        system: Option<&str>,
        max_tokens: Option<u32>,
        temperature: Option<f64>,
    ) -> anyhow::Result<Value> {
        // Pre-flight: same `cost_per_call_max` gate subagent spawns get,
        // against the conservative 4K-in + 1K-out projection.
        pre_flight_cost_ceiling(meter, &choice.config)?;

        let provider = resolver
            .build_provider(&choice.config)
            .await
            .with_context(|| {
                format!(
                    "ModelCall: failed to build provider for '{}'",
                    choice.config.id
                )
            })?;
        let options = ChatOptions {
            temperature: temperature.map(|t| t as f32),
            max_tokens,
            ..Default::default()
        };
        let response = provider
            .chat_response_with_options(system, prompt, &choice.model_id, &options)
            .await
            .with_context(|| {
                format!(
                    "ModelCall: completion against '{}' failed",
                    choice.config.id
                )
            })?;

        let text: String = response
            .content
            .iter()
            .filter_map(|cb| match cb {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();

        // Server-side charge from provider-reported usage (never from
        // request params). A quota trip here fails the call — the
        // tokens were spent, and the meter reflects that.
        charge_meter(meter, &choice.config, &response.usage).await?;

        Ok(json!({
            "mode": "completion",
            "text": text,
            "model": choice.config.id,
            "usage": usage_json(&response.usage),
        }))
    }

    /// Judgment mode: POST `{model, state, questions}` to the entry's
    /// `{base_url}/v1/evaluate` and pass the typed answers through.
    async fn run_judgment(
        &self,
        resolver: &LlmResolver,
        meter: &QuotaMeter,
        choice: &ResolvedChoice,
        state: Value,
        questions: Value,
    ) -> anyhow::Result<Value> {
        let config = &choice.config;
        if !config.spec.is_some_and(|s| s.decisions) {
            bail!(
                "ModelCall judgment mode requires a judgment-class model: catalog entry \
                 '{}' does not declare `decisions` in its ModelSpec. Hand-edit \
                 `models.toml` to set `spec.decisions = true` on a decision-API entry \
                 (ADR-061 D4).",
                config.id
            );
        }

        // Credential through the identical chain `build_provider` uses
        // (credential provider → secret store → empty when the entry
        // declares `requires_key = false`). Never logged.
        let api_key = resolver
            .resolve_api_key(config)
            .with_context(|| format!("ModelCall: no credential available for '{}'", config.id))?;

        let url = format!("{}{}", config.base_url.trim_end_matches('/'), JUDGMENT_PATH);
        let body = json!({
            "model": choice.model_id,
            "state": state,
            // `questions` is an object map keyed by question id; passed
            // through verbatim so the wire format tracks the decision
            // API 1:1.
            "questions": questions,
        });
        let mut request = self.http.post(&url).json(&body);
        {
            let key = api_key.expose_secret();
            if !key.is_empty() {
                request = request.bearer_auth(key);
            }
        }
        // Per-model catalog headers (tenant ids, beta flags) apply to
        // the judgment endpoint exactly as the provider factory would
        // apply them to a chat endpoint.
        for (name, value) in &config.headers {
            request = request.header(name, value);
        }

        let response = request
            .send()
            .await
            .map_err(|e| anyhow!("ModelCall: judgment request to '{url}' failed: {e}"))?;
        let status = response.status();
        let body_text = response.text().await.map_err(|e| {
            anyhow!("ModelCall: reading judgment response from '{url}' failed: {e}")
        })?;
        if !status.is_success() {
            let excerpt: String = body_text.chars().take(500).collect();
            bail!("ModelCall: judgment endpoint '{url}' returned HTTP {status}: {excerpt}");
        }
        let parsed: Value = serde_json::from_str(&body_text).with_context(|| {
            format!("ModelCall: judgment response from '{url}' is not valid JSON")
        })?;

        // Lenient parse: the public decision-API docs pin the boolean
        // shape (`probability` 0..1) but not the choice/score response
        // fields, so per-question answers pass through verbatim —
        // `probability` rides along inside each answer when present.
        let answers = parsed
            .get("answers")
            .filter(|a| a.is_object())
            .cloned()
            .ok_or_else(|| {
                anyhow!("ModelCall: judgment response from '{url}' is missing the `answers` object")
            })?;

        // Judgment APIs bill input only (ADR-061 D4); when the response
        // carries a usage block, charge exactly what it reports. When it
        // carries none, only the meter's request counter advances —
        // synthesizing token counts from request params is forbidden.
        let usage = parse_judgment_usage(parsed.get("usage")).unwrap_or_default();
        charge_meter(meter, config, &usage).await?;

        let mut out = json!({
            "mode": "judgment",
            "model": config.id,
            "answers": answers,
        });
        if parsed.get("usage").is_some() {
            out["usage"] = usage_json(&usage);
        }
        Ok(out)
    }
}

impl std::fmt::Debug for ModelCallTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelCallTool").finish_non_exhaustive()
    }
}

#[async_trait]
impl Tool for ModelCallTool {
    fn name(&self) -> &'static str {
        MODEL_CALL_TOOL_NAME
    }

    fn description(&self) -> String {
        "One-shot LLM call with no session, no tool loop, and no streaming. \
         Two modes (exactly one per call): completion (`prompt`, optional \
         `system` / `max_tokens` / `temperature`) returns `{text, model, usage}`; \
         judgment (`state` + `questions`) POSTs to a judgment-class model's \
         decision API and returns typed answers with probabilities. \
         Judgment mode requires a catalog entry whose spec declares \
         `decisions: true`. `model` defaults to this principal's preferred model. \
         Use when: cheap inline classification or a single completion without \
         spawning a subagent (e.g. from a workflow). Don't use when: you need \
         tool use, multi-turn context, or streaming — that's a full turn. \
         Every call charges this principal's quota meter."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "model": {
                    "type": "string",
                    "description": "Catalog id of a configured model (see model_list). Defaults to the calling principal's preferred model."
                },
                "prompt": {
                    "type": "string",
                    "description": "Completion mode: the user prompt for a one-shot chat completion. Mutually exclusive with `state`/`questions`."
                },
                "system": {
                    "type": "string",
                    "description": "Completion mode only: optional system prompt prepended to the call."
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Completion mode only: maximum output tokens."
                },
                "temperature": {
                    "type": "number",
                    "description": "Completion mode only: sampling temperature."
                },
                "state": {
                    "type": ["string", "object"],
                    "description": "Judgment mode: unstructured state (string or object) for the judgment model to evaluate. Requires `questions`; only valid against models whose spec declares `decisions: true`."
                },
                "questions": {
                    "type": "object",
                    "description": "Judgment mode: map of question key to question spec (`{\"type\": \"boolean\"|\"choice\"|\"score\", \"instructions\": \"...\"}`, plus `options` for choice or `min`/`max` for score). Passed through to the judgment API verbatim."
                }
            },
            "additionalProperties": false
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::Direct
    }

    fn parallelizable(&self) -> bool {
        // Stateless HTTP calls; the meter serializes its own counters.
        true
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        // The funnel always routes through `execute_with_context`;
        // without a ToolContext there is no principal to attribute and
        // meter the call to, so the bare entry point refuses.
        Err(ToolError::Other(
            "ModelCall requires a ToolContext (principal attribution); \
             invoke it through the extension funnel, not Tool::execute"
                .to_string(),
        )
        .into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        if ctx.is_aborted() {
            return Err(ToolError::Aborted.into());
        }

        // ── 1. Mode resolution ───────────────────────────────────────
        let model = params.get("model").and_then(Value::as_str);
        let prompt = params.get("prompt").and_then(Value::as_str);
        let system = params.get("system").and_then(Value::as_str);
        let max_tokens = params.get("max_tokens").and_then(Value::as_u64);
        let temperature = params.get("temperature").and_then(Value::as_f64);
        let state = params.get("state").cloned();
        let questions = params.get("questions").cloned();

        let completion_mode = prompt.is_some();
        let judgment_mode = state.is_some() || questions.is_some();
        match (completion_mode, judgment_mode) {
            (true, true) => bail!(
                "ModelCall: `prompt` (completion) and `state`/`questions` (judgment) \
                 are mutually exclusive — pick exactly one mode per call"
            ),
            (false, false) => bail!(
                "ModelCall: missing mode — pass `prompt` for a one-shot completion \
                 or `state` + `questions` for a judgment"
            ),
            _ => {}
        }
        if judgment_mode {
            if system.is_some() || max_tokens.is_some() || temperature.is_some() {
                bail!(
                    "ModelCall: `system`, `max_tokens`, and `temperature` only apply to \
                     completion mode (`prompt`); judgment mode takes `state` + `questions`"
                );
            }
            if state.is_none() || questions.is_none() {
                bail!("ModelCall: judgment mode requires both `state` and `questions`");
            }
            match questions.as_ref() {
                Some(q) if q.as_object().is_some_and(|m| !m.is_empty()) => {}
                _ => bail!("ModelCall: `questions` must be a non-empty object map"),
            }
        }

        // ── 2. Caller attribution (server-side) + model resolution ──
        let (principal, resolver) = self.resolve_caller(ctx).await?;
        let choice = Self::resolve_model(&resolver, &principal, model).await?;
        let meter = Arc::clone(&principal.quota_meter);

        // ── 3. Dispatch ──────────────────────────────────────────────
        let result = if completion_mode {
            let max_tokens = max_tokens
                .map(u32::try_from)
                .transpose()
                .map_err(|_| anyhow!("ModelCall: `max_tokens` exceeds u32 range"))?;
            self.run_completion(
                &resolver,
                &meter,
                &choice,
                prompt.unwrap_or_default(),
                system,
                max_tokens,
                temperature,
            )
            .await?
        } else {
            self.run_judgment(
                &resolver,
                &meter,
                &choice,
                state.unwrap_or(Value::Null),
                questions.unwrap_or(Value::Null),
            )
            .await?
        };
        Ok(result)
    }
}

/// Completion-mode pre-flight against `cost_per_call_max`, mirroring
/// the subagent spawn gate (`pre_flight_cost_ceiling` in
/// `agents::subagent_executor`): refuse before any LLM traffic when the
/// conservative 4K-in + 1K-out projection against the entry's
/// `PricingHint` exceeds the ceiling. No-op when either side is
/// unconfigured.
fn pre_flight_cost_ceiling(meter: &QuotaMeter, config: &ModelConfig) -> anyhow::Result<()> {
    let Some(ceiling) = meter.config().cost_per_call_max else {
        return Ok(());
    };
    let Some(pricing) = config.spec.and_then(|s| s.pricing) else {
        return Ok(());
    };
    let estimated = estimate_spawn_cost_usd(&pricing);
    if estimated > ceiling {
        bail!(
            "ModelCall refused by cost ceiling: estimated ${estimated:.6} for model '{}' \
             exceeds cost_per_call_max ${ceiling}",
            config.id
        );
    }
    Ok(())
}

/// Charge the calling principal's meter from provider-reported usage,
/// folding USD cost (from the entry's `PricingHint`) alongside the
/// token counters so `budget_per_cycle` applies. A limit trip fails
/// the call after the fact — the spend is real and recorded.
async fn charge_meter(
    meter: &QuotaMeter,
    config: &ModelConfig,
    usage: &TokenUsage,
) -> anyhow::Result<()> {
    let cost = compute_cost_usd(
        config.spec.and_then(|s| s.pricing),
        usage.input,
        usage.output,
    );
    meter
        .charge_with_cost(usage, cost)
        .await
        .map_err(|e| anyhow!("ModelCall charge rejected by quota meter: {e}"))
}

/// Project a `TokenUsage` into the tool-result wire shape.
fn usage_json(usage: &TokenUsage) -> Value {
    json!({
        "input": usage.input,
        "output": usage.output,
        "total": usage.total,
    })
}

/// Lenient judgment-response usage extraction. Accepts the common
/// spellings (`input_tokens` / `prompt_tokens` / `input`,
/// `output_tokens` / `completion_tokens` / `output`); returns `None`
/// when the response carries no `usage` block at all.
fn parse_judgment_usage(raw: Option<&Value>) -> Option<TokenUsage> {
    let usage = raw?;
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| usage.get(*k).and_then(Value::as_u64))
    };
    let input = pick(&["input_tokens", "prompt_tokens", "input"]).unwrap_or(0);
    let output = pick(&["output_tokens", "completion_tokens", "output"]).unwrap_or(0);
    Some(TokenUsage {
        input,
        output,
        total: input + output,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use peko_auth::Subject;
    use peko_extension_api::Capabilities;
    use peko_message::MessageRole;
    use peko_providers::catalog::{ApiFormat, ModelCatalog, ModelCatalogFile};
    use peko_providers::secret_store::InMemorySecretStore;
    use peko_providers::spec::{ModelSpec, PricingHint};
    use peko_providers::MockAdapter;
    use peko_quota::QuotaConfig;
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn principal_config(
        name: &str,
        preferred_model: Option<&str>,
        quota: Option<QuotaConfig>,
    ) -> crate::principal::PrincipalConfig {
        crate::principal::PrincipalConfig {
            name: name.to_string(),
            id: None,
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Capabilities::starter_bundle(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![],
            preferred_model_id: preferred_model.map(str::to_string),
            quota,
            children: Default::default(),
        }
    }

    /// Build a PrincipalManager (in a tempdir) with the given resolver
    /// bound, containing one principal with the given preferred model
    /// and quota. Returns the tempdir guard alongside so the fixture
    /// stays alive for the test's duration.
    async fn manager_with_principal(
        temp: &TempDir,
        name: &str,
        preferred_model: Option<&str>,
        quota: Option<QuotaConfig>,
        resolver: Arc<LlmResolver>,
    ) -> Arc<PrincipalManager> {
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let manager = PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver);
        manager
            .create(principal_config(name, preferred_model, quota))
            .await
            .expect("create principal");
        Arc::new(manager)
    }

    fn ctx_for(principal_name: &str) -> ToolContext {
        ToolContext::default_for_tool(MODEL_CALL_TOOL_NAME).with_principal_name(principal_name)
    }

    fn generous_quota() -> QuotaConfig {
        QuotaConfig {
            input_tokens: Some(100_000_000),
            output_tokens: Some(100_000_000),
            request_count: Some(1_000),
            ..Default::default()
        }
    }

    // ── Mock HTTP server for judgment-mode tests ─────────────────────

    struct RecordedHttp {
        request_line: String,
        authorization: Option<String>,
        body: String,
    }

    /// Minimal one-shot-per-connection HTTP server on loopback. Returns
    /// `response_body` (JSON) with a 200 for every request and records
    /// `(request line, authorization header, body)` for assertions.
    /// Avoids a wiremock/httpmock dev-dependency (ADR-061: no new
    /// crates); the surface we need is a single POST.
    async fn serve_json(
        response_body: String,
    ) -> (
        std::net::SocketAddr,
        Arc<std::sync::Mutex<Vec<RecordedHttp>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let addr = listener.local_addr().expect("local addr");
        let received: Arc<std::sync::Mutex<Vec<RecordedHttp>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let received_w = Arc::clone(&received);
        let handle = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return; // connection closed before headers
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                    if buf.len() > 64 * 1024 {
                        return; // unreasonably large headers; bail
                    }
                };
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let header_value = |name: &str| {
                    headers.lines().find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        if k.trim().eq_ignore_ascii_case(name) {
                            Some(v.trim().to_string())
                        } else {
                            None
                        }
                    })
                };
                let content_length: usize = header_value("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < header_end + content_length {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length])
                    .to_string();
                received_w
                    .lock()
                    .expect("received lock")
                    .push(RecordedHttp {
                        request_line: headers.lines().next().unwrap_or("").to_string(),
                        authorization: header_value("authorization"),
                        body,
                    });
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (addr, received, handle)
    }

    /// Catalog entry for a judgment-class model pointing at the mock
    /// server. `credential` controls the auth wiring: `Some(id)` pairs
    /// `credential_id` with `requires_key: true`; `None` models a local
    /// keyless endpoint.
    fn judgment_entry(addr: std::net::SocketAddr, credential: Option<&str>) -> ModelConfig {
        ModelConfig {
            id: "jev".to_string(),
            display_name: "Jev".to_string(),
            template_id: None,
            api_format: ApiFormat::OpenaiCompletions,
            base_url: format!("http://{addr}"),
            model_id: "jev-1".to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: BTreeMap::new(),
            credential_id: credential.map(str::to_string),
            requires_key: credential.is_some(),
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
            spec: Some(ModelSpec {
                decisions: true,
                pricing: Some(PricingHint {
                    input_per_million: Some(2.0),
                    output_per_million: None,
                }),
                ..ModelSpec::default()
            }),
            note: None,
        }
    }

    /// Resolver backed by an in-memory catalog + secret store holding
    /// the judgment entry (and its credential when the entry declares
    /// one). No mock adapter: judgment mode never builds a `Provider`.
    async fn judgment_resolver(entry: ModelConfig) -> Arc<LlmResolver> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.toml");
        let mut entries = BTreeMap::new();
        let credential_id = entry.credential_id.clone();
        entries.insert(entry.id.clone(), entry);
        let file = ModelCatalogFile {
            version: "1".to_string(),
            entries,
        };
        std::fs::write(&path, toml::to_string(&file).expect("serialize catalog"))
            .expect("write catalog");
        let catalog = ModelCatalog::load_or_init(&path)
            .await
            .expect("load catalog");
        let secrets: Arc<dyn peko_providers::secret_store::SecretStore> =
            match credential_id.as_deref() {
                Some(id) => Arc::new(InMemorySecretStore::from_pairs(&[(id, "test-key-123")])),
                None => Arc::new(InMemorySecretStore::default()),
            };
        Arc::new(LlmResolver::new(catalog, secrets))
    }

    // ── Completion mode ──────────────────────────────────────────────

    /// Completion mode against the MockAdapter: text + usage returned,
    /// the principal's meter charged from provider-reported usage, and
    /// the default model comes from the principal's
    /// `preferred_model_id` when `model` is omitted.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn completion_returns_text_and_charges_meter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        adapter.queue_text("hello one-shot");
        let (resolver, adapter) = LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let out = tool
            .execute_with_context(
                json!({"prompt": "Say hi", "system": "Be brief."}),
                &ctx_for("caller"),
            )
            .await
            .expect("completion");

        assert_eq!(out["mode"], "completion");
        assert_eq!(out["text"], "hello one-shot");
        assert_eq!(out["model"], "mock", "catalog id, not the wire id");
        assert!(out["usage"]["output"].as_u64().unwrap_or(0) > 0);

        // The request carried no tools and placed the system prompt first.
        let recorded = adapter.recorded_requests();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].tools.is_empty(), "tools: None on the wire path");
        assert_eq!(recorded[0].messages.len(), 2);
        assert!(matches!(recorded[0].messages[0].role, MessageRole::System));
        assert!(matches!(recorded[0].messages[1].role, MessageRole::User));

        // The calling principal's meter was charged server-side.
        let meter = &manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .quota_meter;
        let snap = meter.snapshot();
        assert_eq!(snap.request_count, 1);
        assert!(snap.output_tokens > 0, "provider-reported usage charged");
    }

    /// Explicit `model` overrides the principal default.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn completion_explicit_model_overrides_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        adapter.queue_text("override ok");
        let (resolver, _adapter) =
            LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        // Principal prefers a (nonexistent) default; the explicit
        // `mock` override must win resolution.
        let manager =
            manager_with_principal(&temp, "caller", None, Some(generous_quota()), resolver).await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let out = tool
            .execute_with_context(json!({"model": "mock", "prompt": "hi"}), &ctx_for("caller"))
            .await
            .expect("completion with override");
        assert_eq!(out["text"], "override ok");
        assert_eq!(out["model"], "mock");
    }

    /// The `cost_per_call_max` pre-flight refuses completion before any
    /// provider traffic when the conservative projection exceeds the
    /// ceiling, and the meter stays untouched.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn completion_preflight_refuses_over_cost_ceiling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        adapter.queue_text("should never be consumed");
        let (resolver, adapter) = LlmResolver::mock(adapter, temp.path().join("models.toml")).await;

        // Give the mock entry an aggressive pricing hint so the 4K-in +
        // 1K-out projection clears the tiny ceiling: $100/Mtok both
        // ways ⇒ $0.40 + $0.10 = $0.50 per call.
        let mut entry = resolver.catalog().get("mock").await.expect("mock entry");
        entry.spec = Some(ModelSpec {
            pricing: Some(PricingHint {
                input_per_million: Some(100.0),
                output_per_million: Some(100.0),
            }),
            ..ModelSpec::default()
        });
        resolver
            .catalog()
            .upsert(entry)
            .await
            .expect("upsert pricing");

        let quota = QuotaConfig {
            cost_per_call_max: Some(0.0001),
            ..generous_quota()
        };
        let manager =
            manager_with_principal(&temp, "caller", Some("mock"), Some(quota), resolver).await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let err = tool
            .execute_with_context(json!({"prompt": "hi"}), &ctx_for("caller"))
            .await
            .expect_err("pre-flight must refuse");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cost_per_call_max"),
            "error names the knob: {msg}"
        );

        // No provider traffic, no charge.
        assert!(adapter.recorded_requests().is_empty());
        let snap = manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .quota_meter
            .snapshot();
        assert_eq!(snap.request_count, 0);
        assert_eq!(snap.output_tokens, 0);
    }

    // ── Judgment mode ────────────────────────────────────────────────

    /// Judgment mode POSTs `{model, state, questions}` to
    /// `{base_url}/v1/evaluate` with Bearer auth from the vault, passes
    /// the answers JSON through verbatim, and charges the meter from
    /// the response's usage block.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn judgment_posts_evaluate_and_passes_answers_through() {
        let response_body = json!({
            "answers": {
                "continueWorking": {"answer": true, "probability": 0.92},
                "priority": {"answer": "high", "probability": 0.7}
            },
            "usage": {"input_tokens": 1200}
        })
        .to_string();
        let (addr, received, _server) = serve_json(response_body).await;

        let resolver = judgment_resolver(judgment_entry(addr, Some("jev-cred"))).await;
        let temp = tempfile::tempdir().expect("tempdir");
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("jev"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let questions = json!({
            "continueWorking": {"type": "boolean", "instructions": "Is there pending work?"},
            "priority": {"type": "choice", "options": ["high", "low"], "instructions": "How urgent?"}
        });
        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let out = tool
            .execute_with_context(
                json!({"state": "deploy failed twice", "questions": questions}),
                &ctx_for("caller"),
            )
            .await
            .expect("judgment");

        // The request hit /v1/evaluate with Bearer auth and the exact
        // wire body (questions passed through 1:1).
        let (request_line, authorization, sent_body) = {
            let recorded = received.lock().expect("received lock");
            assert_eq!(recorded.len(), 1);
            (
                recorded[0].request_line.clone(),
                recorded[0].authorization.clone(),
                recorded[0].body.clone(),
            )
        };
        assert_eq!(request_line, "POST /v1/evaluate HTTP/1.1");
        assert_eq!(authorization.as_deref(), Some("Bearer test-key-123"));
        let sent: Value = serde_json::from_str(&sent_body).expect("request body json");
        assert_eq!(sent["model"], "jev-1", "wire model id, not catalog id");
        assert_eq!(sent["state"], "deploy failed twice");
        assert_eq!(sent["questions"], questions);

        // Answers pass through verbatim (probability rides along).
        assert_eq!(out["mode"], "judgment");
        assert_eq!(out["model"], "jev");
        assert_eq!(out["answers"]["continueWorking"]["probability"], 0.92);
        assert_eq!(out["answers"]["priority"]["answer"], "high");
        assert_eq!(out["usage"]["input"], 1200);

        // Meter charged from response-reported usage: 1200 input tokens
        // at $2/Mtok input (judgment bills input only) = $0.0024.
        let snap = manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .quota_meter
            .snapshot();
        assert_eq!(snap.request_count, 1);
        assert_eq!(snap.input_tokens, 1200);
        let cost = snap.cost_usd.unwrap_or(0.0);
        assert!((cost - 0.0024).abs() < 1e-12, "cost fold: {cost}");
    }

    /// A keyless entry (`requires_key: false`, no `credential_id`) sends
    /// no Authorization header — local judgment endpoints work.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn judgment_without_key_omits_auth_header() {
        let (addr, received, _server) = serve_json(
            json!({"answers": {"ok": {"answer": true, "probability": 0.5}}}).to_string(),
        )
        .await;
        let resolver = judgment_resolver(judgment_entry(addr, None)).await;
        let temp = tempfile::tempdir().expect("tempdir");
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("jev"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let out = tool
            .execute_with_context(
                json!({"state": "x", "questions": {"ok": {"type": "boolean", "instructions": "?"}}}),
                &ctx_for("caller"),
            )
            .await
            .expect("judgment without key");
        assert_eq!(out["answers"]["ok"]["probability"], 0.5);
        let authorization = {
            let recorded = received.lock().expect("received lock");
            assert_eq!(recorded.len(), 1);
            recorded[0].authorization.clone()
        };
        assert_eq!(authorization, None, "no Bearer header for keyless entry");
        // No usage block in the response: the meter's request counter
        // still advances, but no token counts are synthesized.
        let snap = manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .quota_meter
            .snapshot();
        assert_eq!(snap.request_count, 1);
        assert_eq!(snap.input_tokens, 0);
    }

    /// Judgment mode refuses models whose `ModelSpec` lacks
    /// `decisions: true`, naming the flag.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn judgment_requires_decisions_flag() {
        let temp = tempfile::tempdir().expect("tempdir");
        // The mock catalog entry carries `spec: None` — a plain chat model.
        let adapter = MockAdapter::new();
        let (resolver, _adapter) =
            LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let err = tool
            .execute_with_context(
                json!({"state": "x", "questions": {"ok": {"type": "boolean"}}}),
                &ctx_for("caller"),
            )
            .await
            .expect_err("decisions gate must refuse");
        let msg = format!("{err:#}");
        assert!(msg.contains("decisions"), "error names the flag: {msg}");
        assert!(msg.contains("mock"), "error names the model: {msg}");
    }

    // ── Mode + resolution gates ──────────────────────────────────────

    /// Both modes present at once is an error.
    #[tokio::test]
    async fn rejects_prompt_and_questions_together() {
        let tool = ModelCallTool::new(Weak::new());
        let err = tool
            .execute_with_context(
                json!({"prompt": "hi", "state": "x", "questions": {"q": {"type": "boolean"}}}),
                &ctx_for("caller"),
            )
            .await
            .expect_err("both modes must error");
        assert!(format!("{err:#}").contains("mutually exclusive"));
    }

    /// Neither mode present is an error.
    #[tokio::test]
    async fn rejects_neither_prompt_nor_questions() {
        let tool = ModelCallTool::new(Weak::new());
        let err = tool
            .execute_with_context(json!({"model": "mock"}), &ctx_for("caller"))
            .await
            .expect_err("no mode must error");
        assert!(format!("{err:#}").contains("missing mode"));
    }

    /// Judgment mode needs both `state` and `questions`.
    #[tokio::test]
    async fn judgment_requires_state_and_questions() {
        let tool = ModelCallTool::new(Weak::new());
        let err = tool
            .execute_with_context(
                json!({"questions": {"q": {"type": "boolean"}}}),
                &ctx_for("caller"),
            )
            .await
            .expect_err("questions without state must error");
        assert!(format!("{err:#}").contains("requires both `state` and `questions`"));
    }

    /// Completion-only knobs are rejected in judgment mode.
    #[tokio::test]
    async fn judgment_rejects_completion_only_params() {
        let tool = ModelCallTool::new(Weak::new());
        let err = tool
            .execute_with_context(
                json!({"state": "x", "questions": {"q": {"type": "boolean"}}, "temperature": 0.5}),
                &ctx_for("caller"),
            )
            .await
            .expect_err("temperature in judgment mode must error");
        assert!(format!("{err:#}").contains("only apply to completion mode"));
    }

    /// Unknown model ids fail resolution with a clear error.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn unknown_model_id_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        let (resolver, _adapter) =
            LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let err = tool
            .execute_with_context(json!({"model": "nope", "prompt": "hi"}), &ctx_for("caller"))
            .await
            .expect_err("unknown model must error");
        assert!(
            format!("{err:#}").contains("nope"),
            "error names the model id: {err:#}"
        );
    }

    /// No principal context → fail closed. An unmetered LLM call is
    /// never allowed.
    #[tokio::test]
    async fn fails_closed_without_principal_context() {
        let tool = ModelCallTool::new(Weak::new());
        let ctx = ToolContext::default_for_tool(MODEL_CALL_TOOL_NAME);
        let err = tool
            .execute_with_context(json!({"prompt": "hi"}), &ctx)
            .await
            .expect_err("missing principal context must error");
        assert!(format!("{err:#}").contains("principal"));

        // An empty principal name (the funnel's unset placeholder) fails
        // closed too.
        let ctx = ToolContext::default_for_tool(MODEL_CALL_TOOL_NAME).with_principal_name("");
        let err = tool
            .execute_with_context(json!({"prompt": "hi"}), &ctx)
            .await
            .expect_err("empty principal name must error");
        assert!(format!("{err:#}").contains("principal"));
    }

    /// Unknown principal name → fail closed (the identity is
    /// server-derived; a name the manager doesn't know is unattributable).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn fails_closed_for_unknown_principal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        let (resolver, _adapter) =
            LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let tool = ModelCallTool::new(Arc::downgrade(&manager));
        let err = tool
            .execute_with_context(json!({"prompt": "hi"}), &ctx_for("ghost"))
            .await
            .expect_err("unknown principal must error");
        assert!(format!("{err:#}").contains("ghost"));
    }

    /// The bare `execute` entry point (no ToolContext) refuses — the
    /// funnel always routes through `execute_with_context`.
    #[tokio::test]
    async fn bare_execute_refuses_without_context() {
        let tool = ModelCallTool::new(Weak::new());
        let err = tool
            .execute(json!({"prompt": "hi"}))
            .await
            .expect_err("bare execute must error");
        assert!(format!("{err:#}").contains("ToolContext"));
    }

    /// Metadata pins: name, exposure, parallelism, and the schema's
    /// mode exclusivity surface.
    #[tokio::test]
    async fn metadata_and_schema_shape() {
        let tool = ModelCallTool::new(Weak::new());
        assert_eq!(tool.name(), MODEL_CALL_TOOL_NAME);
        assert_eq!(tool.exposure(), ToolExposure::Direct);
        assert!(tool.parallelizable());
        let schema = tool.parameters();
        let props = schema["properties"].as_object().expect("properties");
        for key in [
            "model",
            "prompt",
            "system",
            "max_tokens",
            "temperature",
            "state",
            "questions",
        ] {
            assert!(props.contains_key(key), "schema missing {key}");
        }
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    // ── Funnel-level (registration + gate + ctx threading) ──────────

    /// End-to-end through the F37 funnel: system-scope registration,
    /// F32b schema validation, capability gate, and the
    /// `principal_name` hop from `HookInput::ToolCall` into the tool's
    /// `ToolContext` — the exact path the agentic loop and the
    /// `ExecuteTool` IPC handler drive.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn funnel_executes_completion_with_principal_attribution() {
        use crate::extensions::builtin::BuiltinToolAdapter;
        use crate::extensions::framework::core::ExtensionCore;

        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        adapter.queue_text("via funnel");
        let (resolver, _adapter) =
            LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;
        let principal_id = manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .id
            .0
            .clone();

        let core = ExtensionCore::new();
        BuiltinToolAdapter::register_tool_system(
            &core,
            Arc::new(ModelCallTool::new(Arc::downgrade(&manager))),
        )
        .await
        .expect("register");

        let (_text, json, success) = peko_engine::funnel::execute_tool_via_core_with_context(
            &core,
            MODEL_CALL_TOOL_NAME,
            json!({"prompt": "hi"}),
            None,
            None,
            None,
            None,
            Some(principal_id),
            Some("caller".to_string()),
            Some(vec!["tool:ModelCall".to_string()]),
            None,
            None,
        )
        .await
        .expect("funnel call");
        assert!(success, "granted call succeeds: {json}");
        assert_eq!(json["text"], "via funnel");
        let snap = manager
            .get_by_name("caller")
            .await
            .expect("principal")
            .quota_meter
            .snapshot();
        assert_eq!(snap.request_count, 1);
    }

    /// The capability gate refuses a principal lacking `tool:ModelCall`
    /// — and the refusal arrives as `success: false` data, not a
    /// transport error (the `ExecuteTool` triplet contract).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn funnel_denies_without_tool_grant() {
        use crate::extensions::builtin::BuiltinToolAdapter;
        use crate::extensions::framework::core::ExtensionCore;

        let temp = tempfile::tempdir().expect("tempdir");
        let adapter = MockAdapter::new();
        let (resolver, adapter) = LlmResolver::mock(adapter, temp.path().join("models.toml")).await;
        let manager = manager_with_principal(
            &temp,
            "caller",
            Some("mock"),
            Some(generous_quota()),
            resolver,
        )
        .await;

        let core = ExtensionCore::new();
        BuiltinToolAdapter::register_tool_system(
            &core,
            Arc::new(ModelCallTool::new(Arc::downgrade(&manager))),
        )
        .await
        .expect("register");

        let (text, _json, success) = peko_engine::funnel::execute_tool_via_core_with_context(
            &core,
            MODEL_CALL_TOOL_NAME,
            json!({"prompt": "hi"}),
            None,
            None,
            None,
            None,
            None,
            Some("caller".to_string()),
            Some(vec!["tool:Bash".to_string()]),
            None,
            None,
        )
        .await
        .expect("funnel call");
        assert!(!success, "missing tool:ModelCall grant must deny");
        assert!(text.contains("currently disabled"), "gate message: {text}");
        assert!(
            adapter.recorded_requests().is_empty(),
            "denied call must not reach the provider"
        );
    }
}
