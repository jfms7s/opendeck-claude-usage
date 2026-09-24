use chrono::{DateTime, Utc};
use serde_json::{Value, json};

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

/// "resets in Xd Yh" / "resets in Xh Ym" / "resets in Ym" /
/// "resets in <1m" / "resets now", taking `now` explicitly so this stays a
/// pure, deterministic function rather than reading the system clock itself.
pub fn format_countdown(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match format_remaining(resets_at, now) {
        Some(remaining) => format!("resets in {remaining}"),
        None => "resets now".to_string(),
    }
}

/// The bare remaining time - "Xd Yh" / "Xh Ym" / "Ym" / "<1m" - or `None`
/// once `resets_at` has passed. Days kick in at 24h so a weekly window
/// reads "6d 10h" rather than "154h 34m".
fn format_remaining(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let remaining = resets_at - now;
    if remaining <= chrono::Duration::zero() {
        return None;
    }
    let total_minutes = remaining.num_minutes();
    let days = total_minutes / (24 * 60);
    let hours = total_minutes / 60 % 24;
    let minutes = total_minutes % 60;
    Some(if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "<1m".to_string()
    })
}

/// Keypad-tile variant of `format_countdown`: the tile has room for about
/// a dozen characters, so it drops the "resets in" prefix.
fn format_countdown_short(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format_remaining(resets_at, now).unwrap_or_else(|| "now".to_string())
}

/// Everything needed to render one instance's current state, independent of
/// which controller (Encoder touch-strip feedback, Keypad title+icon) ends
/// up drawing it - both surfaces render from the same computed values so
/// they can never drift apart.
pub struct UsageDisplay {
    pub percent_text: String,
    pub color: &'static str,
    pub detail_text: String,
    /// Shorter `detail_text` for the keypad tile's second line.
    pub tile_detail: String,
    /// Clamped to 0..=100 - a genuine >100% (e.g. an overage) still shows as
    /// "105%" in `percent_text`, but an out-of-range bar/gauge value renders
    /// undefined on the actual hardware.
    pub bar_value: f64,
}

/// Computes what to show for one instance's current state, independent of
/// which controller ends up rendering it. `feedback_for_display` below turns
/// this into an Encoder's `setFeedback` payload; `icon::build_icon` and a
/// two-line title turn it into a Keypad tile.
pub fn build_display(
    snapshot: &UsageSnapshot,
    window: WindowKind,
    now: DateTime<Utc>,
) -> UsageDisplay {
    match window {
        WindowKind::Session => window_display(&snapshot.session, now),
        WindowKind::Weekly => window_display(&snapshot.weekly, now),
        WindowKind::Monthly => monthly_display(&snapshot.monthly),
    }
}

fn window_display(window: &WindowUsage, now: DateTime<Utc>) -> UsageDisplay {
    let (detail, tile_detail) = match window.resets_at {
        Some(resets_at) => (
            format_countdown(resets_at, now),
            format_countdown_short(resets_at, now),
        ),
        None => ("no reset info".to_string(), "\u{2014}".to_string()),
    };
    make_display(
        window.percent,
        bar_color(window.percent),
        detail,
        tile_detail,
    )
}

fn monthly_display(monthly: &MonthlyUsage) -> UsageDisplay {
    if !monthly.enabled {
        return UsageDisplay {
            percent_text: "\u{2014}".to_string(),
            color: DISABLED_COLOR,
            detail_text: "not enabled".to_string(),
            tile_detail: "not enabled".to_string(),
            bar_value: 0.0,
        };
    }
    let percent = monthly.percent.unwrap_or(0.0);
    let (detail, tile_detail) = match (monthly.used_dollars, monthly.limit_dollars) {
        (Some(used), Some(limit)) => (
            format!("${used:.2} / ${limit:.2}"),
            format!("${used:.2}/${limit:.0}"),
        ),
        _ => ("spend unavailable".to_string(), "no spend".to_string()),
    };
    make_display(percent, bar_color(percent), detail, tile_detail)
}

fn make_display(
    percent: f64,
    color: &'static str,
    detail_text: String,
    tile_detail: String,
) -> UsageDisplay {
    UsageDisplay {
        percent_text: format_percent(percent),
        color,
        detail_text,
        tile_detail,
        bar_value: percent.clamp(0.0, 100.0),
    }
}

/// Converts an already-computed `UsageDisplay` into an Encoder's
/// `setFeedback` payload: a flat object keyed by each layout item's `key`
/// (see assets/layouts/usage.json) - "percent" and "detail" are plain
/// strings (Text items), "bar" is an object updating both the fill value
/// and its color in one push (Bar items accept either a bare number/string
/// for just `value`, or an object for `value` plus other fields like
/// `bar_fill_c`).
pub fn feedback_for_display(display: &UsageDisplay) -> Value {
    json!({
        "bar": { "value": display.bar_value, "bar_fill_c": display.color },
        "percent": display.percent_text,
        "detail": display.detail_text,
    })
}

/// Computed when `UsageSource::read()` fails - a clearly-labeled "no data"
/// state, never a blank or stale display.
pub fn error_display() -> UsageDisplay {
    UsageDisplay {
        percent_text: "\u{2014}".to_string(),
        color: DISABLED_COLOR,
        detail_text: "no data".to_string(),
        tile_detail: "no data".to_string(),
        bar_value: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, second)
            .unwrap()
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
        assert_eq!(
            format_countdown(dt(22, 40, 0), dt(20, 30, 0)),
            "resets in 2h 10m"
        );
    }

    #[test]
    fn countdown_under_an_hour() {
        assert_eq!(
            format_countdown(dt(20, 45, 0), dt(20, 30, 0)),
            "resets in 15m"
        );
    }

    #[test]
    fn countdown_under_a_minute() {
        assert_eq!(
            format_countdown(dt(20, 30, 30), dt(20, 30, 0)),
            "resets in <1m"
        );
    }

    #[test]
    fn countdown_of_a_day_or_more_uses_days() {
        assert_eq!(
            format_countdown(
                dt(20, 30, 0) + chrono::Duration::minutes(154 * 60 + 34),
                dt(20, 30, 0)
            ),
            "resets in 6d 10h"
        );
    }

    #[test]
    fn short_countdown_drops_the_prefix() {
        assert_eq!(
            format_countdown_short(dt(22, 40, 0), dt(20, 30, 0)),
            "2h 10m"
        );
        assert_eq!(format_countdown_short(dt(20, 0, 0), dt(20, 30, 0)), "now");
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

    // `build_feedback`/`error_feedback` aren't kept as production functions
    // (nothing outside tests calls them since `render()` in action.rs
    // dispatches through `UsageDisplay` instead) - these two just compose
    // the same pipeline for the JSON-shape assertions below.
    fn build_feedback(snapshot: &UsageSnapshot, window: WindowKind, now: DateTime<Utc>) -> Value {
        feedback_for_display(&build_display(snapshot, window, now))
    }

    fn error_feedback() -> Value {
        feedback_for_display(&error_display())
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
    fn bar_value_is_clamped_but_percent_text_is_not() {
        let mut s = snapshot();
        s.session.percent = 142.0;
        let feedback = build_feedback(&s, WindowKind::Session, dt(20, 30, 0));
        assert_eq!(feedback["percent"], "142%");
        assert_eq!(feedback["bar"]["value"], 100.0);
    }

    #[test]
    fn error_feedback_is_a_clear_no_data_state() {
        let feedback = error_feedback();
        assert_eq!(feedback["detail"], "no data");
        assert_eq!(feedback["bar"]["value"], 0.0);
    }

    #[test]
    fn display_session() {
        let d = build_display(&snapshot(), WindowKind::Session, dt(20, 30, 0));
        assert_eq!(d.percent_text, "33%");
        assert_eq!(d.color, "#22c55e");
        assert_eq!(d.detail_text, "resets in 2h 10m");
        assert_eq!(d.tile_detail, "2h 10m");
        assert_eq!(d.bar_value, 33.0);
    }

    #[test]
    fn display_enabled_monthly_tile_detail_is_compact() {
        let d = build_display(&snapshot(), WindowKind::Monthly, dt(20, 30, 0));
        assert_eq!(d.tile_detail, "$12.50/$50");
    }

    #[test]
    fn display_disabled_monthly() {
        let mut s = snapshot();
        s.monthly = MonthlyUsage {
            enabled: false,
            percent: None,
            used_dollars: None,
            limit_dollars: None,
        };
        let d = build_display(&s, WindowKind::Monthly, dt(20, 30, 0));
        assert_eq!(d.percent_text, "\u{2014}");
        assert_eq!(d.color, DISABLED_COLOR);
        assert_eq!(d.detail_text, "not enabled");
    }

    #[test]
    fn display_clamps_bar_value_but_not_percent_text() {
        let mut s = snapshot();
        s.session.percent = 142.0;
        let d = build_display(&s, WindowKind::Session, dt(20, 30, 0));
        assert_eq!(d.percent_text, "142%");
        assert_eq!(d.bar_value, 100.0);
    }

    #[test]
    fn error_display_is_a_clear_no_data_state() {
        let d = error_display();
        assert_eq!(d.detail_text, "no data");
        assert_eq!(d.bar_value, 0.0);
        assert_eq!(d.color, DISABLED_COLOR);
    }
}
