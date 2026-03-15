//! Delete tool

use serde_json::Value;
use tracing::info;

/// Delete a memory
pub async fn execute(args: Value) -> Result<String, String> {
    let key = args
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    info!("Deleting memory from '{}': {}", namespace, key);

    let db = super::get_db()?;
    let full_key = super::namespaced_key(namespace, key);

    let removed = db.remove(full_key.as_bytes())
        .map_err(|e| format!("Failed to delete: {}", e))?;

    db.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    match removed {
        Some(_) => Ok(format!("Deleted '{}' from namespace '{}'", key, namespace)),
        None => Ok(format!("Key '{}' not found in namespace '{}'", key, namespace)),
    }
}
