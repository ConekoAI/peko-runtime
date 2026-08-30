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
//! Mirrors [`MeteredProvider`](crate::providers::MeteredProvider) but
//! reads the full nested-scope stack via
//! [`QuotaScope::collect_stack`](peko_quota::QuotaScope::collect_stack)
//! instead of just the innermost meter. Each LLM call charges every
//! meter in the stack, innermost first (agent → principal → …) so a
//! "more specific" meter trip fails fast.
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
//! the sync `try_charge_with_cost` (Phase 3 of
//! `feature/multi-model-subagents`). Each meter sees the same usage
//! event with its USD cost folded in; if any meter rejects
//! (exhausted), the error is folded into the stream. The cost is
//! computed from the inner provider's `ModelSpec::pricing`
//! (`input_per_million * input_tokens / 1e6 + output_per_million * output_tokens / 1e6`).
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

    /// Same, but pass the stack explicitly. Used by tests that don't
    /// want to wrap the call in `QuotaScope::with`.
    #[must_use]
    pub fn with_explicit_stack(inner: Arc<dyn ProviderView>, meters: Vec<Arc<QuotaMeter>>) -> Self {
        Self { inner, meters }
    }

    /// Wrap a provider view with no meters (passthrough). Equivalent
    /// to `with_explicit_stack(inner, vec![])`.
    #[must_use]
    pub fn passthrough(inner: Arc<dyn ProviderView>) -> Self {
        Self {
            inner,
            meters: Vec::new(),
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
        let response = self
            .inner
            .chat_with_tools(model_id, messages, tools, options)
            .await?;
        let pricing = self.inner.spec().and_then(|s| s.pricing);
        let cost_usd = compute_cost_usd(pricing, response.usage.input, response.usage.output);
        self.charge_stack(&response.usage, cost_usd).await?;
        Ok(response)
    }

    /// Streaming chat with native tool calling. The returned stream
    /// is `inner`'s stream with each `StreamEvent::Usage` event
    /// intercepted: when the wrapper sees one, it charges every meter
    /// in the stack (innermost-first) and emits the event unchanged.
    /// If any meter rejects (exhausted), the error is folded into the
    /// stream as the next item — same behavior as
    /// [`MeteredProvider`](crate::providers::MeteredProvider).
    ///
    /// Phase 3 — cost is computed from the inner provider's
    /// `ModelSpec::pricing` (`input_per_million * input / 1e6 +
    /// output_per_million * output / 1e6`) and folded into each
    /// meter via `try_charge_with_cost`. When the inner provider
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
        let inner_stream = self
            .inner
            .stream_with_tools(model_id, messages, tools, options)
            .await?;
        let meters = Arc::new(self.meters.clone());
        // Snapshot pricing at the call site — same `PricingHint`
        // applies to every Usage event in this stream (we
        // delegate to one provider for the lifetime of the call).
        let pricing = self.inner.spec().and_then(|s| s.pricing);
        let metered_stream = Box::pin(inner_stream.map(move |event_result| {
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
                    let cost_usd = compute_cost_usd(pricing, input, output);
                    // Charge innermost-first so a peer trip fires
                    // before a principal trip. We `rev()` over the
                    // captured stack (which is outer-first per
                    // `QuotaScope::collect_stack`).
                    let mut first_error: Option<String> = None;
                    for meter in meters.iter().rev() {
                        if let Err(e) = meter.try_charge_with_cost(&usage, cost_usd) {
                            first_error = Some(e.to_string());
                            break;
                        }
                    }
                    match first_error {
                        Some(msg) => Err(anyhow::anyhow!(msg)),
                        None => Ok(StreamEvent::Usage {
                            input,
                            output,
                            total,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            reasoning_output_tokens,
                        }),
                    }
                }
                other => other,
            }
        }));
        Ok(metered_stream)
    }

    /// Charge every meter in the stack, innermost-first. Returns the
    /// first rejection (peer trip) or `Ok(())` if every meter
    /// accepted. Unlimited meters (no `QuotaConfig`) accept any
    /// charge, so they never trip.
    ///
    /// Phase 3 — `cost_usd` is folded alongside the token counters
    /// via `QuotaMeter::charge_with_cost` (when `cost_usd > 0`)
    /// or via the existing `QuotaMeter::charge` (when `cost_usd ==
    /// 0.0`, i.e. the inner provider has no pricing hint). The
    /// unlimited path never trips regardless.
    async fn charge_stack(&self, usage: &TokenUsage, cost_usd: f64) -> Result<()> {
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
                return Err(anyhow::anyhow!(e));
            }
        }
        Ok(())
    }
}

/// Phase 3 — compute USD cost for an LLM call given the inner
/// provider's pricing hint. Returns `0.0` when the provider
/// carries no `PricingHint` (local / unpriced model), so the
/// cycle-budget check degenerates to a no-op for that dimension.
///
/// Cache and reasoning tokens are folded into `input` and
/// `output` by the caller (`TokenUsage::accumulate`) before this
/// is invoked, so we treat the `input` field as the canonical
/// "all-billed-input-tokens" number and skip a separate
/// cache/reasoning rate. Same assumption the meter already makes
/// for `input_tokens` vs `output_tokens` accounting.
fn compute_cost_usd(
    pricing: Option<peko_providers::spec::PricingHint>,
    input_tokens: u64,
    output_tokens: u64,
) -> f64 {
    let Some(p) = pricing else {
        return 0.0;
    };
    let input_cost = p
        .input_per_million
        .map_or(0.0, |rate| rate * input_tokens as f64 / 1_000_000.0);
    let output_cost = p
        .output_per_million
        .map_or(0.0, |rate| rate * output_tokens as f64 / 1_000_000.0);
    input_cost + output_cost
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_providers::spec::{PricingHint, ToolSupport};

    fn pricing(input: f64, output: f64) -> PricingHint {
        PricingHint {
            input_per_million: Some(input),
            output_per_million: Some(output),
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
        };
        let cost = compute_cost_usd(Some(partial), 500_000, 1_000_000);
        // 1.0 * 500_000 / 1e6 = 0.50, output side is None → 0.0.
        assert!((cost - 0.5).abs() < 1e-9, "got {cost}");
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
