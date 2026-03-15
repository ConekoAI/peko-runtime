//! Retrieve tool

use serde_json::Value;
use tracing::info;

/// Retrieve a memory
pub async fn execute(args: Value) -> Result<String, String> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    info!("Retrieving memory from '{}': {}", namespace, key);

    let db = super::get_db()?;
    let full_key = super::namespaced_key(namespace, key);

    let value = db.get(full_key.as_bytes())
        .map_err(|e| format!("Failed to retrieve: {}", e))?;

    match value {
        Some(data) => {
            let s = String::from_utf8_lossy(&data);
            let json: serde_json::Value = serde_json::from_str(&s)
                .map_err(|e| format!("Failed to parse stored data: {}", e))?;
            
            let value = json.get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("[invalid data]");
            
            Ok(format!("Value for '{}':\n{}", key, value))
        }
        None => Ok(format!("Key '{}' not found in namespace '{}'", key, namespace)),
    }
}
