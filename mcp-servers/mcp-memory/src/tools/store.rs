//! Store tool

use chrono::Utc;
use serde_json::Value;
use tracing::info;

/// Store a memory
pub async fn execute(args: Value) -> Result<String, String> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    let value = args
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'value' parameter")?;

    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    info!("Storing memory in '{}': {}", namespace, key);

    let db = super::get_db()?;
    let full_key = super::namespaced_key(namespace, key);
    
    // Store with timestamp metadata
    let data = serde_json::json!({
        "value": value,
        "created_at": Utc::now().to_rfc3339(),
        "updated_at": Utc::now().to_rfc3339(),
    });

    db.insert(full_key.as_bytes(), data.to_string().as_bytes())
        .map_err(|e| format!("Failed to store: {}", e))?;

    db.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(format!("Stored '{}' in namespace '{}'", key, namespace))
}
