# Usage Heatmap — Design

Sub-project **4a** of the "new tiles and dials" effort (roadmap in
`2026-09-27-thresholds-pace-design.md`). Sub-project 4 was split in two:
**4a** is this Heatmap, which uses only local logs and needs no new storage.
**4b** is the Sparkline together with a persisted %-of-limit history, and
gets its own spec. 4a builds on the press gesture, `tile::text_at` and the
`styles` helpers from sub-projects 2–3, and on Metric Tile's
`LogUsageSource`.

## Goal

A new **Usage Heatmap** action, on keys and dials, that shows daily Claude
Code usage (Tokens or estimated Cost, chosen per key) as heat cells:

- **7 days:** seven tall cells, today on the right.
- **4 weeks:** a 4×7 grid of the last 28 days.

Each cell's shade is that day's usage relative to the busiest day shown.
A short press flips between the two views and the choice is remembered; a
long press refreshes. On a dial, the same chart is drawn on the touch strip.

Non-goals: %-of-limit history and sparklines (4b); per-project breakdown;
a heatmap of Monthly or extra-usage spend; Cursor-specific ideas.

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| What a cell measures | Tokens or Cost from local logs, per key (same numbers as Metric Tile) |
| Shading | Relative to the busiest day in the view; a zero day is track grey |
| Views | 7 days ↔ 4 weeks, flipped by a short press (persisted); long press refreshes |
| Days | Local calendar days (midnight to midnight), today last |
| Dials | Yes: the chart is sent as an SVG image to a full-strip `pixmap` layout item |
| Color | One per-key color picker, default Claude copper `#d97757` |
| Data refresh | Own 60-second loop over the shared `LogUsageSource` (not the usage-API hub) |

## Components

### `src/heatmap.rs` — model and aggregation (pure)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeatmapView { #[default] SevenDays, FourWeeks }
impl HeatmapView {
    pub fn flipped(self) -> Self;
    pub fn days(self) -> usize;               // 7 or 28
    pub fn caption(self) -> &'static str;     // "7 DAYS" | "4 WEEKS"
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "HeatmapSettingsWire", into = "HeatmapSettingsWire")]
pub struct HeatmapSettings { pub metric: MetricKind, pub view: HeatmapView, pub color: String }
// Default: Tokens, SevenDays, "#d97757"

/// Per-day totals for the `days` local calendar days ending today,
/// oldest first.
pub fn daily_totals<Tz: TimeZone>(entries: &[LogEntry], metric: MetricKind, now: DateTime<Tz>, days: usize) -> Vec<f64>;

pub struct HeatmapDisplay {
    pub caption: String,              // "7 DAYS · 1.2M", "4 WEEKS · $38.20", or "no data"
    pub cells: Vec<Option<f64>>,      // oldest first; Some(0..=1 shade ratio), None = zero day
    pub weekday_letters: Vec<char>,   // last 7 days' initials, oldest first (M T W T F S S)
    pub color: String,
}
pub fn build_heatmap<Tz: TimeZone>(entries: &[LogEntry], settings: &HeatmapSettings, now: DateTime<Tz>) -> HeatmapDisplay;
```

- **Wire format.** The keys are `metric` (`"tokens"` | `"cost"`), `view`
  (`"sevenDays"` | `"fourWeeks"`) and `color` (`#rrggbb`). Each is a raw
  `Value` and falls back to its own default: an unknown metric or view
  gives the default, and a non-hex color gives `#d97757`. This is the same
  lenient pattern as `level::ColorSettingsWire`.
- **`daily_totals`.** A local day starts at local midnight in `Tz`. An
  entry counts toward the day of its timestamp converted to `Tz`. Entries
  outside the window are ignored.
  - Tokens use `LogEntry::total_tokens()`.
  - Cost uses `pricing::cost_for_entry`, the same as Metric Tile.
  - Production passes `Local::now()`; tests pass a `FixedOffset`.
- **`build_heatmap`.**
  - `max` is the largest day in the view.
  - A cell is `None` when its day is 0. Otherwise it is `Some(v / max)`.
  - The caption total is formatted with `metric::format_tokens` or
    `metric::format_cost`.
  - When `entries` is empty (no logs at all), the caption is `"no data"`
    and every cell is `None`.

### `src/styles/heatmap.rs` — renderers

```rust
pub fn render_key(display: &HeatmapDisplay) -> String;    // 100×100 bare SVG
pub fn render_strip(display: &HeatmapDisplay) -> String;  // 200×100 bare SVG
```

A cell with `Some(r)` is drawn as `fill="{color}" fill-opacity="{0.25 +
0.75·r:.2}"`. A `None` cell is drawn in `TRACK_COLOR`. All cells have
`rx 2`.

**Key (100×100, on `tile::card()`).**

- **Caption:** at `text_line(14, 11, bold, MUTED_TEXT_COLOR)`.
- **Columns:** `x_i = 11 + 11.5·i` for i in 0..7, cell width 9.
- **7 days:** one row of cells, y 22, height 56.
- **4 weeks:** rows `r` in 0..4 at `y = 22 + 14·r`, height 12. The oldest
  week is on top, and day `k` (0-based, oldest first) sits at row `k / 7`,
  column `k % 7`.
- **Weekday letters:** at `text_at(x_i + 4.5, 92, Middle, 10, regular,
  MUTED_TEXT_COLOR)`, under each column, in both views.

**Strip (200×100).**

- **Background:** a card rect of 200×100 filled with `CARD_COLOR`.
- **Caption:** at `text_at(10, 16, Start, 14, bold, TEXT_COLOR)`.
- **Columns:** `x_i = 12 + 26.3·i`, cell width 20.
- **7 days:** one row of cells, y 26, height 64.
- **4 weeks:** four rows at `y = 26 + 16.5·r`, height 14.
- There are no weekday letters on the strip.

The strip SVG uses its own `<svg viewBox="0 0 200 100">` wrapper, because
`styles::svg` is for the 100×100 key.

### Dial layout — `assets/layouts/chart.json`

```json
{ "$schema": "https://schemas.elgato.com/streamdeck/plugins/layout.json",
  "id": "com.jfms7s.claudeusage.chart-layout",
  "items": [ { "key": "chart", "type": "pixmap", "rect": [0, 0, 200, 100], "value": "" } ] }
```

The feedback is `{"chart": tile::data_uri(&render_strip(..))}`. The layout
is shared with 4b's Sparkline.

**Unverified.** That OpenDeck renders an SVG data URI in a `pixmap` item.
The binary bundles resvg and has a pixmap component, but this hasn't been
tried on hardware; the README smoke list covers it.

### `src/heatmap_action.rs` — the action

- **Identity:** UUID `com.jfms7s.claudeusage.heatmap`, name **Usage
  Heatmap**, controllers `Encoder` + `Keypad`.
- **Assets:** `Encoder.layout = "layouts/chart.json"`, PI
  `propertyInspector/heatmap.html`, manifest `Actions[5]`.
- **State:** `Arc<LogUsageSource>`, shared with Metric Tile.
  `MetricTileAction::new` changes to take an `Arc<LogUsageSource>`, and
  `main.rs` builds one and hands clones to both actions so they share the
  mtime cache. The action also holds a `DashMap<String, HeatmapSettings>`
  registry and a `PressTimer`.
- **`render(instance, settings)`.** Reads `entries()`, calls
  `build_heatmap(&entries, settings, Local::now())`, then:
  - **Keypad:** clears the title and sets the image to
    `data_uri(render_key)`.
  - **Dial:** `set_feedback({"chart": data_uri(render_strip)})`.
- **Tick loop.** Every 60 s, re-renders every tracked instance. It's
  spawned from `main.rs`.
- **Lifecycle:**
  - `will_appear` / `did_receive_settings`: track, then render.
  - `will_disappear`: forget the press, then untrack.
  - `dial_down` / `dial_up`: the same press gesture as the key (amended
    after final review: a dial that only refreshed could never reach the
    4-week view).
- **Presses:**
  - `key_down`: `timer.down`.
  - `key_up`: `timer.up`.
    - Long: render.
    - Short: `set_settings(&flipped)`. A failure is logged with `warn!`.
      Then track and render the flipped settings.

### Property Inspector — `assets/propertyInspector/heatmap.html`

- **Metric:** a Tokens/Cost select.
- **Color:** an `<input type="color">`.
- **Hint:** "Short press flips between 7 days and 4 weeks. Hold to
  refresh."
- The stored `view` is passed through untouched, the same way `style` is on
  Usage Gauge.

## Behavior change for existing users

None: this is a new action. Metric Tile behaves identically; the only
change is internal, sharing its log cache.

## Error handling

- **No log directory or no entries:** caption "no data", all cells grey.
  This is never an error on the key.
- **Unknown model in Cost mode:** contributes 0, as Metric Tile does today.
- **Bad settings values:** fall back per field.
- **`set_settings` failure:** logged; the flip still shows.

## Testing

- **`heatmap.rs`:**
  - `daily_totals` buckets by local day. With a `FixedOffset` of +2h, an
    entry at 23:30 UTC lands on the next local day.
  - Entries older than the window are dropped, and today is the last
    element.
  - Tokens and Cost both sum correctly.
  - `build_heatmap`:
    - A shade ratio of 1.0 marks the busiest day.
    - A zero day gives `None`.
    - Empty entries give "no data".
    - The caption total and format match (`7 DAYS · 1.2M`).
    - Weekday letters match a known date.
  - Wire:
    - `{}` gives the defaults.
    - Garbage in each field falls back on its own.
    - Settings round-trip.
    - `flipped` works.
- **`styles/heatmap.rs`:**
  - The key has 7 cells (7 days) or 28 cells (4 weeks) at exact
    coordinates.
  - `fill-opacity` reads 1.00 for the busiest day and 0.25 at the minimum
    ratio.
  - `None` cells use `TRACK_COLOR`.
  - The 7 weekday letters are present.
  - The strip uses a 200×100 viewBox, with 7 or 28 cells and the caption.
- **Action:**
  - The manifest `Actions[5]` UUID, layout and PI path match.
  - The feedback key `chart` exists in `chart.json`.
  - The PI includes the metric select and passes `view` through.
- **Metric Tile:** its tests still pass with an `Arc<LogUsageSource>`.
- **README:** a "Using Usage Heatmap" section, plus smoke items: both
  views on a key, the flip persists, and the chart shows on a dial strip
  (this verifies SVG in a pixmap).
