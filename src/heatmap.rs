//! Usage Heatmap: per-local-day Tokens or Cost from Claude Code's logs,
//! shaded relative to the busiest day in view. Pure - the time zone comes
//! in with `now`, so tests pin it with a `FixedOffset`.

use chrono::{DateTime, Datelike, Days, TimeZone, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::level::{DEFAULT_NORMAL, is_hex_color};
use crate::metric::{MetricKind, format_cost, format_tokens};
use crate::pricing::{CostTotal, cost_for_entry};
use crate::settings::lenient;
use crate::source::logs::LogEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeatmapView {
    #[default]
    SevenDays,
    FourWeeks,
}

/// Each field falls back alone (see `settings::lenient`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeatmapSettings {
    #[serde(default, deserialize_with = "lenient")]
    pub metric: MetricKind,
    /// Changed only by a short press on the key.
    #[serde(default, deserialize_with = "lenient")]
    pub view: HeatmapView,
    #[serde(default = "default_color", deserialize_with = "hex_color")]
    pub color: String,
}

fn default_color() -> String {
    DEFAULT_NORMAL.to_string()
}

/// A `#rrggbb` color, lowercased; anything else is the default.
fn hex_color<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .filter(|s| is_hex_color(s))
        .map_or_else(default_color, str::to_ascii_lowercase))
}

impl Default for HeatmapSettings {
    fn default() -> Self {
        Self {
            metric: MetricKind::default(),
            view: HeatmapView::default(),
            color: DEFAULT_NORMAL.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeatmapDisplay {
    /// "7 DAYS · 1.2M", "4 WEEKS · $38.20", or "no data".
    pub caption: String,
    /// Oldest first. `Some(ratio)` in 0..=1 of the busiest day; `None` for
    /// a day with no usage.
    pub cells: Vec<Option<f64>>,
    /// Initials of the last seven days, oldest first.
    pub weekday_letters: Vec<char>,
    pub color: String,
}

impl HeatmapView {
    pub fn flipped(self) -> Self {
        match self {
            HeatmapView::SevenDays => HeatmapView::FourWeeks,
            HeatmapView::FourWeeks => HeatmapView::SevenDays,
        }
    }

    pub fn days(self) -> usize {
        match self {
            HeatmapView::SevenDays => 7,
            HeatmapView::FourWeeks => 28,
        }
    }

    pub fn caption(self) -> &'static str {
        match self {
            HeatmapView::SevenDays => "7 DAYS",
            HeatmapView::FourWeeks => "4 WEEKS",
        }
    }
}

fn entry_value(entry: &LogEntry, metric: MetricKind) -> f64 {
    match metric {
        MetricKind::Tokens => entry.total_tokens() as f64,
        // Unknown models contribute nothing, as on Metric Tile.
        MetricKind::Cost => cost_for_entry(entry).unwrap_or(0.0),
    }
}

/// Each entry from the `days` local calendar days ending today (in `now`'s
/// time zone), with its day's index, oldest first.
fn entries_by_day<'a, Tz: TimeZone>(
    entries: &'a [LogEntry],
    now: &DateTime<Tz>,
    days: usize,
) -> impl Iterator<Item = (usize, &'a LogEntry)> {
    let tz = now.timezone();
    let today = now.date_naive();
    let first = today - Days::new(days.saturating_sub(1) as u64);
    entries.iter().filter_map(move |entry| {
        let day = entry.timestamp.with_timezone(&tz).date_naive();
        (first..=today)
            .contains(&day)
            .then(|| ((day - first).num_days() as usize, entry))
    })
}

/// Totals for the `days` local calendar days ending today (in `now`'s time
/// zone), oldest first.
pub fn daily_totals<Tz: TimeZone>(
    entries: &[LogEntry],
    metric: MetricKind,
    now: DateTime<Tz>,
    days: usize,
) -> Vec<f64> {
    let mut totals = vec![0.0; days];
    for (day, entry) in entries_by_day(entries, &now, days) {
        totals[day] += entry_value(entry, metric);
    }
    totals
}

fn weekday_letter(day: Weekday) -> char {
    match day {
        Weekday::Mon => 'M',
        Weekday::Tue | Weekday::Thu => 'T',
        Weekday::Wed => 'W',
        Weekday::Fri => 'F',
        Weekday::Sat | Weekday::Sun => 'S',
    }
}

pub fn build_heatmap<Tz: TimeZone>(
    entries: &[LogEntry],
    settings: &HeatmapSettings,
    now: DateTime<Tz>,
) -> HeatmapDisplay {
    let days = settings.view.days();
    let today = now.date_naive();
    let weekday_letters = (0..7u64)
        .rev()
        .map(|back| weekday_letter((today - Days::new(back)).weekday()))
        .collect();
    if entries.is_empty() {
        return HeatmapDisplay {
            caption: "no data".to_string(),
            cells: vec![None; days],
            weekday_letters,
            color: settings.color.clone(),
        };
    }
    let totals = daily_totals(entries, settings.metric, now.clone(), days);
    let max = totals.iter().copied().fold(0.0, f64::max);
    let cells = totals
        .iter()
        .map(|&v| if v > 0.0 { Some(v / max) } else { None })
        .collect();
    let sum: f64 = totals.iter().sum();
    let total_text = match settings.metric {
        MetricKind::Tokens => format_tokens(sum.round() as u64),
        MetricKind::Cost => {
            // A trailing "+" when some usage in view couldn't be priced.
            let cost = CostTotal::of(entries_by_day(entries, &now, days).map(|(_, e)| e));
            let marker = if cost.partial { "+" } else { "" };
            format!("{}{marker}", caption_cost(sum))
        }
    };
    HeatmapDisplay {
        caption: format!("{} \u{b7} {total_text}", settings.view.caption()),
        cells,
        weekday_letters,
        color: settings.color.clone(),
    }
}

/// Cost for the caption: cents only below $1000, so a big month still
/// fits on a key without being squeezed.
fn caption_cost(total: f64) -> String {
    if total >= 1000.0 {
        format!("${total:.0}")
    } else {
        format_cost(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};
    use serde_json::json;

    fn entry(ts: &str, tokens: u64) -> LogEntry {
        LogEntry {
            timestamp: ts.parse::<DateTime<Utc>>().unwrap(),
            model: "claude-sonnet-5".into(),
            input_tokens: tokens,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    /// Wednesday 2026-09-30, noon, UTC+2.
    fn now() -> DateTime<FixedOffset> {
        FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 30, 12, 0, 0)
            .unwrap()
    }

    #[test]
    fn view_basics() {
        assert_eq!(HeatmapView::SevenDays.days(), 7);
        assert_eq!(HeatmapView::FourWeeks.days(), 28);
        assert_eq!(HeatmapView::SevenDays.flipped(), HeatmapView::FourWeeks);
        assert_eq!(HeatmapView::FourWeeks.flipped(), HeatmapView::SevenDays);
        assert_eq!(HeatmapView::FourWeeks.caption(), "4 WEEKS");
    }

    #[test]
    fn buckets_by_local_day() {
        // 23:30 UTC on the 29th is 01:30 on the 30th at UTC+2 -> today.
        // 21:30 UTC on the 29th is 23:30 on the 29th -> yesterday.
        let entries = [
            entry("2026-09-29T23:30:00Z", 100),
            entry("2026-09-29T21:30:00Z", 40),
        ];
        let t = daily_totals(&entries, MetricKind::Tokens, now(), 7);
        assert_eq!(t.len(), 7);
        assert_eq!(t[6], 100.0);
        assert_eq!(t[5], 40.0);
    }

    /// KI-24: local days follow the zone's own offset on each date, not
    /// today's. UK clocks go back at 02:00 BST on Sunday 2026-10-25, so a
    /// single fixed offset (+1 or +0) would misfile one of these.
    #[test]
    fn buckets_by_local_day_across_a_dst_change() {
        use chrono_tz::Europe::London;
        let now = London.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap();
        let entries = [
            entry("2026-10-24T22:30:00Z", 1),    // 23:30 BST Sat 24
            entry("2026-10-24T23:30:00Z", 10),   // 00:30 BST Sun 25
            entry("2026-10-25T23:30:00Z", 100),  // 23:30 GMT Sun 25
            entry("2026-10-26T00:30:00Z", 1000), // 00:30 GMT Mon 26
        ];
        let t = daily_totals(&entries, MetricKind::Tokens, now, 7);
        assert_eq!(&t[4..], &[1.0, 110.0, 1000.0]);
    }

    /// The spring change: the 23-hour Sunday is still one cell.
    #[test]
    fn a_short_dst_day_is_still_one_cell() {
        use chrono_tz::Europe::London;
        let now = London.with_ymd_and_hms(2026, 3, 30, 12, 0, 0).unwrap();
        let entries = [
            entry("2026-03-29T00:30:00Z", 1),   // 00:30 GMT Sun 29
            entry("2026-03-29T22:30:00Z", 10),  // 23:30 BST Sun 29
            entry("2026-03-29T23:30:00Z", 100), // 00:30 BST Mon 30
        ];
        let t = daily_totals(&entries, MetricKind::Tokens, now, 7);
        assert_eq!(&t[5..], &[11.0, 100.0]);
    }

    #[test]
    fn entries_outside_the_window_are_dropped() {
        let entries = [
            entry("2026-09-20T12:00:00Z", 999), // 10 days ago
            entry("2026-10-01T12:00:00Z", 999), // tomorrow
            entry("2026-09-24T12:00:00Z", 5),   // first day of a 7-day window
        ];
        let t = daily_totals(&entries, MetricKind::Tokens, now(), 7);
        assert_eq!(t.iter().sum::<f64>(), 5.0);
        assert_eq!(t[0], 5.0);
    }

    /// KI-02: "4 WEEKS · $5177.06" gets squeezed on a key; cents don't
    /// matter at that size.
    #[test]
    fn caption_cost_drops_cents_from_1000() {
        assert_eq!(caption_cost(38.2), "$38.20");
        assert_eq!(caption_cost(999.99), "$999.99");
        assert_eq!(caption_cost(1000.0), "$1000");
        assert_eq!(caption_cost(5177.06), "$5177");
    }

    #[test]
    fn cost_uses_the_price_table() {
        let e = entry("2026-09-30T08:00:00Z", 1_000_000);
        let t = daily_totals(std::slice::from_ref(&e), MetricKind::Cost, now(), 7);
        assert_eq!(t[6], cost_for_entry(&e).unwrap());
    }

    #[test]
    fn busiest_day_is_full_shade_and_zero_days_are_none() {
        let entries = [
            entry("2026-09-30T08:00:00Z", 200),
            entry("2026-09-28T08:00:00Z", 50),
        ];
        let d = build_heatmap(&entries, &HeatmapSettings::default(), now());
        assert_eq!(d.cells.len(), 7);
        assert_eq!(d.cells[6], Some(1.0));
        assert_eq!(d.cells[4], Some(0.25));
        assert_eq!(d.cells[0], None);
        assert_eq!(d.caption, "7 DAYS \u{b7} 250");
    }

    #[test]
    fn four_weeks_has_28_cells_and_formats_cost() {
        let settings = HeatmapSettings {
            metric: MetricKind::Cost,
            view: HeatmapView::FourWeeks,
            ..HeatmapSettings::default()
        };
        let d = build_heatmap(
            &[entry("2026-09-30T08:00:00Z", 1_000_000)],
            &settings,
            now(),
        );
        assert_eq!(d.cells.len(), 28);
        assert!(
            d.caption.starts_with("4 WEEKS \u{b7} $"),
            "got {}",
            d.caption
        );
    }

    /// Usage from a model the price table doesn't know makes the Cost
    /// caption a lower bound, and says so.
    #[test]
    fn a_cost_caption_with_unpriced_usage_is_marked() {
        let settings = HeatmapSettings {
            metric: MetricKind::Cost,
            ..HeatmapSettings::default()
        };
        let mut unpriced = entry("2026-09-30T09:00:00Z", 1_000_000);
        unpriced.model = "some-future-model".into();
        let priced = entry("2026-09-30T08:00:00Z", 1_000_000);
        let d = build_heatmap(&[priced.clone(), unpriced], &settings, now());
        assert_eq!(d.caption, "7 DAYS \u{b7} $2.00+");
        let d = build_heatmap(&[priced], &settings, now());
        assert_eq!(d.caption, "7 DAYS \u{b7} $2.00");
    }

    #[test]
    fn large_totals_use_token_suffixes() {
        let d = build_heatmap(
            &[entry("2026-09-30T08:00:00Z", 1_234_567)],
            &HeatmapSettings::default(),
            now(),
        );
        assert_eq!(d.caption, "7 DAYS \u{b7} 1.2M");
    }

    #[test]
    fn empty_entries_is_no_data() {
        let d = build_heatmap(&[], &HeatmapSettings::default(), now());
        assert_eq!(d.caption, "no data");
        assert!(d.cells.iter().all(Option::is_none));
        assert_eq!(d.cells.len(), 7);
    }

    /// KI-25: entries exist, just none in view - a zero total, not
    /// "no data" (that means no logs at all).
    #[test]
    fn entries_all_outside_the_window_caption_zero() {
        let d = build_heatmap(
            &[entry("2026-09-20T12:00:00Z", 999)],
            &HeatmapSettings::default(),
            now(),
        );
        assert_eq!(d.caption, "7 DAYS \u{b7} 0");
        assert_eq!(d.cells, vec![None; 7]);
    }

    #[test]
    fn weekday_letters_end_today() {
        // Thu 24 .. Wed 30.
        let d = build_heatmap(&[], &HeatmapSettings::default(), now());
        assert_eq!(d.weekday_letters, vec!['T', 'F', 'S', 'S', 'M', 'T', 'W']);
    }

    #[test]
    fn empty_json_is_the_default() {
        let s: HeatmapSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, HeatmapSettings::default());
    }

    #[test]
    fn garbage_fields_fall_back_alone() {
        let s: HeatmapSettings =
            serde_json::from_str(r#"{"view":3,"color":"red","metric":"cost"}"#).unwrap();
        assert_eq!(s.view, HeatmapView::SevenDays);
        assert_eq!(s.color, DEFAULT_NORMAL);
        assert_eq!(s.metric, MetricKind::Cost);
        let s: HeatmapSettings =
            serde_json::from_str(r#"{"metric":"x","view":"fourWeeks"}"#).unwrap();
        assert_eq!(s.metric, MetricKind::Tokens);
        assert_eq!(s.view, HeatmapView::FourWeeks);
    }

    #[test]
    fn wire_round_trips() {
        let s = HeatmapSettings {
            metric: MetricKind::Cost,
            view: HeatmapView::FourWeeks,
            color: "#123abc".to_string(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            json!({"metric": "cost", "view": "fourWeeks", "color": "#123abc"})
        );
        assert_eq!(serde_json::from_value::<HeatmapSettings>(v).unwrap(), s);
    }
}
