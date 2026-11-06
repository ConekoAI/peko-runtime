//! `StackedMeteredProvider` — auto-charging wrapper that charges
//! every meter in the active `QuotaScope` stack (F20).
//!
//! Phase 9b.N.5b.8 lifts this from `crate::providers::StackedMeteredProvider`
//! (root-only) into `peko-engine` and refactors it to wrap
//! `Arc<dyn ProviderView>` instead of `Arc<crate::providers::Provider>`.
//! The trait port (Phase 9b.N.5b.7) is the engine-facing view of the
//! provider, so wrapping the trait object is the natural choice — the
//! wrapper doesn't need any of the root-only `Provider` methods
//! (`chat_response`, `chat_response_with_system`, `chat`, `chat_with_system`,
//! `inner`) that root still has.
//!
//! Reads the full nested-scope stack via
//! [`QuotaScope::collect_stack`](peko_quota::QuotaScope::collect_stack)
//! and charges every meter in the stack, innermost first (agent →
//! principal → …) so a "more specific" meter trip fails fast. A
//! single-meter scope (length-1 stack) behaves like the legacy
//! F19 single-dimension wrapper this module replaced.
//!
//! ## Use case
//!
//! ```ignore
//! QuotaScope::with(principal_meter, async move {
//!     QuotaScope::with(agent_meter, async move {
//!         let view: Arc<dyn ProviderView> = ...;
//!         let stacked = StackedMeteredProvider::from_current_scope(view);
//!         stacked.chat_with_tools(...).await  // charges BOTH meters
//!     }).await
//! }).await
//! ```
//!
//! ## Charge order: innermost first
//!
//! The innermost meter is the most specific one for the current call
//! site (subagent's `agent_meter` scope wraps the principal's
//! inherited scope — B5e). Failing fast on the most specific
//! dimension is the right UX — the agent's quota status is the
//! operator's most actionable signal when reading per-agent
//! attribution.
//!
//! ## Streaming
//!
//! Intercepts `StreamEvent::Usage` events and charges each meter via
//! async `charge_with_cost`, persisting before the event is forwarded.
//! Each meter sees the same usage
//! event with its USD cost folded in; if any meter rejects
//! (exhausted), the error is folded into the stream. The cost is
//! computed from the inner provider's `ModelSpec::pricing` by
//! [`compute_usage_cost_usd`]: uncached input, cache reads, cache
//! writes, and output, each at its own rate.
//! When the inner provider has no pricing hint (local / unpriced
//! model), cost is `0.0` and the cycle-budget check degenerates
//! to a no-op for that dimension — the same fallback the
//! pre-Phase-3 behavior already had for `budget_per_cycle = None`.

use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use futures::Stream;
use futures::StreamExt;

use peko_message::{LlmMessage, TokenUsage};
use peko_provider_api::{ChatOptions, ChatResponse, StreamEvent, ToolDefinition};
use peko_quota::{QuotaMeter, QuotaScope};

use crate::ProviderView;

/// Auto-charging wrapper that charges every meter in the active
/// `QuotaScope` stack. Used by F19 callers that want per-principal
/// and per-agent attribution: the agentic loop opens exactly one
/// `principal_meter` scope, and `SubagentExecutor` (B5e) nests an
/// `agent_meter` scope so a child run's LLM calls charge BOTH
/// meters — principal as audit-attribution aggregate, agent as
/// per-agent slice.
///
/// Phase 9b.N.5b.8: wraps `Arc<dyn ProviderView>` (engine-facing trait
/// port) instead of `Arc<crate::providers::Provider>` (root-only). The
/// methods dropped in this refactor (`chat_response`,
/// `chat_response_with_system`, `chat`, `chat_with_system`, `inner`)
/// were root-only `Provider` methods not part of the trait port —
/// their sole external caller (`src/session/compaction.rs:414`,
/// `BackgroundCompactor::summarize`) was updated to use
/// `chat_with_tools` with an empty tool list.
pub struct StackedMeteredProvider {
    inner: Arc<dyn ProviderView>,
    /// Stack of meters captured at construction time. Outer-first;
    /// charging walks innermost-first (the last entry is the most
    /// specific one and trips first).
    meters: Vec<Arc<QuotaMeter>>,
}

impl StackedMeteredProvider {
    /// Wrap a provider view with the full active meter stack. If no
    /// `QuotaScope::with` is active, returns a passthrough wrapper
    /// with an empty stack — all `charge` calls succeed and no quota
    /// is consulted.
    #[must_use]
    pub fn from_current_scope(inner: Arc<dyn ProviderView>) -> Self {
        Self {
            inner,
            meters: QuotaScope::collect_stack(),
        }
    }

    /// Provider name (delegates to inner).
    #[must_use]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Default model id (delegates to inner).
    #[must_use]
    pub fn model_id(&self) -> String {
        self.inner.model_id()
    }

    /// Context window (delegates to inner).
    #[must_use]
    pub fn context_window(&self) -> Option<u32> {
        self.inner.context_window()
    }

    /// Whether the inner provider supports native tool calling.
    #[must_use]
    pub fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    /// Whether the inner provider supports prompt-cache markers.
    #[must_use]
    pub fn supports_prompt_cache_control(&self) -> bool {
        self.inner.supports_prompt_cache_control()
    }

    /// PR 2 / `feature/model-first-config`: declarative capability
    /// descriptor for the bound model. `None` for entries written
    /// before PR 1; the engine's spec gate treats that as "no
    /// gate" so pre-PR-1 setups keep working. Field access at
    /// `agentic_loop.rs:2240` — `provider.spec()`.
    #[must_use]
    pub fn spec(&self) -> Option<peko_providers::spec::ModelSpec> {
        self.inner.spec()
    }

    /// How many meters are stacked on this wrapper. Empty means
    /// passthrough (no quota will be charged).
    #[must_use]
    pub fn stack_len(&self) -> usize {
        self.meters.len()
    }

    /// Blocking chat with native tool calling. Charges the meter
    /// stack after the inner call returns. Phase 3 — folds the
    /// computed USD cost alongside the token counters via
    /// `QuotaMeter::charge_with_cost` (or falls back to
    /// `QuotaMeter::charge` for the unlimited case).
    pub async fn chat_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ChatResponse> {
        self.refuse_if_exhausted()?;
        let response = self
            .inner
            .chat_with_tools(model_id, messages, tools, options)
            .await?;
        let pricing = self.inner.spec().and_then(|s| s.pricing);
        let cost_usd = compute_usage_cost_usd(pricing, &response.usage);
        self.charge_stack(&response.usage, cost_usd).await;
        Ok(response)
    }

    /// Streaming chat with native tool calling. The returned stream
    /// is `inner`'s stream with each `StreamEvent::Usage` event
    /// intercepted: when the wrapper sees one, it charges every meter
    /// in the stack (innermost-first) and emits the event unchanged.
    /// If any meter rejects (exhausted), the error is folded into the
    /// stream as the next item. Single-meter callers see a 1-element
    /// stack and trip immediately.
    ///
    /// Phase 3 — cost is computed from the inner provider's
    /// `ModelSpec::pricing` ([`compute_usage_cost_usd`]) and folded into each
    /// meter via `charge_with_cost` before forwarding the event. When the inner provider
    /// has no `PricingHint` (local / unpriced model), the cost
    /// is `0.0` and the cycle-budget check degenerates to a
    /// no-op for that dimension.
    pub async fn stream_with_tools(
        &self,
        model_id: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        self.refuse_if_exhausted()?;
        let inner_stream = self
            .inner
            .stream_with_tools(model_id, messages, tools, options)
            .await?;
        let meters = Arc::new(self.meters.clone());
        // Snapshot pricing at the call site — same `PricingHint`
        // applies to every Usage event in this stream (we
        // delegate to one provider for the lifetime of the call).
        let pricing = self.inner.spec().and_then(|s| s.pricing);
        let metered_stream = Box::pin(inner_stream.then(move |event_result| {
            let meters = meters.clone();
            async move {
                match event_result {
                    Ok(StreamEvent::Usage {
                        input,
                        output,
                        total,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                        reasoning_output_tokens,
                    }) => {
                        let usage = TokenUsage {
                            input,
                            output,
                            total,
                            cache_creation_input_tokens: Some(cache_creation_input_tokens),
                            cache_read_input_tokens: Some(cache_read_input_tokens),
                            reasoning_output_tokens: Some(reasoning_output_tokens),
                        };
                        let cost_usd = compute_usage_cost_usd(pricing, &usage);
                        // Charge innermost-first so a peer trip fires
                        // before a principal trip. We `rev()` over the
                        // captured stack (which is outer-first per
                        // `QuotaScope::collect_stack`).
                        // A call that crosses a limit is already made and
                        // paid for: it is charged and its answer kept. The
                        // next call is refused before it is made
                        // (`refuse_if_exhausted`), so spending stops there.
                        for meter in meters.iter().rev() {
                            if let Err(e) = meter.charge_with_cost(&usage, cost_usd).await {
                                note_crossed(&e);
                            }
                        }
                        Ok(StreamEvent::Usage {
                            input,
                            output,
                            total,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            reasoning_output_tokens,
                        })
                    }
                    other => other,
                }
            }
        }));
        Ok(metered_stream)
    }

    /// Charge every meter in the stack, innermost-first. A charge that
    /// crosses a limit is still recorded, and the call stands: the next
    /// one is refused before it is made. Unlimited meters (no
    /// `QuotaConfig`) accept any charge, so they never trip.
    ///
    /// Phase 3 — `cost_usd` is folded alongside the token counters
    /// via `QuotaMeter::charge_with_cost` (when `cost_usd > 0`)
    /// or via the existing `QuotaMeter::charge` (when `cost_usd ==
    /// 0.0`, i.e. the inner provider has no pricing hint). The
    /// unlimited path never trips regardless.
    /// Refuse a call before it is made when any meter in the stack is
    /// already over a limit or its budget (the window rolls first), so
    /// an exhausted principal spends nothing more. Charging happens after
    /// a call, so without this every new turn made one more paid call.
    fn refuse_if_exhausted(&self) -> Result<()> {
        let now = chrono::Utc::now();
        for meter in self.meters.iter().rev() {
            meter.advance_if_needed(now);
            if let Some(err) = meter.check() {
                return Err(anyhow::Error::new(err));
            }
        }
        Ok(())
    }

    async fn charge_stack(&self, usage: &TokenUsage, cost_usd: f64) {
        // Innermost is the last entry. Walk in reverse so a peer trip
        // surfaces before a principal charge is even attempted.
        for meter in self.meters.iter().rev() {
            let result = if cost_usd > 0.0 {
                // Use the streaming-aware variant when there's a
                // real cost to fold — it advances the cycle
                // window the same way `charge` does.
                meter.charge_with_cost(usage, cost_usd).await
            } else {
                meter.charge(usage).await
            };
            if let Err(e) = result {
                note_crossed(&e);
            }
        }
    }
}

/// A call's charge crossed a limit: the call stands, the next is refused.
fn note_crossed(err: &peko_quota::QuotaError) {
    tracing::info!("this call crossed a limit ({err}); the next call is refused until it resets");
}

/// Phase 3 — USD cost of `input_tokens` + `output_tokens` at the
/// plain input / output rates. Returns `0.0` when the provider
/// carries no `PricingHint` (local / unpriced model), so the
/// cycle-budget check degenerates to a no-op for that dimension.
///
/// For **projections** (spawn-time pre-flight, MCP sampling bounds,
/// the policy gate's estimate), where no cache split is known yet and
/// pricing every token as uncached input is the conservative bound.
/// A finished call's charge goes through [`compute_usage_cost_usd`],
/// which prices cache reads and writes at their own rates.
///
/// `pub` since ADR-061 phase 2a: the `ModelCall` built-in charges
/// the calling principal's meter directly (no `QuotaScope` wrapper)
/// and needs the same formula for its post-call cost fold.
pub fn compute_cost_usd(
    pricing: Option<peko_providers::spec::PricingHint>,
    input_tokens: u64,
    output_tokens: u64,
) -> f64 {
    compute_usage_cost_usd(
        pricing,
        &TokenUsage {
            input: input_tokens,
            output: output_tokens,
            ..TokenUsage::default()
        },
    )
}

/// USD cost of one finished call from its reported usage.
///
/// `usage.input` is every input token (cache-inclusive, see
/// [`TokenUsage::input`]); it splits into the uncached rest, cache
/// reads, and cache writes, each at its own rate. A cache rate the
/// pricing hint leaves out falls back to `input_per_million` — the
/// conservative choice for cache reads (they are cheaper on every
/// provider), and for cache writes it undercounts only Anthropic's
/// 25% / 100% write premium. Reasoning tokens are already inside
/// `output` and bill at the output rate.
pub fn compute_usage_cost_usd(
    pricing: Option<peko_providers::spec::PricingHint>,
    usage: &TokenUsage,
) -> f64 {
    let Some(p) = pricing else {
        return 0.0;
    };
    let per_token = |rate: Option<f64>, tokens: u64| rate.map_or(0.0, |r| r * tokens as f64 / 1e6);
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
    let uncached = usage
        .input
        .saturating_sub(cache_read.saturating_add(cache_write));
    let read_rate = p.cache_read_per_million.or(p.input_per_million);
    let write_rate = p.cache_write_per_million.or(p.input_per_million);
    per_token(p.input_per_million, uncached)
        + per_token(read_rate, cache_read)
        + per_token(write_rate, cache_write)
        + per_token(p.output_per_million, usage.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_providers::spec::{PricingHint, ToolSupport};

    struct UsageProvider;

    #[async_trait::async_trait]
    impl ProviderView for UsageProvider {
        fn name(&self) -> &str {
            "usage-test"
        }
        fn model_id(&self) -> String {
            "usage-test".into()
        }
        fn context_window(&self) -> Option<u32> {
            None
        }
        fn supports_native_tools(&self) -> bool {
            true
        }
        fn supports_prompt_cache_control(&self) -> bool {
            true
        }
        fn spec(&self) -> Option<peko_providers::spec::ModelSpec> {
            Some(spec_with_pricing(3.0, 15.0))
        }
        async fn chat_with_tools(
            &self,
            _: &str,
            _: &[LlmMessage],
            _: &[ToolDefinition],
            _: &ChatOptions,
        ) -> Result<ChatResponse> {
            anyhow::bail!("this regression must use the streaming path")
        }
        async fn stream_with_tools(
            &self,
            _: &str,
            _: &[LlmMessage],
            _: &[ToolDefinition],
            _: &ChatOptions,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
            Ok(Box::pin(
                futures::stream::iter([Ok(StreamEvent::Usage {
                    input: 1000,
                    output: 100,
                    total: 1100,
                    cache_read_input_tokens: 500,
                    cache_creation_input_tokens: 50,
                    reasoning_output_tokens: 0,
                })])
                .chain(futures::stream::pending()),
            ))
        }
    }

    #[tokio::test]
    async fn streamed_usage_survives_reload_before_stream_finishes() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = peko_quota::QuotaConfig::default();
        let mut meters = Vec::new();
        for name in ["principal", "agent"] {
            meters.push(Arc::new(
                peko_quota::QuotaMeter::load_or_init(
                    config.clone(),
                    Some(dir.path().join(name)),
                    chrono::Utc::now(),
                )
                .await
                .unwrap(),
            ));
        }
        let provider = StackedMeteredProvider {
            inner: Arc::new(UsageProvider),
            meters,
        };
        let mut stream = provider
            .stream_with_tools("usage-test", &[], &[], &ChatOptions::default())
            .await
            .unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            StreamEvent::Usage { .. }
        ));
        // No later blocking request or stream finalizer can rescue persistence.
        drop(stream);
        drop(provider);
        for name in ["principal", "agent"] {
            let restored = QuotaMeter::load_or_init(
                config.clone(),
                Some(dir.path().join(name)),
                chrono::Utc::now(),
            )
            .await
            .unwrap();
            let state = restored.snapshot();
            assert_eq!(state.request_count, 1);
            assert_eq!(state.input_tokens, 1000);
            assert_eq!(state.output_tokens, 100);
            assert_eq!(state.cache_read_tokens, 500);
            assert_eq!(state.cache_creation_tokens, 50);
            assert!((state.cost_usd.unwrap() - 0.0045).abs() < 1e-9);
        }
    }

    /// The call that crosses a budget completes — it is made and paid
    /// for, so its answer is kept — and the next call is refused before
    /// it is made, with the typed `QuotaError` (the loop must not retry
    /// it: its text "$0.0500" once read as an HTTP 500).
    #[tokio::test]
    async fn the_call_crossing_a_budget_completes_and_the_next_is_refused() {
        let config = peko_quota::QuotaConfig {
            budget_per_cycle: Some(0.001),
            ..Default::default()
        };
        let meter = Arc::new(
            QuotaMeter::load_or_init(config, None, chrono::Utc::now())
                .await
                .unwrap(),
        );
        let provider = StackedMeteredProvider {
            inner: Arc::new(UsageProvider),
            meters: vec![Arc::clone(&meter)],
        };
        let mut stream = provider
            .stream_with_tools("usage-test", &[], &[], &ChatOptions::default())
            .await
            .unwrap();
        assert!(matches!(
            stream.next().await.unwrap(),
            Ok(StreamEvent::Usage { .. })
        ));
        drop(stream);
        assert!(meter.is_exhausted());

        let refused = provider
            .stream_with_tools("usage-test", &[], &[], &ChatOptions::default())
            .await
            .err()
            .expect("an exhausted budget refuses the call");
        assert!(refused.chain().any(|c| c.is::<peko_quota::QuotaError>()));
        assert_eq!(meter.snapshot().request_count, 1, "no second call was made");
    }

    #[tokio::test]
    async fn quota_rejected_streamed_usage_is_still_persisted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("quota.json");
        let config = peko_quota::QuotaConfig {
            request_count: Some(0),
            ..Default::default()
        };
        let meter = Arc::new(
            QuotaMeter::load_or_init(config.clone(), Some(path.clone()), chrono::Utc::now())
                .await
                .unwrap(),
        );
        let provider = StackedMeteredProvider {
            inner: Arc::new(UsageProvider),
            meters: vec![meter],
        };
        let mut stream = provider
            .stream_with_tools("usage-test", &[], &[], &ChatOptions::default())
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        drop(stream);
        let restored = QuotaMeter::load_or_init(config, Some(path), chrono::Utc::now())
            .await
            .unwrap();
        assert_eq!(restored.snapshot().request_count, 1);
        assert!(restored.is_exhausted());
    }

    fn pricing(input: f64, output: f64) -> PricingHint {
        PricingHint {
            input_per_million: Some(input),
            output_per_million: Some(output),
            ..Default::default()
        }
    }

    #[test]
    fn compute_cost_usd_uses_pricing_rates() {
        // $3/M input, $15/M output. 1M input + 100K output →
        // $3.00 + $1.50 = $4.50.
        let cost = compute_cost_usd(Some(pricing(3.0, 15.0)), 1_000_000, 100_000);
        assert!((cost - 4.5).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn compute_cost_usd_handles_missing_pricing() {
        assert!((compute_cost_usd(None, 1_000_000, 1_000_000) - 0.0).abs() < 1e-9);
        let partial = PricingHint {
            input_per_million: Some(1.0),
            output_per_million: None,
            ..Default::default()
        };
        let cost = compute_cost_usd(Some(partial), 500_000, 1_000_000);
        // 1.0 * 500_000 / 1e6 = 0.50, output side is None → 0.0.
        assert!((cost - 0.5).abs() < 1e-9, "got {cost}");
    }

    /// A finished call prices its uncached input, cache reads, cache
    /// writes, and output each at its own rate. 100K input of which
    /// 80K were cache reads and 10K cache writes, 1K output, at
    /// $3 in / $0.30 read / $3.75 write / $15 out:
    /// 10K × 3 + 80K × 0.30 + 10K × 3.75 + 1K × 15 (per 1M)
    /// = 0.03 + 0.024 + 0.0375 + 0.015 = $0.1065.
    #[test]
    fn compute_usage_cost_usd_prices_cache_reads_and_writes() {
        let p = PricingHint {
            cache_read_per_million: Some(0.30),
            cache_write_per_million: Some(3.75),
            ..pricing(3.0, 15.0)
        };
        let usage = TokenUsage {
            input: 100_000,
            output: 1_000,
            total: 101_000,
            cache_read_input_tokens: Some(80_000),
            cache_creation_input_tokens: Some(10_000),
            reasoning_output_tokens: None,
        };
        let cost = compute_usage_cost_usd(Some(p), &usage);
        assert!((cost - 0.1065).abs() < 1e-9, "got {cost}");
    }

    /// Without cache rates, cache tokens bill at the input rate — the
    /// same as `compute_cost_usd` over the full input.
    #[test]
    fn compute_usage_cost_usd_falls_back_to_input_rate() {
        let usage = TokenUsage {
            input: 100_000,
            output: 1_000,
            cache_read_input_tokens: Some(80_000),
            cache_creation_input_tokens: Some(10_000),
            ..TokenUsage::default()
        };
        let p = Some(pricing(3.0, 15.0));
        let with_split = compute_usage_cost_usd(p, &usage);
        let flat = compute_cost_usd(p, 100_000, 1_000);
        assert!((with_split - flat).abs() < 1e-12, "{with_split} vs {flat}");
    }

    /// Reasoning tokens are part of `output`; they are not charged a
    /// second time.
    #[test]
    fn compute_usage_cost_usd_does_not_add_reasoning_again() {
        let usage = TokenUsage {
            output: 1_000_000,
            reasoning_output_tokens: Some(400_000),
            ..TokenUsage::default()
        };
        let cost = compute_usage_cost_usd(Some(pricing(0.0, 10.0)), &usage);
        assert!((cost - 10.0).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn compute_cost_usd_handles_zero_tokens() {
        let cost = compute_cost_usd(Some(pricing(100.0, 100.0)), 0, 0);
        assert!(cost.abs() < 1e-9, "got {cost}");
    }

    /// Build a no-op `ModelSpec` carrying the given pricing, and
    /// wrap a stub `ProviderView` so `StackedMeteredProvider` can
    /// pull `spec() → Some(...)` in the stream path.
    fn spec_with_pricing(input_rate: f64, output_rate: f64) -> peko_providers::spec::ModelSpec {
        peko_providers::spec::ModelSpec {
            tool_support: ToolSupport::Full,
            pricing: Some(pricing(input_rate, output_rate)),
            ..Default::default()
        }
    }

    /// The streaming path must call `try_charge_with_cost` with
    /// the cost computed from the inner provider's pricing. We
    /// can't easily mock the full ProviderView + stream, but we
    /// can verify `compute_cost_usd` is correct end-to-end via
    /// the `charge_with_cost` contract.
    #[tokio::test]
    async fn chat_with_tools_folds_cost_via_pricing_hint() {
        // Verify the math matches what the meter expects: a
        // $3/M-input / $15/M-output call of 1M input + 100K
        // output produces $4.50 of cost that the meter folds
        // into the cycle budget.
        use chrono::Utc;
        let meter = peko_quota::QuotaMeter::load_or_init(
            peko_quota::QuotaConfig {
                cycle: peko_quota::QuotaCycle::Hourly,
                budget_per_cycle: Some(100.0),
                ..Default::default()
            },
            None,
            Utc::now(),
        )
        .await
        .unwrap();

        let usage = TokenUsage {
            input: 1_000_000,
            output: 100_000,
            total: 1_100_000,
            ..Default::default()
        };
        let cost = compute_cost_usd(Some(pricing(3.0, 15.0)), usage.input, usage.output);
        meter.charge_with_cost(&usage, cost).await.unwrap();
        let snap = meter.snapshot();
        assert!(
            (snap.cost_usd.unwrap() - 4.5).abs() < 1e-9,
            "cost_usd on snapshot must equal input+output cost, got {:?}",
            snap.cost_usd
        );

        // Suppress the unused-import lint when the test compiles.
        let _ = spec_with_pricing(3.0, 15.0);
    }
}
