use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};

const CURSOR_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    version: u8,
    thread: String,
    before: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("invalid chat-log cursor encoding")]
    Encoding,
    #[error("invalid chat-log cursor payload: {0}")]
    Payload(String),
    #[error("unsupported chat-log cursor version {0}")]
    Version(u8),
    #[error("chat-log cursor belongs to another thread")]
    WrongThread,
}

pub fn encode(thread: &str, before: u64) -> Result<String, CursorError> {
    let payload = CursorPayload {
        version: CURSOR_VERSION,
        thread: thread.to_string(),
        before,
    };
    let json = serde_json::to_vec(&payload).map_err(|e| CursorError::Payload(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

pub fn decode(value: &str, expected_thread: &str) -> Result<u64, CursorError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CursorError::Encoding)?;
    let payload: CursorPayload =
        serde_json::from_slice(&bytes).map_err(|e| CursorError::Payload(e.to_string()))?;
    if payload.version != CURSOR_VERSION {
        return Err(CursorError::Version(payload.version));
    }
    if payload.thread != expected_thread {
        return Err(CursorError::WrongThread);
    }
    Ok(payload.before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_and_is_thread_bound() {
        let encoded = encode("thread-a", 42).unwrap();
        assert_eq!(decode(&encoded, "thread-a").unwrap(), 42);
        assert!(matches!(
            decode(&encoded, "thread-b"),
            Err(CursorError::WrongThread)
        ));
    }
}
