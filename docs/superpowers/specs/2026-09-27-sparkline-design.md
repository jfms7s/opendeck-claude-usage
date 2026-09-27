# Usage Sparkline + Recorded History — Design

Sub-project **4b**, the last one in the "new tiles and dials" effort (roadmap
in `2026-09-27-thresholds-pace-design.md`; 4a Heatmap shipped in v0.10.0).
This design builds on:

- the shared `UsageHub` and colors from sub-project 1;
- `PressTimer`, `tile::text_at` and the `styles` helpers from sub-projects
  2–3;
- the `layouts/chart.json` pixmap strip from 4a.

## Goal

1. **Record** the %-of-limit history that the usage API never gives back.
   Every successful poll appends the session and weekly percentages (plus
   their reset times) to a small file. Readings where nothing changed are
   skipped, and the file keeps the last 8 days.
2. Add a new **Usage Sparkline** action for keys and dials. It shows one
   window (Session or Weekly, chosen per key) as a trend line with a big
   headline number. A short press cycles through four series; a long press
   refreshes.

The four series:

| Series | Line | Headline |
|---|---|---|
| **Usage trend** | % of limit over the current window | current %, e.g. `42%` |
| **Between polls** | how much each reading added | last increase, e.g. `+2.1pp` |
| **Today's increment** | cumulative % added since local midnight | e.g. `8.4pp` |
| **Vs even burn** | even-burn ratio over the current window | current ratio, e.g. `1.2x` |

Non-goals: Monthly (it has no window); a per-key series checklist (all four
always cycle); history of tokens (the Heatmap covers that); syncing history
across machines; Cursor-specific ideas.

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| History source | Persist to a small file (user's choice over memory-only) |
| Series | Usage trend, Between polls, Today's increment, Vs even burn — all four, cycled |
| Shape | New action, separate from Usage Gauge |
| Dials | Yes — same chart as an SVG in the shared `layouts/chart.json` pixmap |
| Press | Short (key or dial) = next series (persisted); long = refresh |
| Line color | Current level color from the shared Colors & thresholds (pace mode honored) |

## Components

### `src/history.rs` — recorded readings

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: DateTime<Utc>,
    pub session: f64,
    pub session_resets_at: Option<DateTime<Utc>>,
    pub weekly: f64,
    pub weekly_resets_at: Option<DateTime<Utc>>,
}

pub const RETENTION: chrono::Duration; // 8 days

pub struct HistoryStore { /* Mutex<Vec<Reading>>, Option<PathBuf> */ }
impl HistoryStore {
    pub fn default_path() -> PathBuf;                         // $XDG_STATE_HOME or ~/.local/state, + opendeck-claude-usage/history.jsonl
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self>;
    pub fn in_memory() -> Arc<Self>;                          // tests; never touches disk
    pub fn record(&self, snapshot: &UsageSnapshot, now: DateTime<Utc>);
    pub fn readings(&self) -> Vec<Reading>;                   // oldest first
}
```

- **File format.** One JSON `Reading` per line (JSONL). It holds only
  percentages and timestamps, never tokens, credentials or account data.
- **`load`.** Reads the file if it exists. Lines that fail to parse are
  skipped, readings older than `now − RETENTION` are dropped, and the
  result is sorted by `at`. The pruned set is then rewritten to the file
  (write a temp file, then rename). A missing file or directory is not an
  error: the store simply starts empty.
- **`record`.** Builds a `Reading` from the snapshot. It is skipped if the
  session %, weekly % and both reset times all equal the last reading's,
  since unchanged polls add nothing to a trend. Otherwise it is pushed, the
  in-memory list is pruned to `RETENTION`, and one line is appended to the
  file (the directory is created on first write).
- **I/O failures.** A failed read, write or directory creation is logged
  with `warn!` only the first time it happens. The store keeps working in
  memory. A disk problem never breaks the keys.
- **Where it's called.** `UsageHub::read_and_cache` calls `record` on
  every successful read. The history then covers the ~20 s poll loop and
  manual refreshes, throttled by the existing once-a-minute API cache.

### `src/sparkline.rs` — series math (pure)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SparkSeries { #[default] Trend, BetweenPolls, Today, EvenBurn }
impl SparkSeries { pub fn next(self) -> Self; pub fn label(self) -> &'static str; } // "TREND" | "PER POLL" | "TODAY" | "VS EVEN"

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(from = "SparkSettingsWire", into = "SparkSettingsWire")]
pub struct SparkSettings { pub window: WindowKind, pub series: SparkSeries }

pub struct SparkDisplay {
    pub caption: String,         // "TREND · 5H", "TODAY · 7D"
    pub headline: String,        // "42%", "+2.1pp", "8.4pp", "1.2x", or "—"
    pub points: Vec<(f64, f64)>, // (x 0..=1 across the span, y value); empty = not enough history
    pub color: String,
}

pub fn build_sparkline<Tz: TimeZone>(readings: &[Reading], settings: &SparkSettings, colors: &ColorSettings, now: DateTime<Tz>) -> SparkDisplay;
```

- **Settings wire.** `window` is `"session"` or `"weekly"`; a stored
  `"monthly"` or garbage falls back to Session. `series` takes
  `"trend" | "betweenPolls" | "today" | "evenBurn"`, and garbage falls back
  to Trend. Both are raw `Value`s, the same lenient pattern as
  `ColorSettingsWire`.
- **Per-reading values.** For the chosen window, each reading contributes
  the pair `(pct, resets_at)`.
- **Current window.** Taken from the latest reading's `resets_at`: the
  readings with `at ≥ resets_at − window_length`. With no `resets_at`,
  every reading counts.
- **Trend.** The points are the current window's `(at, pct)` pairs. The
  headline is the latest pct, formatted with `format::format_percent`.
- **Between polls.** Uses consecutive readings in the current window.
  - A step's delta is `pct_i − pct_{i−1}`.
  - If `resets_at` changed between the two readings, the delta is `pct_i`
    instead: that's usage since the reset.
  - Negative deltas clamp to 0.
  - Points are `(at_i, delta)`, keeping only the last 30.
  - The headline is the last delta, e.g. `+2.1pp` (one decimal).
- **Today.** Starts from readings at or after local midnight of `now`'s
  date. The baseline is the last reading before midnight if there is one;
  otherwise the first reading today, which then counts as 0.
  - Positive deltas are summed cumulatively (a reset delta is `pct_i`), and
    points are `(at, running total)`.
  - The headline is the total, e.g. `8.4pp`.
- **Vs even burn.** For each reading in the current window, compute
  `pace(WindowUsage { percent, resets_at }, kind, at)`. Readings where
  `pace` returns `None` (the 10% too-early guard) are skipped.
  - Points are `(at, even_burn)`, and the headline is the latest ratio as
    `1.2x`.
- **X-axis scaling.** x is scaled from the first to the last point's `at`
  onto 0..=1.
- **Too little history.** With fewer than two points, `points` is empty
  and the headline is `—`; the renderer then shows "collecting…".
- **Color.** The level color of the latest reading for the window, using
  `colors.level(pct, projected)` where `projected` comes from `pace`. With
  no reading at all, it is `format::DISABLED_COLOR`.
- **Caption.** `"{label} · 5H"` for Session, `"{label} · 7D"` for Weekly.

### `src/styles/sparkline.rs` — renderers

`render_key(&SparkDisplay) -> String` draws a 100×100 SVG and
`render_strip(&SparkDisplay) -> String` draws a 200×100 one. The strip has
its own wrapper, the same way as the heatmap's.

The y-axis runs from `min(0, min y)` to `max(y) × 1.1`, and never spans
less than 1 (so a flat line doesn't divide by zero).

**Key**

- **Caption:** `text_line(14, 11, bold, MUTED)`.
- **Headline:** `text_line(46, 26, bold, color)`.
- **Line:** a polyline across x 8..92 and y 58..90, drawn with
  `stroke={color}`, width 2, round joins.
- **Fill:** a closed area under the line in the same color with
  `fill-opacity="0.2"`.
- **End dot:** `r 2.5` in the same color.
- **Not enough history:** instead of the line, `text_line(78, 11, regular,
  MUTED, "collecting…")`.

**Strip**

- **Caption:** at (10, 18), 14px bold `TEXT_COLOR`.
- **Headline:** right-aligned at (190, 20), 20px bold color.
- **Line:** across x 10..190 and y 34..92.
- **Not enough history:** "collecting…" at (100, 70), centered.

### Hub

- `UsageHub::new(source, history: Arc<HistoryStore>)`. `read_and_cache`
  calls `history.record(&snapshot, Utc::now())` on success.
- The hub gains a new view:

  ```rust
  View::Sparkline { settings: SparkSettings, colors: ColorSettings }
  ```

- `output_for` gains a `history: &[Reading]` parameter. Its callers pass
  `&self.history.readings()`.
- For `View::Sparkline`, `output_for` calls `build_sparkline(history, …,
  now.with_timezone(&Local))`, then:
  - **Keypad:** `Image(data_uri(render_key))`.
  - **Dial:** `Feedback({"chart": data_uri(render_strip)})`.
- `main.rs` does `HistoryStore::load(HistoryStore::default_path(),
  Utc::now())` and passes the result to `UsageHub::new`.

### `src/sparkline_action.rs` — the action

- **Identity:** UUID `com.jfms7s.claudeusage.sparkline`, name **Usage
  Sparkline**, controllers `Encoder` + `Keypad`.
- **Assets:** `Encoder.layout = "layouts/chart.json"`, PI
  `propertyInspector/sparkline.html`, manifest `Actions[6]`.
- **Settings:** `SparklineSettings { #[serde(flatten)] spark: SparkSettings,
  #[serde(flatten)] colors: ColorSettings }`. Its `cycled()` returns the
  settings with `series.next()`.
- **Presses:** the same gesture on keys and dials. `key_down` and
  `dial_down` record the press through `PressTimer`, and `key_up` and
  `dial_up` share one handler:
  - **Short:** `set_settings(&cycled)`. A failure is logged with `warn!`.
    It then re-tracks and calls `render_cached`.
  - **Long:** `refresh_one`.
- **Lifecycle:** as on Session + Weekly (track or untrack, and `forget`
  the press on `will_disappear`).

### Property Inspector — `assets/propertyInspector/sparkline.html`

- A Window select (Session / Weekly).
- The shared colors section, with the Fixed/Pace mode shown.
- The hint: "Short press (key or dial) cycles Trend → Per poll → Today →
  Vs even. Hold to refresh. History builds up as the plugin runs."
- The stored `series` is passed through untouched.

## Behavior change for existing users

- **New file on disk:**
  `~/.local/state/opendeck-claude-usage/history.jsonl`. It holds a few KB
  of percentages and timestamps.
- **README privacy text:** "nothing is written to disk" becomes a precise
  description of that file.
- **Existing actions:** unchanged.

## Error handling

- **No readings yet or only one:** "collecting…" with headline `—`.
- **Unwritable state directory:** a single warning; the history still
  works in memory until OpenDeck restarts.
- **Corrupt lines:** skipped on load.
- **Clock going backwards:** readings are sorted on load. `record` doesn't
  reorder; a backwards step shows up as a zero delta (clamped).
- **Bad settings:** each field falls back on its own.

## Testing

- **`history.rs`:** all in a `tempfile` dir.
  - `load` of a missing file gives an empty store.
  - `load` skips a garbage line and prunes readings older than 8 days.
  - `load` rewrites the file pruned.
  - `record` appends a line.
  - An unchanged snapshot isn't recorded; a changed one is.
  - A read-only directory → `record` still keeps the reading in memory.
  - `in_memory` never creates files.
- **`sparkline.rs`:** each series against hand-built reading lists.
  - Trend covers only the current window.
  - Between polls:
    - A delta across a reset is the new pct.
    - A negative delta clamps to 0.
    - Only the last 30 points are kept.
  - Today:
    - Uses a baseline from before midnight.
    - With no baseline it starts at 0.
    - Reset deltas are handled.
  - Even burn skips readings before 10% elapsed.
  - Fewer than two points gives empty points and `—`.
  - The level color uses pace in Pace mode.
  - Settings wire: garbage falls back, `monthly` becomes Session, and
    values round-trip.
  - `next` cycles through all four series.
- **`styles/sparkline.rs`:**
  - A polyline and fill area exist with the right x span.
  - A flat line doesn't panic.
  - The collecting state has no polyline.
  - The strip is 200 wide with an unsqueezed caption.
- **`hub.rs`:**
  - Existing tests are updated for the new `output_for` parameter and the
    `UsageHub::new` signature.
  - `read_and_cache` records into the history.
  - `View::Sparkline` gives an Image on a keypad and `chart` Feedback on a
    dial.
- **Action:**
  - Manifest `Actions[6]`.
  - The `chart` feedback key matches the layout.
  - `cycled` keeps the colors and window.
  - The PI passes `series` through.
- **README:** the privacy paragraph is updated, plus a "Using Usage
  Sparkline" section and smoke items (series cycle on key and dial,
  history file created and pruned, and chart visible on the strip).

## Amendments after final review

- **Reset detection tolerates request jitter.** The API stamps `resets_at`
  with each request's sub-second fraction, so exact equality saw a "reset"
  on nearly every poll. `history::same_reset` now treats reset times within
  5 minutes as the same reset. Both the dedup in `record` and the step
  detection in the series math use it.
- **Latest values are held up to now.** With at least two readings,
  `build_sparkline` appends a synthetic reading at `now` holding the latest
  values. Idle time then shows as a flat line end, a `+0.0pp` step and an
  even-burn ratio that falls over time. The color is also computed at
  `now`, not at the last change.
- **Today starts at local midnight when a baseline exists.** It begins at
  0 at local midnight, so a day with one change (or none yet) still draws.
- **Window without a reset time.** When the latest reading has no reset
  time, the current window is the last window length up to `now`.
- **Load decodes lossily and never wipes on error.** `load` decodes the
  file lossily, and only rewrites it after a successful read, so a read
  error can't wipe the history.
