//! Usage Sparkline series, computed from recorded readings: the usage
//! trend, per-poll increases, today's running increase, and the even-burn
//! ratio. Pure - `now` carries the time zone for "today".

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::burn::burn_window;
use crate::format::{DISABLED_COLOR, format_percent};
use crate::history::Reading;
use crate::level::ColorSettings;
use crate::pace::{pace, window_length};
use crate::source::{WindowKind, WindowUsage};

/// Between-polls keeps only the most recent steps, so a busy day doesn't
/// compress the line into noise.
pub const MAX_STEPS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SparkSeries {
    #[default]
    Trend,
    BetweenPolls,
    Today,
    EvenBurn,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "SparkSettingsWire", into = "SparkSettingsWire")]
pub struct SparkSettings {
    pub window: WindowKind,
    /// Changed only by a short press.
    pub series: SparkSeries,
}

/// Raw `Value`s for the same reason as `level::ColorSettingsWire`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SparkSettingsWire {
    window: Value,
    series: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SparkDisplay {
    pub caption: String,
    pub headline: String,
    /// `(x in 0..=1 across the time span, value)`; empty when there isn't
    /// enough history yet.
    pub points: Vec<(f64, f64)>,
    pub color: String,
}

impl SparkSeries {
    pub fn next(self) -> Self {
        match self {
            SparkSeries::Trend => SparkSeries::BetweenPolls,
            SparkSeries::BetweenPolls => SparkSeries::Today,
            SparkSeries::Today => SparkSeries::EvenBurn,
            SparkSeries::EvenBurn => SparkSeries::Trend,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SparkSeries::Trend => "TREND",
            SparkSeries::BetweenPolls => "PER POLL",
            SparkSeries::Today => "TODAY",
            SparkSeries::EvenBurn => "VS EVEN",
        }
    }
}

impl From<SparkSettingsWire> for SparkSettings {
    fn from(w: SparkSettingsWire) -> Self {
        Self {
            // Monthly has no window to trend over - fall back to Session.
            window: burn_window(serde_json::from_value(w.window).unwrap_or_default()),
            series: serde_json::from_value(w.series).unwrap_or_default(),
        }
    }
}

impl From<SparkSettings> for SparkSettingsWire {
    fn from(s: SparkSettings) -> Self {
        Self {
            window: json!(s.window),
            series: json!(s.series),
        }
    }
}

type Usage = (f64, Option<DateTime<Utc>>);
type Series = Vec<(DateTime<Utc>, f64)>;

fn usage(reading: &Reading, kind: WindowKind) -> Usage {
    match kind {
        WindowKind::Weekly => (reading.weekly, reading.weekly_resets_at),
        _ => (reading.session, reading.session_resets_at),
    }
}

/// Usage added between two consecutive readings: the new % minus the old,
/// or the new % itself when the window reset in between. Never negative.
fn step(prev: Usage, cur: Usage) -> f64 {
    let added = if prev.1 != cur.1 {
        cur.0
    } else {
        cur.0 - prev.0
    };
    added.max(0.0)
}

/// Readings in the latest reading's window (all of them if it has no
/// reset time).
fn current_window(readings: &[Reading], kind: WindowKind) -> Vec<&Reading> {
    let Some(last) = readings.last() else {
        return Vec::new();
    };
    match (usage(last, kind).1, window_length(kind)) {
        (Some(resets_at), Some(length)) => readings
            .iter()
            .filter(|r| r.at >= resets_at - length)
            .collect(),
        _ => readings.iter().collect(),
    }
}

fn trend(window: &[&Reading], kind: WindowKind) -> Series {
    window.iter().map(|r| (r.at, usage(r, kind).0)).collect()
}

fn between_polls(window: &[&Reading], kind: WindowKind) -> Series {
    let mut steps: Series = window
        .windows(2)
        .map(|pair| (pair[1].at, step(usage(pair[0], kind), usage(pair[1], kind))))
        .collect();
    if steps.len() > MAX_STEPS {
        steps.drain(..steps.len() - MAX_STEPS);
    }
    steps
}

/// Running total of usage added since local midnight. The baseline is the
/// last reading before midnight; without one, the first reading today
/// counts as zero.
fn today<Tz: TimeZone>(readings: &[Reading], kind: WindowKind, now: &DateTime<Tz>) -> Series {
    let tz = now.timezone();
    let today = now.date_naive();
    let Some(first) = readings
        .iter()
        .position(|r| r.at.with_timezone(&tz).date_naive() >= today)
    else {
        return Vec::new();
    };
    let (mut prev, start, mut out) = if first > 0 {
        (usage(&readings[first - 1], kind), first, Vec::new())
    } else {
        (usage(&readings[0], kind), 1, vec![(readings[0].at, 0.0)])
    };
    let mut total = 0.0;
    for reading in &readings[start..] {
        let cur = usage(reading, kind);
        total += step(prev, cur);
        prev = cur;
        out.push((reading.at, total));
    }
    out
}

fn even_burn(window: &[&Reading], kind: WindowKind) -> Series {
    window
        .iter()
        .filter_map(|r| {
            let (percent, resets_at) = usage(r, kind);
            pace(&WindowUsage { percent, resets_at }, kind, r.at).map(|p| (r.at, p.even_burn))
        })
        .collect()
}

pub fn build_sparkline<Tz: TimeZone>(
    readings: &[Reading],
    settings: &SparkSettings,
    colors: &ColorSettings,
    now: DateTime<Tz>,
) -> SparkDisplay {
    let kind = burn_window(settings.window);
    let span = if kind == WindowKind::Weekly {
        "7D"
    } else {
        "5H"
    };
    let caption = format!("{} \u{b7} {span}", settings.series.label());
    let window = current_window(readings, kind);
    let series = match settings.series {
        SparkSeries::Trend => trend(&window, kind),
        SparkSeries::BetweenPolls => between_polls(&window, kind),
        SparkSeries::Today => today(readings, kind, &now),
        SparkSeries::EvenBurn => even_burn(&window, kind),
    };
    let color = match readings.last() {
        Some(r) => {
            let (percent, resets_at) = usage(r, kind);
            let projected =
                pace(&WindowUsage { percent, resets_at }, kind, r.at).map(|p| p.projected);
            colors
                .palette
                .color(colors.level(percent, projected))
                .to_string()
        }
        None => DISABLED_COLOR.to_string(),
    };
    if series.len() < 2 {
        return SparkDisplay {
            caption,
            headline: "\u{2014}".to_string(),
            points: Vec::new(),
            color,
        };
    }
    let last = series[series.len() - 1].1;
    let headline = match settings.series {
        SparkSeries::Trend => format_percent(last),
        SparkSeries::BetweenPolls => format!("+{last:.1}pp"),
        SparkSeries::Today => format!("{last:.1}pp"),
        SparkSeries::EvenBurn => format!("{last:.1}x"),
    };
    let t0 = series[0].0;
    let span_secs = (series[series.len() - 1].0 - t0).num_seconds().max(1) as f64;
    let points = series
        .iter()
        .map(|(at, v)| ((*at - t0).num_seconds() as f64 / span_secs, *v))
        .collect();
    SparkDisplay {
        caption,
        headline,
        points,
        color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{ColorMode, DEFAULT_CRITICAL, DEFAULT_NORMAL};
    use chrono::FixedOffset;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn session(at: &str, pct: f64, resets: &str) -> Reading {
        Reading {
            at: t(at),
            session: pct,
            session_resets_at: Some(t(resets)),
            weekly: 0.0,
            weekly_resets_at: None,
        }
    }

    fn weekly(at: &str, pct: f64, resets: &str) -> Reading {
        Reading {
            at: t(at),
            session: 0.0,
            session_resets_at: None,
            weekly: pct,
            weekly_resets_at: Some(t(resets)),
        }
    }

    fn build(
        readings: &[Reading],
        window: WindowKind,
        series: SparkSeries,
        now: &str,
    ) -> SparkDisplay {
        let settings = SparkSettings { window, series };
        build_sparkline(readings, &settings, &ColorSettings::default(), t(now))
    }

    const R: &str = "2026-09-30T12:00:00Z"; // session 07:00-12:00

    #[test]
    fn series_cycle_and_labels() {
        assert_eq!(SparkSeries::Trend.next(), SparkSeries::BetweenPolls);
        assert_eq!(SparkSeries::BetweenPolls.next(), SparkSeries::Today);
        assert_eq!(SparkSeries::Today.next(), SparkSeries::EvenBurn);
        assert_eq!(SparkSeries::EvenBurn.next(), SparkSeries::Trend);
        assert_eq!(SparkSeries::BetweenPolls.label(), "PER POLL");
    }

    #[test]
    fn trend_uses_only_the_current_window() {
        let readings = [
            session("2026-09-30T06:00:00Z", 90.0, "2026-09-30T07:00:00Z"),
            session("2026-09-30T08:00:00Z", 10.0, R),
            session("2026-09-30T09:00:00Z", 20.0, R),
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::Trend,
            "2026-09-30T09:00:00Z",
        );
        assert_eq!(d.points, vec![(0.0, 10.0), (1.0, 20.0)]);
        assert_eq!(d.headline, "20%");
        assert_eq!(d.caption, "TREND \u{b7} 5H");
    }

    #[test]
    fn step_across_reset_is_the_new_percent() {
        assert_eq!(
            step((90.0, Some(t("2026-09-30T07:00:00Z"))), (10.0, Some(t(R)))),
            10.0
        );
        assert_eq!(step((20.0, Some(t(R))), (15.0, Some(t(R)))), 0.0);
        assert_eq!(step((20.0, Some(t(R))), (25.5, Some(t(R)))), 5.5);
    }

    #[test]
    fn between_polls_plots_each_increase() {
        let readings = [
            session("2026-09-30T08:00:00Z", 10.0, R),
            session("2026-09-30T09:00:00Z", 25.0, R),
            session("2026-09-30T09:30:00Z", 27.1, R),
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::BetweenPolls,
            "2026-09-30T09:30:00Z",
        );
        assert_eq!(d.points.len(), 2);
        assert_eq!(d.points[0], (0.0, 15.0));
        assert_eq!(d.headline, "+2.1pp");
    }

    #[test]
    fn between_polls_keeps_the_last_30_steps() {
        let readings: Vec<Reading> = (0..40)
            .map(|i| Reading {
                at: t("2026-09-30T08:00:00Z") + chrono::Duration::minutes(i),
                session: i as f64,
                session_resets_at: Some(t(R)),
                weekly: 0.0,
                weekly_resets_at: None,
            })
            .collect();
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::BetweenPolls,
            "2026-09-30T09:00:00Z",
        );
        assert_eq!(d.points.len(), MAX_STEPS);
    }

    const W: &str = "2026-10-03T00:00:00Z";

    fn today_at_plus_2(readings: &[Reading]) -> SparkDisplay {
        // 14:00 local (UTC+2) on the 30th; local midnight is 22:00Z on the 29th.
        let now = FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 30, 14, 0, 0)
            .unwrap();
        build_sparkline(
            readings,
            &SparkSettings {
                window: WindowKind::Weekly,
                series: SparkSeries::Today,
            },
            &ColorSettings::default(),
            now,
        )
    }

    #[test]
    fn today_starts_from_the_last_reading_before_midnight() {
        let d = today_at_plus_2(&[
            weekly("2026-09-29T21:00:00Z", 30.0, W), // 23:00 local, yesterday
            weekly("2026-09-29T23:00:00Z", 32.0, W), // 01:00 local, today
            weekly("2026-09-30T05:00:00Z", 35.0, W),
        ]);
        assert_eq!(d.points, vec![(0.0, 2.0), (1.0, 5.0)]);
        assert_eq!(d.headline, "5.0pp");
        assert_eq!(d.caption, "TODAY \u{b7} 7D");
    }

    #[test]
    fn today_without_a_baseline_starts_at_zero() {
        let d = today_at_plus_2(&[
            weekly("2026-09-29T23:00:00Z", 32.0, W),
            weekly("2026-09-30T05:00:00Z", 35.0, W),
        ]);
        assert_eq!(d.points, vec![(0.0, 0.0), (1.0, 3.0)]);
        assert_eq!(d.headline, "3.0pp");
    }

    #[test]
    fn today_counts_resets_as_new_usage() {
        let d = today_at_plus_2(&[
            weekly("2026-09-29T21:00:00Z", 90.0, "2026-09-29T22:30:00Z"),
            weekly("2026-09-29T23:00:00Z", 2.0, W),
            weekly("2026-09-30T05:00:00Z", 5.0, W),
        ]);
        assert_eq!(d.headline, "5.0pp");
    }

    #[test]
    fn even_burn_skips_the_too_early_guard() {
        let readings = [
            session("2026-09-30T07:10:00Z", 5.0, R), // 3% elapsed -> skipped
            session("2026-09-30T08:15:00Z", 30.0, R), // 25% -> 1.2x
            session("2026-09-30T09:30:00Z", 50.0, R), // 50% -> 1.0x
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::EvenBurn,
            "2026-09-30T09:30:00Z",
        );
        assert_eq!(d.points.len(), 2);
        assert!((d.points[0].1 - 1.2).abs() < 1e-9);
        assert_eq!(d.headline, "1.0x");
    }

    #[test]
    fn fewer_than_two_points_is_collecting() {
        let d = build(
            &[session("2026-09-30T08:00:00Z", 10.0, R)],
            WindowKind::Session,
            SparkSeries::Trend,
            R,
        );
        assert!(d.points.is_empty());
        assert_eq!(d.headline, "\u{2014}");
        let empty = build(&[], WindowKind::Session, SparkSeries::Trend, R);
        assert_eq!(empty.color, DISABLED_COLOR);
    }

    #[test]
    fn color_follows_the_latest_level_and_pace_mode() {
        let readings = [session("2026-09-30T08:15:00Z", 30.0, R)]; // projected 120%
        let fixed = build(&readings, WindowKind::Session, SparkSeries::Trend, R);
        assert_eq!(fixed.color, DEFAULT_NORMAL);
        let pace_colors = ColorSettings {
            mode: ColorMode::Pace,
            ..ColorSettings::default()
        };
        let settings = SparkSettings::default();
        let paced = build_sparkline(&readings, &settings, &pace_colors, t(R));
        assert_eq!(paced.color, DEFAULT_CRITICAL);
    }

    #[test]
    fn settings_wire_falls_back_per_field() {
        let s: SparkSettings =
            serde_json::from_str(r#"{"window":"monthly","series":"evenBurn"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.series, SparkSeries::EvenBurn);
        let s: SparkSettings = serde_json::from_str(r#"{"window":"weekly","series":9}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.series, SparkSeries::Trend);
        assert_eq!(
            serde_json::from_str::<SparkSettings>("{}").unwrap(),
            SparkSettings::default()
        );
    }

    #[test]
    fn settings_round_trip() {
        let s = SparkSettings {
            window: WindowKind::Weekly,
            series: SparkSeries::Today,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({"window": "weekly", "series": "today"}));
        assert_eq!(serde_json::from_value::<SparkSettings>(v).unwrap(), s);
    }
}
