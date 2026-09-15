use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::pricing::cost_for_entry;
use crate::source::logs::LogEntry;

pub const TOKENS_ACCENT: &str = "#38bdf8";
pub const COST_ACCENT: &str = "#fb923c";
const NO_DATA_ACCENT: &str = "#6b7280";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    #[default]
    Tokens,
    Cost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RangeKind {
    #[default]
    Today,
    #[serde(rename = "sevenday")]
    SevenDay,
    Session,
}

pub struct MetricDisplay {
    pub label: &'static str,
    pub value_text: String,
    pub subtitle: &'static str,
    pub accent_color: &'static str,
}

/// `[start, end]` bounds for `range`, in UTC. `Today`/`SevenDay` are
/// rolling windows ending at `now` (not calendar-aligned). `Session`
/// mirrors the existing Usage Gauge's 5-hour rate-limit window:
/// `[resets_at - 5h, resets_at]` when `session_resets_at` is known
/// (read from the same statusline-usage.json the gauge reads), falling
/// back to a rolling last-5h window when it isn't - a fallback, not an
/// error, matching this plugin's existing convention.
pub fn range_bounds(
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    match range {
        RangeKind::Today => (now - Duration::hours(24), now),
        RangeKind::SevenDay => (now - Duration::hours(24 * 7), now),
        RangeKind::Session => {
            let end = session_resets_at.unwrap_or(now);
            let start = end - Duration::hours(5);
            (start, end)
        }
    }
}

/// `< 1000` as an exact integer; otherwise one decimal place with a
/// K/M suffix, trimming a trailing ".0" (`318000` -> `"318K"`, `318500`
/// -> `"318.5K"`, `1234567` -> `"1.2M"`) - matches the mockup this tile
/// is based on.
pub fn format_tokens(total: u64) -> String {
    if total < 1000 {
        return total.to_string();
    }
    let (value, suffix) = if total < 1_000_000 {
        (total as f64 / 1_000.0, "K")
    } else {
        (total as f64 / 1_000_000.0, "M")
    };
    let formatted = format!("{value:.1}");
    let trimmed = formatted.strip_suffix(".0").unwrap_or(&formatted);
    format!("{trimmed}{suffix}")
}

/// Always two decimal places, e.g. `"$8.40"`.
pub fn format_cost(total: f64) -> String {
    format!("${total:.2}")
}

/// Computes what to show for one instance's current metric/range
/// selection from the full set of parsed log entries. Always produces a
/// real number (possibly zero) - callers decide separately whether "no
/// entries at all were found anywhere" warrants `error_display()`
/// instead (see `metric_action.rs`), since a legitimate zero-usage range
/// is a different situation from no log data existing at all.
pub fn build_metric_display(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>,
) -> MetricDisplay {
    let (start, end) = range_bounds(range, now, session_resets_at);
    let in_range: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.timestamp >= start && e.timestamp <= end)
        .collect();

    let subtitle = match range {
        RangeKind::Today => "today",
        RangeKind::SevenDay => "7 days",
        RangeKind::Session => "session",
    };

    match metric {
        MetricKind::Tokens => {
            let total: u64 = in_range.iter().map(|e| e.total_tokens()).sum();
            MetricDisplay {
                label: "Tokens",
                value_text: format_tokens(total),
                subtitle,
                accent_color: TOKENS_ACCENT,
            }
        }
        MetricKind::Cost => {
            let total: f64 = in_range.iter().filter_map(|e| cost_for_entry(e)).sum();
            MetricDisplay {
                label: "Cost",
                value_text: format_cost(total),
                subtitle,
                accent_color: COST_ACCENT,
            }
        }
    }
}

/// No log data available anywhere (a fresh install, or `~/.claude/projects`
/// missing/unreadable) - a clearly-labeled "no data" state, mirroring
/// `format::error_display()`.
pub fn error_display() -> MetricDisplay {
    MetricDisplay {
        label: "\u{2014}",
        value_text: "\u{2014}".to_string(),
        subtitle: "no data",
        accent_color: NO_DATA_ACCENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    #[test]
    fn today_is_a_rolling_24h_window_ending_now() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::Today, now, None);
        assert_eq!(start, now - Duration::hours(24));
        assert_eq!(end, now);
    }

    #[test]
    fn seven_day_is_a_rolling_168h_window_ending_now() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::SevenDay, now, None);
        assert_eq!(start, now - Duration::hours(168));
        assert_eq!(end, now);
    }

    #[test]
    fn session_uses_the_five_hour_window_ending_at_resets_at_when_known() {
        let now = dt(20, 0);
        let resets_at = dt(22, 40);
        let (start, end) = range_bounds(RangeKind::Session, now, Some(resets_at));
        assert_eq!(start, resets_at - Duration::hours(5));
        assert_eq!(end, resets_at);
    }

    #[test]
    fn session_falls_back_to_a_rolling_five_hour_window_when_resets_at_is_unknown() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::Session, now, None);
        assert_eq!(start, now - Duration::hours(5));
        assert_eq!(end, now);
    }

    #[test]
    fn format_tokens_under_a_thousand_is_exact() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands_trims_a_trailing_zero_decimal() {
        assert_eq!(format_tokens(318_000), "318K");
    }

    #[test]
    fn format_tokens_thousands_keeps_a_meaningful_decimal() {
        assert_eq!(format_tokens(318_500), "318.5K");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_234_567), "1.2M");
        assert_eq!(format_tokens(2_000_000), "2M");
    }

    #[test]
    fn format_cost_always_shows_two_decimals() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(8.4), "$8.40");
        assert_eq!(format_cost(1234.567), "$1234.57");
    }

    fn entry(timestamp: DateTime<Utc>, model: &str, input: u64, output: u64) -> LogEntry {
        LogEntry {
            timestamp,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    #[test]
    fn build_metric_display_sums_tokens_only_within_range() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 100, 50), // within last 24h
            entry(now - Duration::hours(30), "claude-sonnet-5", 999, 999), // outside the Today (24h) window
        ];
        let display = build_metric_display(&entries, MetricKind::Tokens, RangeKind::Today, now, None);
        assert_eq!(display.label, "Tokens");
        assert_eq!(display.value_text, "150");
        assert_eq!(display.subtitle, "today");
        assert_eq!(display.accent_color, TOKENS_ACCENT);
    }

    #[test]
    fn build_metric_display_cost_skips_entries_with_an_unrecognized_model() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 1_000_000, 0), // $3.00 at sonnet input rate
            entry(now - Duration::hours(1), "some-future-model", 1_000_000, 0), // unrecognized - excluded
        ];
        let display = build_metric_display(&entries, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.label, "Cost");
        assert_eq!(display.value_text, "$3.00");
    }

    #[test]
    fn build_metric_display_subtitle_matches_each_range() {
        let now = dt(20, 0);
        let entries: Vec<LogEntry> = vec![];
        assert_eq!(build_metric_display(&entries, MetricKind::Tokens, RangeKind::SevenDay, now, None).subtitle, "7 days");
        assert_eq!(build_metric_display(&entries, MetricKind::Tokens, RangeKind::Session, now, None).subtitle, "session");
    }

    #[test]
    fn error_display_is_a_clear_no_data_state() {
        let display = error_display();
        assert_eq!(display.subtitle, "no data");
        assert_eq!(display.accent_color, NO_DATA_ACCENT);
    }
}
