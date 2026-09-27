# Gauge Styles + Tap-Cycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the keypad Usage Gauge six styles (Speedometer, Bar, Soft pill, Open donut, Tracked donut, Thin ring) with tick marks at the key's marks, cycled by a short press (long press refreshes) through a per-key checklist, with the current style persisted in settings.

**Architecture:** A pure `style.rs` holds the style enum, the lenient `StyleSettings` wire format, `next_style` and `classify_press`. `src/styles/` holds one renderer per style family (`speedometer.rs` moved from `icon.rs`, `bar.rs`, `donut.rs`, `ring.rs`) plus shared SVG helpers and a `build_styled_icon` dispatcher. `View::Gauge` carries the style so the hub's poll draws each key in its own style; `UsageGaugeAction` times `key_down`→`key_up` to decide cycle vs refresh and persists the new style with `set_settings`.

**Tech Stack:** Rust 2024, openaction 2.7, serde/serde_json, dashmap, tokio. Plain HTML/JS property inspector.

**Spec:** `docs/superpowers/specs/2026-09-27-gauge-styles-design.md`

## Global Constraints

- No new crates.
- Style wire names (camelCase): `speedometer`, `bar`, `softPill`, `openDonut`, `trackedDonut`, `thinRing`; settings keys `style` and `cycleStyles`.
- Defaults: `style` = Speedometer; `cycleStyles` = all six in `ALL_STYLES` order.
- `LONG_PRESS` = **500 ms**; a held duration `>= LONG_PRESS` is Long; `None` (no recorded `key_down`) is Short.
- `next_style` returns `None` for fewer than 2 entries; current not in cycle → `cycle[0]`.
- Track color `#374151`; tick color `tile::TEXT_COLOR`; progress color `display.color`.
- A bad `style`/`cycleStyles` value must never make openaction reset the whole settings struct (lenient `Value` wire fields, same pattern as `level::ColorSettingsWire`).
- Dials and Burn Rate are unchanged (dial press still refreshes; Burn Rate tap still refreshes).
- Existing conventions: doc comments explain *why*; tests at the bottom of each file in `#[cfg(test)] mod tests`; keypad text via `tile::text_line`; SVG coordinates in a 100×100 viewBox.

## Review Focus

1. **Settings saved by v0.7.0** (window + colors, no style keys) load as Speedometer with all six in the cycle and keep window/colors. Test: Task 6 `v070_settings_load_as_speedometer_with_all_styles`.
2. **PI sends a one-entry or garbage `cycleStyles`** — a short press does nothing (no crash, no style change). Tests: Task 1 `garbage_cycle_falls_back_to_all`, `single_style_cycle_has_no_next`; Task 6 `cycled_is_none_with_one_style`.
3. **`key_up` with no `key_down`** (plugin restarted mid-press) is treated as a short press. Test: Task 1 `no_key_down_is_short`.
4. **Marks at 0 or 100** — ticks sit at the track ends, no NaN/negative widths. Tests: Task 3 `ticks_at_track_ends`, Task 4 `tick_at_zero_and_hundred`.
5. **Monthly not enabled / no data** (`"—"`, `bar_value` 0) renders with no progress fill in every style. Tests: Task 2 `number_text_strips_percent`, Tasks 3–5 `zero_draws_no_fill` variants.

---

### Task 1: Style model (`src/style.rs`)

**Files:**
- Create: `src/style.rs`
- Modify: `src/main.rs` (add `mod style;`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub enum GaugeStyle { #[default] Speedometer, Bar, SoftPill, OpenDonut, TrackedDonut, ThinRing }
  pub const ALL_STYLES: [GaugeStyle; 6];
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  #[serde(from = "StyleSettingsWire", into = "StyleSettingsWire")]
  pub struct StyleSettings { pub style: GaugeStyle, pub cycle: Vec<GaugeStyle> }
  impl Default for StyleSettings; // Speedometer, ALL_STYLES
  pub fn next_style(current: GaugeStyle, cycle: &[GaugeStyle]) -> Option<GaugeStyle>;
  pub const LONG_PRESS: Duration; // 500ms
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Press { Short, Long }
  pub fn classify_press(held: Option<Duration>) -> Press;
  ```

- [ ] **Step 1: Write the skeleton and failing tests**

Create `src/style.rs`:

```rust
//! Usage Gauge keypad styles and the short-press cycle through them. Pure,
//! so the cycle rules and the lenient settings wire format are unit-testable.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GaugeStyle {
    #[default]
    Speedometer,
    Bar,
    SoftPill,
    OpenDonut,
    TrackedDonut,
    ThinRing,
}

/// Every style, in the order the Property Inspector lists them and the
/// default cycle visits them.
pub const ALL_STYLES: [GaugeStyle; 6] = [
    GaugeStyle::Speedometer,
    GaugeStyle::Bar,
    GaugeStyle::SoftPill,
    GaugeStyle::OpenDonut,
    GaugeStyle::TrackedDonut,
    GaugeStyle::ThinRing,
];

/// Held at least this long, a press refreshes instead of cycling.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StyleSettingsWire", into = "StyleSettingsWire")]
pub struct StyleSettings {
    /// The style the key currently shows - changed only by a short press.
    pub style: GaugeStyle,
    /// The styles a short press visits, in order.
    pub cycle: Vec<GaugeStyle>,
}

impl Default for StyleSettings {
    fn default() -> Self {
        Self {
            style: GaugeStyle::default(),
            cycle: ALL_STYLES.to_vec(),
        }
    }
}

/// Raw `Value` fields for the same reason as `level::ColorSettingsWire`:
/// a malformed value must fall back on its own rather than make openaction
/// reset the key's whole settings struct.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StyleSettingsWire {
    style: Value,
    cycle_styles: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use GaugeStyle::*;

    #[test]
    fn next_wraps_around() {
        assert_eq!(next_style(Bar, &[Speedometer, Bar]), Some(Speedometer));
        assert_eq!(next_style(Speedometer, &[Speedometer, Bar]), Some(Bar));
    }

    #[test]
    fn next_skips_unticked_styles() {
        assert_eq!(next_style(Bar, &[Bar, ThinRing]), Some(ThinRing));
    }

    #[test]
    fn current_not_in_cycle_goes_to_first() {
        assert_eq!(next_style(Speedometer, &[OpenDonut, ThinRing]), Some(OpenDonut));
    }

    #[test]
    fn single_style_cycle_has_no_next() {
        assert_eq!(next_style(Bar, &[Bar]), None);
        assert_eq!(next_style(Bar, &[]), None);
    }

    #[test]
    fn press_threshold_is_500ms() {
        assert_eq!(classify_press(Some(Duration::from_millis(499))), Press::Short);
        assert_eq!(classify_press(Some(Duration::from_millis(500))), Press::Long);
    }

    #[test]
    fn no_key_down_is_short() {
        assert_eq!(classify_press(None), Press::Short);
    }

    #[test]
    fn empty_json_is_the_default() {
        let s: StyleSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, StyleSettings::default());
        assert_eq!(s.cycle, ALL_STYLES.to_vec());
    }

    #[test]
    fn unknown_style_falls_back_to_speedometer() {
        let s: StyleSettings = serde_json::from_str(r#"{"style":"hologram"}"#).unwrap();
        assert_eq!(s.style, Speedometer);
    }

    #[test]
    fn unknown_and_duplicate_cycle_entries_are_dropped() {
        let s: StyleSettings =
            serde_json::from_str(r#"{"cycleStyles":["thinRing","nope","bar","thinRing",3]}"#)
                .unwrap();
        assert_eq!(s.cycle, vec![ThinRing, Bar]);
    }

    #[test]
    fn garbage_cycle_falls_back_to_all() {
        for json in [r#"{"cycleStyles":[]}"#, r#"{"cycleStyles":"bar"}"#, r#"{"cycleStyles":["x"]}"#] {
            let s: StyleSettings = serde_json::from_str(json).unwrap();
            assert_eq!(s.cycle, ALL_STYLES.to_vec(), "for {json}");
        }
    }

    #[test]
    fn wire_round_trips() {
        let s = StyleSettings {
            style: SoftPill,
            cycle: vec![SoftPill, OpenDonut],
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["style"], "softPill");
        assert_eq!(v["cycleStyles"], json!(["softPill", "openDonut"]));
        let back: StyleSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back, s);
    }
}
```

Add `mod style;` to `src/main.rs` (alphabetically, after `mod source;`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test style::`
Expected: compile errors — `next_style`, `classify_press`, the `From` conversions missing.

- [ ] **Step 3: Implement**

Insert above `#[cfg(test)]`:

```rust
/// The style a short press moves to: the one after `current` in `cycle`
/// (wrapping), or `cycle[0]` if `current` was unticked since. `None` when
/// fewer than two styles are ticked - there's nothing to cycle between.
pub fn next_style(current: GaugeStyle, cycle: &[GaugeStyle]) -> Option<GaugeStyle> {
    if cycle.len() < 2 {
        return None;
    }
    Some(match cycle.iter().position(|s| *s == current) {
        Some(i) => cycle[(i + 1) % cycle.len()],
        None => cycle[0],
    })
}

/// `None` means `key_up` arrived with no recorded `key_down` (e.g. the
/// plugin restarted mid-press) - treated as the cheaper, reversible short
/// press.
pub fn classify_press(held: Option<Duration>) -> Press {
    match held {
        Some(d) if d >= LONG_PRESS => Press::Long,
        _ => Press::Short,
    }
}

impl From<StyleSettingsWire> for StyleSettings {
    fn from(w: StyleSettingsWire) -> Self {
        let mut cycle: Vec<GaugeStyle> = Vec::new();
        for entry in w.cycle_styles.as_array().into_iter().flatten() {
            if let Ok(style) = serde_json::from_value::<GaugeStyle>(entry.clone()) {
                if !cycle.contains(&style) {
                    cycle.push(style);
                }
            }
        }
        if cycle.is_empty() {
            cycle = ALL_STYLES.to_vec();
        }
        Self {
            style: serde_json::from_value(w.style).unwrap_or_default(),
            cycle,
        }
    }
}

impl From<StyleSettings> for StyleSettingsWire {
    fn from(s: StyleSettings) -> Self {
        Self {
            style: json!(s.style),
            cycle_styles: json!(s.cycle),
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test style::`
Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
git add src/style.rs src/main.rs
git commit -m "feat: add gauge style model and press classification"
```

---

### Task 2: Display label/number and `src/styles/` scaffold

**Files:**
- Modify: `src/format.rs` (add `label`, `number_text` to `UsageDisplay`)
- Create: `src/styles/mod.rs`; Move: `src/icon.rs` → `src/styles/speedometer.rs`
- Modify: `src/main.rs` (`mod icon;` → `mod styles;`), `src/hub.rs` (import path)

**Interfaces:**
- Produces:
  ```rust
  // UsageDisplay gains:
  pub label: &'static str,     // "SESSION" | "WEEKLY" | "MONTHLY" | "USAGE" (error)
  pub number_text: String,     // percent_text without '%'
  // styles/mod.rs:
  pub const TRACK_COLOR: &str = "#374151";
  pub fn polar(cx: f64, cy: f64, r: f64, deg: f64) -> (f64, f64);
  pub fn arc(cx: f64, cy: f64, r: f64, start_deg: f64, end_deg: f64, color: &str, width: f64, round_caps: bool) -> String;
  pub fn radial_tick(cx: f64, cy: f64, r: f64, deg: f64) -> String;
  pub fn svg(body: &str) -> String; // wraps body in the 100x100 <svg> with tile::card()
  // styles/speedometer.rs:
  pub fn build_icon(display: &UsageDisplay) -> String; // unchanged behavior, new path
  ```

- [ ] **Step 1: Failing tests for the display fields**

In `src/format.rs` tests, add:

```rust
    #[test]
    fn display_labels_each_window() {
        let s = snapshot();
        let c = ColorSettings::default();
        assert_eq!(build_display(&s, WindowKind::Session, &c, dt(20, 30, 0)).label, "SESSION");
        assert_eq!(build_display(&s, WindowKind::Weekly, &c, dt(20, 30, 0)).label, "WEEKLY");
        assert_eq!(build_display(&s, WindowKind::Monthly, &c, dt(20, 30, 0)).label, "MONTHLY");
        assert_eq!(error_display().label, "USAGE");
    }

    #[test]
    fn number_text_strips_percent() {
        let c = ColorSettings::default();
        assert_eq!(build_display(&snapshot(), WindowKind::Session, &c, dt(20, 30, 0)).number_text, "33");
        let mut s = snapshot();
        s.monthly.enabled = false;
        assert_eq!(build_display(&s, WindowKind::Monthly, &c, dt(20, 30, 0)).number_text, "\u{2014}");
        assert_eq!(error_display().number_text, "\u{2014}");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test format::`
Expected: compile error — no field `label` / `number_text`.

- [ ] **Step 3: Implement the display fields**

In `src/format.rs`:
- Add to `UsageDisplay` after `tile_detail`:

```rust
    /// Uppercase window name for styles that caption the value.
    pub label: &'static str,
    /// `percent_text` without the `%` sign, for the donut/ring centers.
    pub number_text: String,
```

- Add a helper above `build_display`:

```rust
fn window_label(window: WindowKind) -> &'static str {
    match window {
        WindowKind::Session => "SESSION",
        WindowKind::Weekly => "WEEKLY",
        WindowKind::Monthly => "MONTHLY",
    }
}
```

- `make_display` gains a first parameter `label: &'static str`, and sets `label,` and `number_text: format_percent(percent).trim_end_matches('%').to_string(),` (compute `let percent_text = format_percent(percent);` once and use it for both).
- `window_display` passes `window_label(kind)`; `monthly_display` passes `"MONTHLY"`.
- The disabled-monthly literal adds `label: "MONTHLY", number_text: "\u{2014}".to_string(),`.
- `error_display` adds `label: "USAGE", number_text: "\u{2014}".to_string(),`.

- [ ] **Step 4: Move `icon.rs` and add `styles/mod.rs` with failing helper tests**

```bash
mkdir -p src/styles && git mv src/icon.rs src/styles/speedometer.rs
```

In `src/main.rs` replace `mod icon;` with `mod styles;` (keep alphabetical order: after `mod style;`).
In `src/hub.rs` replace `use crate::icon::build_icon;` with `use crate::styles::speedometer::build_icon;`.
In `src/styles/speedometer.rs` tests, the `display()` helper and the disabled-color literal each add `label: "SESSION", number_text: "…".to_string(),` (use `format!("{bar_value}")` in the helper and `"\u{2014}".to_string()` in the disabled literal).
In `src/format.rs` update the `build_display` doc comment's `icon::build_icon` to `styles::build_styled_icon`.

Create `src/styles/mod.rs`:

```rust
//! Keypad renderers for the Usage Gauge's styles, plus the SVG helpers they
//! share. Each renderer returns a bare SVG string; `build_styled_icon`
//! wraps the chosen one as the data URI OpenDeck's `setImage` expects.

pub mod speedometer;

use crate::tile::{self, TEXT_COLOR};

/// Unfilled part of every bar/arc - dark enough to read as "empty" on the
/// card, light enough to show the track's extent.
pub const TRACK_COLOR: &str = "#374151";

/// The 100x100 `<svg>` wrapper with the shared dark card underneath.
pub fn svg(body: &str) -> String {
    let card = tile::card();
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">{card}{body}</svg>"#)
}

/// A point on a circle at `deg` degrees measured clockwise from +x in
/// screen space (y grows downward, so +sin is down): 0 = right, 90 =
/// bottom, 180 = left, 270/-90 = top.
pub fn polar(cx: f64, cy: f64, r: f64, deg: f64) -> (f64, f64) {
    let t = deg.to_radians();
    (cx + r * t.cos(), cy + r * t.sin())
}

/// A stroked arc sweeping clockwise from `start_deg` to `end_deg`. Nothing
/// for a zero/negative sweep; a `<circle>` for a full turn (an SVG arc
/// whose endpoints coincide draws nothing).
// Geometry plus stroke: a params struct would only rename these eight values.
#[allow(clippy::too_many_arguments)]
pub fn arc(
    cx: f64,
    cy: f64,
    r: f64,
    start_deg: f64,
    end_deg: f64,
    color: &str,
    width: f64,
    round_caps: bool,
) -> String {
    let sweep = end_deg - start_deg;
    if sweep <= 0.0 {
        return String::new();
    }
    if sweep >= 360.0 {
        return format!(
            r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="none" stroke="{color}" stroke-width="{width}" />"#
        );
    }
    let (sx, sy) = polar(cx, cy, r, start_deg);
    let (ex, ey) = polar(cx, cy, r, end_deg);
    let large = if sweep > 180.0 { 1 } else { 0 };
    let cap = if round_caps { "round" } else { "butt" };
    format!(
        r#"<path d="M {sx:.2} {sy:.2} A {r} {r} 0 {large} 1 {ex:.2} {ey:.2}" fill="none" stroke="{color}" stroke-width="{width}" stroke-linecap="{cap}" />"#
    )
}

/// A short radial line across a ring at `deg` - how donuts and rings mark
/// the Watch/Risk/Critical thresholds.
pub fn radial_tick(cx: f64, cy: f64, r: f64, deg: f64) -> String {
    let (x1, y1) = polar(cx, cy, r - 7.0, deg);
    let (x2, y2) = polar(cx, cy, r + 7.0, deg);
    format!(
        r#"<line x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="{TEXT_COLOR}" stroke-width="1.5" />"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polar_is_clockwise_in_screen_space() {
        let (x, y) = polar(50.0, 50.0, 10.0, 90.0);
        assert!((x - 50.0).abs() < 1e-9 && (y - 60.0).abs() < 1e-9);
        let (x, y) = polar(50.0, 50.0, 10.0, -90.0);
        assert!((x - 50.0).abs() < 1e-9 && (y - 40.0).abs() < 1e-9);
    }

    #[test]
    fn large_arc_flag_only_above_180() {
        assert!(arc(50.0, 50.0, 10.0, 0.0, 180.0, "#fff", 2.0, true).contains(" 0 0 1 "));
        assert!(arc(50.0, 50.0, 10.0, 0.0, 181.0, "#fff", 2.0, true).contains(" 0 1 1 "));
    }

    #[test]
    fn full_turn_is_a_circle_and_zero_is_nothing() {
        assert!(arc(50.0, 50.0, 10.0, -90.0, 270.0, "#fff", 2.0, true).starts_with("<circle"));
        assert_eq!(arc(50.0, 50.0, 10.0, 10.0, 10.0, "#fff", 2.0, true), "");
    }

    #[test]
    fn caps_follow_the_flag() {
        assert!(arc(50.0, 50.0, 10.0, 0.0, 90.0, "#fff", 2.0, true).contains("round"));
        assert!(arc(50.0, 50.0, 10.0, 0.0, 90.0, "#fff", 2.0, false).contains("butt"));
    }

    #[test]
    fn radial_tick_spans_the_ring() {
        // At 0deg (right) a tick on r=28 around (50,47) runs x 71..85.
        let t = radial_tick(50.0, 47.0, 28.0, 0.0);
        assert!(t.contains(r#"x1="71.00" y1="47.00" x2="85.00" y2="47.00""#), "got: {t}");
    }
}
```

- [ ] **Step 5: Run to verify**

Run: `cargo test`
Expected: all pass (moved speedometer tests still pass; new format and styles helper tests pass). `svg`, `arc`, `radial_tick`, `TRACK_COLOR`, `label` and `number_text` are unused outside tests until Tasks 3–5, so `cargo clippy -- -D warnings` fails on dead code here; that is expected and cleared by Task 5 (which runs clippy). Do not add `#[allow(dead_code)]`.

- [ ] **Step 6: Commit**

```bash
git add -A src/
git commit -m "refactor: move speedometer under styles/ and add window label to display"
```

---

### Task 3: Bar and Soft pill (`src/styles/bar.rs`)

**Files:**
- Create: `src/styles/bar.rs`; Modify: `src/styles/mod.rs` (`pub mod bar;`)

**Interfaces:**
- Consumes: `UsageDisplay` (`label`, `percent_text`, `tile_detail`, `color`, `bar_value`, `marks`), `styles::{svg, TRACK_COLOR}`, `tile::{text_line, TEXT_COLOR, MUTED_TEXT_COLOR}`.
- Produces: `pub fn render(display: &UsageDisplay, pill: bool) -> String` (bare SVG).

- [ ] **Step 1: Failing tests**

Create `src/styles/bar.rs`:

```rust
//! Bar and Soft pill: caption, big percent, a horizontal bar with tick
//! marks at the key's thresholds, and the countdown underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const TRACK_X: f64 = 12.0;
const TRACK_W: f64 = 76.0;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::level::{Marks, Palette};

    pub(crate) fn display(v: f64) -> UsageDisplay {
        UsageDisplay {
            percent_text: format!("{v}%"),
            color: "#d97757".to_string(),
            detail_text: "resets in 1h".to_string(),
            tile_detail: "3h 54m".to_string(),
            label: "SESSION",
            number_text: format!("{v}"),
            bar_value: v,
            marks: Marks::default(),
            palette: Palette::default(),
        }
    }

    #[test]
    fn bar_draws_label_percent_countdown_and_fill() {
        let s = render(&display(42.0), false);
        assert!(s.starts_with("<svg"), "got: {s}");
        assert!(s.contains(">SESSION</text>"));
        assert!(s.contains(r##"fill="#d97757">42%</text>"##), "got: {s}");
        assert!(s.contains(">3h 54m</text>"));
        // 76 * 0.42 = 31.92
        assert!(s.contains(r##"width="31.92" height="8" fill="#d97757""##), "got: {s}");
    }

    #[test]
    fn bar_ticks_at_default_marks() {
        let s = render(&display(42.0), false);
        // watch 50 -> 50.00, risk 75 -> 69.00, critical 90 -> 80.40
        for x in ["50.00", "69.00", "80.40"] {
            assert!(s.contains(&format!(r#"x1="{x}" y1="61" x2="{x}" y2="75""#)), "tick {x} in {s}");
        }
    }

    #[test]
    fn custom_marks_move_ticks() {
        let mut d = display(42.0);
        d.marks = Marks { watch: 25.0, risk: 60.0, critical: 95.0 };
        assert!(render(&d, false).contains(r#"x1="31.00""#));
    }

    #[test]
    fn ticks_at_track_ends() {
        let mut d = display(42.0);
        d.marks = Marks { watch: 0.0, risk: 50.0, critical: 100.0 };
        let s = render(&d, false);
        assert!(s.contains(r#"x1="12.00""#) && s.contains(r#"x1="88.00""#), "got: {s}");
    }

    #[test]
    fn zero_draws_no_fill() {
        for pill in [false, true] {
            let s = render(&display(0.0), pill);
            assert!(!s.contains(r##"fill="#d97757" rx"##) && !s.contains(r##"height="8" fill="#d97757""##), "got: {s}");
        }
    }

    #[test]
    fn full_bar_spans_the_track() {
        assert!(render(&display(100.0), false).contains(r##"width="76.00" height="8" fill="#d97757""##));
    }

    #[test]
    fn pill_is_rounded_with_a_minimum_width_and_white_percent() {
        let s = render(&display(3.0), true);
        assert!(s.contains(r##"width="12.00" height="12" rx="6" fill="#d97757""##), "got: {s}");
        assert!(s.contains(&format!(r#"fill="{TEXT_COLOR}">3%</text>"#)), "got: {s}");
        assert!(s.contains(r#"x1="50.00" y1="60" x2="50.00" y2="78""#), "got: {s}");
    }
}
```

Add `pub mod bar;` to `src/styles/mod.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test styles::bar`
Expected: compile error — `render` not found.

- [ ] **Step 3: Implement**

Above `#[cfg(test)]` in `src/styles/bar.rs`:

```rust
/// `pill` = Soft pill: a thicker, rounded bar that carries the level color
/// itself, so the percent above it stays white.
pub fn render(display: &UsageDisplay, pill: bool) -> String {
    let (track_y, height, rx) = if pill { (63.0, 12.0, 6.0) } else { (64.0, 8.0, 0.0) };
    let rounding = if pill { format!(r#" rx="{rx}""#) } else { String::new() };

    let label = tile::text_line(22.0, 12.0, true, MUTED_TEXT_COLOR, display.label);
    let percent_color = if pill { TEXT_COLOR } else { display.color.as_str() };
    let percent = tile::text_line(54.0, 28.0, true, percent_color, &display.percent_text);
    let detail = tile::text_line(91.0, 13.0, false, MUTED_TEXT_COLOR, &display.tile_detail);

    let track = format!(
        r#"<rect x="{TRACK_X}" y="{track_y}" width="{TRACK_W}" height="{height}"{rounding} fill="{TRACK_COLOR}" />"#
    );
    let fill = if display.bar_value > 0.0 {
        let mut width = TRACK_W * display.bar_value / 100.0;
        if pill {
            // Narrower than its height, a rounded rect's caps overlap and
            // it renders as a lopsided blob.
            width = width.max(height);
        }
        format!(
            r#"<rect x="{TRACK_X}" y="{track_y}" width="{width:.2}" height="{height}"{rounding} fill="{}" />"#,
            display.color
        )
    } else {
        String::new()
    };
    let (y1, y2) = (track_y - 3.0, track_y + height + 3.0);
    let ticks: String = [display.marks.watch, display.marks.risk, display.marks.critical]
        .iter()
        .map(|m| {
            let x = TRACK_X + TRACK_W * m / 100.0;
            format!(
                r#"<line x1="{x:.2}" y1="{y1}" x2="{x:.2}" y2="{y2}" stroke="{TEXT_COLOR}" stroke-width="1.2" />"#
            )
        })
        .collect();

    svg(&format!("{label}{percent}{track}{fill}{ticks}{detail}"))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test styles::bar`
Expected: 7 passed. (If `zero_draws_no_fill`'s negative asserts pass trivially because of attribute order, confirm by temporarily reading the 42% output: the fill rect is `width=".." height="8" fill="#d97757"` — the assertion targets exactly that substring.)

- [ ] **Step 5: Commit**

```bash
git add src/styles/
git commit -m "feat: add Bar and Soft pill gauge styles"
```

---

### Task 4: Open and Tracked donut (`src/styles/donut.rs`)

**Files:**
- Create: `src/styles/donut.rs`; Modify: `src/styles/mod.rs` (`pub mod donut;`)

**Interfaces:**
- Consumes: `styles::{svg, arc, radial_tick, TRACK_COLOR}`, `UsageDisplay`, `styles::bar::tests::display` (test helper).
- Produces: `pub fn render(display: &UsageDisplay, tracked: bool) -> String`.

- [ ] **Step 1: Failing tests**

Create `src/styles/donut.rs`:

```rust
//! Open donut (a 270deg arc, gap at the bottom) and Tracked donut (the same
//! arc as ten segments), with the number in the middle and the window name
//! underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, arc, radial_tick, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const CX: f64 = 50.0;
const CY: f64 = 47.0;
const R: f64 = 28.0;
const WIDTH: f64 = 9.0;
/// Bottom-left, sweeping clockwise over the top to bottom-right.
const START: f64 = 135.0;
const SWEEP: f64 = 270.0;
const SEGMENTS: usize = 10;
const SEGMENT_GAP: f64 = 6.0;

fn angle(percent: f64) -> f64 {
    START + SWEEP * percent / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Marks;
    use crate::styles::bar::tests::display;

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    #[test]
    fn open_donut_draws_track_progress_number_and_label() {
        let s = render(&display(42.0), false);
        assert!(s.starts_with("<svg"));
        assert!(s.contains(&format!(r#"stroke="{TRACK_COLOR}""#)));
        assert_eq!(count(&s, r##"stroke="#d97757""##), 1, "got: {s}");
        assert!(s.contains(">42</text>") && s.contains(">SESSION</text>"), "got: {s}");
    }

    #[test]
    fn open_donut_ticks_at_default_marks() {
        let s = render(&display(42.0), false);
        // watch 50 -> 270deg (straight up): tick from (50,26) to (50,12)
        assert!(s.contains(r#"x1="50.00" y1="26.00" x2="50.00" y2="12.00""#), "got: {s}");
        assert_eq!(count(&s, "<line"), 3);
    }

    #[test]
    fn tick_at_zero_and_hundred() {
        let mut d = display(42.0);
        d.marks = Marks { watch: 0.0, risk: 50.0, critical: 100.0 };
        let s = render(&d, false);
        assert_eq!(count(&s, "<line"), 3);
        assert!(!s.contains("NaN"));
    }

    #[test]
    fn open_donut_zero_draws_no_fill() {
        assert_eq!(count(&render(&display(0.0), false), r##"stroke="#d97757""##), 0);
    }

    #[test]
    fn tracked_donut_lights_ceil_tenths() {
        assert_eq!(count(&render(&display(42.0), true), r##"stroke="#d97757""##), 5);
        assert_eq!(count(&render(&display(40.0), true), r##"stroke="#d97757""##), 4);
        assert_eq!(count(&render(&display(0.0), true), r##"stroke="#d97757""##), 0);
        assert_eq!(count(&render(&display(100.0), true), r##"stroke="#d97757""##), 10);
    }

    #[test]
    fn tracked_segments_have_butt_caps() {
        let s = render(&display(42.0), true);
        assert_eq!(count(&s, "stroke-linecap=\"butt\""), 10);
    }
}
```

Add `pub mod donut;` to `src/styles/mod.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test styles::donut`
Expected: compile error — `render` not found.

- [ ] **Step 3: Implement**

Above `#[cfg(test)]`:

```rust
pub fn render(display: &UsageDisplay, tracked: bool) -> String {
    let v = display.bar_value;
    let rings = if tracked {
        let span = (SWEEP - SEGMENT_GAP * (SEGMENTS - 1) as f64) / SEGMENTS as f64;
        (0..SEGMENTS)
            .map(|i| {
                let start = START + i as f64 * (span + SEGMENT_GAP);
                // Segment i lights once usage passes its start, so 42%
                // lights five and 0% lights none.
                let color = if v > i as f64 * 10.0 { display.color.as_str() } else { TRACK_COLOR };
                arc(CX, CY, R, start, start + span, color, WIDTH, false)
            })
            .collect::<String>()
    } else {
        let track = arc(CX, CY, R, START, START + SWEEP, TRACK_COLOR, WIDTH, true);
        let progress = arc(CX, CY, R, START, angle(v), &display.color, WIDTH, true);
        format!("{track}{progress}")
    };
    let ticks: String = [display.marks.watch, display.marks.risk, display.marks.critical]
        .iter()
        .map(|m| radial_tick(CX, CY, R, angle(*m)))
        .collect();
    let number = tile::text_line(55.0, 22.0, true, TEXT_COLOR, &display.number_text);
    let label = tile::text_line(92.0, 11.0, true, MUTED_TEXT_COLOR, display.label);
    svg(&format!("{rings}{ticks}{number}{label}"))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test styles::`
Expected: all styles tests pass (donut: 6).

- [ ] **Step 5: Commit**

```bash
git add src/styles/
git commit -m "feat: add Open and Tracked donut gauge styles"
```

---

### Task 5: Thin ring, `build_styled_icon`, and style-aware hub view

**Files:**
- Create: `src/styles/ring.rs`
- Modify: `src/styles/mod.rs` (`pub mod ring;`, `build_styled_icon`), `src/hub.rs` (`View::Gauge.style`, dispatch), `src/action.rs` (view passes default style for now — Task 6 wires settings)

**Interfaces:**
- Consumes: `style::GaugeStyle`, the renderers from Tasks 2–4.
- Produces:
  ```rust
  pub fn render(display: &UsageDisplay) -> String;                 // styles/ring.rs
  pub fn build_styled_icon(display: &UsageDisplay, style: GaugeStyle) -> String; // styles/mod.rs, data URI
  View::Gauge { window: WindowKind, colors: ColorSettings, style: GaugeStyle }
  ```

- [ ] **Step 1: Failing ring tests**

Create `src/styles/ring.rs`:

```rust
//! Thin ring: a full-circle track filled clockwise from the top, with the
//! number inside and the window name underneath.

use crate::format::UsageDisplay;
use crate::styles::{TRACK_COLOR, arc, radial_tick, svg};
use crate::tile::{self, MUTED_TEXT_COLOR, TEXT_COLOR};

const CX: f64 = 50.0;
const CY: f64 = 47.0;
const R: f64 = 30.0;
const WIDTH: f64 = 4.0;
const TOP: f64 = -90.0;

fn angle(percent: f64) -> f64 {
    TOP + 360.0 * percent / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::bar::tests::display;

    #[test]
    fn ring_draws_track_progress_number_and_label() {
        let s = render(&display(42.0));
        assert!(s.contains(&format!(r#"<circle cx="50" cy="47" r="30" fill="none" stroke="{TRACK_COLOR}""#)), "got: {s}");
        assert!(s.contains(r##"stroke="#d97757""##));
        assert!(s.contains(">42</text>") && s.contains(">SESSION</text>"));
    }

    #[test]
    fn ring_ticks_at_default_marks() {
        let s = render(&display(42.0));
        // watch 50 -> 90deg (straight down): tick from (50,70) to (50,84)
        assert!(s.contains(r#"x1="50.00" y1="70.00" x2="50.00" y2="84.00""#), "got: {s}");
    }

    #[test]
    fn zero_draws_no_fill_and_full_is_a_circle() {
        assert!(!render(&display(0.0)).contains(r##"stroke="#d97757""##));
        assert!(render(&display(100.0)).contains(r##"r="30" fill="none" stroke="#d97757""##));
    }
}
```

Add `pub mod ring;` to `src/styles/mod.rs`.

- [ ] **Step 2: Failing dispatcher + hub tests**

Add to `src/styles/mod.rs` tests:

```rust
    #[test]
    fn every_style_builds_a_data_uri() {
        use crate::style::ALL_STYLES;
        let d = crate::styles::bar::tests::display(42.0);
        for style in ALL_STYLES {
            assert!(build_styled_icon(&d, style).starts_with("data:image/svg+xml;base64,"), "{style:?}");
        }
    }

    #[test]
    fn styles_render_differently() {
        use crate::style::GaugeStyle;
        let d = crate::styles::bar::tests::display(42.0);
        assert_ne!(build_styled_icon(&d, GaugeStyle::Bar), build_styled_icon(&d, GaugeStyle::ThinRing));
    }
```

In `src/hub.rs` tests, change `gauge()` to include `style: GaugeStyle::Speedometer,` and add:

```rust
    #[test]
    fn every_gauge_style_is_an_image_on_a_keypad_and_unchanged_on_a_dial() {
        let dial = output_for(&gauge(), Some(&snapshot()), false, now());
        for style in crate::style::ALL_STYLES {
            let view = View::Gauge { window: WindowKind::Session, colors: ColorSettings::default(), style };
            assert!(matches!(output_for(&view, Some(&snapshot()), true, now()), Output::Image(_)));
            assert_eq!(output_for(&view, Some(&snapshot()), false, now()), dial);
        }
    }
```

(and `use crate::style::GaugeStyle;` in the hub tests module).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test`
Expected: compile errors — `ring::render`, `build_styled_icon`, `View::Gauge.style` missing.

- [ ] **Step 4: Implement**

`src/styles/ring.rs`, above `#[cfg(test)]`:

```rust
pub fn render(display: &UsageDisplay) -> String {
    let track = format!(
        r#"<circle cx="{CX}" cy="{CY}" r="{R}" fill="none" stroke="{TRACK_COLOR}" stroke-width="{WIDTH}" />"#
    );
    let progress = arc(CX, CY, R, TOP, angle(display.bar_value), &display.color, WIDTH, true);
    let ticks: String = [display.marks.watch, display.marks.risk, display.marks.critical]
        .iter()
        .map(|m| radial_tick(CX, CY, R, angle(*m)))
        .collect();
    let number = tile::text_line(56.0, 24.0, true, TEXT_COLOR, &display.number_text);
    let label = tile::text_line(92.0, 11.0, true, MUTED_TEXT_COLOR, display.label);
    svg(&format!("{track}{progress}{ticks}{number}{label}"))
}
```

Note: `arc` emits a full circle as `<circle cx="50" cy="47" r="30" fill="none" stroke="#d97757" …>`, which the 100% test matches.

`src/styles/speedometer.rs`: rename `fn render_svg` to `pub(super) fn render` and delete `pub fn build_icon` (the hub now goes through `build_styled_icon`). Its existing tests call `build_icon(&d)`; keep them unchanged by adding this helper at the top of its `tests` module:

```rust
    fn build_icon(display: &UsageDisplay) -> String {
        crate::tile::data_uri(&render(display))
    }
```

Then add to `mod.rs`:

```rust
pub mod bar;
pub mod donut;
pub mod ring;
pub mod speedometer;

use crate::format::UsageDisplay;
use crate::style::GaugeStyle;

/// The keypad image for a Usage Gauge in `style`, as the data URI
/// OpenDeck's `setImage` expects.
pub fn build_styled_icon(display: &UsageDisplay, style: GaugeStyle) -> String {
    let svg = match style {
        GaugeStyle::Speedometer => speedometer::render(display),
        GaugeStyle::Bar => bar::render(display, false),
        GaugeStyle::SoftPill => bar::render(display, true),
        GaugeStyle::OpenDonut => donut::render(display, false),
        GaugeStyle::TrackedDonut => donut::render(display, true),
        GaugeStyle::ThinRing => ring::render(display),
    };
    tile::data_uri(&svg)
}
```

`src/hub.rs`:
- `use crate::style::GaugeStyle;` and replace `use crate::styles::speedometer::build_icon;` with `use crate::styles::build_styled_icon;`.
- `View::Gauge` gains `style: GaugeStyle,`.
- In `output_for`: `View::Gauge { window, colors, style } => { … if keypad { Output::Image(build_styled_icon(&display, *style)) } … }`.

`src/action.rs` `view()`: add `style: crate::style::GaugeStyle::default(),` (Task 6 replaces with the settings value).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass; clippy clean (all helpers now used); fmt clean after `cargo fmt`.

- [ ] **Step 6: Commit**

```bash
git add -A src/
git commit -m "feat: add Thin ring style and render keypad gauges per style"
```

---

### Task 6: Style settings and press handling on Usage Gauge

**Files:**
- Modify: `src/action.rs`

**Interfaces:**
- Consumes: `style::{StyleSettings, GaugeStyle, next_style, classify_press, Press}`, `hub::{UsageHub, View}`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, Default)]
  pub struct UsageGaugeSettings { window, #[serde(flatten)] colors: ColorSettings, #[serde(flatten)] styles: StyleSettings }
  impl UsageGaugeSettings { fn cycled(&self) -> Option<UsageGaugeSettings> }
  // UsageGaugeAction gains pressed_at: Arc<DashMap<String, Instant>>, key_down, reworked key_up
  ```

- [ ] **Step 1: Failing tests**

Add to `src/action.rs` tests:

```rust
    use crate::style::{ALL_STYLES, GaugeStyle, StyleSettings};

    #[test]
    fn v070_settings_load_as_speedometer_with_all_styles() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r##"{"window":"weekly","watch":40,"risk":60,"critical":80,"colorNormal":"#112233"}"##,
        )
        .unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.colors.marks.watch, 40.0);
        assert_eq!(s.colors.palette.normal, "#112233");
        assert_eq!(s.styles, StyleSettings::default());
        assert_eq!(s.styles.cycle, ALL_STYLES.to_vec());
    }

    #[test]
    fn view_carries_the_style() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"style":"thinRing"}"#).unwrap();
        assert!(matches!(s.view(), View::Gauge { style: GaugeStyle::ThinRing, .. }));
    }

    #[test]
    fn cycled_moves_to_the_next_style_and_keeps_everything_else() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"weekly","watch":40,"style":"bar","cycleStyles":["bar","openDonut"]}"#,
        )
        .unwrap();
        let next = s.cycled().unwrap();
        assert_eq!(next.styles.style, GaugeStyle::OpenDonut);
        assert_eq!(next.window, WindowKind::Weekly);
        assert_eq!(next.colors, s.colors);
        assert_eq!(next.styles.cycle, s.styles.cycle);
    }

    #[test]
    fn cycled_is_none_with_one_style() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"cycleStyles":["bar"]}"#).unwrap();
        assert!(s.cycled().is_none());
    }

    #[test]
    fn full_settings_round_trip() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"monthly","critical":95,"style":"softPill","cycleStyles":["softPill","thinRing"]}"#,
        )
        .unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["window"], "monthly");
        assert_eq!(v["style"], "softPill");
        assert_eq!(v["critical"], 95.0);
        let back: UsageGaugeSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back.styles, s.styles);
        assert_eq!(back.colors, s.colors);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test action::`
Expected: compile errors — no field `styles`, no `cycled`.

- [ ] **Step 3: Implement**

In `src/action.rs`:

```rust
use crate::style::{Press, StyleSettings, classify_press, next_style};
use dashmap::DashMap;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
    /// Flattened so the Property Inspector writes plain top-level fields
    /// (`watch`, `colorNormal`, ...). Its lenient wire format means a bad
    /// color can't make openaction reset `window` too.
    #[serde(flatten)]
    pub colors: ColorSettings,
    /// `style` + `cycleStyles`, lenient in the same way.
    #[serde(flatten)]
    pub styles: StyleSettings,
}

impl UsageGaugeSettings {
    fn view(&self) -> View {
        View::Gauge {
            window: self.window,
            colors: self.colors.clone(),
            style: self.styles.style,
        }
    }

    /// These settings with the next ticked style, or `None` when fewer than
    /// two styles are ticked.
    fn cycled(&self) -> Option<UsageGaugeSettings> {
        let next = next_style(self.styles.style, &self.styles.cycle)?;
        let mut updated = self.clone();
        updated.styles.style = next;
        Some(updated)
    }
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    hub: Arc<UsageHub>,
    /// When each key went down, so `key_up` can tell a short press (cycle
    /// style) from a long one (refresh).
    pressed_at: Arc<DashMap<String, Instant>>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            pressed_at: Arc::new(DashMap::new()),
        }
    }

    /// Switches to the next ticked style: persists it (so it survives an
    /// OpenDeck restart), re-tracks the view for the poll loop, and redraws
    /// from the cached snapshot - instant, no API call.
    async fn cycle_style(&self, instance: &Instance, settings: &UsageGaugeSettings) -> OpenActionResult<()> {
        let Some(updated) = settings.cycled() else {
            return Ok(());
        };
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist gauge style: {e}");
        }
        let view = updated.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }
}
```

In `impl Action for UsageGaugeAction`, add `key_down` and replace `key_up`:

```rust
    async fn key_down(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.pressed_at.insert(instance.instance_id.clone(), Instant::now());
        Ok(())
    }

    /// Short press cycles the ticked styles; a long press (>= 500 ms)
    /// forces a refresh, which is what a tap did before styles existed.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let held = self
            .pressed_at
            .remove(&instance.instance_id)
            .map(|(_, down)| down.elapsed());
        match classify_press(held) {
            Press::Long => self.hub.refresh_one(instance, &settings.view()).await,
            Press::Short => self.cycle_style(instance, settings).await,
        }
    }
```

Also in `will_disappear`, add `self.pressed_at.remove(&instance.instance_id);` before `untrack`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: all pass, clean.

- [ ] **Step 5: Commit**

```bash
git add src/action.rs
git commit -m "feat: short press cycles gauge styles, long press refreshes"
```

---

### Task 7: Property Inspector checklist and README

**Files:**
- Modify: `assets/propertyInspector/index.html`, `README.md`, `src/style.rs` (drift test)

- [ ] **Step 1: Failing drift test**

Add to `src/style.rs` tests:

```rust
    /// The Property Inspector's checkboxes must offer exactly the wire
    /// names the plugin accepts.
    #[test]
    fn property_inspector_lists_every_style() {
        let html = include_str!("../assets/propertyInspector/index.html");
        for style in ALL_STYLES {
            let name = serde_json::to_value(style).unwrap();
            let needle = format!(r#"value="{}""#, name.as_str().unwrap());
            assert!(html.contains(&needle), "index.html is missing {needle}");
        }
    }
```

Run: `cargo test style::tests::property_inspector_lists_every_style` — Expected: FAIL (`index.html is missing value="speedometer"`).

- [ ] **Step 2: Add the checklist to `index.html`**

In `<style>` add:

```css
		.check { display: flex; align-items: center; gap: 6px; margin-top: 4px; font-size: 12px; opacity: 1; }
		.check input { width: auto; margin: 0; }
```

After the `window` `<select>` (before `<div id="colors">`), add:

```html
	<label>Cycle styles</label>
	<div id="cycleStyles">
		<label class="check"><input type="checkbox" value="speedometer" /> Speedometer</label>
		<label class="check"><input type="checkbox" value="bar" /> Bar</label>
		<label class="check"><input type="checkbox" value="softPill" /> Soft pill</label>
		<label class="check"><input type="checkbox" value="openDonut" /> Open donut</label>
		<label class="check"><input type="checkbox" value="trackedDonut" /> Tracked donut</label>
		<label class="check"><input type="checkbox" value="thinRing" /> Thin ring</label>
	</div>
	<p class="hint">Short press cycles the checked styles (need at least two). Hold to refresh.</p>
```

In the inline script, after `let uuid;` add `let storedStyle;` and:

```js
		const styleBoxes = () => [...document.querySelectorAll("#cycleStyles input")];
		styleBoxes().forEach((box) => box.addEventListener("change", sendSettings));
```

In `applySettings(settings)` add:

```js
			storedStyle = settings.style;
			const cycle = Array.isArray(settings.cycleStyles) && settings.cycleStyles.length
				? settings.cycleStyles
				: styleBoxes().map((box) => box.value);
			styleBoxes().forEach((box) => { box.checked = cycle.includes(box.value); });
```

In `sendSettings`, replace the payload with:

```js
				payload: {
					window: document.getElementById("window").value,
					...readColorSettings(),
					// The current style changes only by pressing the key; pass
					// it through untouched so saving here doesn't reset it.
					...(storedStyle ? { style: storedStyle } : {}),
					cycleStyles: styleBoxes().filter((box) => box.checked).map((box) => box.value),
				},
```

Run: `cargo test` — Expected: all pass.

- [ ] **Step 3: Browser sanity check**

Serve `assets/propertyInspector/` locally (`python3 -m http.server 8765 --bind 127.0.0.1` from that directory, in the background), open `http://127.0.0.1:8765/index.html` in the browser pane, and in the page run: `applySettings({cycleStyles: ["bar","thinRing"], style: "bar"}); JSON.stringify(styleBoxes().filter(b=>b.checked).map(b=>b.value))` — Expected: `["bar","thinRing"]`. Then `applySettings({}); styleBoxes().every(b=>b.checked)` — Expected: `true`. Stop the server by its task id (not `pkill -f`).

- [ ] **Step 4: README**

- Replace step 3 of "Using a dial or tile" with:
  `3. It updates automatically roughly every 20 seconds. On a dial, press for an immediate refresh. On a keypad tile, a short press switches to the next style (see below) and holding for half a second refreshes.`
- Add after "Colors & thresholds":

```markdown
## Styles (keypad)

A Usage Gauge key can be drawn as a **Speedometer**, **Bar**, **Soft pill**,
**Open donut**, **Tracked donut**, or **Thin ring**; every style except the
speedometer shows your Watch/Risk/Critical marks as tick marks. Tick the
styles you want under **Cycle styles**; a short press moves to the next
ticked one (you need at least two) and the key remembers its style across
restarts. Hold the key for half a second to refresh instead. Dials keep the
touch-strip bar.
```

- Add smoke-checklist items:

```markdown
- [ ] Each of the six styles renders on a keypad tile with tick marks at
      the key's marks. *(not yet verified)*
- [ ] A short press cycles only the ticked styles and the chosen style
      survives an OpenDeck restart. *(not yet verified)*
- [ ] Holding a key ~0.5s refreshes it without changing its style.
      *(not yet verified)*
- [ ] With fewer than two styles ticked, a short press does nothing.
      *(not yet verified)*
```

- [ ] **Step 5: Verify and commit**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check && node -e "require('fs').readFileSync('assets/propertyInspector/index.html','utf8')"`
Expected: all pass.

```bash
git add assets/propertyInspector/index.html README.md src/style.rs
git commit -m "feat: add cycle-styles checklist to Usage Gauge settings and document styles"
```
