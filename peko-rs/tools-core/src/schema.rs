//! JSON Schema composition for tools with an explicit action discriminator.

use serde_json::{json, Map, Value};

/// Compose strict action variants while retaining a top-level property inventory
/// for providers that project schemas without combinators. Execution validates
/// the complete schema, including each action's allowed and required fields.
pub fn action_schema(variants: &[(&str, Value)]) -> Value {
    let mut properties = Map::new();
    let mut branches = Vec::new();
    let actions: Vec<_> = variants.iter().map(|(action, _)| *action).collect();
    for (action, schema) in variants {
        let mut branch = schema.clone();
        let fields = branch["properties"]
            .as_object_mut()
            .expect("action properties");
        properties.extend(fields.clone());
        fields.insert("action".into(), json!({"type": "string", "const": action}));
        let mut required = branch["required"].as_array().cloned().unwrap_or_default();
        required.push(json!("action"));
        branch["required"] = json!(required);
        branch["additionalProperties"] = json!(false);
        branches.push(branch);
    }
    properties.insert("action".into(), json!({"type": "string", "enum": actions}));
    json!({
        "type": "object",
        "properties": properties,
        "required": ["action"],
        "additionalProperties": false,
        "oneOf": branches
    })
}
