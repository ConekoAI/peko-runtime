//! Hook handler trait and closure adapter
//!
//! This module defines the [`HookHandler`] trait that all extension handlers must
//! implement, along with a [`ClosureHookHandler`] wrapper for closure-based handlers.

use crate::extension::core::context::HookContext;
use crate::extension::core::hook_points::HookPoint;
use crate::extension::types::{HookPriority, HookResult};
use async_trait::async_trait;
use std::fmt;

/// Trait for hook handlers
///
/// All extension types implement this trait to hook into the agent lifecycle.
#[async_trait]
pub trait HookHandler: Send + Sync + fmt::Debug {
    /// Handle the hook invocation
    ///
    /// This method is called when the hook point is triggered.
    /// Handlers should examine the context and return an appropriate result.
    ///
    /// # Arguments
    /// * `ctx` - The hook context containing input, state, and services
    ///
    /// # Returns
    /// A `HookResult` indicating how to proceed
    async fn handle(&self, ctx: HookContext) -> HookResult;

    /// Get the hook point this handler attaches to
    fn hook_point(&self) -> HookPoint;

    /// Get the handler priority (higher = earlier execution)
    ///
    /// Default is 100 (normal priority)
    fn priority(&self) -> HookPriority {
        100
    }

    /// Get the handler name for debugging/tracing
    fn name(&self) -> String {
        format!("{self:?}")
    }
}

/// Wrapper for closures as hook handlers
pub struct ClosureHookHandler<F> {
    point: HookPoint,
    priority: HookPriority,
    name: String,
    handler: F,
}

impl<F> ClosureHookHandler<F> {
    /// Create a new closure-based handler
    pub fn new(
        point: HookPoint,
        priority: HookPriority,
        name: impl Into<String>,
        handler: F,
    ) -> Self {
        Self {
            point,
            priority,
            name: name.into(),
            handler,
        }
    }
}

impl<F> fmt::Debug for ClosureHookHandler<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClosureHookHandler")
            .field("point", &self.point)
            .field("priority", &self.priority)
            .field("name", &self.name)
            .finish()
    }
}

#[async_trait]
impl<F, Fut> HookHandler for ClosureHookHandler<F>
where
    F: Fn(HookContext) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = HookResult> + Send,
{
    async fn handle(&self, ctx: HookContext) -> HookResult {
        (self.handler)(ctx).await
    }

    fn hook_point(&self) -> HookPoint {
        self.point.clone()
    }

    fn priority(&self) -> HookPriority {
        self.priority
    }

    fn name(&self) -> String {
        self.name.clone()
    }
}

/// Factory for creating hook handlers from manifests
#[async_trait]
pub trait HookHandlerFactory: Send + Sync + fmt::Debug {
    /// Create a handler instance
    fn create(&self, manifest: crate::extension::types::ExtensionManifest) -> Box<dyn HookHandler>;
}
