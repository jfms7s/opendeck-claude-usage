# Usage Sparkline + Recorded History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record each successful poll's session/weekly % to a small JSONL file (8-day retention) and add a **Usage Sparkline** action (keypad + dial) that draws one window's trend in four cycled series: Usage trend, Between polls, Today's increment, Vs even burn.

**Architecture:** `history.rs` owns a `HistoryStore` (in-memory list + append-only file, pruned and rewritten on load). `UsageHub::read_and_cache` records into it; `output_for` gains a `history: &[Reading]` parameter and a `View::Sparkline` arm. `sparkline.rs` is the pure series math; `styles/sparkline.rs` draws a 100×100 key and a 200×100 strip (shared `layouts/chart.json` pixmap). `SparklineAction` mirrors `ComboAction`, with the press gesture on keys and dials.

**Tech Stack:** Rust 2024, chrono, serde/serde_json, dashmap, openaction 2.7; `tempfile` (already a dev-dependency) for store tests.

**Spec:** `docs/superpowers/specs/2026-09-27-sparkline-design.md`

## Global Constraints

- No new crates.
- History file: `$XDG_STATE_HOME/opendeck-claude-usage/history.jsonl` (absolute `XDG_STATE_HOME` only), else `$HOME/.local/state/opendeck-claude-usage/history.jsonl`. JSONL of `Reading { at, session, session_resets_at, weekly, weekly_resets_at }` only. Retention **8 days**.
- Unchanged readings (both % and both reset times equal to the last) are not recorded.
- Any history I/O failure logs **one** `warn!` and the store keeps working in memory; it never breaks rendering.
- UUID `com.jfms7s.claudeusage.sparkline`, name **Usage Sparkline**, controllers `Encoder` + `Keypad`, layout `layouts/chart.json`, PI `propertyInspector/sparkline.html`, manifest **`Actions[6]`**.
- Settings keys `window` (`session`|`weekly`; `monthly`/garbage → session), `series` (`trend`|`betweenPolls`|`today`|`evenBurn`; garbage → trend) plus the flattened color keys; each falls back alone.
- Series labels `TREND`, `PER POLL`, `TODAY`, `VS EVEN`; caption `"{label} · 5H"` / `"{label} · 7D"`. Headlines `42%`, `+2.1pp`, `8.4pp`, `1.2x`; `—` with fewer than two points.
- Short press (key **or dial**) → next series, persisted; long press → `refresh_one`.
- Dial feedback key `chart`.

## Review Focus

1. **First run / no file / one reading** → "collecting…", headline `—`, no panic. Tests: Task 1 `missing_file_starts_empty`, Task 2 `fewer_than_two_points_is_collecting`, Task 3 `collecting_has_no_line`.
2. **Unwritable state dir** → one warning, readings still recorded in memory. Test: Task 1 `unwritable_path_keeps_readings_in_memory`.
3. **Window reset between readings** → counted as new usage, never negative. Tests: Task 2 `step_across_reset_is_the_new_percent`, `today_counts_resets_as_new_usage`.
4. **A corrupt or truncated line** (e.g. a crash mid-write) → skipped, other readings kept. Test: Task 1 `load_skips_garbage_and_prunes_old`.
5. **Flat history** (all readings 0%) → a flat line, no divide-by-zero. Test: Task 3 `flat_line_does_not_divide_by_zero`.

---

### Task 1: History store (`src/history.rs`)

**Files:** Create `src/history.rs`; Modify `src/main.rs` (`mod history;`)

**Interfaces — Produces:**
```rust
pub struct Reading { pub at: DateTime<Utc>, pub session: f64, pub session_resets_at: Option<DateTime<Utc>>, pub weekly: f64, pub weekly_resets_at: Option<DateTime<Utc>> }
pub fn retention() -> chrono::Duration; // 8 days
pub struct HistoryStore;
impl HistoryStore {
    pub fn default_path() -> PathBuf;
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self>;
    pub fn in_memory() -> Arc<Self>;
    pub fn record(&self, snapshot: &UsageSnapshot, now: DateTime<Utc>);
    pub fn readings(&self) -> Vec<Reading>;
}
```

- [ ] **Step 1: Skeleton + failing tests**

Create `src/history.rs`:

```rust
//! Recorded %-of-limit history. The usage API only ever returns the
//! current session/weekly percentages, so trends need readings kept over
//! time: an in-memory list mirrored to a small append-only JSONL file.
//! It holds percentages and reset times only - no tokens, credentials or
//! account data - and anything older than `retention()` is dropped.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::source::UsageSnapshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: DateTime<Utc>,
    pub session: f64,
    pub session_resets_at: Option<DateTime<Utc>>,
    pub weekly: f64,
    pub weekly_resets_at: Option<DateTime<Utc>>,
}

/// A weekly window is 7 days; one extra day lets "today" and the whole
/// current week always be covered.
pub fn retention() -> Duration {
    Duration::days(8)
}

pub struct HistoryStore {
    readings: Mutex<Vec<Reading>>,
    /// `None` for `in_memory()` - never touches disk.
    path: Option<PathBuf>,
    /// Set after the first I/O failure, so a read-only disk warns once
    /// instead of on every poll.
    warned: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use chrono::TimeZone;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, hour, 0, 0).unwrap()
    }

    fn snapshot(session: f64, weekly: f64) -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage { percent: session, resets_at: Some(at(12)) },
            weekly: WindowUsage { percent: weekly, resets_at: Some(at(23)) },
            monthly: MonthlyUsage { enabled: false, percent: None, used_dollars: None, limit_dollars: None },
        }
    }

    fn line(r: &Reading) -> String {
        serde_json::to_string(r).unwrap()
    }

    fn reading(at: DateTime<Utc>, session: f64) -> Reading {
        Reading { at, session, session_resets_at: None, weekly: 1.0, weekly_resets_at: None }
    }

    #[test]
    fn missing_file_starts_empty_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        let store = HistoryStore::load(path.clone(), at(10));
        assert!(store.readings().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn load_skips_garbage_and_prunes_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let old = reading(at(10) - Duration::days(9), 5.0);
        let recent = reading(at(9), 20.0);
        std::fs::write(&path, format!("{}\n{{not json\n{}\n{{\"at\":", line(&old), line(&recent))).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        assert_eq!(store.readings(), vec![recent.clone()]);
        // Rewritten pruned.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{}\n", line(&recent)));
    }

    #[test]
    fn load_sorts_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let (a, b) = (reading(at(8), 1.0), reading(at(9), 2.0));
        std::fs::write(&path, format!("{}\n{}\n", line(&b), line(&a))).unwrap();
        assert_eq!(HistoryStore::load(path, at(10)).readings(), vec![a, b]);
    }

    #[test]
    fn record_appends_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        let store = HistoryStore::load(path.clone(), at(10));
        store.record(&snapshot(40.0, 20.0), at(10));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let back: Reading = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(back.session, 40.0);
        assert_eq!(back.weekly_resets_at, Some(at(23)));
    }

    #[test]
    fn unchanged_snapshot_is_not_recorded() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(40.0, 20.0), at(9));
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(store.readings().len(), 1);
        store.record(&snapshot(41.0, 20.0), at(11));
        assert_eq!(store.readings().len(), 2);
    }

    #[test]
    fn record_prunes_old_readings_in_memory() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(1.0, 1.0), at(10) - Duration::days(9));
        store.record(&snapshot(2.0, 1.0), at(10));
        assert_eq!(store.readings().len(), 1);
    }

    #[test]
    fn unwritable_path_keeps_readings_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the directory should be: create_dir_all fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let store = HistoryStore::load(blocker.join("history.jsonl"), at(10));
        store.record(&snapshot(40.0, 20.0), at(10));
        store.record(&snapshot(41.0, 20.0), at(11));
        assert_eq!(store.readings().len(), 2);
    }

    #[test]
    fn in_memory_never_writes() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(store.readings().len(), 1);
        assert!(store.path.is_none());
    }

    #[test]
    fn default_path_ends_in_the_plugin_state_dir() {
        let p = HistoryStore::default_path();
        assert!(p.ends_with("opendeck-claude-usage/history.jsonl"), "{p:?}");
    }
}
```

Add `mod history;` to `src/main.rs` (alphabetically after `mod heatmap_action;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test history::`
Expected: compile errors — `load`, `in_memory`, `record`, `readings`, `default_path` missing.

- [ ] **Step 3: Implement**

Above `#[cfg(test)]`:

```rust
impl Reading {
    fn from_snapshot(snapshot: &UsageSnapshot, at: DateTime<Utc>) -> Self {
        Self {
            at,
            session: snapshot.session.percent,
            session_resets_at: snapshot.session.resets_at,
            weekly: snapshot.weekly.percent,
            weekly_resets_at: snapshot.weekly.resets_at,
        }
    }

    /// Same values, ignoring when it was taken - an unchanged poll adds
    /// nothing to a trend.
    fn same_values(&self, other: &Reading) -> bool {
        self.session == other.session
            && self.session_resets_at == other.session_resets_at
            && self.weekly == other.weekly
            && self.weekly_resets_at == other.weekly_resets_at
    }
}

impl HistoryStore {
    /// `$XDG_STATE_HOME` (when absolute, per the XDG spec) or
    /// `~/.local/state`, plus the plugin's own directory.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
                PathBuf::from(home).join(".local/state")
            });
        base.join("opendeck-claude-usage").join("history.jsonl")
    }

    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            readings: Mutex::new(Vec::new()),
            path: None,
            warned: AtomicBool::new(false),
        })
    }

    /// Loads, prunes and sorts the recorded readings, rewriting the file
    /// pruned so it never grows beyond a few days of readings across
    /// restarts. A missing file just starts empty; unreadable lines (e.g.
    /// a crash mid-append) are skipped.
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self> {
        let store = Self {
            readings: Mutex::new(Vec::new()),
            path: Some(path.clone()),
            warned: AtomicBool::new(false),
        };
        let mut readings: Vec<Reading> = match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                store.warn_once(&format!("could not read usage history {}: {e}", path.display()));
                Vec::new()
            }
        };
        let cutoff = now - retention();
        readings.retain(|r| r.at >= cutoff);
        readings.sort_by_key(|r| r.at);
        if path.exists() {
            store.rewrite(&readings);
        }
        *store.readings.lock().unwrap() = readings;
        Arc::new(store)
    }

    pub fn record(&self, snapshot: &UsageSnapshot, now: DateTime<Utc>) {
        let reading = Reading::from_snapshot(snapshot, now);
        {
            let mut readings = self.readings.lock().unwrap();
            if readings.last().is_some_and(|last| last.same_values(&reading)) {
                return;
            }
            readings.push(reading.clone());
            let cutoff = now - retention();
            readings.retain(|r| r.at >= cutoff);
        }
        self.append(&reading);
    }

    pub fn readings(&self) -> Vec<Reading> {
        self.readings.lock().unwrap().clone()
    }

    fn append(&self, reading: &Reading) {
        let Some(path) = &self.path else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let line = serde_json::to_string(reading).map_err(std::io::Error::other)?;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{line}")
        })();
        if let Err(e) = result {
            self.warn_once(&format!("could not write usage history {}: {e}", path.display()));
        }
    }

    /// Write-then-rename, so a crash mid-rewrite leaves the old file.
    fn rewrite(&self, readings: &[Reading]) {
        let Some(path) = &self.path else {
            return;
        };
        let body: String = readings
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .map(|l| l + "\n")
            .collect();
        let tmp = path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
            self.warn_once(&format!("could not rewrite usage history {}: {e}", path.display()));
        }
    }

    fn warn_once(&self, message: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            log::warn!("{message}; keeping usage history in memory only");
        }
    }
}
```

- [ ] **Step 4: Verify**

Run: `cargo fmt && cargo test history::`
Expected: 9 passed. (Clippy `-D warnings` fails on dead code until Task 4 — expected.)

- [ ] **Step 5: Commit**

```bash
git add src/history.rs src/main.rs
git commit -m "feat: add recorded usage history store"
```

---

### Task 2: Series math (`src/sparkline.rs`)

**Files:** Create `src/sparkline.rs`; Modify `src/main.rs` (`mod sparkline;`)

**Interfaces:**
- Consumes: `history::Reading`, `pace::{pace, window_length}`, `burn::burn_window`, `format::{format_percent, DISABLED_COLOR}`, `level::ColorSettings`, `source::{WindowKind, WindowUsage}`.
- Produces:
  ```rust
  pub enum SparkSeries { Trend, BetweenPolls, Today, EvenBurn } // next(), label()
  pub struct SparkSettings { pub window: WindowKind, pub series: SparkSeries } // Clone, Default, lenient wire
  pub struct SparkDisplay { pub caption: String, pub headline: String, pub points: Vec<(f64, f64)>, pub color: String }
  pub fn build_sparkline<Tz: TimeZone>(readings: &[Reading], settings: &SparkSettings, colors: &ColorSettings, now: DateTime<Tz>) -> SparkDisplay;
  ```

- [ ] **Step 1: Skeleton + failing tests**

Create `src/sparkline.rs`:

```rust
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{ColorMode, DEFAULT_CRITICAL, DEFAULT_NORMAL};
    use chrono::FixedOffset;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn session(at: &str, pct: f64, resets: &str) -> Reading {
        Reading { at: t(at), session: pct, session_resets_at: Some(t(resets)), weekly: 0.0, weekly_resets_at: None }
    }

    fn weekly(at: &str, pct: f64, resets: &str) -> Reading {
        Reading { at: t(at), session: 0.0, session_resets_at: None, weekly: pct, weekly_resets_at: Some(t(resets)) }
    }

    fn build(readings: &[Reading], window: WindowKind, series: SparkSeries, now: &str) -> SparkDisplay {
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
        let d = build(&readings, WindowKind::Session, SparkSeries::Trend, "2026-09-30T09:00:00Z");
        assert_eq!(d.points, vec![(0.0, 10.0), (1.0, 20.0)]);
        assert_eq!(d.headline, "20%");
        assert_eq!(d.caption, "TREND \u{b7} 5H");
    }

    #[test]
    fn step_across_reset_is_the_new_percent() {
        assert_eq!(step((90.0, Some(t("2026-09-30T07:00:00Z"))), (10.0, Some(t(R)))), 10.0);
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
        let d = build(&readings, WindowKind::Session, SparkSeries::BetweenPolls, "2026-09-30T09:30:00Z");
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
        let d = build(&readings, WindowKind::Session, SparkSeries::BetweenPolls, "2026-09-30T09:00:00Z");
        assert_eq!(d.points.len(), MAX_STEPS);
    }

    const W: &str = "2026-10-03T00:00:00Z";

    fn today_at_plus_2(readings: &[Reading]) -> SparkDisplay {
        // 14:00 local (UTC+2) on the 30th; local midnight is 22:00Z on the 29th.
        let now = FixedOffset::east_opt(2 * 3600).unwrap().with_ymd_and_hms(2026, 9, 30, 14, 0, 0).unwrap();
        build_sparkline(
            readings,
            &SparkSettings { window: WindowKind::Weekly, series: SparkSeries::Today },
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
            session("2026-09-30T07:10:00Z", 5.0, R),  // 3% elapsed -> skipped
            session("2026-09-30T08:15:00Z", 30.0, R), // 25% -> 1.2x
            session("2026-09-30T09:30:00Z", 50.0, R), // 50% -> 1.0x
        ];
        let d = build(&readings, WindowKind::Session, SparkSeries::EvenBurn, "2026-09-30T09:30:00Z");
        assert_eq!(d.points.len(), 2);
        assert!((d.points[0].1 - 1.2).abs() < 1e-9);
        assert_eq!(d.headline, "1.0x");
    }

    #[test]
    fn fewer_than_two_points_is_collecting() {
        let d = build(&[session("2026-09-30T08:00:00Z", 10.0, R)], WindowKind::Session, SparkSeries::Trend, R);
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
        let pace_colors = ColorSettings { mode: ColorMode::Pace, ..ColorSettings::default() };
        let settings = SparkSettings::default();
        let paced = build_sparkline(&readings, &settings, &pace_colors, t(R));
        assert_eq!(paced.color, DEFAULT_CRITICAL);
    }

    #[test]
    fn settings_wire_falls_back_per_field() {
        let s: SparkSettings = serde_json::from_str(r#"{"window":"monthly","series":"evenBurn"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.series, SparkSeries::EvenBurn);
        let s: SparkSettings = serde_json::from_str(r#"{"window":"weekly","series":9}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.series, SparkSeries::Trend);
        assert_eq!(serde_json::from_str::<SparkSettings>("{}").unwrap(), SparkSettings::default());
    }

    #[test]
    fn settings_round_trip() {
        let s = SparkSettings { window: WindowKind::Weekly, series: SparkSeries::Today };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v, json!({"window": "weekly", "series": "today"}));
        assert_eq!(serde_json::from_value::<SparkSettings>(v).unwrap(), s);
    }
}
```

Add `mod sparkline;` to `src/main.rs` (alphabetically after `mod source;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test sparkline::`
Expected: compile errors — `next`, `label`, `step`, `build_sparkline`, `From` conversions missing.

- [ ] **Step 3: Implement**

Above `#[cfg(test)]`:

```rust
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
    let added = if prev.1 != cur.1 { cur.0 } else { cur.0 - prev.0 };
    added.max(0.0)
}

/// Readings in the latest reading's window (all of them if it has no
/// reset time).
fn current_window(readings: &[Reading], kind: WindowKind) -> Vec<&Reading> {
    let Some(last) = readings.last() else {
        return Vec::new();
    };
    match (usage(last, kind).1, window_length(kind)) {
        (Some(resets_at), Some(length)) => readings.iter().filter(|r| r.at >= resets_at - length).collect(),
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
    let span = if kind == WindowKind::Weekly { "7D" } else { "5H" };
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
            let projected = pace(&WindowUsage { percent, resets_at }, kind, r.at).map(|p| p.projected);
            colors.palette.color(colors.level(percent, projected)).to_string()
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
```

- [ ] **Step 4: Verify**

Run: `cargo fmt && cargo test sparkline::`
Expected: 13 passed.

- [ ] **Step 5: Commit**

```bash
git add src/sparkline.rs src/main.rs
git commit -m "feat: compute sparkline series from recorded history"
```

---

### Task 3: Renderers (`src/styles/sparkline.rs`)

**Files:** Create `src/styles/sparkline.rs`; Modify `src/styles/mod.rs` (`pub mod sparkline;`)

**Interfaces — Produces:** `pub fn render_key(d: &SparkDisplay) -> String` (100×100), `pub fn render_strip(d: &SparkDisplay) -> String` (200×100), `pub fn sparkline_feedback(d: &SparkDisplay) -> serde_json::Value` (`{"chart": data_uri(strip)}`).

- [ ] **Step 1: Failing tests**

Create `src/styles/sparkline.rs`:

```rust
//! Usage Sparkline drawings: caption, big headline, and a filled trend line
//! with an end dot - on a 100x100 key and a 200x100 dial strip.

use serde_json::{Value, json};

use crate::sparkline::SparkDisplay;
use crate::styles::svg;
use crate::tile::{self, CARD_COLOR, MUTED_TEXT_COLOR, TEXT_COLOR};

/// The rectangle the line is drawn in: x from `left` to `right`, y from
/// `top` (highest value) to `bottom` (lowest).
struct Area {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

const KEY_AREA: Area = Area { left: 8.0, right: 92.0, top: 58.0, bottom: 90.0 };
const STRIP_AREA: Area = Area { left: 10.0, right: 190.0, top: 34.0, bottom: 92.0 };

#[cfg(test)]
mod tests {
    use super::*;

    fn display(points: Vec<(f64, f64)>) -> SparkDisplay {
        SparkDisplay {
            caption: "TREND \u{b7} 5H".to_string(),
            headline: "40%".to_string(),
            points,
            color: "#d97757".to_string(),
        }
    }

    fn rising() -> Vec<(f64, f64)> {
        vec![(0.0, 10.0), (0.5, 20.0), (1.0, 40.0)]
    }

    #[test]
    fn key_draws_caption_headline_line_fill_and_dot() {
        let s = render_key(&display(rising()));
        assert!(s.contains(">TREND \u{b7} 5H</text>"), "got: {s}");
        assert!(s.contains(r##"fill="#d97757">40%</text>"##), "got: {s}");
        // y spans 0..44 (40 * 1.1) over 90..58.
        assert!(s.contains(r#"points="8.00,82.73 50.00,75.45 92.00,60.91""#), "got: {s}");
        assert!(s.contains(r##"d="M 8.00 90.00 L 8.00 82.73 L 50.00 75.45 L 92.00 60.91 L 92.00 90.00 Z" fill="#d97757" fill-opacity="0.2""##), "got: {s}");
        assert!(s.contains(r##"<circle cx="92.00" cy="60.91" r="2.5" fill="#d97757""##), "got: {s}");
    }

    #[test]
    fn flat_line_does_not_divide_by_zero() {
        let s = render_key(&display(vec![(0.0, 0.0), (1.0, 0.0)]));
        assert!(s.contains(r#"points="8.00,90.00 92.00,90.00""#), "got: {s}");
        assert!(!s.contains("NaN"));
    }

    #[test]
    fn collecting_has_no_line() {
        let mut d = display(Vec::new());
        d.headline = "\u{2014}".to_string();
        let s = render_key(&d);
        assert!(s.contains(">collecting\u{2026}</text>"), "got: {s}");
        assert!(!s.contains("<polyline"));
    }

    #[test]
    fn strip_is_200_wide_with_right_aligned_headline() {
        let s = render_strip(&display(rising()));
        assert!(s.contains(r#"viewBox="0 0 200 100""#));
        assert!(s.contains(&format!(r#"width="200" height="100" fill="{CARD_COLOR}""#)));
        assert!(s.contains(r##"x="190" y="20" text-anchor="end" font-family="sans-serif" font-size="20" font-weight="700" fill="#d97757">40%</text>"##), "got: {s}");
        assert!(s.contains(r#"points="10.00,"#) && s.contains(" 190.00,"), "got: {s}");
        assert!(!s.contains("textLength"));
    }

    #[test]
    fn strip_collecting_is_centered_text() {
        let s = render_strip(&display(Vec::new()));
        assert!(s.contains(r#"x="100" y="70" text-anchor="middle""#), "got: {s}");
        assert!(!s.contains("<polyline"));
    }

    #[test]
    fn feedback_is_a_chart_data_uri() {
        let f = sparkline_feedback(&display(rising()));
        assert!(f["chart"].as_str().unwrap().starts_with("data:image/svg+xml;base64,"));
    }
}
```

Add `pub mod sparkline;` to `src/styles/mod.rs` (alphabetically after `pub mod ring;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test styles::sparkline`
Expected: compile errors — `render_key`, `render_strip`, `sparkline_feedback` missing.

- [ ] **Step 3: Implement**

Above `#[cfg(test)]`:

```rust
/// The filled area, the line and the end dot for `points` inside `area`.
/// The y range starts at 0 (or lower) and leaves 10% headroom, and never
/// spans less than 1 so a flat line can't divide by zero.
fn chart(points: &[(f64, f64)], area: &Area, color: &str) -> String {
    let lo = points.iter().map(|p| p.1).fold(0.0, f64::min);
    let max = points.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let hi = (max * 1.1).max(lo + 1.0);
    let xy: Vec<(f64, f64)> = points
        .iter()
        .map(|(x, v)| {
            (
                area.left + x * (area.right - area.left),
                area.bottom - (v - lo) / (hi - lo) * (area.bottom - area.top),
            )
        })
        .collect();
    let line: Vec<String> = xy.iter().map(|(x, y)| format!("{x:.2},{y:.2}")).collect();
    let (first_x, _) = xy[0];
    let (last_x, last_y) = xy[xy.len() - 1];
    let bottom = area.bottom;
    let mut path = format!("M {first_x:.2} {bottom:.2}");
    for (x, y) in &xy {
        path.push_str(&format!(" L {x:.2} {y:.2}"));
    }
    path.push_str(&format!(" L {last_x:.2} {bottom:.2} Z"));
    format!(
        r#"<path d="{path}" fill="{color}" fill-opacity="0.2" /><polyline points="{}" fill="none" stroke="{color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round" /><circle cx="{last_x:.2}" cy="{last_y:.2}" r="2.5" fill="{color}" />"#,
        line.join(" ")
    )
}

pub fn render_key(display: &SparkDisplay) -> String {
    let caption = tile::text_line(14.0, 11.0, true, MUTED_TEXT_COLOR, &display.caption);
    let headline = tile::text_line(46.0, 26.0, true, &display.color, &display.headline);
    let body = if display.points.is_empty() {
        tile::text_line(78.0, 11.0, false, MUTED_TEXT_COLOR, "collecting\u{2026}")
    } else {
        chart(&display.points, &KEY_AREA, &display.color)
    };
    svg(&format!("{caption}{headline}{body}"))
}

/// Own 200-wide wrapper and unsqueezed text, like the heatmap strip -
/// `styles::svg` and `tile::text_at` assume a 100-wide key.
pub fn render_strip(display: &SparkDisplay) -> String {
    let caption = tile::escape_xml(&display.caption);
    let headline = tile::escape_xml(&display.headline);
    let color = &display.color;
    let body = if display.points.is_empty() {
        format!(
            r#"<text x="100" y="70" text-anchor="middle" font-family="sans-serif" font-size="14" font-weight="500" fill="{MUTED_TEXT_COLOR}">collecting&#8230;</text>"#
        )
    } else {
        chart(&display.points, &STRIP_AREA, color)
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100"><rect x="0" y="0" width="200" height="100" fill="{CARD_COLOR}" /><text x="10" y="18" text-anchor="start" font-family="sans-serif" font-size="14" font-weight="700" fill="{TEXT_COLOR}">{caption}</text><text x="190" y="20" text-anchor="end" font-family="sans-serif" font-size="20" font-weight="700" fill="{color}">{headline}</text>{body}</svg>"#
    )
}

/// Dial payload for the shared `layouts/chart.json`.
pub fn sparkline_feedback(display: &SparkDisplay) -> Value {
    json!({ "chart": tile::data_uri(&render_strip(display)) })
}
```

- [ ] **Step 4: Verify**

Run: `cargo fmt && cargo test styles::sparkline`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add src/styles/
git commit -m "feat: draw sparkline charts for keys and dial strips"
```

---

### Task 4: Hub records history and renders `View::Sparkline`

**Files:** Modify `src/hub.rs`, `src/main.rs`

**Interfaces:**
- Produces: `UsageHub::new(source, history: Arc<HistoryStore>) -> Arc<Self>`; `View::Sparkline { settings: SparkSettings, colors: ColorSettings }`; `output_for(view, snapshot, history: &[Reading], keypad, now)`.

- [ ] **Step 1: Update existing hub tests to the new signatures (failing)**

In `src/hub.rs` tests:
- Every `UsageHub::new(X)` → `UsageHub::new(X, HistoryStore::in_memory())`; add `use crate::history::HistoryStore;` to the tests module.
- Every test call `output_for(<view>, <snapshot>, <true|false>, now())` → insert `&[],` before the keypad argument. Do it with this one-off script (the regex relies on the view/snapshot arguments containing no commas, true for every current call):

```bash
python3 - <<'EOF'
import re
p='src/hub.rs'; s=open(p).read()
t=s.index("#[cfg(test)]")
head, tests = s[:t], s[t:]
tests = re.sub(r"output_for\((\s*)([^,]+),(\s*)([^,]+),(\s*)(true|false),", r"output_for(\1\2,\3\4,\5&[],\5\6,", tests)
open(p,'w').write(head+tests)
EOF
```

Then add:

```rust
    #[tokio::test]
    async fn read_and_cache_records_history() {
        let history = HistoryStore::in_memory();
        let hub = UsageHub::new(AlwaysOk, history.clone());
        hub.read_and_cache().await.unwrap();
        assert_eq!(history.readings().len(), 1);
        assert_eq!(history.readings()[0].session, 33.0);
    }

    fn sparkline_view() -> View {
        View::Sparkline {
            settings: crate::sparkline::SparkSettings::default(),
            colors: ColorSettings::default(),
        }
    }

    #[test]
    fn sparkline_on_a_keypad_is_an_image() {
        assert!(matches!(
            output_for(&sparkline_view(), Some(&snapshot()), &[], true, now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn sparkline_on_a_dial_is_chart_feedback() {
        let Output::Feedback(f) = output_for(&sparkline_view(), None, &[], false, now()) else {
            panic!("expected feedback");
        };
        assert!(f["chart"].as_str().unwrap().starts_with("data:image/svg+xml;base64,"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test hub::`
Expected: compile errors — `UsageHub::new` takes 1 argument, `output_for` takes 4, no `View::Sparkline`.

- [ ] **Step 3: Implement**

In `src/hub.rs`:
- Module doc: "(Usage Gauge, Burn Rate)" → "(Usage Gauge, Burn Rate, Session + Weekly, Usage Sparkline)"; add a sentence: "It also records each successful read into the `HistoryStore` the sparkline draws from."
- Imports: `use crate::history::{HistoryStore, Reading};`, `use crate::sparkline::{SparkSettings, build_sparkline};`, `use crate::styles::sparkline::{render_key as sparkline_key, sparkline_feedback};`.
- `View` gains:

```rust
    Sparkline {
        settings: SparkSettings,
        colors: ColorSettings,
    },
```

- `output_for` signature: add `history: &[Reading],` after `snapshot`, with doc line "`history` is the recorded readings; only `View::Sparkline` uses it." New arm:

```rust
        View::Sparkline { settings, colors } => {
            let display = build_sparkline(history, settings, colors, now.with_timezone(&chrono::Local));
            if keypad {
                Output::Image(tile::data_uri(&sparkline_key(&display)))
            } else {
                Output::Feedback(sparkline_feedback(&display))
            }
        }
```

- `UsageHub` gains a field `history: Arc<HistoryStore>,` (doc: "Every successful read is recorded here for the sparkline."); `new(source: impl UsageSource + 'static, history: Arc<HistoryStore>)` stores it.
- `read_and_cache`: inside `if let Ok(snapshot) = &result {` add `self.history.record(snapshot, Utc::now());`.
- The three production `output_for(` calls (`render_cached`, `refresh_one`, `refresh_all`) pass `&self.history.readings(),` after the snapshot argument.

In `src/main.rs`:
- `use history::HistoryStore;`
- Replace `let hub = UsageHub::new(usage.clone());` with:

```rust
    // Recorded %-of-limit readings for Usage Sparkline, kept in a small
    // file under ~/.local/state so trends survive restarts.
    let history = HistoryStore::load(HistoryStore::default_path(), chrono::Utc::now());
    let hub = UsageHub::new(usage.clone(), history);
```

- [ ] **Step 4: Verify**

Run: `cargo fmt && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass. Clippy may still flag `SparkSeries::next` / `label` users only in the action (Task 5) — `next` is unused until Task 5; if clippy fails only on that, it's expected and cleared by Task 5 (do not add `#[allow]`).

- [ ] **Step 5: Commit**

```bash
git add src/hub.rs src/main.rs
git commit -m "feat: record usage history in the hub and render sparkline views"
```

---

### Task 5: `SparklineAction`, manifest, wiring

**Files:** Create `src/sparkline_action.rs`; Modify `src/main.rs`, `assets/manifest.json`

**Interfaces — Produces:** `pub struct SparklineSettings { #[serde(flatten)] pub spark: SparkSettings, #[serde(flatten)] pub colors: ColorSettings }`; `pub struct SparklineAction; impl SparklineAction { pub fn new(hub: Arc<UsageHub>) -> Self }`.

- [ ] **Step 1: Failing tests**

Create `src/sparkline_action.rs`:

```rust
use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::press::{Press, PressTimer};
use crate::sparkline::SparkSettings;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SparklineSettings {
    /// `window` + `series`.
    #[serde(flatten)]
    pub spark: SparkSettings,
    /// Marks/palette/mode for the line's level color.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::WindowKind;
    use crate::sparkline::SparkSeries;
    use serde_json::Value;

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: Value = serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][6];
        assert_eq!(entry["UUID"].as_str().unwrap(), <SparklineAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/chart.json");
        assert_eq!(entry["PropertyInspectorPath"], "propertyInspector/sparkline.html");
    }

    #[test]
    fn cycled_moves_to_the_next_series_and_keeps_the_rest() {
        let s: SparklineSettings =
            serde_json::from_str(r#"{"window":"weekly","series":"today","critical":95}"#).unwrap();
        let c = s.cycled();
        assert_eq!(c.spark.series, SparkSeries::EvenBurn);
        assert_eq!(c.spark.window, WindowKind::Weekly);
        assert_eq!(c.colors, s.colors);
        assert!(matches!(c.view(), View::Sparkline { .. }));
    }

    #[test]
    fn settings_round_trip_flat() {
        let s: SparklineSettings =
            serde_json::from_str(r#"{"window":"weekly","series":"betweenPolls","watch":40}"#).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["series"], "betweenPolls");
        assert_eq!(v["watch"], 40.0);
        let back: SparklineSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back.spark, s.spark);
        assert_eq!(back.colors, s.colors);
    }

    #[test]
    fn empty_settings_are_session_trend() {
        let s: SparklineSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.spark, SparkSettings::default());
        assert_eq!(s.colors, ColorSettings::default());
    }
}
```

Add `mod sparkline_action;` to `src/main.rs` (after `mod sparkline;`).

Run: `cargo test sparkline_action::` — Expected: compile errors (`SparklineAction`, `cycled`, `view`).

- [ ] **Step 2: Implement**

Above `#[cfg(test)]`:

```rust
impl SparklineSettings {
    fn view(&self) -> View {
        View::Sparkline {
            settings: self.spark.clone(),
            colors: self.colors.clone(),
        }
    }

    /// These settings with the next series - everything else unchanged.
    fn cycled(&self) -> SparklineSettings {
        let mut updated = self.clone();
        updated.spark.series = self.spark.series.next();
        updated
    }
}

#[derive(Clone)]
pub struct SparklineAction {
    hub: Arc<UsageHub>,
    /// Tells a short press (next series) from a long one (refresh), on
    /// keys and dials alike.
    presses: Arc<PressTimer>,
}

impl SparklineAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: Arc::new(PressTimer::default()),
        }
    }

    async fn released(&self, instance: &Instance, settings: &SparklineSettings) -> OpenActionResult<()> {
        match self.presses.up(&instance.instance_id) {
            Press::Long => self.hub.refresh_one(instance, &settings.view()).await,
            Press::Short => {
                let updated = settings.cycled();
                if let Err(e) = instance.set_settings(&updated).await {
                    log::warn!("could not persist sparkline series: {e}");
                }
                let view = updated.view();
                self.hub.track(&instance.instance_id, view.clone());
                self.hub.render_cached(instance, &view).await
            }
        }
    }
}

#[async_trait]
impl Action for SparklineAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.sparkline";
    type Settings = SparklineSettings;

    async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.presses.forget(&instance.instance_id);
        self.hub.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_down(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    /// Same gesture as the key: a dial that only refreshed could never
    /// change series.
    async fn dial_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.released(instance, settings).await
    }

    async fn key_down(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.released(instance, settings).await
    }
}
```

- [ ] **Step 3: Manifest + main**

Append to `assets/manifest.json` `"Actions"` (after Usage Heatmap):

```json
		{
			"UUID": "com.jfms7s.claudeusage.sparkline",
			"Name": "Usage Sparkline",
			"Icon": "icons/icon",
			"Tooltip": "Shows a trend line of your Claude session or weekly usage: usage trend, increase per poll, today's increase, or pace vs even burn",
			"Controllers": ["Encoder", "Keypad"],
			"PropertyInspectorPath": "propertyInspector/sparkline.html",
			"States": [{ "Image": "icons/actionDefaultImage" }],
			"Encoder": {
				"layout": "layouts/chart.json"
			}
		}
```

`src/main.rs`: `use sparkline_action::SparklineAction;`; after `let combo = ComboAction::new(hub.clone());` add `let sparkline = SparklineAction::new(hub.clone());`; after `register_action(heatmap).await;` add `register_action(sparkline).await;`; hub comment "(gauge, burn rate, combo)" → "(gauge, burn rate, combo, sparkline)".

- [ ] **Step 4: Verify**

Run: `cargo fmt && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass, clean.

- [ ] **Step 5: Commit**

```bash
git add src/ assets/manifest.json
git commit -m "feat: add Usage Sparkline action for keys and dials"
```

---

### Task 6: Property Inspector and README

**Files:** Create `assets/propertyInspector/sparkline.html`; Modify `README.md`, `src/sparkline_action.rs` (PI test)

- [ ] **Step 1: Failing test**

Add to `src/sparkline_action.rs` tests:

```rust
    /// The PI must load the shared colors section, offer both windows, and
    /// pass the stored series through, or saving it would reset the series.
    #[test]
    fn property_inspector_offers_windows_and_keeps_series() {
        let html = include_str!("../assets/propertyInspector/sparkline.html");
        assert!(html.contains(r#"<script src="colors.js"></script>"#));
        assert!(html.contains(r#"<option value="weekly">"#));
        assert!(html.contains("storedSeries"));
        assert!(html.contains("key or dial"));
    }
```

Run: `cargo test sparkline_action::` — Expected: compile error (file missing).

- [ ] **Step 2: Create `assets/propertyInspector/sparkline.html`**

```html
<!doctype html>
<html lang="en">
<head>
	<meta charset="utf-8" />
	<meta name="viewport" content="width=device-width, initial-scale=1" />
	<style>
		body { font: 12px system-ui, sans-serif; margin: 0; padding: 8px 12px; }
		label { display: block; margin-top: 10px; font-size: 11px; opacity: 0.8; }
		select, input { width: 100%; box-sizing: border-box; margin-top: 2px; }
		details { margin-top: 12px; }
		summary { cursor: pointer; font-size: 11px; opacity: 0.8; }
		.hint { font-size: 10px; opacity: 0.6; }
		button { margin-top: 8px; }
	</style>
</head>
<body>
	<label for="window">Window</label>
	<select id="window">
		<option value="session">Session (5 hour)</option>
		<option value="weekly">Weekly (7 day)</option>
	</select>
	<p class="hint">Short press (key or dial) cycles Trend → Per poll → Today → Vs even. Hold to refresh. History builds up as the plugin runs and is kept for 8 days.</p>

	<div id="colors"></div>
	<script src="colors.js"></script>

	<script>
		window.connectOpenActionSocketData = new Promise((resolve) => {
			window.connectOpenActionSocket = (...args) => resolve(args);
			window.connectElgatoStreamDeckSocket = window.connectOpenActionSocket;
		});

		let websocket;
		let uuid;
		let storedSeries;

		mountColorSection(document.getElementById("colors"), { showMode: true, onChange: sendSettings });

		window.connectOpenActionSocketData.then(([inPort, inUUID, inRegisterEvent, inInfo, inActionInfo]) => {
			uuid = inUUID;
			const actionInfo = JSON.parse(inActionInfo);
			websocket = new WebSocket(`ws://127.0.0.1:${inPort}`);

			websocket.onopen = () => {
				websocket.send(JSON.stringify({ event: inRegisterEvent, uuid: inUUID }));
				applySettings(actionInfo.payload.settings || {});
			};

			websocket.onmessage = (event) => {
				const message = JSON.parse(event.data);
				if (message.event === "didReceiveSettings") {
					applySettings(message.payload.settings || {});
				}
			};
		});

		function applySettings(settings) {
			storedSeries = settings.series;
			document.getElementById("window").value = settings.window === "weekly" ? "weekly" : "session";
			applyColorSettings(settings);
		}

		function sendSettings() {
			websocket.send(JSON.stringify({
				event: "setSettings",
				context: uuid,
				payload: {
					window: document.getElementById("window").value,
					...readColorSettings(),
					// The series changes only by pressing; pass it through
					// untouched so saving here doesn't reset it.
					...(storedSeries ? { series: storedSeries } : {}),
				},
			}));
		}

		document.getElementById("window").addEventListener("change", sendSettings);
	</script>
</body>
</html>
```

- [ ] **Step 3: README**

- Intro: "six actions" → "seven actions", adding **Usage Sparkline** to the list (after **Usage Heatmap**).
- Replace in "Where the data comes from":

```
`~/.claude/.credentials.json`, and keeps the answer in memory only - nothing
is written to disk. It works the same
```

with

```
`~/.claude/.credentials.json`, and keeps the answer in memory. The only file
the plugin writes is the usage history for **Usage Sparkline** (see below).
It works the same
```

- Add after "Using Usage Heatmap":

```markdown
## Using Usage Sparkline

1. Add a **Usage Sparkline** key on a dial or a keypad tile and pick the
   window: **Session** or **Weekly**.
2. A short press (key or dial) cycles the series; hold to refresh:
   - **Trend** — % of limit over the current window.
   - **Per poll** — how much each reading added (e.g. `+2.1pp`).
   - **Today** — running increase since local midnight (e.g. `8.4pp`).
   - **Vs even** — pace vs an even burn over the window (`1.0x` = on track).
3. The line takes the key's level color (**Colors & thresholds**). A new key
   says "collecting…" until at least two readings exist.

Anthropic's usage endpoint only reports the current percentages, so the
plugin records them itself: each successful poll whose numbers changed is
appended to `~/.local/state/opendeck-claude-usage/history.jsonl` (or under
`$XDG_STATE_HOME`). It holds only session/weekly percentages and reset
times - no tokens, credentials or account data - and anything older than 8
days is dropped when OpenDeck starts. Delete the file any time to reset the
history. If the folder can't be written, the plugin logs one warning and
keeps the history in memory until OpenDeck restarts.
```

- Smoke-checklist items:

```markdown
- [ ] Usage Sparkline says "collecting…" at first, then draws a line after
      a few polls; a short press on the key or dial cycles all four series.
      *(not yet verified)*
- [ ] `~/.local/state/opendeck-claude-usage/history.jsonl` is created, only
      grows when usage changes, and survives an OpenDeck restart (the line
      is still there). *(not yet verified)*
- [ ] On a dial the sparkline image fills the touch strip, headline and
      caption visible. *(not yet verified)*
```

- [ ] **Step 4: Verify and commit**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass, clean.

```bash
git add assets/propertyInspector/sparkline.html README.md src/sparkline_action.rs
git commit -m "feat: add Usage Sparkline settings page and document the history file"
```
