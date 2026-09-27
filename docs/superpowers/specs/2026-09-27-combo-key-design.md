# Session + Weekly Combo Key — Design

Sub-project **3 of 4** in the "new tiles and dials" effort (roadmap in
`2026-09-27-thresholds-pace-design.md`). Builds on sub-project 1's marks,
palette, pace mode and `UsageHub` (v0.7.0), and on sub-project 2's press
gesture and `src/styles/` renderers (v0.8.0).

## Goal

Add a new **Session + Weekly** action that shows both the 5-hour session
and the 7-day weekly window on one key or dial.

- **Keypad:** two layouts. **Horizontal** is two stacked rows with bars and
  reset times. **Vertical** is two tall bars side by side. A short press
  flips between them and the choice is remembered; a long press refreshes.
- **Dial:** a touch-strip layout with the two windows as stacked bars, each
  showing its % and reset time. A dial press refreshes.
- **Colors:** one shared set of marks, palette and color mode for both bars.
  Each bar gets its own level color from its own usage, including pace.

Non-goals: Monthly on this key; tick marks on the touch strip (the native
strip layout can't draw them, same trade-off as the gauge's strip); separate
marks per window; Cursor-specific ideas.

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| Shape | New action, keypad + dial |
| Dial | Yes: two stacked native bars |
| Marks/colors | One shared set; each bar colored by its own level |
| Keypad press | Short = flip horizontal ↔ vertical (persisted); long (≥ 500 ms) = refresh |
| Dial press | Refresh |
| Structure | New `View::Combo` in the existing hub; press timing extracted into a shared `PressTimer` |

## Components

### `src/press.rs` — shared press timing

`LONG_PRESS`, `Press` and `classify_press` move here from `style.rs`
unchanged (their tests move with them). The file adds:

```rust
#[derive(Default)]
pub struct PressTimer { /* DashMap<String, Instant> */ }
impl PressTimer {
    pub fn down(&self, instance_id: &str);          // records now
    pub fn up(&self, instance_id: &str) -> Press;   // removes + classifies (None -> Short)
    pub fn forget(&self, instance_id: &str);        // on will_disappear
}
```

`UsageGaugeAction` swaps its `pressed_at` map for an `Arc<PressTimer>`, with
no behavior change.

### `src/combo.rs` — layout model (pure)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComboLayout { #[default] Horizontal, Vertical }
impl ComboLayout { pub fn flipped(self) -> Self; }

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "LayoutSettingsWire", into = "LayoutSettingsWire")]
pub struct LayoutSettings { pub layout: ComboLayout }
```

The wire key is `layout`. It's a raw `Value`, so an unknown or wrong-typed
value falls back to Horizontal and never makes openaction reset the whole
settings struct (the same pattern as `ColorSettingsWire`).

### `src/tile.rs` — anchored text

The file adds an `Anchor { Start, Middle, End }` enum and
`pub fn text_at(x, y, anchor, size, bold, color, content) -> String`.

Squeezing uses the room available from `x` for that anchor, with a 3-unit
margin:

| Anchor | Available width |
|---|---|
| Start | `97 − x` |
| End | `x − 3` |
| Middle | `2·min(x, 100 − x) − 6` |

`text_line(y, …)` becomes `text_at(50, y, Middle, …)`. At x = 50 that
gives exactly 94, today's `MAX_TEXT_WIDTH`, so every existing key renders
byte-identically.

### `src/styles/combo.rs` — keypad renderer

```rust
pub fn render(session: &UsageDisplay, weekly: &UsageDisplay, layout: ComboLayout) -> String; // bare SVG
```

Each `UsageDisplay` comes from `build_display(snapshot, Session|Weekly,
colors, now)`, so each carries its own level color, `bar_value`, marks,
`percent_text` and `detail_text`. With no snapshot, both come from
`error_display()` (grey, "—").

**Horizontal.** Rows start at `y0` = 8 (Session) and 54 (Weekly), in the
100×100 viewBox. For each row:

- **Name:** "Session" or "Weekly", at `text_at(8, y0+12, Start, 11, bold, TEXT_COLOR)`.
- **Percent:** at `text_at(92, y0+12, End, 13, bold, level color)`.
- **Track:** x 8, y `y0+17`, w 84, h 6, rx 3, `TRACK_COLOR`.
- **Fill:** only when `v > 0`. Width `max(84·v/100, 6)`, rx 3, level color.
- **Ticks:** at `x = 8 + 84·mark/100`, from `y0+15` to `y0+25`, stroke
  `TEXT_COLOR` 1.
- **Detail:** `detail_text` ("resets in 6h 12m" / "no reset info" /
  "no data") at `text_at(8, y0+35, Start, 10, regular, MUTED_TEXT_COLOR)`.

**Vertical.** Two columns centered at `cx` = 32 (Session) and 68 (Weekly).
For each column:

- **Percent:** at `text_at(cx, 17, Middle, 13, bold, level color)`.
- **Track:** x `cx−10`, y 23, w 20, h 58, rx 4, `TRACK_COLOR`.
- **Fill:** only when `v > 0`. Height `max(58·v/100, 8)`, anchored to the
  bottom (y = `81 − height`), rx 4, level color.
- **Ticks:** horizontal at `y = 81 − 58·mark/100`, from x `cx−13` to
  `cx+13`, stroke `TEXT_COLOR` 1.
- **Label:** "5h" or "7d" at `text_at(cx, 94, Middle, 12, regular, MUTED_TEXT_COLOR)`.

### Dial layout — `assets/layouts/combo.json`

The strip is 200×100 and uses native items. The labels are static in the
layout; the other four items get their values from feedback.

| Key | Type | Rect | Content |
|---|---|---|---|
| `s_label` | text | `[10, 4, 40, 22]` | "5h", left-aligned, bold |
| `s_value` | text | `[50, 4, 140, 22]` | right-aligned |
| `s_bar` | bar | `[10, 28, 180, 14]` | |
| `w_label` | text | `[10, 52, 40, 22]` | "7d", left-aligned, bold |
| `w_value` | text | `[50, 52, 140, 22]` | right-aligned |
| `w_bar` | bar | `[10, 76, 180, 14]` | |

The bar styling (background, border, subtype) matches `usage.json`.

`combo_feedback(session, weekly) -> Value` returns:

```
{ "s_value": "46% · 6h 12m",
  "s_bar": { "value": v, "bar_fill_c": color },
  "w_value": …,
  "w_bar": … }
```

The value text is `"{percent_text} · {tile_detail}"`, where `tile_detail`
is already "—" when there's no reset time. With no data, the value is "—"
and the bar is grey at value 0.

### Hub

The hub gains a third view:

```rust
View::Combo { colors: ColorSettings, layout: ComboLayout }
```

`output_for` handles it as follows:

- **Keypad:** `Image(tile::data_uri(&styles::combo::render(&s, &w, layout)))`.
- **Dial:** `Feedback(combo_feedback(&s, &w))`.

### `src/combo_action.rs` — the action

- UUID `com.jfms7s.claudeusage.combo`, name **Session + Weekly**,
  controllers `Encoder` + `Keypad`, `Encoder.layout = "layouts/combo.json"`,
  PI `propertyInspector/combo.html`, manifest `Actions[4]`.
- `ComboSettings { #[serde(flatten)] colors: ColorSettings, #[serde(flatten)] layout: LayoutSettings }`,
  deriving `Clone` and `Default`.
- `view()` returns `View::Combo`. `flipped()` returns the settings with the
  layout flipped and everything else unchanged.
- **will_appear / did_receive_settings:** track, then `render_cached`.
- **will_disappear:** `forget` the press, then untrack.
- **dial_up:** `refresh_one`.
- **key_down:** `timer.down`.
- **key_up:** `timer.up`.
  - Long → `refresh_one`.
  - Short → `set_settings(&flipped)`. A failure is logged with `warn!`. It
    then re-tracks and calls `render_cached`.

### Property Inspector — `assets/propertyInspector/combo.html`

The page has the shared colors section with the Fixed/Pace mode visible,
and the hint "Short press flips between horizontal and vertical. Hold to
refresh." There is no layout control. The stored `layout` is passed
through untouched when saving, the same way `index.html` passes `style`.

## Behavior change for existing users

None: this is a new action.

## Error handling

- **No snapshot or read failed:** both bars show the grey "—" state on
  keys and the strip.
- **A window without `resets_at`:** that row's detail reads "no reset
  info" (keypad) or "46% · —" (strip).
- **Bad `layout` value:** becomes Horizontal. `set_settings` failing is
  logged, and the flip still shows for the session.

## Testing

- `press.rs`: the moved classify tests still pass. `PressTimer` tests:
  - `up` without `down` returns Short.
  - `down` then an immediate `up` returns Short.
  - `forget` clears the entry.
  - Long presses are covered through `classify_press`.
- `combo.rs`:
  - `{}` gives Horizontal.
  - `"vertical"` parses.
  - Garbage falls back.
  - `flipped` round-trips.
- `tile.rs`:
  - `text_line` output is unchanged (the existing tests pass).
  - `text_at` with each anchor emits the right `text-anchor` and `x`.
  - End at x 92 squeezes content wider than 89.
- `styles/combo.rs`, both layouts:
  - Each bar is filled in its own display's color (two different colors
    appear).
  - Ticks sit at exact coordinates for the default marks.
  - 0% draws no fill, 100% draws a full fill, and `error_display` shows
    grey "—" twice.
  - A vertical fill is anchored at the bottom.
- `hub.rs`: `View::Combo` gives an `Image` on the keypad and `Feedback`
  with the four keys on a dial.
- `combo_action.rs`:
  - The manifest `Actions[4]` UUID, layout and PI path match.
  - The feedback keys exist in `combo.json`.
  - Settings `{}` give the defaults, a v0.8.0-style colors blob keeps its
    colors, and the full settings round-trip.
  - `flipped` keeps the colors.
- `action.rs` (gauge): existing tests pass after the `PressTimer` swap.
- README: a "Using Session + Weekly" section and smoke-checklist items
  (both layouts render, a short press flips and persists, a long press
  refreshes, the dial strip shows both bars).
