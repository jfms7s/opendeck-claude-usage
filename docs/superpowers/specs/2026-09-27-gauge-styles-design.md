# Gauge Styles + Tap-Cycle — Design

Sub-project **2 of 4** in the "new tiles and dials" effort (roadmap in
`2026-09-27-thresholds-pace-design.md`). Builds on sub-project 1's marks,
palette and `UsageHub` (shipped in v0.7.0).

## Goal

Give the keypad **Usage Gauge** six visual styles — **Speedometer**
(today's), **Bar**, **Soft pill**, **Open donut**, **Tracked donut**,
**Thin ring** — with the key's Watch/Risk/Critical marks drawn as tick
marks. A **short press cycles** through the styles ticked in a per-key
checklist; a **long press refreshes**.

Non-goals: dial/touch-strip styles (dials keep today's bar layout — styling
the strip fits better with sub-project 3); styles for Burn Rate (its tap
stays refresh); sparkline/heatmap views (sub-project 4 adds them to the
same cycle later); Cursor-specific ideas from the reference.

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| Press behavior | Short press = cycle checked styles; long press (≥ 500 ms) = refresh |
| Styles offered | Speedometer, Bar, Soft pill, Open donut, Tracked donut, Thin ring |
| Dials | Keypad only; dials unchanged |
| Tick marks | Yes, on every style (Speedometer already shows zones) |
| Current style | Persisted in the key's settings via `set_settings` |
| Defaults | Current = Speedometer (no visual change on upgrade); cycle = all six |
| < 2 styles ticked | Short press does nothing; long press still refreshes |
| Long-press detection | Decided on `key_up` from time since `key_down` (no timer) |
| Structure | One renderer module per style family under `src/styles/` |

## Components

### `src/style.rs` — style model (pure)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GaugeStyle { #[default] Speedometer, Bar, SoftPill, OpenDonut, TrackedDonut, ThinRing }

pub const ALL_STYLES: [GaugeStyle; 6];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "StyleSettingsWire", into = "StyleSettingsWire")]
pub struct StyleSettings { pub style: GaugeStyle, pub cycle: Vec<GaugeStyle> }
// Default: style Speedometer, cycle = ALL_STYLES

pub fn next_style(current: GaugeStyle, cycle: &[GaugeStyle]) -> Option<GaugeStyle>;

pub const LONG_PRESS: std::time::Duration; // 500 ms
pub enum Press { Short, Long }
pub fn classify_press(held: Option<std::time::Duration>) -> Press;
```

- **Wire format** (flattened into `UsageGaugeSettings`, next to the color
  fields): `"style": "speedometer"`, `"cycleStyles": ["speedometer",
  "bar", ...]`. Same lenient approach as `ColorSettingsWire`: fields are raw
  `Value`s, so a bad value never makes openaction reset the whole settings
  struct. An unknown or missing `style` becomes Speedometer. Unknown names
  in `cycleStyles` are dropped and duplicates are removed, keeping the first
  occurrence and the list order. A missing or non-array value becomes all
  six. An explicit array that ends up empty (every box unticked) stays
  empty, meaning a short press does nothing. (Amended after final review:
  falling back to all six there did the opposite of what unticking asks.)
- **`next_style`**: `None` when `cycle` has fewer than 2 entries. Otherwise,
  if `current` is in `cycle`, it returns the entry after it (wrapping); if
  not, it returns `cycle[0]`.
- **`classify_press`**: `Some(d)` with `d >= LONG_PRESS` → `Long`,
  anything else (including `None` — a `key_up` with no recorded
  `key_down`, e.g. after a plugin restart mid-press) → `Short`.

### `UsageDisplay` additions (`src/format.rs`)

- `label: &'static str` — `"SESSION"` / `"WEEKLY"` / `"MONTHLY"`.
- `number_text: String` — `percent_text` without the `%` sign (`"42"`,
  `"142"`, `"—"`), for the donut/ring centers.
- `error_display()` uses label `"USAGE"`.

`build_display` fills both. Everything else is unchanged.

### `src/styles/` — renderers

`src/styles/mod.rs` exposes one entry point and the shared SVG helpers:

```rust
pub fn build_styled_icon(display: &UsageDisplay, style: GaugeStyle) -> String; // data URI
```

- `speedometer.rs` — the current `icon.rs` rendering, moved unchanged
  (its tests move with it). `icon.rs` is removed; `hub.rs` calls
  `build_styled_icon`.
- `bar.rs` — **Bar** and **Soft pill** (one module, a `pill: bool` flag).
- `donut.rs` — **Open donut** and **Tracked donut** (a `tracked: bool` flag).
- `ring.rs` — **Thin ring**.
- `mod.rs` helpers: `polar(cx, cy, r, deg)` (degrees clockwise from +x in
  screen space) and `arc(cx, cy, r, start_deg, end_deg, color, width,
  round_caps)`, which sets `large-arc-flag` when the sweep exceeds 180°.
  Sweeps ≥ 360° are drawn as a `<circle>`.

All styles draw on `tile::card()` with text via `tile::text_line`. Track
color is `#374151` and tick color is `TEXT_COLOR`. The fill uses
`display.color` (the level color from sub-project 1). Coordinates are in the
100×100 viewBox:

| Style | Layout |
|---|---|
| Bar | label y 22 (12px bold muted); percent y 54 (28px bold, level color); track x 12, y 64, w 76, h 8; fill width `76·v/100`; ticks at `x = 12 + 76·mark/100` from y 61 to 75; `tile_detail` y 91 (13px muted) |
| Soft pill | as Bar but track y 63, h 12, `rx 6`; a non-zero fill is at least 4 wide (a visible dot), and its end radius is `min(6, width/2)` so its rounded ends never invert (was a 12-wide minimum; see KI-04); percent text in `TEXT_COLOR` (the pill carries the color) |
| Open donut | center (50, 47), r 28, stroke 9, round caps; 270° track from 135° to 405° (gap at the bottom); progress from 135° to `135 + 270·v/100`; `number_text` y 55 (22px bold); label y 92 (11px bold muted) |
| Tracked donut | the same 270° track split into 10 segments with 6° gaps (butt caps); segment `i` (0-based) is filled when `v > i·10`, so `ceil(v/10)` segments are lit and 0% lights none |
| Thin ring | center (50, 47), r 30, stroke 4; full-circle track; progress from −90° (top) clockwise by `360·v/100`, round caps; `number_text` y 56 (24px bold); label y 92 |

**Tick marks** on the donuts and ring are radial lines at each mark's angle,
from `r − 7` to `r + 7`, 1.5 wide. On Bar and Soft pill they're vertical
lines. A mark at 0 or 100 is still drawn: it sits at the end of the track.
`v` is `display.bar_value`, which is already clamped to 0..=100.

### Hub and action

- `View::Gauge` gains `style: GaugeStyle`; `output_for` renders keypad
  gauges via `build_styled_icon(&display, style)`. Dials are unchanged.
- `UsageGaugeSettings` gains `#[serde(flatten)] pub styles: StyleSettings`,
  and `view()` passes `styles.style`.
- `UsageGaugeAction` gains `pressed_at: Arc<DashMap<String, Instant>>`:
  - `key_down` records `Instant::now()` for the instance.
  - `key_up` removes the entry, classifies the press, then:
    - **Long** → `hub.refresh_one` (today's tap behavior).
    - **Short** → `next_style(...)`. On `Some(next)` it sets
      `settings.styles.style = next`, calls `instance.set_settings(&new
      settings)`, re-tracks the view, and calls `hub.render_cached`, so the
      change is instant with no API call. On `None` it does nothing.
  - `dial_up` is unchanged (refresh).
- `set_settings` persists the whole settings struct. The color fields
  round-trip through `ColorSettingsWire`, and the styles through
  `StyleSettingsWire`.

### Property Inspector (`index.html`)

A **Cycle styles** section with six checkboxes in `ALL_STYLES` order,
labelled Speedometer, Bar, Soft pill, Open donut, Tracked donut, Thin ring.
Underneath is the hint "Short press cycles the checked styles (need at
least two). Hold to refresh." It reads and writes `cycleStyles` and
preserves the stored `style` untouched when sending (`style` is changed
only by pressing the key).

## Behavior change for existing users

None visible on upgrade: keys load as Speedometer. The only change is that a
short press now cycles styles instead of refreshing — refresh moves to a
long press. Release notes and README must say so.

## Error handling

- Invalid `style` / `cycleStyles` values are sanitized per the wire rules,
  and never surface as errors.
- `set_settings` failing is logged with `warn!`, but the key is still
  re-rendered in the new style for this session.
- `key_up` without `key_down` counts as a short press.

## Testing

- `style.rs`: `next_style` wraps, skips unticked styles, returns `None`
  with 0 or 1 entries, and goes to `cycle[0]` when current isn't in it.
  `classify_press` at 499 ms is Short, at 500 ms is Long, and `None` is
  Short. Wire: `{}` gives the defaults; unknown/duplicate/empty
  `cycleStyles` and an unknown `style` are sanitized; round-trip works.
  v0.7.0 settings (window + colors, no style fields) load as Speedometer
  with all six styles.
- `styles/*`: each style produces valid SVG containing the level color,
  the label or number text, and ticks at the expected coordinates for the
  default marks (checked for one mark per style with exact coordinates).
  0% draws no progress fill/arc, and 100% draws a full one (Thin ring
  becomes a `<circle>`). The Tracked donut at 42% lights 5 segments.
  Custom marks move the ticks.
- `arc` helper: `large-arc-flag` is 1 only above 180°.
- `hub.rs`: `output_for` with each `GaugeStyle` gives an `Image` for keypad
  and unchanged `Feedback` for a dial.
- `action.rs`: the full settings (window + colors + styles) round-trip, and
  an old settings blob keeps its window and colors.
- README: new "Styles" section, updated tap instructions, smoke-checklist
  items (each style renders; short press cycles and persists across an
  OpenDeck restart; long press refreshes; < 2 ticked makes a short press do
  nothing).
