use chrono::{DateTime, Utc};

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
}
