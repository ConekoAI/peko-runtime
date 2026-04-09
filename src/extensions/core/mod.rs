//! Extension Core module
//!
//! This module provides the foundation for the Unified Extension Architecture.
//! It defines hook points in the agentic loop and manages registration/invocation
//! of extension handlers.
//!
//! # Architecture
//!
//! ```
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    EXTENSION CORE                               │
//! │                                                                 │
//! │  Hook Points          Registry            Context               │
//! │  ───────────          ────────            ───────               │
//! │  PromptSystemSection  ExtensionCore      HookContext            │
//! │  ToolRegister         RegisteredHook     HookState              │
//! │  SessionStateChange   HookId             ExtensionServices      │
//! │  ChannelInput                                                   │
//! │  EventSubscribe                                                 │
//! │  ...                                                            │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!                    ┌─────────┴─────────┐
//!                    ▼                   ▼
//!           ┌─────────────┐    ┌─────────────────┐
//!           │   Adapters  │    │ Extension Manager│
//!           │  (Phase 2+) │    │   (Phase 7+)     │
//!           └─────────────┘    └─────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use pekobot::extensions::core::{ExtensionCore, HookPoint};
//!
//! // Create the core
//! let core = ExtensionCore::new();
//!
//! // Register a handler
//! let handler = Arc::new(MyHandler);
//! let registration = core.register_hook(
//!     HookPoint::PromptSystemSection { section: "tools".to_string(), priority: 100 },
//!     handler,
//!     &ExtensionId::new("my-extension"),
//! ).await?;
//!
//! // Invoke hooks
//! let result = core.invoke_hook(HookPoint::ToolRegister, HookInput::Unit).await;
//! ```

// Re-export hook point definitions
pub use hook_points::{common, HookPoint, HookPointBuilder};

// Re-export async adapter
pub use async_adapter::ExtensionAsyncAdapter;

// Re-export context types
pub use context::{
    ClosureHookHandler,
    ExtensionConfig,
    ExtensionServices,
    HookBinding,
    HookBindingBuilder,
    HookContext,
    HookHandler,
    HookHandlerFactory,
    HookState,
    TelemetryService,
};

// Re-export registry types
pub use registry::{global_core, init_global_core, ExtensionCore, RegisteredHook};

// Submodules
pub mod async_adapter;
pub mod context;
pub mod hook_points;
pub mod registry;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::extensions::types::{ExtensionId, HookId, HookInput, HookOutput, HookResult};
    use std::sync::Arc;

    /// Integration test handler that tracks invocations
    #[derive(Debug)]
    struct TrackingHandler {
        point: HookPoint,
        name: String,
        invocations: std::sync::atomic::AtomicUsize,
    }

    impl TrackingHandler {
        fn new(point: HookPoint, name: impl Into<String>) -> Self {
            Self {
                point,
                name: name.into(),
                invocations: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn invocation_count(&self) -> usize {
            self.invocations.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl HookHandler for TrackingHandler {
        async fn handle(&self, _ctx: HookContext) -> HookResult {
            self.invocations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            HookResult::PassThrough
        }

        fn hook_point(&self) -> HookPoint {
            self.point.clone()
        }

        fn name(&self) -> String {
            self.name.clone()
        }
    }

    #[tokio::test]
    async fn test_full_lifecycle() {
        let core = ExtensionCore::new();

        // Register multiple handlers
        let handler1 = Arc::new(TrackingHandler::new(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100,
            },
            "tools-handler",
        ));

        let handler2 = Arc::new(TrackingHandler::new(
            HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: 100,
            },
            "skills-handler",
        ));

        let ext_id = ExtensionId::new("test-extension");

        let reg1 = core
            .register_hook(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: 100,
                },
                handler1.clone(),
                &ext_id,
            )
            .await
            .unwrap();

        let reg2 = core
            .register_hook(
                HookPoint::PromptSystemSection {
                    section: "skills".to_string(),
                    priority: 100,
                },
                handler2.clone(),
                &ext_id,
            )
            .await
            .unwrap();

        // Verify registrations
        assert_eq!(core.hook_count().await, 2);

        // Invoke hooks
        let _ = core
            .invoke_hook(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: 100,
                },
                HookInput::Unit,
            )
            .await;

        assert_eq!(handler1.invocation_count(), 1);
        assert_eq!(handler2.invocation_count(), 0);

        // Disable one handler
        core.disable_hook(&reg1.id).await.unwrap();

        // Invoke again
        let _ = core
            .invoke_hook(
                HookPoint::PromptSystemSection {
                    section: "tools".to_string(),
                    priority: 100,
                },
                HookInput::Unit,
            )
            .await;

        // First handler should not be invoked (disabled)
        assert_eq!(handler1.invocation_count(), 1);
        assert_eq!(handler2.invocation_count(), 0);

        // Re-enable
        core.enable_hook(&reg1.id).await.unwrap();

        // Unregister second handler
        core.unregister_hook(&reg2.id).await.unwrap();

        assert_eq!(core.hook_count().await, 1);

        // Get hooks for extension
        let ext_hooks = core.get_hooks_for_extension(&ext_id).await;
        assert_eq!(ext_hooks.len(), 1);
    }

    #[tokio::test]
    async fn test_multiple_handlers_same_point() {
        let core = ExtensionCore::new();

        let handler1 = Arc::new(TrackingHandler::new(HookPoint::ToolRegister, "handler1"));
        let handler2 = Arc::new(TrackingHandler::new(HookPoint::ToolRegister, "handler2"));
        let handler3 = Arc::new(TrackingHandler::new(HookPoint::ToolRegister, "handler3"));

        let ext_id = ExtensionId::new("test");

        // Register in reverse priority order
        core.register_hook(HookPoint::ToolRegister, handler3.clone(), &ext_id)
            .await
            .unwrap();
        core.register_hook(HookPoint::ToolRegister, handler2.clone(), &ext_id)
            .await
            .unwrap();
        core.register_hook(HookPoint::ToolRegister, handler1.clone(), &ext_id)
            .await
            .unwrap();

        // Invoke
        let _ = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        // All should be invoked
        assert_eq!(handler1.invocation_count(), 1);
        assert_eq!(handler2.invocation_count(), 1);
        assert_eq!(handler3.invocation_count(), 1);
    }

    #[tokio::test]
    async fn test_handler_error_handling() {
        #[derive(Debug)]
        struct ErrorHandler;

        #[async_trait::async_trait]
        impl HookHandler for ErrorHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                HookResult::Error(anyhow::anyhow!("Test error"))
            }

            fn hook_point(&self) -> HookPoint {
                HookPoint::ToolRegister
            }
        }

        let core = ExtensionCore::new();
        let handler = Arc::new(ErrorHandler);
        let ext_id = ExtensionId::new("test");

        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::Error(e) => {
                assert!(e.to_string().contains("Test error"));
            }
            _ => panic!("Expected Error result, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_handler_replace() {
        #[derive(Debug)]
        struct ReplaceHandler;

        #[async_trait::async_trait]
        impl HookHandler for ReplaceHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                HookResult::Replace(HookOutput::text("replaced"))
            }

            fn hook_point(&self) -> HookPoint {
                HookPoint::ToolRegister
            }
        }

        let core = ExtensionCore::new();
        let handler = Arc::new(ReplaceHandler);
        let ext_id = ExtensionId::new("test");

        core.register_hook(HookPoint::ToolRegister, handler, &ext_id)
            .await
            .unwrap();

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::Replace(HookOutput::Text(text)) => {
                assert_eq!(text, "replaced");
            }
            _ => panic!("Expected Replace result, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_handler_handled() {
        #[derive(Debug)]
        struct HandledHandler;

        #[async_trait::async_trait]
        impl HookHandler for HandledHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                HookResult::Handled
            }

            fn hook_point(&self) -> HookPoint {
                HookPoint::ToolRegister
            }
        }

        #[derive(Debug)]
        struct SecondHandler {
            invoked: std::sync::atomic::AtomicBool,
        }

        #[async_trait::async_trait]
        impl HookHandler for SecondHandler {
            async fn handle(&self, _ctx: HookContext) -> HookResult {
                self.invoked.store(true, std::sync::atomic::Ordering::SeqCst);
                HookResult::PassThrough
            }

            fn hook_point(&self) -> HookPoint {
                HookPoint::ToolRegister
            }
        }

        let core = ExtensionCore::new();

        let handler1 = Arc::new(HandledHandler);
        let handler2 = Arc::new(SecondHandler {
            invoked: std::sync::atomic::AtomicBool::new(false),
        });

        let ext_id = ExtensionId::new("test");

        core.register_hook(HookPoint::ToolRegister, handler1, &ext_id)
            .await
            .unwrap();
        core.register_hook(HookPoint::ToolRegister, handler2.clone(), &ext_id)
            .await
            .unwrap();

        let result = core
            .invoke_hook(HookPoint::ToolRegister, HookInput::Unit)
            .await;

        match result {
            HookResult::Handled => (), // Expected
            _ => panic!("Expected Handled result, got {:?}", result),
        }

        // Second handler should NOT be invoked because first returned Handled
        assert!(!handler2
            .invoked
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_global_instance() {
        // Note: This test may run after other tests that already set the global core.
        // We can only verify that global_core() returns consistent results.
        
        // If global core is already set, verify it's consistent
        if let Some(global1) = global_core() {
            let global2 = global_core().unwrap();
            assert!(Arc::ptr_eq(&global1, &global2));
        }
        
        // Try to set our own core
        let core = Arc::new(ExtensionCore::new());
        init_global_core(core.clone());

        // Verify global instance is set (either ours or from a previous test)
        assert!(global_core().is_some());
    }
}
