//! Usage Sparkline series, computed from recorded readings: the usage
//! trend, per-poll increases, today's running increase, and the even-burn
//! ratio. Pure - `now` carries the time zone for "today".

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::burn::burn_window;
use crate::format::{DISABLED_COLOR, format_percent};
use crate::history::{Reading, same_reset};
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
    let added = if same_reset(prev.1, cur.1) {
        cur.0 - prev.0
    } else {
        cur.0
    };
    added.max(0.0)
}

/// Readings in the latest reading's window. Without a reset time (an idle,
/// expired session), the last window length up to `now` - never all eight
/// days of history. Once that window's reset has passed with no newer
/// reading, the old window is over: only readings from its reset on count.
fn current_window(readings: &[Reading], kind: WindowKind, now: DateTime<Utc>) -> Vec<&Reading> {
    let Some(last) = readings.last() else {
        return Vec::new();
    };
    let Some(length) = window_length(kind) else {
        return readings.iter().collect();
    };
    let start = match usage(last, kind).1 {
        Some(resets_at) if resets_at <= now => resets_at,
        Some(resets_at) => resets_at - length,
        None => now - length,
    };
    readings.iter().filter(|r| r.at >= start).collect()
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
        // Start at 0 at local midnight, so a day with a single change (or
        // none yet) still draws.
        let midnight = today
            .and_hms_opt(0, 0, 0)
            .and_then(|m| tz.from_local_datetime(&m).earliest())
            .map(|m| m.with_timezone(&Utc));
        (
            usage(&readings[first - 1], kind),
            first,
            midnight.map(|m| vec![(m, 0.0)]).unwrap_or_default(),
        )
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
    let now_utc = now.with_timezone(&Utc);
    // Readings are only recorded when something changes, so hold the
    // latest values up to now: idle time then shows as a flat end, a 0pp
    // step and a falling even-burn ratio instead of stale numbers.
    let mut extended = readings.to_vec();
    if let Some(last) = readings.last()
        && readings.len() >= 2
        && now_utc > last.at
    {
        extended.push(Reading {
            at: now_utc,
            ..last.clone()
        });
    }
    let readings = extended.as_slice();
    let window = current_window(readings, kind, now_utc);
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
                pace(&WindowUsage { percent, resets_at }, kind, now_utc).map(|p| p.projected);
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
        // Midnight (22:00Z) at 0, the two changes, then "now" (12:00Z) holding 5.
        assert_eq!(d.points.len(), 4);
        assert_eq!(d.points[0], (0.0, 0.0));
        assert_eq!(d.points[3], (1.0, 5.0));
        assert_eq!(d.headline, "5.0pp");
        assert_eq!(d.caption, "TODAY \u{b7} 7D");
    }

    #[test]
    fn today_without_a_baseline_starts_at_zero() {
        let d = today_at_plus_2(&[
            weekly("2026-09-29T23:00:00Z", 32.0, W),
            weekly("2026-09-30T05:00:00Z", 35.0, W),
        ]);
        assert_eq!(d.points.first(), Some(&(0.0, 0.0)));
        assert_eq!(d.points.last(), Some(&(1.0, 3.0)));
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
        // Colored as of now: 08:15 is 25% into the window, projecting 120%.
        let at = "2026-09-30T08:15:00Z";
        let readings = [session(at, 30.0, R)];
        let fixed = build(&readings, WindowKind::Session, SparkSeries::Trend, at);
        assert_eq!(fixed.color, DEFAULT_NORMAL);
        let pace_colors = ColorSettings {
            mode: ColorMode::Pace,
            ..ColorSettings::default()
        };
        let settings = SparkSettings::default();
        let paced = build_sparkline(&readings, &settings, &pace_colors, t(at));
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

    fn jitter(r: &str, millis: i64) -> String {
        (t(r) + chrono::Duration::milliseconds(millis)).to_rfc3339()
    }

    #[test]
    fn request_jitter_on_resets_at_is_not_a_reset() {
        let readings = [
            session("2026-09-30T08:00:00Z", 10.0, &jitter(R, 186)),
            session("2026-09-30T09:00:00Z", 12.0, &jitter(R, -447)),
            session("2026-09-30T09:30:00Z", 13.5, &jitter(R, 552)),
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::BetweenPolls,
            "2026-09-30T09:30:00Z",
        );
        assert_eq!(d.points[0].1, 2.0);
        assert_eq!(d.headline, "+1.5pp");
    }

    #[test]
    fn without_a_reset_time_the_window_is_the_last_window_length() {
        let none = |at: &str, pct: f64| Reading {
            at: t(at),
            session: pct,
            session_resets_at: None,
            weekly: 0.0,
            weekly_resets_at: None,
        };
        let readings = [
            none("2026-09-29T09:00:00Z", 90.0),
            none("2026-09-30T08:00:00Z", 10.0),
            none("2026-09-30T09:00:00Z", 20.0),
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::Trend,
            "2026-09-30T09:00:00Z",
        );
        assert_eq!(d.points, vec![(0.0, 10.0), (1.0, 20.0)]);
    }

    #[test]
    fn an_expired_window_is_not_shown_as_current() {
        // The 07:00-12:00 session ended with no reading since: at 13:00
        // there is no current window yet, so nothing to plot.
        let readings = [
            session("2026-09-30T08:00:00Z", 10.0, R),
            session("2026-09-30T09:00:00Z", 20.0, R),
        ];
        for series in [
            SparkSeries::Trend,
            SparkSeries::BetweenPolls,
            SparkSeries::EvenBurn,
        ] {
            let d = build(
                &readings,
                WindowKind::Session,
                series,
                "2026-09-30T13:00:00Z",
            );
            assert!(d.points.is_empty(), "{series:?}: {:?}", d.points);
            assert_eq!(d.headline, "\u{2014}");
        }
    }

    #[test]
    fn idle_time_is_reflected_at_now() {
        // 50% by 08:00, then nothing changes: at 11:00 (80% elapsed) even
        // burn is 50/80 = 0.625x, not the stale ratio from 08:00.
        let readings = [
            session("2026-09-30T07:40:00Z", 20.0, R),
            session("2026-09-30T08:00:00Z", 50.0, R),
        ];
        let d = build(
            &readings,
            WindowKind::Session,
            SparkSeries::EvenBurn,
            "2026-09-30T11:00:00Z",
        );
        assert_eq!(d.headline, "0.6x");
        let per_poll = build(
            &readings,
            WindowKind::Session,
            SparkSeries::BetweenPolls,
            "2026-09-30T11:00:00Z",
        );
        assert_eq!(per_poll.headline, "+0.0pp");
    }

    #[test]
    fn an_idle_day_with_a_baseline_reads_zero() {
        // Only readings from yesterday; nothing changed yet today.
        let d = today_at_plus_2(&[
            weekly("2026-09-29T18:00:00Z", 28.0, W),
            weekly("2026-09-29T21:00:00Z", 30.0, W),
        ]);
        assert_eq!(d.headline, "0.0pp");
    }
}
