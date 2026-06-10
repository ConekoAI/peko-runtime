//! Scaffold Template Engine
//!
//! Simple variable substitution for embedded templates.
//! Uses `{{key}}` placeholders. No conditionals, no loops — just
//! straight replacement. This is intentional: templates are small
//! and structurally simple.

use std::collections::HashMap;

/// A template with named variables
pub struct Template {
    content: String,
}

impl Template {
    /// Create a template from a string
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    /// Render the template by replacing `{{key}}` with values from `vars`
    pub fn render(&self, vars: &HashMap<String, String>) -> String {
        let mut result = self.content.clone();
        for (key, value) in vars {
            let placeholder = format!("{{{{{}}}}}" , key);
            result = result.replace(&placeholder, value);
        }
        result
    }
}

/// Build a variable map from common scaffold parameters
pub fn build_vars(
    id: &str,
    name: &str,
    description: &str,
    extra: &[(String, String)],
) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("id".to_string(), id.to_string());
    vars.insert("name".to_string(), name.to_string());
    vars.insert("description".to_string(), description.to_string());
    for (k, v) in extra {
        vars.insert(k.clone(), v.clone());
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_render() {
        let template = Template::new("Hello, {{name}}!");
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "World".to_string());
        assert_eq!(template.render(&vars), "Hello, World!");
    }

    #[test]
    fn test_multiple_vars() {
        let template = Template::new("{{greeting}}, {{name}}!");
        let mut vars = HashMap::new();
        vars.insert("greeting".to_string(), "Hi".to_string());
        vars.insert("name".to_string(), "Peko".to_string());
        assert_eq!(template.render(&vars), "Hi, Peko!");
    }

    #[test]
    fn test_missing_var_left_intact() {
        let template = Template::new("{{a}} and {{b}}");
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), "X".to_string());
        assert_eq!(template.render(&vars), "X and {{b}}");
    }

    #[test]
    fn test_build_vars() {
        let vars = build_vars("my-ext", "My Ext", "A test", &[]);
        assert_eq!(vars.get("id"), Some(&"my-ext".to_string()));
        assert_eq!(vars.get("name"), Some(&"My Ext".to_string()));
        assert_eq!(vars.get("description"), Some(&"A test".to_string()));
    }
}
