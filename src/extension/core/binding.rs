//! Hook binding types
//!
//! This module defines the binding between hook points and handler factories,
//! along with a convenience builder for common binding patterns.

use crate::extension::core::handler::HookHandlerFactory;
use crate::extension::core::hook_points::HookPoint;

/// Binding between a hook point and a handler factory
#[derive(Debug)]
pub struct HookBinding {
    /// The hook point to bind to
    pub point: HookPoint,

    /// Factory for creating the handler
    pub handler_factory: Box<dyn HookHandlerFactory>,
}

impl HookBinding {
    /// Create a new hook binding
    #[must_use]
    pub fn new(point: HookPoint, factory: Box<dyn HookHandlerFactory>) -> Self {
        Self {
            point,
            handler_factory: factory,
        }
    }
}

/// Convenience builder for common hook bindings
pub struct HookBindingBuilder;

impl HookBindingBuilder {
    /// Create a tool registration binding
    pub fn tool_register<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ToolRegister,
            handler_factory: Box::new(factory),
        }
    }

    /// Create a prompt section binding
    pub fn prompt_section<F>(section: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::PromptSystemSection {
                section: section.into(),
                priority: 100,
            },
            handler_factory: Box::new(factory),
        }
    }

    /// Create a channel input binding
    pub fn channel_input<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ChannelInput,
            handler_factory: Box::new(factory),
        }
    }

    /// Create a channel output binding
    pub fn channel_output<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ChannelOutput,
            handler_factory: Box::new(factory),
        }
    }

    /// Create an event subscription binding
    pub fn event_subscribe<F>(topic_pattern: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::EventSubscribe {
                topic_pattern: topic_pattern.into(),
            },
            handler_factory: Box::new(factory),
        }
    }

    /// Create an event emission binding
    pub fn event_emit<F>(factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::EventEmit,
            handler_factory: Box::new(factory),
        }
    }

    /// Create a tool execution binding
    pub fn tool_execute<F>(tool_name: impl Into<String>, factory: F) -> HookBinding
    where
        F: HookHandlerFactory + 'static,
    {
        HookBinding {
            point: HookPoint::ToolExecute {
                tool_name: tool_name.into(),
            },
            handler_factory: Box::new(factory),
        }
    }
}
