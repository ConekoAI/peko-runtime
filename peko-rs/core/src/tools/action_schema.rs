//! Cached validation of declared action variants for direct built-in calls.
//! Execution and caller resolution remain in each tool's domain.

use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct ActionSchema {
    name: &'static str,
    default: Option<String>,
    variants: BTreeMap<String, Variant>,
}

struct Variant {
    validator: jsonschema::Validator,
    integers: Vec<String>,
}

impl ActionSchema {
    pub(crate) fn new(name: &'static str, schema: Value) -> Self {
        let variants = schema["oneOf"]
            .as_array()
            .expect("action variants")
            .iter()
            .map(|variant| {
                let action = variant["properties"]["action"]["const"]
                    .as_str()
                    .expect("action discriminator")
                    .to_owned();
                let validator =
                    jsonschema::validator_for(variant).expect("valid built-in action schema");
                let integers = variant["properties"]
                    .as_object()
                    .expect("action properties")
                    .iter()
                    .filter(|(_, schema)| schema["type"] == "integer")
                    .map(|(field, _)| field.clone())
                    .collect();
                (
                    action,
                    Variant {
                        validator,
                        integers,
                    },
                )
            })
            .collect();
        Self {
            name,
            default: schema["properties"]["action"]["default"]
                .as_str()
                .map(str::to_owned),
            variants,
        }
    }

    pub(crate) fn validate(&self, params: &Value) -> anyhow::Result<()> {
        let action = match params.get("action") {
            Some(value) => value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{} 'action' must be a string", self.name))?,
            None => self
                .default
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("{} requires 'action'", self.name))?,
        };
        let validator = self.variants.get(action).ok_or_else(|| {
            anyhow::anyhow!(
                "Invalid action for {}: unknown action '{action}' — valid actions: {}",
                self.name,
                self.variants.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        validator.validator.validate(params).map_err(|error| {
            let detail = match error.kind() {
                jsonschema::error::ValidationErrorKind::AnyOf { context }
                | jsonschema::error::ValidationErrorKind::OneOfNotValid { context } => context
                    .iter()
                    .flatten()
                    .take(5)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
                _ => error.to_string(),
            };
            anyhow::anyhow!(
                "Invalid arguments for {} action '{action}': {detail}",
                self.name
            )
        })
    }
    /// JSON Schema treats 1.0 as an integer. Canonicalize that accepted spelling
    /// so unsigned Rust argument parsers do not reject it or silently default.
    pub(crate) fn normalize(&self, params: &mut Value) -> anyhow::Result<()> {
        self.validate(params)?;
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .or(self.default.as_deref())
            .expect("validated action");
        let fields = &self.variants[action].integers;
        for field in fields {
            if let Some(value) = params.get_mut(field) {
                if value.as_u64().is_none() {
                    let number = value.as_f64().expect("validated integer");
                    anyhow::ensure!(
                        number >= 0.0 && number < u64::MAX as f64,
                        "{} '{field}' must fit an unsigned integer",
                        self.name
                    );
                    *value = Value::from(number as u64);
                }
            }
        }
        Ok(())
    }
}
