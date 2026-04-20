//! Time formatting utilities
//!
//! Provides consistent timestamp formatting across the CLI, converting UTC timestamps
//! to the local timezone for display.

/// Format an RFC3339 timestamp for display (converts to local timezone)
///
/// # Arguments
/// * `ts` - An RFC3339 formatted timestamp string (e.g., "2026-03-17T10:30:00+00:00")
///
/// # Returns
/// A formatted string in the local timezone (e.g., "2026-03-17 18:30") or the original
/// string if parsing fails.
///
/// # Example
/// ```
/// # use pekobot::common::time::format_timestamp;
/// let local_time = format_timestamp("2026-03-17T10:30:00+00:00");
/// ```
#[must_use]
pub fn format_timestamp(ts: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    } else {
        ts.to_string()
    }
}

/// Format a millisecond timestamp (unix epoch) for display (converts to local timezone)
///
/// # Arguments
/// * `ts_ms` - Unix timestamp in milliseconds
///
/// # Returns
/// A formatted string in the local timezone (e.g., "2026-03-17 18:30") or the original
/// timestamp as a string if conversion fails.
///
/// # Example
/// ```
/// # use pekobot::common::time::format_timestamp_ms;
/// let local_time = format_timestamp_ms(1_712_200_200_000);
/// ```
#[must_use]
pub fn format_timestamp_ms(ts_ms: u64) -> String {
    let secs = (ts_ms / 1000) as i64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u32;
    if let Some(dt) = chrono::DateTime::from_timestamp(secs, nanos) {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    } else {
        format!("{ts_ms}")
    }
}

/// Format a millisecond timestamp to RFC3339 string
#[must_use]
pub fn format_timestamp_rfc3339(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_timestamp_valid() {
        // Test that parsing works (actual local time depends on system timezone)
        let result = format_timestamp("2026-03-17T10:30:00+00:00");
        // Should not return the original string, should be formatted
        assert!(!result.contains('T')); // RFC3339 contains 'T'
        assert!(result.contains('-')); // Date format contains '-'
        assert!(result.contains(':')); // Time format contains ':'
    }

    #[test]
    fn test_format_timestamp_invalid() {
        assert_eq!(format_timestamp("invalid"), "invalid");
        assert_eq!(format_timestamp(""), "");
    }

    #[test]
    fn test_format_timestamp_ms_valid() {
        // 2026-03-17 10:30:00 UTC = 1712200200000 ms
        let result = format_timestamp_ms(1_712_200_200_000);
        // Should be formatted, not the raw number
        assert!(!result.chars().all(|c| c.is_ascii_digit()));
        assert!(result.contains('-')); // Date format contains '-'
        assert!(result.contains(':')); // Time format contains ':'
    }

    #[test]
    fn test_format_timestamp_ms_invalid() {
        // Very large value that would cause overflow
        assert_eq!(format_timestamp_ms(u64::MAX), format!("{}", u64::MAX));
    }
}
