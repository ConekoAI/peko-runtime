//! Turn JSON-Schema validation failures into messages an agent can act on.
//!
//! Multi-action tools (Session, Task, Plan, Cron, Async, Agent) declare one
//! `oneOf` variant per action. A raw validator reports a bad call to them as
//! "not valid under any of the schemas listed in the 'oneOf' keyword", which
//! names neither the missing field nor the stray one. [`explain`] picks the
//! variant the caller chose (by its `action`) and reports that variant's
//! errors in plain terms: a missing required field, a field the action does
//! not take (with the fields it does take), or a required either/or group.

use jsonschema::error::ValidationErrorKind;
use jsonschema::ValidationError;
use serde_json::Value;

/// Most distinct problems reported for one call.
const MAX_PROBLEMS: usize = 5;

/// Describe why `args` failed `schema` for `tool`, e.g.
/// `Invalid arguments for Session action 'find': missing required 'query'`.
pub(crate) fn explain(schema: &Value, args: &Value, tool: &str) -> Option<String> {
    let validator = jsonschema::validator_for(schema).ok()?;
    let errors: Vec<ValidationError<'_>> = validator.iter_errors(args).collect();
    if errors.is_empty() {
        return None;
    }
    let chosen = chosen_variant(schema, args);
    let label = match &chosen {
        Some((_, action)) => format!("{tool} action '{action}'"),
        None => tool.to_string(),
    };
    let mut problems = Vec::new();
    for error in &errors {
        describe(
            error,
            schema,
            chosen.as_ref().map(|(i, _)| *i),
            &mut problems,
        );
    }
    let stray = problems.iter().any(|p| p.contains("is not a parameter"));
    // A failed variant pick is reported through the action field itself.
    if problems.is_empty() {
        problems.extend(errors.iter().map(ToString::to_string));
    }
    let mut unique = Vec::new();
    for problem in problems {
        if !unique.contains(&problem) {
            unique.push(problem);
        }
    }
    let more = unique.len().saturating_sub(MAX_PROBLEMS);
    unique.truncate(MAX_PROBLEMS);
    let mut message = format!("Invalid arguments for {label}: {}", unique.join("; "));
    if more > 0 {
        message.push_str(&format!(" (and {more} more)"));
    }
    if stray {
        let accepted = match &chosen {
            Some((index, _)) => variant_fields(schema, *index),
            None => top_level_fields(schema),
        };
        if let Some(accepted) = accepted {
            message.push_str(&format!(". Accepted fields: {}", accepted.join(", ")));
        }
    }
    Some(message)
}

/// The `oneOf` variant selected by `args["action"]` (or the variant that
/// does not require `action`, when it is omitted), with its action name.
fn chosen_variant(schema: &Value, args: &Value) -> Option<(usize, String)> {
    let variants = schema.get("oneOf")?.as_array()?;
    let action_of = |variant: &Value| {
        variant
            .pointer("/properties/action/const")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match args.get("action") {
        Some(Value::String(requested)) => variants
            .iter()
            .position(|v| action_of(v).as_deref() == Some(requested))
            .map(|i| (i, requested.clone())),
        Some(_) => None,
        None => variants.iter().enumerate().find_map(|(i, v)| {
            let requires_action = v["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f == "action"));
            (!requires_action)
                .then(|| action_of(v).map(|a| (i, a)))
                .flatten()
        }),
    }
}

/// Sorted field names a variant accepts.
fn variant_fields(schema: &Value, index: usize) -> Option<Vec<String>> {
    sorted_keys(schema.pointer(&format!("/oneOf/{index}/properties"))?)
}

/// Sorted field names a single-shape schema accepts.
fn top_level_fields(schema: &Value) -> Option<Vec<String>> {
    sorted_keys(schema.get("properties")?)
}

fn sorted_keys(properties: &Value) -> Option<Vec<String>> {
    let mut fields: Vec<String> = properties.as_object()?.keys().cloned().collect();
    fields.sort();
    Some(fields)
}

fn describe(
    error: &ValidationError<'_>,
    schema: &Value,
    chosen: Option<usize>,
    out: &mut Vec<String>,
) {
    let path = error.schema_path().as_str().to_string();
    match error.kind() {
        ValidationErrorKind::OneOfNotValid { context } if path == "/oneOf" => {
            let branch = match chosen {
                Some(index) => context.get(index),
                // No action discriminator: explain the closest variant.
                None if action_free(schema) => context.iter().min_by_key(|errors| errors.len()),
                // A missing or unknown action is reported on `action` itself.
                None => None,
            };
            for inner in branch.into_iter().flatten() {
                describe(inner, schema, chosen, out);
            }
        }
        ValidationErrorKind::Enum { options } if path == "/properties/action/enum" => {
            let names: Vec<String> = options
                .as_array()
                .map(|o| {
                    o.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            out.push(format!(
                "unknown action {} — valid actions: {}",
                error.instance(),
                names.join(", ")
            ));
        }
        ValidationErrorKind::Required { property } => {
            let mut problem = format!("missing required {}", quoted(property));
            if property == "action" && error.instance_path().as_str().is_empty() {
                if let Some(actions) = action_names(schema) {
                    problem.push_str(&format!(" (one of: {})", actions.join(", ")));
                }
            }
            out.push(problem);
        }
        ValidationErrorKind::AdditionalProperties { unexpected } => {
            for field in unexpected {
                out.push(format!("'{field}' is not a parameter here"));
            }
        }
        ValidationErrorKind::AnyOf { .. } => match required_groups(schema, &path) {
            Some(names) => out.push(format!("needs at least one of: {}", names.join(", "))),
            None => out.push(at(error)),
        },
        ValidationErrorKind::OneOfNotValid { context } => match required_groups(schema, &path) {
            Some(names) => out.push(format!("needs exactly one of: {}", names.join(", "))),
            None => match context.iter().min_by_key(|errors| errors.len()) {
                Some(errors) if !errors.is_empty() => {
                    for inner in errors {
                        describe(inner, schema, chosen, out);
                    }
                }
                _ => out.push(at(error)),
            },
        },
        ValidationErrorKind::OneOfMultipleValid { .. } => match required_groups(schema, &path) {
            Some(names) => out.push(format!("give only one of: {}", names.join(", "))),
            None => out.push(at(error)),
        },
        ValidationErrorKind::Not { schema: negated } => {
            let excluded = negated
                .get("anyOf")
                .and_then(required_names)
                .or_else(|| negated.get("required").and_then(names_of));
            match excluded {
                Some(names) => {
                    let trigger = path
                        .strip_suffix("/then/not")
                        .and_then(|parent| schema.pointer(&format!("{parent}/if/required")))
                        .and_then(names_of);
                    out.push(match trigger {
                        Some(trigger) => format!(
                            "{} cannot be combined with: {}",
                            trigger.join(", "),
                            names.join(", ")
                        ),
                        None => format!("cannot be combined with: {}", names.join(", ")),
                    });
                }
                None => out.push(at(error)),
            }
        }
        _ => out.push(at(error)),
    }
}

/// Whether the top-level `oneOf` variants carry no action discriminator
/// (e.g. ModelCall's completion-vs-judgment modes).
fn action_free(schema: &Value) -> bool {
    action_names(schema).is_none()
}

fn action_names(schema: &Value) -> Option<Vec<String>> {
    let names: Vec<String> = schema
        .get("oneOf")?
        .as_array()?
        .iter()
        .filter_map(|v| {
            v.pointer("/properties/action/const")
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// Field names of a combinator whose branches are each just
/// `{"required": [...]}` — an either/or group.
fn required_groups(schema: &Value, path: &str) -> Option<Vec<String>> {
    required_names(schema.pointer(path)?)
}

fn required_names(groups: &Value) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for group in groups.as_array()? {
        let object = group.as_object()?;
        if object.len() != 1 {
            return None;
        }
        names.extend(names_of(object.get("required")?)?);
    }
    Some(names)
}

fn names_of(list: &Value) -> Option<Vec<String>> {
    list.as_array()?
        .iter()
        .map(|v| v.as_str().map(|s| format!("'{s}'")))
        .collect()
}

fn quoted(property: &Value) -> String {
    match property.as_str() {
        Some(name) => format!("'{name}'"),
        None => property.to_string(),
    }
}

/// The validator's own message, prefixed with the offending field.
fn at(error: &ValidationError<'_>) -> String {
    let field = error
        .instance_path()
        .as_str()
        .trim_start_matches('/')
        .replace('/', ".");
    if field.is_empty() {
        error.to_string()
    } else {
        format!("'{field}': {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "update", "list"], "default": "list"},
                "taskId": {"type": "string"},
                "status": {"type": "string"},
                "owner": {"type": "string"},
            },
            "required": ["action"],
            "oneOf": [
                {"type": "object", "additionalProperties": false,
                 "properties": {"action": {"const": "get"}, "taskId": {"type": "string"}},
                 "required": ["action", "taskId"]},
                {"type": "object", "additionalProperties": false,
                 "properties": {"action": {"const": "update"}, "taskId": {"type": "string"},
                                "status": {"type": "string"}, "owner": {"type": "string"}},
                 "required": ["action", "taskId"],
                 "anyOf": [{"required": ["status"]}, {"required": ["owner"]}]},
                {"type": "object", "additionalProperties": false,
                 "properties": {"action": {"const": "list"}},
                 "required": ["action"]},
            ]
        })
    }

    #[test]
    fn names_the_missing_field_of_the_chosen_action() {
        let message = explain(&tool_schema(), &json!({"action": "get"}), "Task").unwrap();
        assert_eq!(
            message,
            "Invalid arguments for Task action 'get': missing required 'taskId'"
        );
    }

    #[test]
    fn names_a_stray_field_and_lists_what_the_action_takes() {
        let message = explain(
            &tool_schema(),
            &json!({"action": "get", "task_id": "x"}),
            "Task",
        )
        .unwrap();
        assert!(
            message.contains("'task_id' is not a parameter here"),
            "{message}"
        );
        assert!(message.contains("missing required 'taskId'"), "{message}");
        assert!(
            message.ends_with("Accepted fields: action, taskId"),
            "{message}"
        );
    }

    #[test]
    fn names_an_either_or_group() {
        let message = explain(
            &tool_schema(),
            &json!({"action": "update", "taskId": "t"}),
            "Task",
        )
        .unwrap();
        assert_eq!(
            message,
            "Invalid arguments for Task action 'update': needs at least one of: 'status', 'owner'"
        );
    }

    #[test]
    fn lists_the_valid_actions_for_an_unknown_one() {
        let message = explain(&tool_schema(), &json!({"action": "delete"}), "Task").unwrap();
        assert!(
            message.contains("unknown action \"delete\" — valid actions: get, update, list"),
            "{message}"
        );
        assert!(!message.contains("oneOf"), "{message}");
    }

    #[test]
    fn explains_a_conditional_exclusion() {
        let schema = json!({
            "type": "object",
            "properties": {"delay": {}, "at": {}, "cron": {}},
            "allOf": [{
                "if": {"required": ["delay"]},
                "then": {"not": {"anyOf": [{"required": ["at"]}, {"required": ["cron"]}]}}
            }]
        });
        let message = explain(
            &schema,
            &json!({"delay": "5m", "cron": "* * * * *"}),
            "Cron",
        )
        .unwrap();
        assert_eq!(
            message,
            "Invalid arguments for Cron: 'delay' cannot be combined with: 'at', 'cron'"
        );
    }

    #[test]
    fn explains_the_closest_mode_without_a_discriminator() {
        let schema = json!({
            "type": "object",
            "properties": {"prompt": {}, "state": {}, "questions": {}},
            "oneOf": [
                {"required": ["prompt"],
                 "not": {"anyOf": [{"required": ["state"]}, {"required": ["questions"]}]}},
                {"required": ["state", "questions"],
                 "not": {"anyOf": [{"required": ["prompt"]}]}}
            ]
        });
        let message =
            explain(&schema, &json!({"prompt": "hi", "state": "s"}), "ModelCall").unwrap();
        assert_eq!(
            message,
            "Invalid arguments for ModelCall: cannot be combined with: 'state', 'questions'"
        );
    }

    #[test]
    fn valid_arguments_need_no_explanation() {
        assert!(explain(&tool_schema(), &json!({"action": "list"}), "Task").is_none());
    }
}
