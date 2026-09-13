use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::source::{MonthlyUsage, UsageSnapshot, WindowKind, WindowUsage};

pub const DISABLED_COLOR: &str = "#6b7280";

/// Rounds to the nearest whole percent, matching the existing statusline
/// convention (`printf "%.0f"`).
pub fn format_percent(percent: f64) -> String {
    format!("{:.0}%", percent.round())
}

/// Threshold colors matching the existing statusline convention: green below
/// 50%, yellow 50-79%, red 80% and up.
pub fn bar_color(percent: f64) -> &'static str {
    if percent >= 80.0 {
        "#ef4444"
    } else if percent >= 50.0 {
        "#eab308"
    } else {
        "#22c55e"
    }
}

/// "resets in Xh Ym" / "resets in Ym" / "resets in <1m" / "resets now",
/// taking `now` explicitly so this stays a pure, deterministic function
/// rather than reading the system clock itself.
pub fn format_countdown(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = resets_at - now;
    if remaining <= chrono::Duration::zero() {
        return "resets now".to_string();
    }
    let total_minutes = remaining.num_minutes();
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    if hours > 0 {
        format!("resets in {hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("resets in {minutes}m")
    } else {
        "resets in <1m".to_string()
    }
}

/// Builds the `setFeedback` payload for one dial: a flat object keyed by
/// each layout item's `key` (see assets/layouts/usage.json) - "percent" and
/// "detail" are plain strings (Text items), "bar" is an object updating both
/// the fill value and its color in one push (Bar items accept either a bare
/// number/string for just `value`, or an object for `value` plus any of its
/// other fields like `bar_fill_c`).
pub fn build_feedback(snapshot: &UsageSnapshot, window: WindowKind, now: DateTime<Utc>) -> Value {
    match window {
        WindowKind::Session => window_feedback(&snapshot.session, now),
        WindowKind::Weekly => window_feedback(&snapshot.weekly, now),
        WindowKind::Monthly => monthly_feedback(&snapshot.monthly),
    }
}

fn window_feedback(window: &WindowUsage, now: DateTime<Utc>) -> Value {
    let detail = match window.resets_at {
        Some(resets_at) => format_countdown(resets_at, now),
        None => "no reset info".to_string(),
    };
    bar_feedback(window.percent, bar_color(window.percent), &format_percent(window.percent), &detail)
}

fn monthly_feedback(monthly: &MonthlyUsage) -> Value {
    if !monthly.enabled {
        return bar_feedback(0.0, DISABLED_COLOR, "\u{2014}", "not enabled");
    }
    let percent = monthly.percent.unwrap_or(0.0);
    let detail = match (monthly.used_dollars, monthly.limit_dollars) {
        (Some(used), Some(limit)) => format!("${used:.2} / ${limit:.2}"),
        _ => "spend unavailable".to_string(),
    };
    bar_feedback(percent, bar_color(percent), &format_percent(percent), &detail)
}

fn bar_feedback(bar_value: f64, bar_color: &str, percent_text: &str, detail_text: &str) -> Value {
    json!({
        "bar": { "value": bar_value, "bar_fill_c": bar_color },
        "percent": percent_text,
        "detail": detail_text,
    })
}

/// Rendered when `UsageSource::read()` fails - a clearly-labeled "no data"
/// state, never a blank or stale display.
pub fn error_feedback() -> Value {
    bar_feedback(0.0, DISABLED_COLOR, "\u{2014}", "no data")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, second).unwrap()
    }

    #[test]
    fn formats_percent_rounded_to_whole_number() {
        assert_eq!(format_percent(32.6), "33%");
        assert_eq!(format_percent(0.0), "0%");
        assert_eq!(format_percent(100.0), "100%");
    }

    #[test]
    fn bar_color_thresholds() {
        assert_eq!(bar_color(0.0), "#22c55e");
        assert_eq!(bar_color(49.9), "#22c55e");
        assert_eq!(bar_color(50.0), "#eab308");
        assert_eq!(bar_color(79.9), "#eab308");
        assert_eq!(bar_color(80.0), "#ef4444");
        assert_eq!(bar_color(100.0), "#ef4444");
    }

    #[test]
    fn countdown_with_hours_and_minutes() {
        assert_eq!(format_countdown(dt(22, 40, 0), dt(20, 30, 0)), "resets in 2h 10m");
    }

    #[test]
    fn countdown_under_an_hour() {
        assert_eq!(format_countdown(dt(20, 45, 0), dt(20, 30, 0)), "resets in 15m");
    }

    #[test]
    fn countdown_under_a_minute() {
        assert_eq!(format_countdown(dt(20, 30, 30), dt(20, 30, 0)), "resets in <1m");
    }

    #[test]
    fn countdown_already_passed() {
        assert_eq!(format_countdown(dt(20, 0, 0), dt(20, 30, 0)), "resets now");
        assert_eq!(format_countdown(dt(20, 30, 0), dt(20, 30, 0)), "resets now");
    }

    fn snapshot() -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: 33.0,
                resets_at: Some(dt(22, 40, 0)),
            },
            weekly: WindowUsage {
                percent: 29.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 17, 6, 0, 0).unwrap()),
            },
            monthly: MonthlyUsage {
                enabled: true,
                percent: Some(25.0),
                used_dollars: Some(12.5),
                limit_dollars: Some(50.0),
            },
        }
    }

    #[test]
    fn builds_session_feedback() {
        let feedback = build_feedback(&snapshot(), WindowKind::Session, dt(20, 30, 0));
        assert_eq!(feedback["percent"], "33%");
        assert_eq!(feedback["bar"]["value"], 33.0);
        assert_eq!(feedback["bar"]["bar_fill_c"], "#22c55e");
        assert_eq!(feedback["detail"], "resets in 2h 10m");
    }

    #[test]
    fn builds_weekly_feedback() {
        let feedback = build_feedback(&snapshot(), WindowKind::Weekly, dt(20, 30, 0));
        assert_eq!(feedback["percent"], "29%");
        assert_eq!(feedback["bar"]["bar_fill_c"], "#22c55e");
    }

    #[test]
    fn builds_enabled_monthly_feedback() {
        let feedback = build_feedback(&snapshot(), WindowKind::Monthly, dt(20, 30, 0));
        assert_eq!(feedback["percent"], "25%");
        assert_eq!(feedback["detail"], "$12.50 / $50.00");
    }

    #[test]
    fn builds_disabled_monthly_feedback() {
        let mut s = snapshot();
        s.monthly = MonthlyUsage {
            enabled: false,
            percent: None,
            used_dollars: None,
            limit_dollars: None,
        };
        let feedback = build_feedback(&s, WindowKind::Monthly, dt(20, 30, 0));
        assert_eq!(feedback["percent"], "\u{2014}");
        assert_eq!(feedback["detail"], "not enabled");
        assert_eq!(feedback["bar"]["bar_fill_c"], DISABLED_COLOR);
    }

    #[test]
    fn error_feedback_is_a_clear_no_data_state() {
        let feedback = error_feedback();
        assert_eq!(feedback["detail"], "no data");
        assert_eq!(feedback["bar"]["value"], 0.0);
    }
}
