use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::format::DISABLED_COLOR;
use crate::pricing::CostTotal;
use crate::source::logs::LogEntry;

pub const TOKENS_ACCENT: &str = "#38bdf8";
pub const COST_ACCENT: &str = "#fb923c";

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

/// `[start, end]` bounds for `range`, in UTC. `Today` runs from local
/// midnight (in `now`'s time zone, like the Heatmap's days and the
/// Sparkline's "Today"); `SevenDay` is a rolling 7x24h ending at `now`.
/// `Session`
/// mirrors the existing Usage Gauge's 5-hour rate-limit window:
/// `[resets_at - 5h, resets_at]` when `session_resets_at` is known
/// (from the same usage fetch the gauge renders), falling
/// back to a rolling last-5h window when it isn't - a fallback, not an
/// error, matching this plugin's existing convention.
pub fn range_bounds<Tz: TimeZone>(
    range: RangeKind,
    now: DateTime<Tz>,
    session_resets_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let local_midnight = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|m| now.timezone().from_local_datetime(&m).earliest())
        .map(|m| m.with_timezone(&Utc));
    let now = now.with_timezone(&Utc);
    match range {
        // A zone with no 00:00 that day (a DST jump at midnight) falls back
        // to the last 24 hours.
        RangeKind::Today => (local_midnight.unwrap_or(now - Duration::hours(24)), now),
        RangeKind::SevenDay => (now - Duration::hours(24 * 7), now),
        RangeKind::Session => {
            let end = session_resets_at.unwrap_or(now);
            let start = end - Duration::hours(5);
            (start, end)
        }
    }
}

/// `< 1000` as an exact integer; otherwise one decimal place with a
/// K/M/B suffix, trimming a trailing ".0" (`318000` -> `"318K"`, `318500`
/// -> `"318.5K"`, `1234567` -> `"1.2M"`) - matches the mockup this tile
/// is based on.
pub fn format_tokens(total: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    const B: u64 = 1_000_000_000;
    // The lower-tier cutoff for each boundary is 999_950 (or its scaled
    // equivalent), not the round power of ten - a raw `< 1_000_000` check
    // would let e.g. 999_950 format at the K tier, round to "1000.0" at
    // one decimal place, and display as the nonsensical "1000K" instead
    // of switching tiers to "1M".
    if total < 1000 {
        return total.to_string();
    }
    let (divisor, suffix) = if total < 999_950 {
        (K, "K")
    } else if total < 999_950 * K {
        (M, "M")
    } else {
        (B, "B")
    };
    let value = total as f64 / divisor as f64;
    let formatted = format!("{value:.1}");
    let trimmed = formatted.strip_suffix(".0").unwrap_or(&formatted);
    format!("{trimmed}{suffix}")
}

/// Always two decimal places, e.g. `"$8.40"`.
pub fn format_cost(total: f64) -> String {
    format!("${total:.2}")
}

/// `format_cost`, plus a trailing `+` when some usage couldn't be priced
/// (a model missing from `pricing.rs`): the real total is at least this.
pub fn format_cost_total(total: CostTotal) -> String {
    let marker = if total.partial { "+" } else { "" };
    format!("{}{marker}", format_cost(total.dollars))
}

/// Computes what to show for one instance's current metric/range
/// selection from the full set of parsed log entries. Always produces a
/// real number (possibly zero) - callers decide separately whether "no
/// entries at all were found anywhere" warrants `error_display()`
/// instead (see `metric_action.rs`), since a legitimate zero-usage range
/// is a different situation from no log data existing at all.
pub fn build_metric_display<Tz: TimeZone>(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    now: DateTime<Tz>,
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
        MetricKind::Cost => MetricDisplay {
            label: "Cost",
            value_text: format_cost_total(CostTotal::of(in_range.iter().copied())),
            subtitle,
            accent_color: COST_ACCENT,
        },
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
        accent_color: DISABLED_COLOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    /// "Today" is the local calendar day, as on the Heatmap and the
    /// Sparkline - not the last 24 hours.
    #[test]
    fn today_starts_at_local_midnight() {
        let plus2 = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        // 09:00 local on the 13th = 07:00 UTC.
        let now = dt(7, 0).with_timezone(&plus2);
        let (start, end) = range_bounds(RangeKind::Today, now, None);
        // Local midnight on the 13th = 22:00 UTC on the 12th.
        assert_eq!(start, dt(7, 0) - Duration::hours(9));
        assert_eq!(end, dt(7, 0));
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
    fn format_tokens_billions() {
        assert_eq!(format_tokens(2_586_419_159), "2.6B");
        assert_eq!(format_tokens(5_000_000_000), "5B");
    }

    #[test]
    fn format_tokens_switches_tiers_at_the_rounding_boundary_not_the_power_of_ten() {
        assert_eq!(format_tokens(999_949), "999.9K");
        assert_eq!(format_tokens(999_950), "1M");
        assert_eq!(format_tokens(999_999), "1M");
    }

    #[test]
    fn format_tokens_switches_from_millions_to_billions_at_the_same_rounding_boundary() {
        assert_eq!(format_tokens(999_999_999), "1B");
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
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    #[test]
    fn build_metric_display_sums_tokens_only_within_range() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 100, 50), // today
            entry(now - Duration::hours(21), "claude-sonnet-5", 999, 999), // yesterday, within 24h
        ];
        let display =
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::Today, now, None);
        assert_eq!(display.label, "Tokens");
        assert_eq!(display.value_text, "150");
        assert_eq!(display.subtitle, "today");
        assert_eq!(display.accent_color, TOKENS_ACCENT);
    }

    #[test]
    fn build_metric_display_cost_marks_entries_with_an_unrecognized_model() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 1_000_000, 0), // $2.00 at Sonnet 5's input rate
            entry(now - Duration::hours(1), "some-future-model", 1_000_000, 0), // unpriced
        ];
        let display = build_metric_display(&entries, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.label, "Cost");
        // At least $2.00: one entry couldn't be priced.
        assert_eq!(display.value_text, "$2.00+");
        let priced = &entries[..1];
        let display = build_metric_display(priced, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.value_text, "$2.00");
    }

    #[test]
    fn build_metric_display_subtitle_matches_each_range() {
        let now = dt(20, 0);
        let entries: Vec<LogEntry> = vec![];
        assert_eq!(
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::SevenDay, now, None)
                .subtitle,
            "7 days"
        );
        assert_eq!(
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::Session, now, None)
                .subtitle,
            "session"
        );
    }

    #[test]
    fn error_display_is_a_clear_no_data_state() {
        let display = error_display();
        assert_eq!(display.subtitle, "no data");
        assert_eq!(display.accent_color, DISABLED_COLOR);
    }
}
