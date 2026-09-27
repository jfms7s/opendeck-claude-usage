# Thresholds & Pace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fixed green/yellow/red 50/80 gauge colors with per-key Watch/Risk/Critical marks and an editable palette (neutral copper by default), add an optional pace-based color mode, add a new **Burn Rate** action (Pace / Even burn / Runway), and extract a shared `UsageHub` so every usage-driven action shares one poller.

**Architecture:** Two new pure modules carry the rules: `level.rs` (marks, palette, level, lenient settings wire format) and `pace.rs` (elapsed fraction, projection, runway). `format.rs`/`icon.rs` consume them for the existing gauge; `burn.rs`/`burn_icon.rs` build the new Burn Rate display. `hub.rs` takes over the snapshot cache, instance registry and 20s poll loop from `action.rs` and dispatches rendering on a `View` enum, so `UsageGaugeAction` and the new `BurnRateAction` become thin `Action` impls holding an `Arc<UsageHub>`. Property Inspectors share one `colors.js` snippet.

**Tech Stack:** Rust 2024, openaction 2.7, tokio, serde/serde_json, chrono, dashmap. Plain HTML/JS property inspectors.

**Spec:** `docs/superpowers/specs/2026-09-27-thresholds-pace-design.md`

## Global Constraints

- No new crates in `Cargo.toml`.
- Default marks **50 / 75 / 90** (% used); default palette normal **`#d97757`**, watch `#eab308`, risk `#f97316`, critical `#ef4444`.
- Marks are **inclusive** (`p >= mark` enters that level).
- Settings wire fields (camelCase, flattened into each action's settings): `watch`, `risk`, `critical`, `colorNormal`, `colorWatch`, `colorRisk`, `colorCritical`, `colorMode` (`"fixed"` | `"pace"`).
- A malformed color-settings field falls back to **that field's** default only — it must never make openaction reset the whole settings struct (and with it `window`).
- Pace is `None` when less than **10%** of the window has elapsed, when `resets_at` is missing, or for Monthly.
- Window lengths: Session **5h**, Weekly **7d**.
- Burn Rate UUID `com.jfms7s.claudeusage.burnrate`, controllers `Encoder` + `Keypad`, layout `layouts/usage.json`, PI `propertyInspector/burnrate.html`, manifest `Actions[3]`. Burn Rate color is **always** pace-based.
- Existing file conventions: doc comments explain *why*; tests in a `#[cfg(test)] mod tests` block at the bottom of each file; keypad text drawn inside the SVG via `tile::text_line` (see `tile.rs`).
- Exactly **one** usage poll loop runs, spawned from `main.rs`.

## Review Focus

1. **Settings saved by v0.6.0 (only `{"window":"weekly"}`)** — the key keeps its window and gets default marks/palette. Test: Task 5 `old_settings_keep_window_and_get_default_colors`.
2. **Property Inspector sends a number as a string, an empty string, or `null`** (e.g. a cleared number input) — only that field falls back; `window` survives. Tests: Task 1 `string_numbers_are_accepted` / `garbage_field_falls_back_alone`, Task 5 `bad_color_field_does_not_reset_window`.
3. **Marks entered out of order or equal (e.g. Watch 80, Risk 60)** — defaults are used instead of a broken gauge. Tests: Task 1 `non_increasing_marks_fall_back_to_defaults`, Task 3 zone tests.
4. **Session just reset with pace mode on** (3% used after 5 minutes projects to 180%) — no false Critical. Tests: Task 2 `too_early_returns_none`, Task 3 `pace_mode_ignores_too_early_projection`.
5. **Clock skew / stale snapshot with `now` past `resets_at`** — no panic, no negative runway. Test: Task 2 `now_past_reset_clamps_and_lasts_to_reset`.

---

### Task 1: Color model (`src/level.rs`)

**Files:**
- Create: `src/level.rs`
- Modify: `src/main.rs` (add `mod level;`)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  ```rust
  pub const DEFAULT_NORMAL: &str; DEFAULT_WATCH; DEFAULT_RISK; DEFAULT_CRITICAL;
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Marks { pub watch: f64, pub risk: f64, pub critical: f64 }
  impl Default for Marks; impl Marks { pub fn sanitized(self) -> Marks }
  #[derive(Debug, Clone, PartialEq)] pub struct Palette { pub normal: String, pub watch: String, pub risk: String, pub critical: String }
  impl Default for Palette; impl Palette { pub fn color(&self, level: Level) -> &str }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)] pub enum Level { Normal, Watch, Risk, Critical }
  impl Level { pub fn for_percent(percent: f64, marks: &Marks) -> Level }
  #[derive(.., Default, Serialize, Deserialize)] pub enum ColorMode { #[default] Fixed, Pace }
  #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)] pub struct ColorSettings { pub marks: Marks, pub palette: Palette, pub mode: ColorMode }
  impl ColorSettings {
      pub fn level(&self, actual: f64, projected: Option<f64>) -> Level;       // honors mode
      pub fn pace_level(&self, actual: f64, projected: Option<f64>) -> Level;  // always max
  }
  ```

- [ ] **Step 1: Write the module skeleton with failing tests**

Create `src/level.rs`:

```rust
//! Per-key color model: user-set Watch/Risk/Critical marks (in % used), an
//! editable four-color palette, and the optional pace-based mode. Pure - no
//! I/O - so every rule here is unit-testable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Calm Claude copper - a key only turns a warning color once a mark is
/// crossed ("color when it counts").
pub const DEFAULT_NORMAL: &str = "#d97757";
pub const DEFAULT_WATCH: &str = "#eab308";
pub const DEFAULT_RISK: &str = "#f97316";
pub const DEFAULT_CRITICAL: &str = "#ef4444";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marks {
    pub watch: f64,
    pub risk: f64,
    pub critical: f64,
}

impl Default for Marks {
    fn default() -> Self {
        Self {
            watch: 50.0,
            risk: 75.0,
            critical: 90.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub normal: String,
    pub watch: String,
    pub risk: String,
    pub critical: String,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            normal: DEFAULT_NORMAL.to_string(),
            watch: DEFAULT_WATCH.to_string(),
            risk: DEFAULT_RISK.to_string(),
            critical: DEFAULT_CRITICAL.to_string(),
        }
    }
}

/// Declared in increasing severity so `Ord`/`max` pick the worse level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Watch,
    Risk,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    #[default]
    Fixed,
    Pace,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "ColorSettingsWire", into = "ColorSettingsWire")]
pub struct ColorSettings {
    pub marks: Marks,
    pub palette: Palette,
    pub mode: ColorMode,
}

/// The flat JSON the Property Inspector reads and writes. Every field is a
/// raw `Value` so a wrong type (a number sent as a string, `null`, garbage)
/// can't fail deserialization: openaction replaces the *whole* settings
/// struct with `Default::default()` on any failure, which would otherwise
/// also silently reset the key's `window`. Conversion to `ColorSettings`
/// falls back per field instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ColorSettingsWire {
    watch: Value,
    risk: Value,
    critical: Value,
    color_normal: Value,
    color_watch: Value,
    color_risk: Value,
    color_critical: Value,
    color_mode: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(watch: f64, risk: f64, critical: f64) -> Marks {
        Marks { watch, risk, critical }
    }

    #[test]
    fn levels_are_inclusive_at_each_mark() {
        let m = Marks::default();
        assert_eq!(Level::for_percent(0.0, &m), Level::Normal);
        assert_eq!(Level::for_percent(49.9, &m), Level::Normal);
        assert_eq!(Level::for_percent(50.0, &m), Level::Watch);
        assert_eq!(Level::for_percent(74.9, &m), Level::Watch);
        assert_eq!(Level::for_percent(75.0, &m), Level::Risk);
        assert_eq!(Level::for_percent(89.9, &m), Level::Risk);
        assert_eq!(Level::for_percent(90.0, &m), Level::Critical);
        assert_eq!(Level::for_percent(140.0, &m), Level::Critical);
    }

    #[test]
    fn increasing_marks_survive_sanitizing() {
        assert_eq!(marks(10.0, 20.0, 30.0).sanitized(), marks(10.0, 20.0, 30.0));
    }

    #[test]
    fn out_of_range_marks_are_clamped() {
        assert_eq!(marks(-5.0, 60.0, 150.0).sanitized(), marks(0.0, 60.0, 100.0));
    }

    #[test]
    fn non_increasing_marks_fall_back_to_defaults() {
        assert_eq!(marks(80.0, 60.0, 90.0).sanitized(), Marks::default());
        assert_eq!(marks(50.0, 50.0, 90.0).sanitized(), Marks::default());
        assert_eq!(marks(f64::NAN, 60.0, 90.0).sanitized(), Marks::default());
    }

    #[test]
    fn palette_maps_each_level() {
        let p = Palette::default();
        assert_eq!(p.color(Level::Normal), DEFAULT_NORMAL);
        assert_eq!(p.color(Level::Watch), DEFAULT_WATCH);
        assert_eq!(p.color(Level::Risk), DEFAULT_RISK);
        assert_eq!(p.color(Level::Critical), DEFAULT_CRITICAL);
    }

    #[test]
    fn fixed_mode_ignores_projection() {
        let c = ColorSettings::default();
        assert_eq!(c.level(30.0, Some(140.0)), Level::Normal);
    }

    #[test]
    fn pace_mode_takes_the_worse_level() {
        let c = ColorSettings {
            mode: ColorMode::Pace,
            ..ColorSettings::default()
        };
        assert_eq!(c.level(30.0, Some(80.0)), Level::Risk);
        assert_eq!(c.level(95.0, Some(60.0)), Level::Critical);
        assert_eq!(c.level(30.0, None), Level::Normal);
    }

    #[test]
    fn pace_level_ignores_mode() {
        let c = ColorSettings::default(); // Fixed
        assert_eq!(c.pace_level(30.0, Some(140.0)), Level::Critical);
    }

    #[test]
    fn empty_json_is_the_default() {
        let c: ColorSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(c, ColorSettings::default());
    }

    #[test]
    fn full_wire_format_round_trips() {
        let json = r##"{"watch":40,"risk":60,"critical":80,
            "colorNormal":"#112233","colorWatch":"#445566",
            "colorRisk":"#778899","colorCritical":"#AABBCC","colorMode":"pace"}"##;
        let c: ColorSettings = serde_json::from_str(json).unwrap();
        assert_eq!(c.marks, marks(40.0, 60.0, 80.0));
        assert_eq!(c.palette.critical, "#aabbcc");
        assert_eq!(c.mode, ColorMode::Pace);
        let back: ColorSettings =
            serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn string_numbers_are_accepted() {
        let c: ColorSettings =
            serde_json::from_str(r#"{"watch":"40","risk":" 60 ","critical":80}"#).unwrap();
        assert_eq!(c.marks, marks(40.0, 60.0, 80.0));
    }

    #[test]
    fn garbage_field_falls_back_alone() {
        let c: ColorSettings = serde_json::from_str(
            r##"{"watch":null,"colorWatch":"yellow","colorRisk":"#123456","colorMode":7}"##,
        )
        .unwrap();
        assert_eq!(c.marks, Marks::default());
        assert_eq!(c.palette.watch, DEFAULT_WATCH);
        assert_eq!(c.palette.risk, "#123456");
        assert_eq!(c.mode, ColorMode::Fixed);
    }

    #[test]
    fn empty_number_input_falls_back() {
        let c: ColorSettings = serde_json::from_str(r#"{"watch":""}"#).unwrap();
        assert_eq!(c.marks.watch, 50.0);
    }
}
```

Add `mod level;` to `src/main.rs` (alphabetically after `mod icon;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test level::`
Expected: compile errors — `for_percent`, `sanitized`, `color`, `level`, `pace_level`, and the `From` conversions don't exist.

- [ ] **Step 3: Implement**

Insert above `#[cfg(test)]` in `src/level.rs`:

```rust
impl Marks {
    /// Clamps each mark to 0..=100; marks that aren't strictly increasing
    /// can't draw sensible zones, so they're replaced wholesale by the
    /// defaults rather than guessed at.
    pub fn sanitized(self) -> Marks {
        let clamped = Marks {
            watch: self.watch.clamp(0.0, 100.0),
            risk: self.risk.clamp(0.0, 100.0),
            critical: self.critical.clamp(0.0, 100.0),
        };
        if clamped.watch < clamped.risk && clamped.risk < clamped.critical {
            clamped
        } else {
            log::warn!("ignoring non-increasing marks {self:?}; using defaults");
            Marks::default()
        }
    }
}

impl Palette {
    pub fn color(&self, level: Level) -> &str {
        match level {
            Level::Normal => &self.normal,
            Level::Watch => &self.watch,
            Level::Risk => &self.risk,
            Level::Critical => &self.critical,
        }
    }
}

impl Level {
    /// Each mark is inclusive: exactly 50% with Watch at 50 is Watch.
    pub fn for_percent(percent: f64, marks: &Marks) -> Level {
        if percent >= marks.critical {
            Level::Critical
        } else if percent >= marks.risk {
            Level::Risk
        } else if percent >= marks.watch {
            Level::Watch
        } else {
            Level::Normal
        }
    }
}

impl ColorSettings {
    /// The level to color a gauge with: actual % only in Fixed mode, the
    /// worse of actual and projected-at-reset in Pace mode. A `None`
    /// projection (too early in the window, or no window length) counts as
    /// "no extra warning", never as calm-override.
    pub fn level(&self, actual: f64, projected: Option<f64>) -> Level {
        match self.mode {
            ColorMode::Fixed => Level::for_percent(actual, &self.marks),
            ColorMode::Pace => self.pace_level(actual, projected),
        }
    }

    /// Pace-based level regardless of `mode` - Burn Rate always colors this
    /// way, since a burn readout colored by actual % alone would contradict
    /// the number it shows.
    pub fn pace_level(&self, actual: f64, projected: Option<f64>) -> Level {
        let by_actual = Level::for_percent(actual, &self.marks);
        match projected {
            Some(p) => by_actual.max(Level::for_percent(p, &self.marks)),
            None => by_actual,
        }
    }
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn number_or(value: &Value, default: f64) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
        .unwrap_or(default)
}

fn color_or(value: &Value, default: &str) -> String {
    value
        .as_str()
        .filter(|s| is_hex_color(s))
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| default.to_string())
}

impl From<ColorSettingsWire> for ColorSettings {
    fn from(w: ColorSettingsWire) -> Self {
        let d = Marks::default();
        Self {
            marks: Marks {
                watch: number_or(&w.watch, d.watch),
                risk: number_or(&w.risk, d.risk),
                critical: number_or(&w.critical, d.critical),
            }
            .sanitized(),
            palette: Palette {
                normal: color_or(&w.color_normal, DEFAULT_NORMAL),
                watch: color_or(&w.color_watch, DEFAULT_WATCH),
                risk: color_or(&w.color_risk, DEFAULT_RISK),
                critical: color_or(&w.color_critical, DEFAULT_CRITICAL),
            },
            mode: serde_json::from_value(w.color_mode).unwrap_or_default(),
        }
    }
}

impl From<ColorSettings> for ColorSettingsWire {
    fn from(c: ColorSettings) -> Self {
        Self {
            watch: json!(c.marks.watch),
            risk: json!(c.marks.risk),
            critical: json!(c.marks.critical),
            color_normal: json!(c.palette.normal),
            color_watch: json!(c.palette.watch),
            color_risk: json!(c.palette.risk),
            color_critical: json!(c.palette.critical),
            color_mode: json!(c.mode),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test level::`
Expected: all 13 tests PASS. (`dead_code` warnings for items not yet used elsewhere are expected until Task 3.)

- [ ] **Step 5: Commit**

```bash
git add src/level.rs src/main.rs
git commit -m "feat: add per-key marks, palette and pace-aware color levels"
```

---

### Task 2: Pace math (`src/pace.rs`) + compact duration formatting

**Files:**
- Create: `src/pace.rs`
- Modify: `src/main.rs` (add `mod pace;`), `src/format.rs:36-57` (extract `format_duration_compact`)

**Interfaces:**
- Consumes: `crate::source::{WindowKind, WindowUsage}`.
- Produces:
  ```rust
  pub const MIN_ELAPSED_FRACTION: f64 = 0.10;
  #[derive(Debug, Clone, Copy, PartialEq)] pub enum Runway { Empty, LastsToReset, Until(chrono::Duration) }
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Pace { pub elapsed_fraction: f64, pub projected: f64, pub even_burn: f64, pub rate_per_hour: f64, pub runway: Runway }
  pub fn window_length(kind: WindowKind) -> Option<chrono::Duration>;
  pub fn pace(window: &WindowUsage, kind: WindowKind, now: DateTime<Utc>) -> Option<Pace>;
  // in format.rs:
  pub fn format_duration_compact(d: chrono::Duration) -> String;
  ```

- [ ] **Step 1: Write failing tests**

Create `src/pace.rs`:

```rust
//! How fast a usage window is burning: elapsed fraction, projected % at
//! reset, even-burn ratio, and runway. Pure, with `now` passed in.

use chrono::{DateTime, Duration, Utc};

use crate::source::{WindowKind, WindowUsage};

/// Below this fraction of a window elapsed, projections are noise (3% used
/// five minutes into a session "projects" to 180%) - report nothing
/// rather than a false alarm.
pub const MIN_ELAPSED_FRACTION: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Runway {
    Empty,
    LastsToReset,
    Until(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    pub elapsed_fraction: f64,
    pub projected: f64,
    pub even_burn: f64,
    pub rate_per_hour: f64,
    pub runway: Runway,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    /// Session resetting at 22:40 started at 17:40.
    fn session(percent: f64) -> WindowUsage {
        WindowUsage {
            percent,
            resets_at: Some(at(22, 40)),
        }
    }

    #[test]
    fn window_lengths() {
        assert_eq!(window_length(WindowKind::Session), Some(Duration::hours(5)));
        assert_eq!(window_length(WindowKind::Weekly), Some(Duration::days(7)));
        assert_eq!(window_length(WindowKind::Monthly), None);
    }

    #[test]
    fn projects_a_known_case() {
        // 25% elapsed (1h15m of 5h), 30% used.
        let p = pace(&session(30.0), WindowKind::Session, at(18, 55)).unwrap();
        assert!((p.elapsed_fraction - 0.25).abs() < 1e-9);
        assert!((p.projected - 120.0).abs() < 1e-9);
        assert!((p.even_burn - 1.2).abs() < 1e-9);
        assert!((p.rate_per_hour - 24.0).abs() < 1e-9);
        // 70% left at 24%/h = 2h55m, before the 3h45m until reset.
        assert_eq!(p.runway, Runway::Until(Duration::minutes(175)));
    }

    #[test]
    fn too_early_returns_none() {
        // 29m of 5h = 9.67% elapsed.
        assert_eq!(pace(&session(3.0), WindowKind::Session, at(18, 9)), None);
    }

    #[test]
    fn exactly_ten_percent_elapsed_is_enough() {
        assert!(pace(&session(3.0), WindowKind::Session, at(18, 10)).is_some());
    }

    #[test]
    fn slow_burn_lasts_to_reset() {
        // 50% elapsed, 10% used: 4%/h needs 22.5h for the remaining 90%.
        let p = pace(&session(10.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::LastsToReset);
    }

    #[test]
    fn zero_usage_lasts_to_reset() {
        let p = pace(&session(0.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::LastsToReset);
        assert_eq!(p.projected, 0.0);
    }

    #[test]
    fn full_usage_is_empty() {
        let p = pace(&session(100.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::Empty);
    }

    #[test]
    fn monthly_has_no_pace() {
        assert_eq!(pace(&session(30.0), WindowKind::Monthly, at(20, 10)), None);
    }

    #[test]
    fn missing_reset_has_no_pace() {
        let w = WindowUsage {
            percent: 30.0,
            resets_at: None,
        };
        assert_eq!(pace(&w, WindowKind::Session, at(20, 10)), None);
    }

    #[test]
    fn now_past_reset_clamps_and_lasts_to_reset() {
        let p = pace(&session(40.0), WindowKind::Session, at(23, 30)).unwrap();
        assert_eq!(p.elapsed_fraction, 1.0);
        assert!((p.projected - 40.0).abs() < 1e-9);
        assert_eq!(p.runway, Runway::LastsToReset);
    }
}
```

Add `mod pace;` to `src/main.rs` (after `mod metric_icon;`).

Add to the tests module in `src/format.rs`:

```rust
    #[test]
    fn compact_duration_formats() {
        assert_eq!(format_duration_compact(chrono::Duration::minutes(175)), "2h 55m");
        assert_eq!(format_duration_compact(chrono::Duration::hours(22)), "22h 00m");
        assert_eq!(format_duration_compact(chrono::Duration::minutes(3 * 24 * 60 + 4 * 60)), "3d 4h");
        assert_eq!(format_duration_compact(chrono::Duration::minutes(45)), "45m");
        assert_eq!(format_duration_compact(chrono::Duration::seconds(20)), "<1m");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test pace:: format::tests::compact_duration_formats`
Expected: compile errors — `window_length`, `pace`, `format_duration_compact` not found.

- [ ] **Step 3: Implement**

In `src/pace.rs`, above `#[cfg(test)]`:

```rust
/// Monthly (`extra_usage`) has no rolling window, so it has no length.
pub fn window_length(kind: WindowKind) -> Option<Duration> {
    match kind {
        WindowKind::Session => Some(Duration::hours(5)),
        WindowKind::Weekly => Some(Duration::days(7)),
        WindowKind::Monthly => None,
    }
}

pub fn pace(window: &WindowUsage, kind: WindowKind, now: DateTime<Utc>) -> Option<Pace> {
    let length_secs = window_length(kind)?.num_seconds() as f64;
    let remaining = window.resets_at? - now;
    // (length - remaining) / length rather than 1 - remaining/length: the
    // latter lands a hair under 0.10 at exactly 10% elapsed in f64.
    let elapsed_fraction =
        ((length_secs - remaining.num_seconds() as f64) / length_secs).clamp(0.0, 1.0);
    if elapsed_fraction < MIN_ELAPSED_FRACTION {
        return None;
    }
    let used = window.percent.max(0.0);
    let rate_per_hour = used / (elapsed_fraction * length_secs / 3600.0);
    let projected = used / elapsed_fraction;
    let runway = if used >= 100.0 {
        Runway::Empty
    } else if rate_per_hour <= 0.0 {
        Runway::LastsToReset
    } else {
        let until = Duration::seconds(((100.0 - used) / rate_per_hour * 3600.0).round() as i64);
        if until >= remaining {
            Runway::LastsToReset
        } else {
            Runway::Until(until)
        }
    };
    Some(Pace {
        elapsed_fraction,
        projected,
        even_burn: projected / 100.0,
        rate_per_hour,
        runway,
    })
}
```

In `src/format.rs`, replace `format_remaining` (lines 36-57) with:

```rust
/// The bare remaining time - "Xd Yh" / "Xh Ym" / "Ym" / "<1m" - or `None`
/// once `resets_at` has passed.
fn format_remaining(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> Option<String> {
    let remaining = resets_at - now;
    if remaining <= chrono::Duration::zero() {
        return None;
    }
    Some(format_duration_compact(remaining))
}

/// "Xd Yh" / "Xh Ym" / "Ym" / "<1m". Days kick in at 24h so a weekly
/// window reads "6d 10h" rather than "154h 34m". Shared by the reset
/// countdown and Burn Rate's runway.
pub fn format_duration_compact(d: chrono::Duration) -> String {
    let total_minutes = d.num_minutes();
    let days = total_minutes / (24 * 60);
    let hours = total_minutes / 60 % 24;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "<1m".to_string()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test`
Expected: all tests PASS (existing countdown tests unchanged).

- [ ] **Step 5: Commit**

```bash
git add src/pace.rs src/main.rs src/format.rs
git commit -m "feat: add window pace, projection and runway math"
```

---

### Task 3: Gauge colors and four-zone speedometer

**Files:**
- Modify: `src/format.rs` (remove `bar_color`, change `UsageDisplay` and `build_display`)
- Modify: `src/icon.rs` (zones from marks/palette)
- Modify: `src/action.rs:66,108,178` (pass `&ColorSettings::default()` for now — Task 5 wires real settings)

**Interfaces:**
- Consumes: `level::{ColorSettings, Level, Marks, Palette}`, `pace::pace`.
- Produces:
  ```rust
  pub struct UsageDisplay {
      pub percent_text: String, pub color: String, pub detail_text: String,
      pub tile_detail: String, pub bar_value: f64, pub marks: Marks, pub palette: Palette,
  }
  pub fn build_display(snapshot: &UsageSnapshot, window: WindowKind, colors: &ColorSettings, now: DateTime<Utc>) -> UsageDisplay;
  pub fn error_display() -> UsageDisplay;           // unchanged signature
  pub fn feedback_for_display(display: &UsageDisplay) -> Value; // unchanged signature
  pub const DISABLED_COLOR: &str;                  // unchanged
  pub fn build_icon(display: &UsageDisplay) -> String; // unchanged signature
  ```

- [ ] **Step 1: Update tests in `src/format.rs` to the new API (failing)**

In `src/format.rs` tests:
- Delete the `bar_color_thresholds` test.
- Add `use crate::level::{ColorMode, ColorSettings, DEFAULT_NORMAL, DEFAULT_RISK, DEFAULT_WATCH};` at the top of the tests module.
- Change the helper `build_feedback` to call `build_display(snapshot, window, &ColorSettings::default(), now)`.
- Replace every other `build_display(&snapshot(), X, dt(..))` / `build_display(&s, X, dt(..))` call with the same call plus `&ColorSettings::default()` before `dt(..)`.
- Replace every `"#22c55e"` expectation with `DEFAULT_NORMAL` (33% and 29% are below the default Watch mark of 50).

Then add:

```rust
    #[test]
    fn crossing_a_mark_changes_the_color() {
        let mut s = snapshot();
        s.session.percent = 60.0;
        let d = build_display(&s, WindowKind::Session, &ColorSettings::default(), dt(20, 30, 0));
        assert_eq!(d.color, DEFAULT_WATCH);
    }

    #[test]
    fn custom_palette_is_used() {
        let mut colors = ColorSettings::default();
        colors.palette.normal = "#123456".to_string();
        let d = build_display(&snapshot(), WindowKind::Session, &colors, dt(20, 30, 0));
        assert_eq!(d.color, "#123456");
        assert_eq!(d.palette.normal, "#123456");
    }

    #[test]
    fn pace_mode_warns_on_fast_burn() {
        // Session 17:40-22:40; at 18:55 (25% elapsed) 20% used projects to 80% -> Risk.
        let mut s = snapshot();
        s.session.percent = 20.0;
        let colors = ColorSettings { mode: ColorMode::Pace, ..ColorSettings::default() };
        let d = build_display(&s, WindowKind::Session, &colors, dt(18, 55, 0));
        assert_eq!(d.color, DEFAULT_RISK);
    }

    #[test]
    fn pace_mode_ignores_too_early_projection() {
        // 5 minutes into the session: 3% would "project" to 180%.
        let mut s = snapshot();
        s.session.percent = 3.0;
        let colors = ColorSettings { mode: ColorMode::Pace, ..ColorSettings::default() };
        let d = build_display(&s, WindowKind::Session, &colors, dt(17, 45, 0));
        assert_eq!(d.color, DEFAULT_NORMAL);
    }

    #[test]
    fn pace_mode_on_monthly_uses_actual_only() {
        let colors = ColorSettings { mode: ColorMode::Pace, ..ColorSettings::default() };
        let d = build_display(&snapshot(), WindowKind::Monthly, &colors, dt(20, 30, 0));
        assert_eq!(d.color, DEFAULT_NORMAL); // 25%
    }
```

- [ ] **Step 2: Update tests in `src/icon.rs` (failing)**

Replace the `display` helper and the two tests that reference zone constants:

```rust
    use crate::level::{DEFAULT_CRITICAL, DEFAULT_NORMAL, DEFAULT_RISK, DEFAULT_WATCH, Level, Marks, Palette};

    fn display(bar_value: f64) -> UsageDisplay {
        UsageDisplay {
            percent_text: format!("{bar_value}%"),
            color: DEFAULT_NORMAL.to_string(),
            detail_text: "resets in 1h".to_string(),
            tile_detail: "1h".to_string(),
            bar_value,
            marks: Marks::default(),
            palette: Palette::default(),
        }
    }

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = decode(&build_icon(&display(50.0)));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        for color in [DEFAULT_NORMAL, DEFAULT_WATCH, DEFAULT_RISK, DEFAULT_CRITICAL] {
            assert!(svg.contains(color), "missing {color} in {svg}");
        }
    }

    #[test]
    fn default_marks_make_four_zones() {
        assert_eq!(
            zone_segments(&Marks::default()),
            vec![
                (0.0, 50.0, Level::Normal),
                (50.0, 75.0, Level::Watch),
                (75.0, 90.0, Level::Risk),
                (90.0, 100.0, Level::Critical),
            ]
        );
    }

    #[test]
    fn zero_length_zone_is_skipped() {
        let marks = Marks { watch: 0.0, risk: 60.0, critical: 100.0 };
        let levels: Vec<Level> = zone_segments(&marks).iter().map(|z| z.2).collect();
        assert!(!levels.contains(&Level::Normal));
        assert!(!levels.contains(&Level::Critical));
    }

    #[test]
    fn zones_crossing_half_are_split_so_no_arc_exceeds_90_degrees() {
        let marks = Marks { watch: 20.0, risk: 80.0, critical: 95.0 };
        let zones = zone_segments(&marks);
        assert!(zones.contains(&(20.0, 50.0, Level::Watch)));
        assert!(zones.contains(&(50.0, 80.0, Level::Watch)));
        for (from, to, _) in zones {
            assert!(to > from && to - from <= 50.0, "zone {from}..{to}");
        }
    }

    #[test]
    fn custom_palette_colors_the_zones() {
        let mut d = display(10.0);
        d.palette.critical = "#abcdef".to_string();
        assert!(decode(&build_icon(&d)).contains("#abcdef"));
    }
```

In `disabled_color_still_renders_a_valid_icon`, change `color: "#6b7280"` to `color: "#6b7280".to_string()` and add `marks: Marks::default(), palette: Palette::default(),`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test`
Expected: compile errors — `build_display` arity, `UsageDisplay` has no `marks`/`palette`, `zone_segments` not found.

- [ ] **Step 4: Implement `format.rs`**

- Delete `bar_color` (lines 14-24).
- Add imports: `use crate::level::{ColorSettings, Marks, Palette};` and `use crate::pace::pace;`.
- Change `UsageDisplay`: `pub color: String,` and add, after `bar_value`:

```rust
    /// The key's own marks and palette, so the keypad speedometer draws
    /// its zones where this key's colors actually change.
    pub marks: Marks,
    pub palette: Palette,
```

- Replace `build_display`, `window_display`, `monthly_display`, `make_display`, `error_display` with:

```rust
pub fn build_display(
    snapshot: &UsageSnapshot,
    window: WindowKind,
    colors: &ColorSettings,
    now: DateTime<Utc>,
) -> UsageDisplay {
    match window {
        WindowKind::Session => window_display(&snapshot.session, window, colors, now),
        WindowKind::Weekly => window_display(&snapshot.weekly, window, colors, now),
        WindowKind::Monthly => monthly_display(&snapshot.monthly, colors),
    }
}

fn window_display(
    window: &WindowUsage,
    kind: WindowKind,
    colors: &ColorSettings,
    now: DateTime<Utc>,
) -> UsageDisplay {
    let (detail, tile_detail) = match window.resets_at {
        Some(resets_at) => (
            format_countdown(resets_at, now),
            format_countdown_short(resets_at, now),
        ),
        None => ("no reset info".to_string(), "\u{2014}".to_string()),
    };
    let projected = pace(window, kind, now).map(|p| p.projected);
    let level = colors.level(window.percent, projected);
    make_display(
        window.percent,
        colors.palette.color(level).to_string(),
        detail,
        tile_detail,
        colors,
    )
}

/// Monthly has no window length, so it's colored by actual % whatever the
/// key's color mode says.
fn monthly_display(monthly: &MonthlyUsage, colors: &ColorSettings) -> UsageDisplay {
    if !monthly.enabled {
        return UsageDisplay {
            percent_text: "\u{2014}".to_string(),
            color: DISABLED_COLOR.to_string(),
            detail_text: "not enabled".to_string(),
            tile_detail: "not enabled".to_string(),
            bar_value: 0.0,
            marks: colors.marks,
            palette: colors.palette.clone(),
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
    let level = colors.level(percent, None);
    make_display(
        percent,
        colors.palette.color(level).to_string(),
        detail,
        tile_detail,
        colors,
    )
}

fn make_display(
    percent: f64,
    color: String,
    detail_text: String,
    tile_detail: String,
    colors: &ColorSettings,
) -> UsageDisplay {
    UsageDisplay {
        percent_text: format_percent(percent),
        color,
        detail_text,
        tile_detail,
        bar_value: percent.clamp(0.0, 100.0),
        marks: colors.marks,
        palette: colors.palette.clone(),
    }
}
```

and in `error_display()` set `color: DISABLED_COLOR.to_string(),` and add `marks: Marks::default(), palette: Palette::default(),`.

`feedback_for_display` is unchanged (`display.color` is now a `String`; `json!` accepts it).

- [ ] **Step 5: Implement `icon.rs`**

- Delete `ZONE_GREEN`, `ZONE_YELLOW`, `ZONE_RED`.
- Change the import line to `use crate::format::UsageDisplay;` plus `use crate::level::{Level, Marks};`.
- Update `arc_path`'s doc comment: "both spans here are <= 90deg (see `zone_segments`)".
- Add:

```rust
/// 0% -> 180deg (left), 100% -> 0deg (right) - the same mapping as
/// `needle_rotation_deg`, so zones and needle can never disagree.
fn percent_to_theta(percent: f64) -> f64 {
    180.0 - percent.clamp(0.0, 100.0) * 1.8
}

/// The colored zones as `(from %, to %, level)`: normal up to Watch, then
/// Watch, Risk, Critical up to 100. Zero-length zones are dropped, and any
/// zone crossing 50% is split there so no arc spans more than 90deg
/// (`arc_path` always uses `large-arc-flag=0`).
fn zone_segments(marks: &Marks) -> Vec<(f64, f64, Level)> {
    let bounds = [0.0, marks.watch, marks.risk, marks.critical, 100.0];
    let levels = [Level::Normal, Level::Watch, Level::Risk, Level::Critical];
    let mut zones = Vec::new();
    for (i, level) in levels.into_iter().enumerate() {
        let (from, to) = (bounds[i], bounds[i + 1]);
        if to <= from {
            continue;
        }
        if from < 50.0 && to > 50.0 {
            zones.push((from, 50.0, level));
            zones.push((50.0, to, level));
        } else {
            zones.push((from, to, level));
        }
    }
    zones
}
```

- In `render_svg`, replace the `arcs` block with:

```rust
    let arcs: String = zone_segments(&display.marks)
        .into_iter()
        .map(|(from, to, level)| {
            arc_path(
                percent_to_theta(from),
                percent_to_theta(to),
                display.palette.color(level),
            )
        })
        .collect();
```

and update `render_svg`'s doc comment: "a four-zone semicircular speedometer drawn from the key's own marks and palette".

- [ ] **Step 6: Keep `action.rs` compiling**

In `src/action.rs`, add `use crate::level::ColorSettings;` and change the three `build_display(&s, window, chrono::Utc::now())` / `build_display(&snapshot, window, …)` / `build_display(snapshot, window, …)` calls to pass `&ColorSettings::default()` as the third argument. (Task 5 replaces this file's internals.)

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: all PASS, no clippy errors. (If clippy flags `dead_code` for `pace_level`/`MIN_ELAPSED_FRACTION`, leave it — they're used in Task 4; do not add `#[allow]`.) If `-D warnings` fails *only* on dead code, run `cargo clippy --all-targets` without `-D warnings` and confirm nothing else is reported.

- [ ] **Step 8: Commit**

```bash
git add src/format.rs src/icon.rs src/action.rs
git commit -m "feat: color gauges from per-key marks and palette"
```

---

### Task 4: Burn Rate display (`src/burn.rs`, `src/burn_icon.rs`)

**Files:**
- Create: `src/burn.rs`, `src/burn_icon.rs`
- Modify: `src/main.rs` (add `mod burn; mod burn_icon;`)

**Interfaces:**
- Consumes: `level::ColorSettings` (`pace_level`, `palette.color`), `pace::{pace, Pace, Runway}`, `format::{DISABLED_COLOR, format_duration_compact}`, `tile::{card, text_line, data_uri, TEXT_COLOR, MUTED_TEXT_COLOR}`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase")] pub enum BurnMetric { #[default] Pace, EvenBurn, Runway }
  pub struct BurnDisplay { pub label: &'static str, pub value_text: String, pub subtitle: String, pub detail_text: String, pub color: String, pub bar_value: f64 }
  pub fn burn_window(window: WindowKind) -> WindowKind;
  pub fn build_burn_display(snapshot: &UsageSnapshot, window: WindowKind, metric: BurnMetric, colors: &ColorSettings, now: DateTime<Utc>) -> BurnDisplay;
  pub fn burn_error_display(metric: BurnMetric) -> BurnDisplay;
  pub fn burn_feedback(display: &BurnDisplay) -> serde_json::Value;
  pub fn build_burn_icon(display: &BurnDisplay) -> String; // in burn_icon.rs
  ```

- [ ] **Step 1: Write failing tests**

Create `src/burn.rs`:

```rust
//! Burn Rate readouts - Pace, Even burn, Runway - for one usage window.
//! Pure; `burn_icon.rs` draws the keypad tile and `burn_feedback` builds
//! the dial payload.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::format::{DISABLED_COLOR, format_duration_compact};
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
        build_burn_display(&snapshot(30.0), window, metric, &ColorSettings::default(), now)
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
        assert_eq!(d.subtitle, "until empty");
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
        assert_eq!(d.value_text, "\u{2713}");
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
        let d = build_burn_display(&s, WindowKind::Session, BurnMetric::Pace, &ColorSettings::default(), at(13, 20, 0));
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
```

Create `src/burn_icon.rs`:

```rust
//! Keypad tile for Burn Rate: label on top, the value large in the level
//! color, the subtitle underneath - all drawn inside the SVG (see tile.rs).

use crate::burn::BurnDisplay;
use crate::tile::{self, MUTED_TEXT_COLOR};

const LABEL_BASELINE: f64 = 24.0;
const LABEL_SIZE: f64 = 13.0;
const VALUE_BASELINE: f64 = 62.0;
const VALUE_SIZE: f64 = 30.0;
const SUBTITLE_BASELINE: f64 = 86.0;
const SUBTITLE_SIZE: f64 = 14.0;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    fn decode(uri: &str) -> String {
        let prefix = "data:image/svg+xml;base64,";
        assert!(uri.starts_with(prefix), "got: {uri}");
        String::from_utf8(STANDARD.decode(&uri[prefix.len()..]).unwrap()).unwrap()
    }

    fn display() -> BurnDisplay {
        BurnDisplay {
            label: "EVEN BURN",
            value_text: "1.4x".to_string(),
            subtitle: "even burn".to_string(),
            detail_text: "even burn \u{b7} session".to_string(),
            color: "#ef4444".to_string(),
            bar_value: 100.0,
        }
    }

    #[test]
    fn draws_label_value_and_subtitle() {
        let svg = decode(&build_burn_icon(&display()));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(">EVEN BURN</text>"), "got: {svg}");
        assert!(svg.contains(">1.4x</text>"), "got: {svg}");
        assert!(svg.contains(">even burn</text>"), "got: {svg}");
    }

    #[test]
    fn value_is_drawn_in_the_level_color() {
        let svg = decode(&build_burn_icon(&display()));
        assert!(svg.contains(r##"fill="#ef4444">1.4x</text>"##), "got: {svg}");
    }
}
```

Add `mod burn;` and `mod burn_icon;` to `src/main.rs` (alphabetically, before `mod clock_action;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test burn`
Expected: compile errors — `build_burn_display`, `burn_window`, `burn_error_display`, `burn_feedback`, `build_burn_icon` not found.

- [ ] **Step 3: Implement `burn.rs`**

Above `#[cfg(test)]` in `src/burn.rs`:

```rust
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
    let window_name = if kind == WindowKind::Weekly { "weekly" } else { "session" };
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
            Runway::LastsToReset => ("\u{2713}".to_string(), "lasts to reset"),
            Runway::Until(d) => (format_duration_compact(d), "until empty"),
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

/// Dial payload for the shared `layouts/usage.json` - same keys as the
/// gauge's `feedback_for_display`.
pub fn burn_feedback(display: &BurnDisplay) -> Value {
    json!({
        "bar": { "value": display.bar_value, "bar_fill_c": display.color },
        "percent": display.value_text,
        "detail": display.detail_text,
    })
}
```

- [ ] **Step 4: Implement `burn_icon.rs`**

Above `#[cfg(test)]` in `src/burn_icon.rs`:

```rust
fn render_svg(display: &BurnDisplay) -> String {
    let card = tile::card();
    let label = tile::text_line(LABEL_BASELINE, LABEL_SIZE, true, MUTED_TEXT_COLOR, display.label);
    let value = tile::text_line(VALUE_BASELINE, VALUE_SIZE, true, &display.color, &display.value_text);
    let subtitle = tile::text_line(
        SUBTITLE_BASELINE,
        SUBTITLE_SIZE,
        false,
        MUTED_TEXT_COLOR,
        &display.subtitle,
    );
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{label}{value}{subtitle}</svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects.
pub fn build_burn_icon(display: &BurnDisplay) -> String {
    tile::data_uri(&render_svg(display))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test burn`
Expected: 14 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/burn.rs src/burn_icon.rs src/main.rs
git commit -m "feat: add Burn Rate pace, even-burn and runway readouts"
```

---

### Task 5: Shared `UsageHub` and Usage Gauge settings

**Files:**
- Create: `src/hub.rs`
- Modify: `src/action.rs` (becomes a thin `Action` impl), `src/main.rs` (build hub, spawn one poll loop)

**Interfaces:**
- Consumes: `format::{build_display, error_display, feedback_for_display}`, `icon::build_icon`, `burn::{BurnMetric, build_burn_display, burn_error_display, burn_feedback}`, `burn_icon::build_burn_icon`, `level::ColorSettings`.
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub enum View {
      Gauge { window: WindowKind, colors: ColorSettings },
      Burn { window: WindowKind, metric: BurnMetric, colors: ColorSettings },
  }
  #[derive(Debug, Clone, PartialEq)] pub enum Output { Image(String), Feedback(serde_json::Value) }
  pub fn output_for(view: &View, snapshot: Option<&UsageSnapshot>, keypad: bool, now: DateTime<Utc>) -> Output;
  pub struct UsageHub { .. }
  impl UsageHub {
      pub fn new(source: impl UsageSource + 'static) -> Arc<Self>;
      pub fn track(&self, instance_id: &str, view: View);
      pub fn untrack(&self, instance_id: &str);
      pub async fn render_cached(&self, instance: &Instance, view: &View) -> OpenActionResult<()>;
      pub async fn refresh_one(&self, instance: &Instance, view: &View) -> OpenActionResult<()>;
      pub async fn poll_loop(self: Arc<Self>);
  }
  // action.rs:
  pub struct UsageGaugeSettings { pub window: WindowKind, #[serde(flatten)] pub colors: ColorSettings }
  impl UsageGaugeAction { pub fn new(hub: Arc<UsageHub>) -> Self }
  ```

- [ ] **Step 1: Create `src/hub.rs` with failing tests**

```rust
//! Shared state behind every usage-driven action (Usage Gauge, Burn Rate):
//! one snapshot cache, one registry of visible instances, one 20s poll
//! loop - so adding an action never adds another poller, and a single
//! read serves every key and dial.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use openaction::{Instance, OpenActionResult};
use tokio::sync::RwLock;

use crate::burn::{BurnMetric, build_burn_display, burn_error_display, burn_feedback};
use crate::burn_icon::build_burn_icon;
use crate::format::{build_display, error_display, feedback_for_display};
use crate::icon::build_icon;
use crate::level::ColorSettings;
use crate::source::{UsageSnapshot, UsageSource, UsageSourceError, WindowKind};

/// The wire value OpenDeck sends as `Instance::controller` for a keypad
/// tile (vs. `"Encoder"` for a dial) - confirmed against openaction 2.7's
/// own `GenericInstancePayload`, which just forwards this string verbatim.
const KEYPAD_CONTROLLER: &str = "Keypad";

/// What one instance shows - built from its action's settings.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Gauge {
        window: WindowKind,
        colors: ColorSettings,
    },
    Burn {
        window: WindowKind,
        metric: BurnMetric,
        colors: ColorSettings,
    },
}

/// A rendered frame for one surface: a keypad tile's icon, or a dial's
/// touch-strip feedback.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Image(String),
    Feedback(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use async_trait::async_trait;
    use chrono::TimeZone;

    struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            unreachable!("this test never triggers a read")
        }
    }

    fn snapshot() -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: 33.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()),
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

    /// Always succeeds with `snapshot()` - lets `read_and_cache` populate
    /// the cache, unlike `NeverCalled`.
    struct AlwaysOk;

    #[async_trait]
    impl UsageSource for AlwaysOk {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            Ok(snapshot())
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 20, 30, 0).unwrap()
    }

    fn gauge() -> View {
        View::Gauge {
            window: WindowKind::Session,
            colors: ColorSettings::default(),
        }
    }

    fn burn() -> View {
        View::Burn {
            window: WindowKind::Session,
            metric: BurnMetric::EvenBurn,
            colors: ColorSettings::default(),
        }
    }

    #[tokio::test]
    async fn read_and_cache_populates_the_cached_snapshot() {
        let hub = UsageHub::new(AlwaysOk);
        hub.read_and_cache().await.unwrap();
        assert!(hub.latest.read().await.is_some());
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("ctx1", gauge());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), gauge());
        hub.untrack("ctx1");
        assert!(hub.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_view() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("ctx1", gauge());
        hub.track("ctx1", burn());
        assert_eq!(*hub.registry.get("ctx1").unwrap(), burn());
    }

    #[test]
    fn gauge_and_burn_instances_share_one_registry() {
        let hub = UsageHub::new(NeverCalled);
        hub.track("gauge", gauge());
        hub.track("burn", burn());
        assert_eq!(hub.registry.len(), 2);
    }

    #[test]
    fn gauge_on_a_keypad_is_an_image() {
        let out = output_for(&gauge(), Some(&snapshot()), true, now());
        assert!(matches!(out, Output::Image(ref s) if s.starts_with("data:image/svg+xml;base64,")));
    }

    #[test]
    fn gauge_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&gauge(), Some(&snapshot()), false, now()) else {
            panic!("expected feedback");
        };
        assert_eq!(f["percent"], "33%");
    }

    #[test]
    fn burn_on_a_dial_is_feedback() {
        let Output::Feedback(f) = output_for(&burn(), Some(&snapshot()), false, now()) else {
            panic!("expected feedback");
        };
        assert!(f["detail"].as_str().unwrap().ends_with("session"));
    }

    #[test]
    fn burn_on_a_keypad_is_an_image() {
        assert!(matches!(output_for(&burn(), Some(&snapshot()), true, now()), Output::Image(_)));
    }

    #[test]
    fn no_snapshot_renders_no_data_for_both_views() {
        for view in [gauge(), burn()] {
            let Output::Feedback(f) = output_for(&view, None, false, now()) else {
                panic!("expected feedback");
            };
            assert_eq!(f["detail"], "no data");
        }
    }
}
```

Add `mod hub;` to `src/main.rs` (after `mod format;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test hub::`
Expected: compile errors — `UsageHub`, `output_for` not found.

- [ ] **Step 3: Implement `UsageHub` and `output_for`**

In `src/hub.rs`, above `#[cfg(test)]` (the method bodies of `read_and_cache`, `log_poll_read_transition`, `poll_loop` and `refresh_all` move from `src/action.rs` with `self.shared.` → `self.`):

```rust
/// Renders one instance's frame. Pure, so every view × surface × data
/// state is unit-testable without an OpenDeck connection.
pub fn output_for(
    view: &View,
    snapshot: Option<&UsageSnapshot>,
    keypad: bool,
    now: DateTime<Utc>,
) -> Output {
    match view {
        View::Gauge { window, colors } => {
            let display = match snapshot {
                Some(s) => build_display(s, *window, colors, now),
                None => error_display(),
            };
            if keypad {
                Output::Image(build_icon(&display))
            } else {
                Output::Feedback(feedback_for_display(&display))
            }
        }
        View::Burn {
            window,
            metric,
            colors,
        } => {
            let display = match snapshot {
                Some(s) => build_burn_display(s, *window, *metric, colors, now),
                None => burn_error_display(*metric),
            };
            if keypad {
                Output::Image(build_burn_icon(&display))
            } else {
                Output::Feedback(burn_feedback(&display))
            }
        }
    }
}

pub struct UsageHub {
    source: Box<dyn UsageSource>,
    latest: RwLock<Option<UsageSnapshot>>,
    registry: DashMap<String, View>,
    /// Tracks whether the poll loop's most recent read succeeded, so it
    /// logs a `warn!` only on the transition into failing (and an `info!`
    /// only on the recovery) instead of every ~20s tick forever. Starts
    /// `true` so the very first failure is logged. Not touched by
    /// `refresh_one` - a single manual press failing isn't part of that
    /// noise pattern.
    poll_last_read_ok: AtomicBool,
}

impl UsageHub {
    pub fn new(source: impl UsageSource + 'static) -> Arc<Self> {
        Arc::new(Self {
            source: Box::new(source),
            latest: RwLock::new(None),
            registry: DashMap::new(),
            poll_last_read_ok: AtomicBool::new(true),
        })
    }

    pub fn track(&self, instance_id: &str, view: View) {
        self.registry.insert(instance_id.to_string(), view);
    }

    pub fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Pushes a frame via whichever surface the instance's controller
    /// has. Keypad text is drawn inside the icon (see tile.rs), so the
    /// native title is cleared to stop OpenDeck painting a second copy.
    async fn push(instance: &Instance, output: Output) -> OpenActionResult<()> {
        match output {
            Output::Image(image) => {
                instance.set_title(Some(String::new()), None).await?;
                instance.set_image(Some(image), None).await
            }
            Output::Feedback(feedback) => instance.set_feedback(&feedback).await,
        }
    }

    fn is_keypad(instance: &Instance) -> bool {
        instance.controller == KEYPAD_CONTROLLER
    }

    /// Renders from the last cached snapshot (no fresh read) - used when an
    /// instance appears or its settings change, so it shows *something*
    /// immediately rather than waiting for the next poll tick.
    pub async fn render_cached(&self, instance: &Instance, view: &View) -> OpenActionResult<()> {
        let snapshot = self.latest.read().await.clone();
        let output = output_for(view, snapshot.as_ref(), Self::is_keypad(instance), Utc::now());
        Self::push(instance, output).await
    }

    /// Reads the source and caches it on success - shared by `refresh_one`
    /// and `refresh_all` so "read, then cache" exists in one place.
    async fn read_and_cache(&self) -> Result<UsageSnapshot, UsageSourceError> {
        let result = self.source.read().await;
        if let Ok(snapshot) = &result {
            *self.latest.write().await = Some(snapshot.clone());
        }
        result
    }

    /// Reads directly and renders just this instance - a dial press or a
    /// keypad tap, without waiting for the next tick.
    pub async fn refresh_one(&self, instance: &Instance, view: &View) -> OpenActionResult<()> {
        let result = self.read_and_cache().await;
        if let Err(e) = &result {
            log::warn!("usage source read failed: {e}");
        }
        let output = output_for(view, result.as_ref().ok(), Self::is_keypad(instance), Utc::now());
        Self::push(instance, output).await
    }

    fn log_poll_read_transition(&self, read_ok: bool, error: Option<&UsageSourceError>) {
        let was_ok = self.poll_last_read_ok.swap(read_ok, Ordering::Relaxed);
        if was_ok && !read_ok {
            if let Some(e) = error {
                log::warn!("usage source read failed: {e}");
            }
        } else if !was_ok && read_ok {
            log::info!("usage source read recovered");
        }
    }

    /// Runs forever: every ~20s, reads once and re-renders every tracked
    /// instance of every usage-driven action. Spawned once from `main.rs`.
    pub async fn poll_loop(self: Arc<Self>) {
        loop {
            self.refresh_all().await;
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    }

    async fn refresh_all(&self) {
        let read_result = self.read_and_cache().await;
        self.log_poll_read_transition(read_result.is_ok(), read_result.as_ref().err());

        // Collect first, releasing the DashMap shard lock before awaiting
        // per instance - holding an iterator guard across an await would
        // keep that shard locked for the whole loop.
        let entries: Vec<(String, View)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();

        for (instance_id, view) in entries {
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // disappeared between the snapshot and now
            };
            let output = output_for(
                &view,
                read_result.as_ref().ok(),
                Self::is_keypad(&instance),
                Utc::now(),
            );
            if let Err(e) = Self::push(&instance, output).await {
                log::warn!("render failed: {e}");
            }
        }
    }
}
```

- [ ] **Step 4: Rewrite `src/action.rs` as a thin action with failing tests**

Replace the whole file with:

```rust
use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::source::WindowKind;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
    /// Flattened so the Property Inspector writes plain top-level fields
    /// (`watch`, `colorNormal`, ...). Its lenient wire format means a bad
    /// color can't make openaction reset `window` too.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

impl UsageGaugeSettings {
    fn view(&self) -> View {
        View::Gauge {
            window: self.window,
            colors: self.colors.clone(),
        }
    }
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    hub: Arc<UsageHub>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl Action for UsageGaugeAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.usagegauge";
    type Settings = UsageGaugeSettings;

    async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }

    /// Keypad's equivalent of `dial_up` - a tap forces an immediate refresh
    /// of just that tile.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{error_display, feedback_for_display};
    use crate::level::{DEFAULT_WATCH, Marks};

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let layout: serde_json::Value =
            serde_json::from_str(include_str!("../assets/layouts/usage.json")).unwrap();
        let keys: Vec<&str> = layout["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        let feedback = feedback_for_display(&error_display());
        for k in feedback.as_object().unwrap().keys() {
            assert!(keys.contains(&k.as_str()), "layout has no item keyed {k}");
        }
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let manifest_uuid = manifest["Actions"][0]["UUID"].as_str().unwrap();
        assert_eq!(manifest_uuid, <UsageGaugeAction as Action>::UUID);
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // openaction falls back to Default::default() when settings JSON
        // fails to deserialize at all - both paths must agree.
        let from_missing_keys: UsageGaugeSettings = serde_json::from_str("{}").unwrap();
        let from_default = UsageGaugeSettings::default();
        assert_eq!(from_missing_keys.window, from_default.window);
        assert_eq!(from_missing_keys.colors, from_default.colors);
        assert_eq!(from_default.window, WindowKind::Session);
    }

    #[test]
    fn old_settings_keep_window_and_get_default_colors() {
        // What a v0.6.0 key has stored.
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"window":"weekly"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn bad_color_field_does_not_reset_window() {
        let s: UsageGaugeSettings =
            serde_json::from_str(r#"{"window":"monthly","colorWatch":42,"watch":"abc"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Monthly);
        assert_eq!(s.colors.palette.watch, DEFAULT_WATCH);
        assert_eq!(s.colors.marks, Marks::default());
    }

    #[test]
    fn settings_round_trip_through_json() {
        let s: UsageGaugeSettings =
            serde_json::from_str(r#"{"window":"weekly","watch":40,"risk":60,"critical":80,"colorMode":"pace"}"#)
                .unwrap();
        let back: UsageGaugeSettings =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.window, WindowKind::Weekly);
        assert_eq!(back.colors, s.colors);
    }
}
```

- [ ] **Step 5: Wire `main.rs`**

In `src/main.rs`, add `use hub::UsageHub;` and replace:

```rust
    let action = UsageGaugeAction::new(usage.clone());
    let poller = action.clone();
    tokio::spawn(async move { poller.poll_loop().await });
```

with:

```rust
    // Every usage-driven action (gauge, burn rate) registers its instances
    // in this one hub, so a single poll loop serves them all.
    let hub = UsageHub::new(usage.clone());
    tokio::spawn(hub.clone().poll_loop());

    let action = UsageGaugeAction::new(hub.clone());
```

Update the comment above `let usage = …` from "shared by both actions" to "shared by every action".

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS; clippy clean; fmt clean (run `cargo fmt` if not, and re-check).

- [ ] **Step 7: Commit**

```bash
git add src/hub.rs src/action.rs src/main.rs
git commit -m "refactor: share one UsageHub poller across usage actions

Usage Gauge now reads per-key marks, palette and color mode from its
settings."
```

---

### Task 6: Burn Rate action, manifest entry, wiring

**Files:**
- Create: `src/burn_action.rs`
- Modify: `assets/manifest.json` (append `Actions[3]`), `src/main.rs` (register)

**Interfaces:**
- Consumes: `hub::{UsageHub, View}`, `burn::{BurnMetric, burn_window, burn_feedback, burn_error_display}`, `level::ColorSettings`.
- Produces:
  ```rust
  pub struct BurnRateSettings { pub window: WindowKind, pub metric: BurnMetric, #[serde(flatten)] pub colors: ColorSettings }
  pub struct BurnRateAction; impl BurnRateAction { pub fn new(hub: Arc<UsageHub>) -> Self }
  // UUID "com.jfms7s.claudeusage.burnrate"
  ```

- [ ] **Step 1: Write the action file with failing tests**

Create `src/burn_action.rs`:

```rust
use crate::burn::{BurnMetric, burn_window};
use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::source::WindowKind;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BurnRateSettings {
    #[serde(default)]
    pub window: WindowKind,
    #[serde(default)]
    pub metric: BurnMetric,
    /// Only marks and palette matter here - Burn Rate always colors
    /// pace-based, so a stored `colorMode` is ignored.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::{burn_error_display, burn_feedback};

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][3];
        assert_eq!(entry["UUID"].as_str().unwrap(), <BurnRateAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(entry["PropertyInspectorPath"], "propertyInspector/burnrate.html");
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let layout: serde_json::Value =
            serde_json::from_str(include_str!("../assets/layouts/usage.json")).unwrap();
        let keys: Vec<&str> = layout["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        let feedback = burn_feedback(&burn_error_display(BurnMetric::Pace));
        for k in feedback.as_object().unwrap().keys() {
            assert!(keys.contains(&k.as_str()), "layout has no item keyed {k}");
        }
    }

    #[test]
    fn empty_settings_are_session_pace() {
        let s: BurnRateSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.metric, BurnMetric::Pace);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn parses_window_metric_and_colors_together() {
        let s: BurnRateSettings =
            serde_json::from_str(r#"{"window":"weekly","metric":"runway","critical":95}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.metric, BurnMetric::Runway);
        assert_eq!(s.colors.marks.critical, 95.0);
    }

    #[test]
    fn monthly_setting_views_as_session() {
        let s: BurnRateSettings = serde_json::from_str(r#"{"window":"monthly"}"#).unwrap();
        assert!(matches!(s.view(), View::Burn { window: WindowKind::Session, .. }));
    }
}
```

Add `mod burn_action;` to `src/main.rs` (after `mod burn;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test burn_action::`
Expected: compile errors — `BurnRateAction`, `view` not found.

- [ ] **Step 3: Implement the action**

Above `#[cfg(test)]` in `src/burn_action.rs`:

```rust
impl BurnRateSettings {
    fn view(&self) -> View {
        View::Burn {
            window: burn_window(self.window),
            metric: self.metric,
            colors: self.colors.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BurnRateAction {
    hub: Arc<UsageHub>,
}

impl BurnRateAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl Action for BurnRateAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.burnrate";
    type Settings = BurnRateSettings;

    async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }
}
```

- [ ] **Step 4: Add the manifest entry**

In `assets/manifest.json`, append after the Metric Tile object (inside `"Actions"`):

```json
		{
			"UUID": "com.jfms7s.claudeusage.burnrate",
			"Name": "Burn Rate",
			"Icon": "icons/icon",
			"Tooltip": "Shows how fast a Claude usage window is burning: pace per hour/day, even-burn ratio, or runway until empty",
			"Controllers": ["Encoder", "Keypad"],
			"PropertyInspectorPath": "propertyInspector/burnrate.html",
			"States": [{ "Image": "icons/actionDefaultImage" }],
			"Encoder": {
				"layout": "layouts/usage.json"
			}
		}
```

- [ ] **Step 5: Register in `main.rs`**

Add `use burn_action::BurnRateAction;`, then after `let action = UsageGaugeAction::new(hub.clone());`:

```rust
    let burn_rate = BurnRateAction::new(hub.clone());
```

and after `register_action(metric_tile).await;`:

```rust
    register_action(burn_rate).await;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS, clean. Existing `Actions[0..2]` UUID tests still pass (entry appended last).

- [ ] **Step 7: Commit**

```bash
git add src/burn_action.rs src/main.rs assets/manifest.json
git commit -m "feat: add Burn Rate action for keys and dials"
```

---

### Task 7: Property Inspectors (shared colors section + Burn Rate PI)

**Files:**
- Create: `assets/propertyInspector/colors.js`, `assets/propertyInspector/burnrate.html`
- Modify: `assets/propertyInspector/index.html`, `src/level.rs` (add a drift test)

**Interfaces:**
- Consumes: the wire field names and defaults from Task 1.
- Produces (browser globals): `COLOR_DEFAULTS`, `mountColorSection(container, { showMode, onChange })`, `applyColorSettings(settings)`, `readColorSettings() -> object`.

- [ ] **Step 1: Write a failing drift test**

Add to the tests module in `src/level.rs`:

```rust
    /// colors.js duplicates the defaults for the Property Inspector - fail
    /// if the two ever disagree.
    #[test]
    fn property_inspector_defaults_match() {
        let js = include_str!("../assets/propertyInspector/colors.js");
        let m = Marks::default();
        for needle in [
            format!("watch: {}", m.watch),
            format!("risk: {}", m.risk),
            format!("critical: {}", m.critical),
            format!("colorNormal: \"{DEFAULT_NORMAL}\""),
            format!("colorWatch: \"{DEFAULT_WATCH}\""),
            format!("colorRisk: \"{DEFAULT_RISK}\""),
            format!("colorCritical: \"{DEFAULT_CRITICAL}\""),
            "colorMode: \"fixed\"".to_string(),
        ] {
            assert!(js.contains(&needle), "colors.js is missing `{needle}`");
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test level::tests::property_inspector_defaults_match`
Expected: compile error — `colors.js` doesn't exist.

- [ ] **Step 3: Create `assets/propertyInspector/colors.js`**

```js
// Shared "Colors & thresholds" section for the Usage Gauge and Burn Rate
// property inspectors. Field names and defaults mirror src/level.rs's wire
// format (a Rust test checks the defaults stay in sync).
const COLOR_DEFAULTS = {
	watch: 50,
	risk: 75,
	critical: 90,
	colorNormal: "#d97757",
	colorWatch: "#eab308",
	colorRisk: "#f97316",
	colorCritical: "#ef4444",
	colorMode: "fixed",
};

function mountColorSection(container, { showMode, onChange }) {
	container.innerHTML = `
		<details>
			<summary>Colors &amp; thresholds</summary>
			<label for="watch">Watch at (% used)</label>
			<input type="number" id="watch" min="0" max="100" step="1" />
			<label for="risk">Risk at (% used)</label>
			<input type="number" id="risk" min="0" max="100" step="1" />
			<label for="critical">Critical at (% used)</label>
			<input type="number" id="critical" min="0" max="100" step="1" />
			<label for="colorNormal">Normal color</label>
			<input type="color" id="colorNormal" />
			<label for="colorWatch">Watch color</label>
			<input type="color" id="colorWatch" />
			<label for="colorRisk">Risk color</label>
			<input type="color" id="colorRisk" />
			<label for="colorCritical">Critical color</label>
			<input type="color" id="colorCritical" />
			<div id="colorModeRow">
				<label for="colorMode">Color by</label>
				<select id="colorMode">
					<option value="fixed">Current usage</option>
					<option value="pace">Pace (also warn when burning too fast)</option>
				</select>
			</div>
			<p class="hint">Marks must increase (Watch &lt; Risk &lt; Critical), otherwise defaults are used.</p>
			<button type="button" id="colorReset">Reset to defaults</button>
		</details>`;
	container.querySelector("#colorModeRow").hidden = !showMode;
	for (const key of Object.keys(COLOR_DEFAULTS)) {
		container.querySelector(`#${key}`).addEventListener("change", onChange);
	}
	container.querySelector("#colorReset").addEventListener("click", () => {
		applyColorSettings(COLOR_DEFAULTS);
		onChange();
	});
}

function applyColorSettings(settings) {
	for (const [key, fallback] of Object.entries(COLOR_DEFAULTS)) {
		document.getElementById(key).value = settings[key] ?? fallback;
	}
}

// Numbers go out as JSON numbers; a cleared number input sends its default
// rather than "" (the plugin would also fall back, but this keeps the
// stored settings readable).
function readColorSettings() {
	const out = {};
	for (const [key, fallback] of Object.entries(COLOR_DEFAULTS)) {
		const raw = document.getElementById(key).value;
		if (typeof fallback === "number") {
			const n = Number(raw);
			out[key] = raw.trim() !== "" && Number.isFinite(n) ? n : fallback;
		} else {
			out[key] = raw;
		}
	}
	return out;
}
```

- [ ] **Step 4: Update `assets/propertyInspector/index.html`**

- In `<style>`, change `select { … }` to `select, input { width: 100%; box-sizing: border-box; margin-top: 2px; }` and add:
  `details { margin-top: 12px; } summary { cursor: pointer; font-size: 11px; opacity: 0.8; } .hint { font-size: 10px; opacity: 0.6; } button { margin-top: 8px; }`
- After the `window` `<select>`, add `<div id="colors"></div>` and `<script src="colors.js"></script>`.
- In the inline script, add right after `let uuid;`:

```js
		mountColorSection(document.getElementById("colors"), { showMode: true, onChange: sendSettings });
```

- Replace `applySettings` and `sendSettings` with:

```js
		function applySettings(settings) {
			document.getElementById("window").value = settings.window || "session";
			applyColorSettings(settings);
		}

		function sendSettings() {
			websocket.send(JSON.stringify({
				event: "setSettings",
				context: uuid,
				payload: { window: document.getElementById("window").value, ...readColorSettings() },
			}));
		}
```

- [ ] **Step 5: Create `assets/propertyInspector/burnrate.html`**

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

	<label for="metric">Show</label>
	<select id="metric">
		<option value="pace">Pace (% per hour / per day)</option>
		<option value="evenBurn">Even burn (1.0x = on track)</option>
		<option value="runway">Runway (time until empty)</option>
	</select>
	<p class="hint">Colors always follow pace: a fast burn warns before usage itself crosses a mark.</p>

	<div id="colors"></div>
	<script src="colors.js"></script>

	<script>
		window.connectOpenActionSocketData = new Promise((resolve) => {
			window.connectOpenActionSocket = (...args) => resolve(args);
			window.connectElgatoStreamDeckSocket = window.connectOpenActionSocket;
		});

		let websocket;
		let uuid;

		mountColorSection(document.getElementById("colors"), { showMode: false, onChange: sendSettings });

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
			document.getElementById("window").value = settings.window === "weekly" ? "weekly" : "session";
			document.getElementById("metric").value = settings.metric || "pace";
			applyColorSettings(settings);
		}

		function sendSettings() {
			websocket.send(JSON.stringify({
				event: "setSettings",
				context: uuid,
				payload: {
					window: document.getElementById("window").value,
					metric: document.getElementById("metric").value,
					...readColorSettings(),
				},
			}));
		}

		document.getElementById("window").addEventListener("change", sendSettings);
		document.getElementById("metric").addEventListener("change", sendSettings);
	</script>
</body>
</html>
```

(`build.mjs` already copies the whole `assets/propertyInspector` directory, so no build change is needed.)

- [ ] **Step 6: Run tests; syntax-check the JS**

Run: `cargo test && node --check assets/propertyInspector/colors.js`
Expected: all PASS; `node --check` prints nothing.

- [ ] **Step 7: Commit**

```bash
git add assets/propertyInspector/ src/level.rs
git commit -m "feat: add colors & thresholds section and Burn Rate property inspector"
```

---

### Task 8: README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update the README**

- Intro paragraph: "three actions" → "four actions", adding **Burn Rate** to the list.
- Replace the keypad-gauge description "a three-zone semicircle (green/yellow/red, at the same 50%/80% thresholds as the dial's bar)" with "a four-zone semicircle drawn from the key's own Watch/Risk/Critical marks and colors".
- Add a section after "Using a Metric Tile":

```markdown
## Colors & thresholds

Usage Gauge and Burn Rate keys stay a calm copper until usage crosses one
of three marks you set per key (in % used): **Watch** (default 50),
**Risk** (75), **Critical** (90), each with its own editable color. Marks
must increase; if they don't, the defaults are used.

On a Usage Gauge you can also color by **pace**: the key warns at
whichever level is worse, current usage or the usage you'd reach at reset
if you keep burning at the current rate. Pace is only computed after 10%
of the window has passed (earlier projections are noise), and never for
Monthly.

## Using Burn Rate

1. Add a **Burn Rate** key on a dial or a keypad tile.
2. Pick the window (Session or Weekly) and what to show:
   - **Pace**: % used per hour (Session) or per day (Weekly) so far.
   - **Even burn**: projected usage at reset ÷ 100%, so `1.0x` is exactly on track.
   - **Runway**: time until 100% at the current rate, or ✓ if it lasts to the reset.
3. Its color always follows pace. It shows "too early" for the first 10% of a window.
```

- Add a "Upgrading from 0.6.0" note under Installing: "Existing Usage Gauge keys switch from green/yellow/red at 50/80% to copper with Watch 50, Risk 75 and Critical 90. Open a key's **Colors & thresholds** section to change this."
- Add to the smoke-test checklist:

```markdown
- [ ] Changing a Usage Gauge's marks/colors updates both a dial's bar
      color and a tile's speedometer zones immediately. *(not yet verified)*
- [ ] Color-by-pace on a Usage Gauge turns it Watch/Risk earlier during a
      fast burn, and not during the first 10% of a window. *(not yet verified)*
- [ ] Burn Rate shows Pace / Even burn / Runway on a key and on a dial for
      Session and Weekly. *(not yet verified)*
- [ ] A key upgraded from 0.6.0 keeps its window setting. *(not yet verified)*
```

- [ ] **Step 2: Verify**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS, clean.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document colors, pace mode and Burn Rate"
```
