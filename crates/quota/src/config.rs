//! Quota configuration: the TOML-deserializable limits block and
//! the calendar cycle enum.
//!
//! ## `QuotaConfig`
//!
//! Parsed from the `[quota]` block inside a `principal.toml` (when
//! present). Any of the three limit fields may be `None` — a
//! missing limit is interpreted as "unlimited" for that dimension.
//!
//! ```toml
//! [quota]
//! input_tokens = 1_000_000
//! output_tokens = 500_000
//! request_count = 10_000
//! cycle = "daily"
//! ```
//!
//! ## `QuotaCycle`
//!
//! Calendar-aligned reset window, all UTC. The `next_boundary` math
//! lives here (not in `meter.rs`) because the math is purely
//! functional and belongs in a value type's impl block. Tests pin
//! the boundary behaviour for known timestamps.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

/// Token + request-count limits for a principal.
///
/// All three limits are independent and optional. A `None` field
/// means "unlimited for this dimension" — the meter will not check
/// or trip on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_count: Option<u64>,
    #[serde(default)]
    pub cycle: QuotaCycle,
}

impl QuotaConfig {
    /// True when at least one limit is set. A principal with no
    /// `QuotaConfig` at all (or an empty one) is unquota'd and
    /// every call is free.
    #[must_use]
    pub fn has_any_limit(&self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some() || self.request_count.is_some()
    }
}

/// Calendar-aligned reset cycle. All boundaries are UTC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaCycle {
    /// Reset at the top of every UTC hour.
    Hourly,
    /// Reset at 00:00 UTC every day. **Default** — the most common
    /// rate-limit cycle; `QuotaConfig::default()` picks this so an
    /// unconfigured cycle still behaves sensibly.
    #[default]
    Daily,
    /// Reset at 00:00 UTC every Monday (ISO week boundary).
    Weekly,
    /// Reset at 00:00 UTC on the 1st of every month.
    Monthly,
}

impl std::fmt::Display for QuotaCycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        };
        f.write_str(s)
    }
}

/// Parse a `QuotaCycle` from a CLI/config string. Accepts the
/// canonical lowercase forms (`hourly` / `daily` / `weekly` /
/// `monthly`) matching the `serde(rename_all = "lowercase")` wire
/// format. This impl lives here in `peko-quota` (not in the CLI
/// crate) because of Rust's orphan rule (E0117): `QuotaCycle` is
/// owned by `peko-quota`, so the trait impl must live alongside
/// the type. The CLI's `parse_cycle` adapter just delegates to it.
impl FromStr for QuotaCycle {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hourly" => Ok(Self::Hourly),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            other => Err(Box::leak(
                format!(
                    "invalid quota cycle '{other}': expected hourly, daily, weekly, or monthly"
                )
                .into_boxed_str(),
            )),
        }
    }
}

impl QuotaCycle {
    /// Compute the start of the current window containing `now`,
    /// and the end of that window (i.e. the next boundary).
    ///
    /// `window_start` is inclusive, `window_end` is exclusive — at
    /// `now == window_end` the meter rolls forward.
    #[must_use]
    pub fn window_bounds(&self, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        let start = match self {
            Self::Hourly => Utc
                .with_ymd_and_hms(now.year(), now.month(), now.day(), now.hour(), 0, 0)
                .single()
                .expect("valid ymd+hms"),
            Self::Daily => Utc
                .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
                .single()
                .expect("valid ymd+hms"),
            Self::Weekly => {
                // Roll back to the most recent Monday 00:00 UTC.
                let days_since_monday = now.weekday().num_days_from_monday() as i64;
                let monday_date = now.date_naive() - Duration::days(days_since_monday);
                Utc.from_utc_datetime(&monday_date.and_hms_opt(0, 0, 0).expect("valid hms"))
            }
            Self::Monthly => Utc
                .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
                .single()
                .expect("valid ymd"),
        };
        let end = self.next_boundary(start);
        (start, end)
    }

    /// The next boundary strictly after `at`. For `Monthly` we
    /// handle year wrap-around (Dec → Jan of next year) explicitly.
    #[must_use]
    pub fn next_boundary(&self, at: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::Hourly => at + Duration::hours(1),
            Self::Daily => at + Duration::days(1),
            Self::Weekly => at + Duration::weeks(1),
            Self::Monthly => {
                let (mut y, mut m) = (at.year(), at.month());
                m += 1;
                if m > 12 {
                    m = 1;
                    y += 1;
                }
                Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0)
                    .single()
                    .expect("valid ymd")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, min, sec)
            .single()
            .unwrap()
    }

    #[test]
    fn hourly_window_bounds() {
        let now = ts(2026, 7, 12, 14, 37, 22);
        let (start, end) = QuotaCycle::Hourly.window_bounds(now);
        assert_eq!(start, ts(2026, 7, 12, 14, 0, 0));
        assert_eq!(end, ts(2026, 7, 12, 15, 0, 0));
    }

    #[test]
    fn daily_window_bounds() {
        let now = ts(2026, 7, 12, 14, 37, 22);
        let (start, end) = QuotaCycle::Daily.window_bounds(now);
        assert_eq!(start, ts(2026, 7, 12, 0, 0, 0));
        assert_eq!(end, ts(2026, 7, 13, 0, 0, 0));
    }

    #[test]
    fn weekly_window_bounds_rolls_back_to_monday() {
        // 2026-07-15 is a Wednesday.
        let wed = ts(2026, 7, 15, 14, 37, 22);
        let (start, end) = QuotaCycle::Weekly.window_bounds(wed);
        assert_eq!(start, ts(2026, 7, 13, 0, 0, 0)); // Monday
        assert_eq!(end, ts(2026, 7, 20, 0, 0, 0)); // next Monday
    }

    #[test]
    fn weekly_window_bounds_on_monday_is_that_week() {
        let mon = ts(2026, 7, 13, 0, 0, 1); // 1 second into Monday
        let (start, end) = QuotaCycle::Weekly.window_bounds(mon);
        assert_eq!(start, ts(2026, 7, 13, 0, 0, 0));
        assert_eq!(end, ts(2026, 7, 20, 0, 0, 0));
    }

    #[test]
    fn monthly_window_bounds() {
        let now = ts(2026, 7, 12, 14, 37, 22);
        let (start, end) = QuotaCycle::Monthly.window_bounds(now);
        assert_eq!(start, ts(2026, 7, 1, 0, 0, 0));
        assert_eq!(end, ts(2026, 8, 1, 0, 0, 0));
    }

    #[test]
    fn monthly_boundary_wraps_year_at_december() {
        let dec_15 = ts(2026, 12, 15, 0, 0, 0);
        let next = QuotaCycle::Monthly.next_boundary(dec_15);
        assert_eq!(next, ts(2027, 1, 1, 0, 0, 0));
    }

    #[test]
    fn daily_boundary_does_not_wrap_year_at_year_end() {
        // Daily cycle boundary at Dec 31 00:00 UTC is Jan 1 of next
        // year — `Duration::days(1)` handles year rollover
        // correctly because chrono normalises.
        let dec_31 = ts(2026, 12, 31, 0, 0, 0);
        let next = QuotaCycle::Daily.next_boundary(dec_31);
        assert_eq!(next, ts(2027, 1, 1, 0, 0, 0));
    }

    #[test]
    fn quota_config_default_is_daily_with_no_limits() {
        let cfg = QuotaConfig::default();
        assert_eq!(cfg.cycle, QuotaCycle::Daily);
        assert!(!cfg.has_any_limit());
    }

    #[test]
    fn quota_config_has_any_limit_detects_each_field() {
        let just_input = QuotaConfig {
            input_tokens: Some(100),
            ..Default::default()
        };
        assert!(just_input.has_any_limit());

        let just_output = QuotaConfig {
            output_tokens: Some(100),
            ..Default::default()
        };
        assert!(just_output.has_any_limit());

        let just_requests = QuotaConfig {
            request_count: Some(10),
            ..Default::default()
        };
        assert!(just_requests.has_any_limit());

        let none = QuotaConfig::default();
        assert!(!none.has_any_limit());
    }

    #[test]
    fn quota_config_toml_round_trip_with_all_fields() {
        let toml_str = r#"
input_tokens = 1000000
output_tokens = 500000
request_count = 10000
cycle = "weekly"
"#;
        let cfg: QuotaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.input_tokens, Some(1_000_000));
        assert_eq!(cfg.output_tokens, Some(500_000));
        assert_eq!(cfg.request_count, Some(10_000));
        assert_eq!(cfg.cycle, QuotaCycle::Weekly);

        // Round-trip back to TOML.
        let serialised = toml::to_string(&cfg).unwrap();
        let cfg2: QuotaConfig = toml::from_str(&serialised).unwrap();
        assert_eq!(cfg, cfg2);
    }

    #[test]
    fn quota_config_toml_round_trip_empty_block_uses_default() {
        let cfg: QuotaConfig = toml::from_str("").unwrap();
        assert_eq!(cfg, QuotaConfig::default());
    }

    #[test]
    fn quota_config_toml_cycle_strings_match_lowercase_rename() {
        // `rename_all = "lowercase"` accepts only the four canonical
        // lowercase forms. We exercise them transitively via
        // `cycle = "..."` inside a `QuotaConfig` because TOML
        // can't parse a bare string into a newtype enum.
        for (s, expected) in [
            ("hourly", QuotaCycle::Hourly),
            ("daily", QuotaCycle::Daily),
            ("weekly", QuotaCycle::Weekly),
            ("monthly", QuotaCycle::Monthly),
        ] {
            let cfg: QuotaConfig = toml::from_str(&format!("cycle = \"{s}\"")).unwrap();
            assert_eq!(cfg.cycle, expected, "cycle string {s} should parse");
        }
    }
}
