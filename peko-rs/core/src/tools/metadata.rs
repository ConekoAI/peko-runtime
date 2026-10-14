//! Registered tool metadata and source.

use peko_provider_api::ToolDefinition;
use serde::{Deserialize, Serialize};

/// Source of a tool (for metadata tracking)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    /// Built-in tool (part of the core codebase)
    BuiltIn,
    /// MCP tool from an MCP server. `untrusted_ok` is the owner's
    /// declaration (`untrusted_ok` in the server's configuration) that
    /// its methods are safe for runs acting for untrusted actors
    /// (ADR-078 D6).
    Mcp { server: String, untrusted_ok: bool },
}

impl ToolSource {
    /// Get a human-readable description of the source
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            ToolSource::BuiltIn => "built-in".to_string(),
            ToolSource::Mcp { server, .. } => format!("MCP server: {server}"),
        }
    }
}

/// Metadata for a registered tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Tool name (unique identifier)
    pub name: String,
    /// Tool description (LLM-optimized)
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: serde_json::Value,
    /// Source of the tool
    pub source: ToolSource,
}

impl ToolMetadata {
    /// Create new tool metadata
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        source: ToolSource,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            source,
        }
    }

    /// Convert to `ToolDefinition` for LLM API. The model sees each field's
    /// schema once: per-action variants keep only what distinguishes them
    /// (see [`compact_action_variants`]); validation still uses the full
    /// [`Self::parameters`].
    #[must_use]
    pub fn to_tool_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: compact_action_variants(&self.parameters),
        }
    }
}

/// Annotation keywords: they inform the model but never change what a
/// schema accepts.
const ANNOTATIONS: [&str; 4] = ["description", "default", "examples", "title"];

/// Shrink a schema whose top-level `oneOf` variants repeat the fields
/// declared under top-level `properties` (the multi-action tools). A
/// variant field whose constraints match the top-level field becomes `{}`;
/// one with its own constraints keeps them without the annotations. The
/// variants still carry their `action` discriminator, `required`, and
/// combinators, so the compact schema accepts exactly the same calls.
#[must_use]
pub fn compact_action_variants(schema: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    let mut compact = schema.clone();
    let top = schema.get("properties").and_then(Value::as_object);
    let Some(variants) = compact.get_mut("oneOf").and_then(Value::as_array_mut) else {
        return compact;
    };
    let constraints = |field: &Value| -> Value {
        let mut field = field.clone();
        if let Some(object) = field.as_object_mut() {
            for key in ANNOTATIONS {
                object.remove(key);
            }
        }
        field
    };
    for variant in variants {
        let Some(properties) = variant.get_mut("properties").and_then(Value::as_object_mut) else {
            continue;
        };
        for (name, field) in properties.iter_mut() {
            if name == "action" {
                continue;
            }
            let own = constraints(field);
            let shared = top
                .and_then(|top| top.get(name))
                .is_some_and(|top_field| constraints(top_field) == own);
            *field = if shared {
                Value::Object(serde_json::Map::new())
            } else {
                own
            };
        }
    }
    compact
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_round_trips_without_exposure() {
        let metadata = ToolMetadata::new(
            "Read",
            "Reads files",
            serde_json::json!({"type":"object"}),
            ToolSource::BuiltIn,
        );
        let encoded = serde_json::to_value(&metadata).unwrap();
        assert!(encoded.get("exposure").is_none());
        let decoded: ToolMetadata = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.to_tool_definition().name, "Read");
    }

    #[test]
    fn variants_keep_only_what_distinguishes_them() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "list"]},
                "id": {"type": "string", "description": "A long explanation"},
                "limit": {"type": "integer", "minimum": 0, "description": "Max"},
            },
            "required": ["action"],
            "oneOf": [
                {"type": "object", "additionalProperties": false, "required": ["action", "id"],
                 "properties": {
                     "action": {"const": "get"},
                     "id": {"type": "string", "description": "A long explanation"}}},
                {"type": "object", "additionalProperties": false, "required": ["action"],
                 "properties": {
                     "action": {"const": "list"},
                     "limit": {"type": "integer", "minimum": 1, "description": "Max", "default": 50}}},
            ]
        });
        let compact = compact_action_variants(&schema);
        assert_eq!(compact["properties"], schema["properties"]);
        assert_eq!(
            compact["oneOf"][0]["properties"]["id"],
            serde_json::json!({})
        );
        assert_eq!(
            compact["oneOf"][0]["properties"]["action"],
            serde_json::json!({"const": "get"})
        );
        assert_eq!(
            compact["oneOf"][1]["properties"]["limit"],
            serde_json::json!({"type": "integer", "minimum": 1}),
            "a variant's own constraint survives"
        );
        assert_eq!(
            compact["oneOf"][0]["required"],
            schema["oneOf"][0]["required"]
        );
    }
}
