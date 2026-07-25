//! Generic JSON utilities shared across Peko

/// Check if `target` JSON contains all fields from `filter` with matching values.
///
/// For objects, every key in `filter` must exist in `target` with a matching
/// value (recursively). For non-objects, values are compared directly.
pub fn json_subset(target: &serde_json::Value, filter: &serde_json::Value) -> bool {
    match (target, filter) {
        (serde_json::Value::Object(target_obj), serde_json::Value::Object(filter_obj)) => {
            for (key, filter_val) in filter_obj {
                match target_obj.get(key) {
                    Some(target_val) => {
                        if !json_subset(target_val, filter_val) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            true
        }
        (a, b) => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_json_subset_exact_match() {
        let target = json!({"a": 1, "b": 2});
        let filter = json!({"a": 1, "b": 2});
        assert!(json_subset(&target, &filter));
    }

    #[test]
    fn test_json_subset_partial_match() {
        let target = json!({"a": 1, "b": 2, "c": 3});
        let filter = json!({"a": 1, "b": 2});
        assert!(json_subset(&target, &filter));
    }

    #[test]
    fn test_json_subset_missing_key() {
        let target = json!({"a": 1});
        let filter = json!({"a": 1, "b": 2});
        assert!(!json_subset(&target, &filter));
    }

    #[test]
    fn test_json_subset_nested() {
        let target = json!({"outer": {"inner": 42, "extra": true}});
        let filter = json!({"outer": {"inner": 42}});
        assert!(json_subset(&target, &filter));
    }

    #[test]
    fn test_json_subset_primitive() {
        let target = json!(42);
        let filter = json!(42);
        assert!(json_subset(&target, &filter));
    }

    #[test]
    fn test_json_subset_mismatch() {
        let target = json!({"a": 1});
        let filter = json!({"a": 2});
        assert!(!json_subset(&target, &filter));
    }
}
