//! Placeholder replacement for system prompt templates
//!
//! Supports dynamic content injection via placeholders like {{tools}}, {{runtime}}, etc.

use std::collections::HashMap;

/// Available placeholders for system prompt templates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placeholder {
    /// Available tools section - {{tools}}
    Tools,
    /// Skills section - {{skills}}
    Skills,
    /// Runtime info (agent, host, OS, model, channel) - {{runtime}}
    Runtime,
    /// Sandbox status - {{sandbox}}
    Sandbox,
    /// Model aliases - {{model_aliases}}
    ModelAliases,
    /// Self-update section - {{self_update}}
    SelfUpdate,
    /// Timezone - {{timezone}}
    Timezone,
    /// Agent name inline - {{agent_name}}
    AgentName,
    /// Workspace path inline - {{workspace}}
    Workspace,
    /// Channel inline - {{channel}}
    Channel,
    /// Thinking level inline - {{thinking_level}}
    ThinkingLevel,
}

impl Placeholder {
    /// Parse placeholder from string (without braces)
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "tools" => Some(Self::Tools),
            "skills" => Some(Self::Skills),
            "runtime" => Some(Self::Runtime),
            "sandbox" => Some(Self::Sandbox),
            "model_aliases" => Some(Self::ModelAliases),
            "self_update" => Some(Self::SelfUpdate),
            "timezone" => Some(Self::Timezone),
            "agent_name" => Some(Self::AgentName),
            "workspace" => Some(Self::Workspace),
            "channel" => Some(Self::Channel),
            "thinking_level" => Some(Self::ThinkingLevel),
            _ => None,
        }
    }

    /// Get the placeholder marker for this variant
    pub fn marker(&self) -> &'static str {
        match self {
            Self::Tools => "{{tools}}",
            Self::Skills => "{{skills}}",
            Self::Runtime => "{{runtime}}",
            Self::Sandbox => "{{sandbox}}",
            Self::ModelAliases => "{{model_aliases}}",
            Self::SelfUpdate => "{{self_update}}",
            Self::Timezone => "{{timezone}}",
            Self::AgentName => "{{agent_name}}",
            Self::Workspace => "{{workspace}}",
            Self::Channel => "{{channel}}",
            Self::ThinkingLevel => "{{thinking_level}}",
        }
    }
}

/// Replace placeholders in template content with provided values
/// 
/// Placeholders not found in `values` are left as-is or removed based on `remove_missing`.
pub fn replace_placeholders(template: &str, values: &HashMap<Placeholder, String>, remove_missing: bool) -> String {
    let mut result = template.to_string();
    
    for (placeholder, value) in values {
        result = result.replace(placeholder.marker(), value);
    }
    
    if remove_missing {
        // Remove any remaining unreplaced placeholders
        // Pattern: {{word_chars}}
        let re = regex::Regex::new(r"\{\{[a-z_]+\}\}").unwrap();
        result = re.replace_all(&result, "").to_string();
    }
    
    result
}

/// Extract all placeholders found in template content
pub fn extract_placeholders(content: &str) -> Vec<Placeholder> {
    let re = regex::Regex::new(r"\{\{([a-z_]+)\}\}").unwrap();
    let mut placeholders = Vec::new();
    
    for cap in re.captures_iter(content) {
        if let Some(name) = cap.get(1) {
            if let Some(placeholder) = Placeholder::from_str(name.as_str()) {
                placeholders.push(placeholder);
            }
        }
    }
    
    placeholders
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_placeholder_from_str() {
        assert_eq!(Placeholder::from_str("tools"), Some(Placeholder::Tools));
        assert_eq!(Placeholder::from_str("runtime"), Some(Placeholder::Runtime));
        assert_eq!(Placeholder::from_str("agent_name"), Some(Placeholder::AgentName));
        assert_eq!(Placeholder::from_str("unknown"), None);
    }

    #[test]
    fn test_placeholder_markers() {
        assert_eq!(Placeholder::Tools.marker(), "{{tools}}");
        assert_eq!(Placeholder::Runtime.marker(), "{{runtime}}");
    }

    #[test]
    fn test_replace_placeholders() {
        let template = "Hello {{agent_name}}, tools: {{tools}}";
        let mut values = HashMap::new();
        values.insert(Placeholder::AgentName, "test-agent".to_string());
        values.insert(Placeholder::Tools, "tool list".to_string());
        
        let result = replace_placeholders(template, &values, false);
        assert_eq!(result, "Hello test-agent, tools: tool list");
    }

    #[test]
    fn test_replace_placeholders_remove_missing() {
        let template = "Hello {{agent_name}}, missing: {{unknown}}";
        let mut values = HashMap::new();
        values.insert(Placeholder::AgentName, "test-agent".to_string());
        
        let result = replace_placeholders(template, &values, true);
        assert_eq!(result, "Hello test-agent, missing: ");
    }

    #[test]
    fn test_extract_placeholders() {
        let content = "{{tools}} and {{skills}} and {{runtime}}";
        let placeholders = extract_placeholders(content);
        
        assert!(placeholders.contains(&Placeholder::Tools));
        assert!(placeholders.contains(&Placeholder::Skills));
        assert!(placeholders.contains(&Placeholder::Runtime));
        assert_eq!(placeholders.len(), 3);
    }

    #[test]
    fn test_extract_placeholders_ignores_unknown() {
        let content = "{{tools}} and {{unknown_placeholder}}";
        let placeholders = extract_placeholders(content);
        
        assert!(placeholders.contains(&Placeholder::Tools));
        assert_eq!(placeholders.len(), 1); // unknown_placeholder is ignored
    }
}
