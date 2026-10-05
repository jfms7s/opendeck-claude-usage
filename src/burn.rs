//! Burn Rate readouts - Pace, Even burn, Runway - for one usage window.
//! Pure; `burn_icon.rs` draws the keypad tile and `burn_feedback` builds
//! the dial payload.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::format::{DISABLED_COLOR, format_duration_compact, usage_feedback};
use crate::level::ColorSettings;
use crate::pace::{Pace as PaceReading, Runway, pace};
use crate::source::{UsageSnapshot, WindowKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BurnMetric {
    #[default]
    Pace,
    EvenBurn,
    Runway,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BurnDisplay {
    /// Small uppercase heading on the keypad tile.
    pub label: &'static str,
    pub value_text: String,
    pub subtitle: String,
    /// Dial detail line: subtitle plus which window it's about.
    pub detail_text: String,
    pub color: String,
    /// Projected % at reset, clamped to 0..=100 (0 when unknown).
    pub bar_value: f64,
}

impl BurnMetric {
    pub fn label(self) -> &'static str {
        match self {
            BurnMetric::Pace => "PACE",
            BurnMetric::EvenBurn => "EVEN BURN",
            BurnMetric::Runway => "RUNWAY",
        }
    }
}

/// Burn Rate has no Monthly (`extra_usage` has no window length) - a
/// stored "monthly" from a hand-edited or future settings blob falls back
/// to Session instead of showing nothing.
pub fn burn_window(window: WindowKind) -> WindowKind {
    match window {
        WindowKind::Monthly => WindowKind::Session,
        other => other,
    }
}

pub fn build_burn_display(
    snapshot: &UsageSnapshot,
    window: WindowKind,
    metric: BurnMetric,
    colors: &ColorSettings,
    now: DateTime<Utc>,
) -> BurnDisplay {
    let kind = burn_window(window);
    let usage = match kind {
        WindowKind::Weekly => &snapshot.weekly,
        _ => &snapshot.session,
    };
    let reading = pace(usage, kind, now);
    let level = colors.pace_level(usage.percent, reading.map(|p| p.projected));
    let (value_text, subtitle) = match reading {
        Some(p) => metric_text(metric, kind, &p),
        None if usage.resets_at.is_none() => ("\u{2014}".to_string(), "no reset info".to_string()),
        None => ("\u{2014}".to_string(), "too early".to_string()),
    };
    let window_name = if kind == WindowKind::Weekly {
        "weekly"
    } else {
        "session"
    };
    BurnDisplay {
        label: metric.label(),
        detail_text: format!("{subtitle} \u{b7} {window_name}"),
        color: colors.palette.color(level).to_string(),
        bar_value: reading.map_or(0.0, |p| p.projected.clamp(0.0, 100.0)),
        value_text,
        subtitle,
    }
}

fn metric_text(metric: BurnMetric, kind: WindowKind, p: &PaceReading) -> (String, String) {
    let (value, subtitle) = match metric {
        BurnMetric::Pace if kind == WindowKind::Weekly => {
            (format!("{:.1}%", p.rate_per_hour * 24.0), "per day")
        }
        BurnMetric::Pace => (format!("{:.1}%", p.rate_per_hour), "per hour"),
        BurnMetric::EvenBurn => (format!("{:.1}x", p.even_burn), "even burn"),
        BurnMetric::Runway => match p.runway {
            Runway::Empty => ("0".to_string(), "empty"),
            Runway::LastsToReset(Some(d)) => (format_duration_compact(d), "lasts to reset"),
            Runway::LastsToReset(None) => ("\u{221e}".to_string(), "lasts to reset"),
            Runway::Until(d) => (format_duration_compact(d), "runs out early"),
        },
    };
    (value, subtitle.to_string())
}

pub fn burn_error_display(metric: BurnMetric) -> BurnDisplay {
    BurnDisplay {
        label: metric.label(),
        value_text: "\u{2014}".to_string(),
        subtitle: "no data".to_string(),
        detail_text: "no data".to_string(),
        color: DISABLED_COLOR.to_string(),
        bar_value: 0.0,
    }
}

/// Dial payload for the shared `layouts/usage.json` (see
/// `format::usage_feedback`).
pub fn burn_feedback(display: &BurnDisplay) -> Value {
    usage_feedback(
        display.bar_value,
        &display.color,
        &display.value_text,
        &display.detail_text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{DEFAULT_CRITICAL, DEFAULT_NORMAL, DEFAULT_WATCH};
    use crate::source::{MonthlyUsage, WindowUsage};
    use chrono::TimeZone;

    fn at(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, day, hour, minute, 0).unwrap()
    }

    /// Session 17:40-22:40 on the 13th; weekly ends 06:00 on the 17th
    /// (started 06:00 on the 10th, so 18:00 on the 13th is exactly 50%).
    fn snapshot(session_percent: f64) -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: session_percent,
                resets_at: Some(at(13, 22, 40)),
            },
            weekly: WindowUsage {
                percent: 29.0,
                resets_at: Some(at(17, 6, 0)),
            },
            monthly: MonthlyUsage {
                enabled: false,
                percent: None,
                used_dollars: None,
                limit_dollars: None,
            },
        }
    }

    fn build(window: WindowKind, metric: BurnMetric, now: DateTime<Utc>) -> BurnDisplay {
        build_burn_display(
            &snapshot(30.0),
            window,
            metric,
            &ColorSettings::default(),
            now,
        )
    }

    #[test]
    fn session_pace_is_per_hour() {
        let d = build(WindowKind::Session, BurnMetric::Pace, at(13, 18, 55));
        assert_eq!(d.label, "PACE");
        assert_eq!(d.value_text, "24.0%");
        assert_eq!(d.subtitle, "per hour");
        assert_eq!(d.detail_text, "per hour \u{b7} session");
    }

    #[test]
    fn weekly_pace_is_per_day() {
        // 29% over 84h = 0.345%/h = 8.3%/day; projected 58% -> Watch.
        let d = build(WindowKind::Weekly, BurnMetric::Pace, at(13, 18, 0));
        assert_eq!(d.value_text, "8.3%");
        assert_eq!(d.subtitle, "per day");
        assert_eq!(d.color, DEFAULT_WATCH);
        assert_eq!(d.detail_text, "per day \u{b7} weekly");
    }

    #[test]
    fn even_burn_is_always_pace_colored() {
        // Fixed mode (default) but 30% at 25% elapsed = 1.2x -> projected 120% -> Critical.
        let d = build(WindowKind::Session, BurnMetric::EvenBurn, at(13, 18, 55));
        assert_eq!(d.label, "EVEN BURN");
        assert_eq!(d.value_text, "1.2x");
        assert_eq!(d.subtitle, "even burn");
        assert_eq!(d.color, DEFAULT_CRITICAL);
        assert_eq!(d.bar_value, 100.0);
    }

    #[test]
    fn runway_until_empty() {
        let d = build(WindowKind::Session, BurnMetric::Runway, at(13, 18, 55));
        assert_eq!(d.label, "RUNWAY");
        assert_eq!(d.value_text, "2h 55m");
        assert_eq!(d.subtitle, "runs out early");
    }

    #[test]
    fn runway_lasts_to_reset() {
        let d = build_burn_display(
            &snapshot(10.0),
            WindowKind::Session,
            BurnMetric::Runway,
            &ColorSettings::default(),
            at(13, 20, 10),
        );
        // 4%/h needs 22.5h for the remaining 90% - past the 22:40 reset.
        assert_eq!(d.value_text, "22h 30m");
        assert_eq!(d.subtitle, "lasts to reset");
    }

    #[test]
    fn runway_with_nothing_used_is_infinite() {
        let d = build_burn_display(
            &snapshot(0.0),
            WindowKind::Session,
            BurnMetric::Runway,
            &ColorSettings::default(),
            at(13, 20, 10),
        );
        assert_eq!(d.value_text, "\u{221e}");
        assert_eq!(d.subtitle, "lasts to reset");
    }

    #[test]
    fn runway_empty() {
        let d = build_burn_display(
            &snapshot(100.0),
            WindowKind::Session,
            BurnMetric::Runway,
            &ColorSettings::default(),
            at(13, 20, 10),
        );
        assert_eq!(d.value_text, "0");
        assert_eq!(d.subtitle, "empty");
    }

    #[test]
    fn too_early_shows_a_dash() {
        let d = build(WindowKind::Session, BurnMetric::Pace, at(13, 18, 0));
        assert_eq!(d.value_text, "\u{2014}");
        assert_eq!(d.subtitle, "too early");
        assert_eq!(d.color, DEFAULT_NORMAL); // actual 30% only
        assert_eq!(d.bar_value, 0.0);
    }

    #[test]
    fn missing_reset_says_so() {
        let mut s = snapshot(30.0);
        s.session.resets_at = None;
        let d = build_burn_display(
            &s,
            WindowKind::Session,
            BurnMetric::Pace,
            &ColorSettings::default(),
            at(13, 20, 0),
        );
        assert_eq!(d.value_text, "\u{2014}");
        assert_eq!(d.subtitle, "no reset info");
    }

    #[test]
    fn monthly_falls_back_to_session() {
        assert_eq!(burn_window(WindowKind::Monthly), WindowKind::Session);
        let d = build(WindowKind::Monthly, BurnMetric::Pace, at(13, 18, 55));
        assert_eq!(d.detail_text, "per hour \u{b7} session");
    }

    #[test]
    fn error_display_is_grey_no_data() {
        let d = burn_error_display(BurnMetric::Runway);
        assert_eq!(d.label, "RUNWAY");
        assert_eq!(d.subtitle, "no data");
        assert_eq!(d.color, DISABLED_COLOR);
    }

    #[test]
    fn feedback_uses_the_usage_layout_keys() {
        let d = build(WindowKind::Session, BurnMetric::EvenBurn, at(13, 18, 55));
        let f = burn_feedback(&d);
        assert_eq!(f["percent"], "1.2x");
        assert_eq!(f["bar"]["value"], 100.0);
        assert_eq!(f["bar"]["bar_fill_c"], DEFAULT_CRITICAL);
        assert_eq!(f["detail"], "even burn \u{b7} session");
    }

    #[test]
    fn metric_wire_names() {
        let m: BurnMetric = serde_json::from_str("\"evenBurn\"").unwrap();
        assert_eq!(m, BurnMetric::EvenBurn);
    }
}
