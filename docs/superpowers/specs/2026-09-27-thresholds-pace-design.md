# Thresholds & Pace — Design

Sub-project **1 of 4** in the "new tiles and dials" effort, inspired by the
Elgato Marketplace *Claude Usage Deck* screenshots (style cycling, "color
when it counts" thresholds, pace/even-burn/runway readouts, a
session+weekly combo key, heatmaps and sparklines).

| # | Sub-project | Depends on |
|---|---|---|
| **1** | **Thresholds & pace** (this spec) — per-key marks + editable palette, pace-based coloring, new Burn Rate action, shared `UsageHub` | — |
| 2 | Gauge styles + tap-cycle — open donut, tracked donut, thin ring, soft pill, bar; per-key checklist of styles a tap cycles | 1 |
| 3 | Session+Weekly combo key — both windows on one key, horizontal/vertical layouts | 1, 2 |
| 4 | Heatmaps & sparklines — day-bucketed log aggregation, poll-history store | 1 |

Each gets its own spec → plan → implementation cycle.

## Goal

1. Replace the fixed green/yellow/red 50%/80% coloring with **per-key,
   user-set Watch / Risk / Critical marks** (in % used) and an **editable
   four-color palette** whose default *normal* color is a calm neutral
   accent — color only shows up once a mark is crossed.
2. Offer an optional **pace-based** color mode that warns earlier when the
   current burn rate would overrun the window before it resets.
3. Add a new **Burn Rate** action (keypad + dial) showing **Pace**,
   **Even burn**, or **Runway** for the Session or Weekly window.
4. Extract a shared **`UsageHub`** (snapshot cache, instance registry,
   single poll loop) so Usage Gauge, Burn Rate, and sub-project 3's combo
   key share one poller instead of each duplicating it.

Non-goals: new gauge shapes and tap-to-cycle (sub-project 2); global /
plugin-wide color settings (explicitly chosen per-key); pace for Monthly
(no window length exists for `extra_usage`); tick marks on gauges (drawn by
sub-project 2's styles, which read the marks defined here).

## Decisions made during brainstorming

| Question | Decision |
|---|---|
| Look below every mark | Neutral accent, all four colors user-editable |
| Where settings live | Per key (each Property Inspector has its own) |
| Mark units | % **used** (same direction as the number on the key) |
| Pace mode semantics | Level = **max**(level of actual %, level of projected %) |
| Where pace/even-burn/runway live | New **Burn Rate** action |
| Structure | Shared `UsageHub` (approach A) |

## Components

### `src/level.rs` — color model (pure, no I/O)

```rust
pub struct Marks { pub watch: f64, pub risk: f64, pub critical: f64 }
pub struct Palette { pub normal: String, pub watch: String, pub risk: String, pub critical: String }
pub enum Level { Normal, Watch, Risk, Critical }   // derives Ord
pub enum ColorMode { Fixed, Pace }                 // serde "fixed" / "pace", default Fixed

pub struct ColorSettings { pub marks: Marks, pub palette: Palette, pub mode: ColorMode }
```

- **Defaults:** marks **50 / 75 / 90**; palette normal **`#d97757`**
  (Claude copper), watch `#eab308`, risk `#f97316`, critical `#ef4444`.
- `Marks::sanitized(self) -> Marks`: each value clamped to 0..=100; if the
  result is not strictly increasing (`watch < risk < critical`), returns
  `Marks::default()` and logs one `warn!`.
- Palette colors are validated as `#rrggbb` (case-insensitive); an invalid
  entry falls back to **that entry's** default only.
- `Level::for_percent(p, &Marks)`: `p >= critical` → Critical, `>= risk`
  → Risk, `>= watch` → Watch, else Normal (each mark is **inclusive**).
- `ColorSettings::level(actual: f64, projected: Option<f64>) -> Level`:
  Fixed → level of `actual`; Pace → `max(level(actual), level(projected))`,
  where `projected == None` means level of `actual` alone.
- `Palette::color(Level) -> &str`.

**Settings wire format** (flattened into each action's settings JSON):

```json
{ "watch": 50, "risk": 75, "critical": 90,
  "colorNormal": "#d97757", "colorWatch": "#eab308",
  "colorRisk": "#f97316", "colorCritical": "#ef4444",
  "colorMode": "fixed" }
```

Every field is `#[serde(default)]` **and** tolerant of a wrong type (a
number sent as a string, `null`, garbage) via a lenient per-field
deserializer that falls back to that field's default. Reason: openaction
replaces the *whole* settings struct with `Default::default()` when
deserialization fails, so one bad color would otherwise also silently reset
the key's `window`.

### `src/pace.rs` — burn math (pure, `now` passed in)

```rust
pub struct Pace {
    pub elapsed_fraction: f64,   // 0..=1
    pub projected: f64,          // % at reset if the current rate holds
    pub even_burn: f64,          // projected / 100  (1.0 = exactly on track)
    pub rate_per_hour: f64,      // % used per hour so far
    pub runway: Runway,
}
pub enum Runway { Empty, LastsToReset, Until(chrono::Duration) }

pub fn window_length(kind: WindowKind) -> Option<chrono::Duration>; // Session 5h, Weekly 7d, Monthly None
pub fn pace(window: &WindowUsage, kind: WindowKind, now: DateTime<Utc>) -> Option<Pace>;
```

- `elapsed_fraction = 1 − (resets_at − now) / length`, clamped to 0..=1.
- Returns `None` when: no `resets_at`, `window_length` is `None`, or
  **`elapsed_fraction < 0.10`** (too early — a 5% burn in the first 10
  minutes would otherwise project to 150%).
- `projected = used / elapsed_fraction`; `even_burn = projected / 100`;
  `rate_per_hour = used / elapsed_hours`.
- Runway: `used >= 100` → `Empty`; `used == 0` (rate 0) → `LastsToReset`;
  otherwise `until = (100 − used) / rate_per_hour` hours, and if
  `until >= resets_at − now` → `LastsToReset`, else `Until(until)`.
- Display formatting (in `format.rs`, reusing `format_remaining`'s compact
  style, which gets a `pub fn format_duration_compact(Duration)` wrapper):
  - Pace: Session `"{:.1}%"` + subtitle `"per hour"`; Weekly `"{:.1}%"` +
    `"per day"` (rate × 24).
  - Even burn: `"{:.1}x"` + `"even burn"`.
  - Runway: `Until(d)` → compact duration (`"22h"`, `"3d 4h"`) +
    `"until empty"`; `LastsToReset` → `"✓"` + `"lasts to reset"`;
    `Empty` → `"0"` + `"empty"`.
  - `pace()` is `None` → `"—"` + `"too early"` (or `"no reset info"` when
    `resets_at` is missing).

### `src/hub.rs` — shared `UsageHub`

Moves out of `action.rs`: the `Box<dyn UsageSource>`, `latest:
RwLock<Option<UsageSnapshot>>`, `poll_last_read_ok`, the transition-only
logging, and the 20s `poll_loop`.

```rust
pub enum View {
    Gauge { window: WindowKind, colors: ColorSettings },
    Burn  { window: WindowKind, metric: BurnMetric, colors: ColorSettings },
}

pub struct UsageHub { /* source, latest, registry: DashMap<String, View>, poll_last_read_ok */ }

impl UsageHub {
    pub fn track(&self, instance_id: &str, view: View);
    pub fn untrack(&self, instance_id: &str);
    pub async fn render_cached(&self, instance: &Instance, view: &View) -> OpenActionResult<()>;
    pub async fn refresh_one(&self, instance: &Instance, view: &View) -> OpenActionResult<()>;
    pub async fn poll_loop(self: Arc<Self>);
}
```

- One registry keyed by instance id holds **every** usage-driven instance
  regardless of action type; the poll loop renders each via
  `render(instance, view, Result<&UsageSnapshot, _>, now)`, which
  dispatches on `View` to the gauge or burn builders.
- `main.rs` builds one `Arc<UsageHub>` around the existing
  `CachedUsageSource`, spawns **one** `poll_loop`, and hands clones of the
  `Arc` to `UsageGaugeAction::new` and `BurnRateAction::new`. The Metric
  Tile keeps receiving the `CachedUsageSource` clone it gets today
  (unchanged).
- Existing tests for track/untrack, read-and-cache, and transition logging
  move with the code into `hub.rs`.

### `src/format.rs` — Usage Gauge display

- `bar_color()` is removed. `build_display(snapshot, window, &ColorSettings,
  now)` computes the level via `ColorSettings::level(actual,
  pace(..).map(|p| p.projected))` (Monthly always passes `None`).
- `UsageDisplay.color` becomes `String`. It also carries the `Marks` and
  `Palette` so the keypad icon can draw zones.
- `DISABLED_COLOR` (grey) is unchanged for "not enabled" / "no data".

### `src/icon.rs` — keypad speedometer zones

The fixed three arcs become **four** arcs from the key's own marks:
`0→watch` normal, `watch→risk` watch, `risk→critical` risk,
`critical→100` critical, each in its palette color. A zero-length zone is
skipped. Arcs spanning more than 90° are split so the existing
`large-arc-flag=0` invariant in `arc_path` still holds. The needle is
unchanged.

### `src/burn_action.rs` + `src/burn_icon.rs` — Burn Rate action

- UUID `com.jfms7s.claudeusage.burnrate`, name **Burn Rate**, controllers
  `Encoder` + `Keypad`, layout `layouts/usage.json`, PI
  `propertyInspector/burnrate.html`.
- Settings: `window` (`session` | `weekly`; a stored `monthly` is treated
  as `session`), `metric` (`pace` | `evenBurn` | `runway`, default `pace`),
  plus the flattened color settings.
- **Color is always pace-based**, whatever `colorMode` says: level =
  max(level(actual), level(projected)). So 1.4x (projected 140%) reads
  Critical with default marks.
- Keypad icon (on the shared `tile::card()`): small uppercase label on top
  (`PACE` / `EVEN BURN` / `RUNWAY`), the value large in the level color,
  the subtitle muted underneath.
- Dial: the existing `usage.json` layout — `percent` = value text, `bar` =
  `min(projected, 100)` in the level color (0 when pace is `None`),
  `detail` = `"{subtitle} · {session|weekly}"`.
- Tap / dial press: immediate `refresh_one`, same as Usage Gauge today.

### Property Inspectors

- `assets/propertyInspector/colors.js`: a shared snippet that injects the
  collapsible **Colors & thresholds** section (three number inputs, four
  `<input type="color">`, a Fixed/Pace select), reads and writes the flat
  fields above, and **sends numbers as JSON numbers**. It has a "Reset to
  defaults" button.
- `index.html` (Usage Gauge) includes it. `burnrate.html` includes it with
  the Fixed/Pace select hidden, and has Window + Metric selects.
- `build.mjs` copies the new files. The manifest gains the Burn Rate entry.

## Behavior change for existing users

Existing Usage Gauge keys pick up the defaults on upgrade: **green becomes
copper**, yellow still starts at 50% (Watch), a new orange band starts at
75% (Risk), and red moves from 80% to **90%** (Critical). This is called out in the
release notes. Nothing else about an existing key's settings changes.

## Error handling

- No snapshot yet / read failed → existing `error_display()` (grey, "no
  data") for both actions. Burn Rate reuses the same card shape.
- Invalid marks or colors → sanitized per the rules above, and never an
  error surfaced on the key.
- Clock skew (`now > resets_at`) → `elapsed_fraction` clamps to 1 and pace
  still computes; the countdown already shows "now".

## Testing

Unit (no OpenDeck needed):

- `level.rs`: inclusive boundaries at every mark; `sanitized` for
  non-increasing, out-of-range, and equal marks; per-entry hex fallback;
  Pace mode picks the max and ignores `None`; settings from `{}` equal
  `Default`; a bad `colorWatch` keeps the other fields **and** the
  sibling `window` field.
- `pace.rs`: elapsed at 9.9% → `None`, at 10% → `Some`; projected and
  even burn for a known case (30% used at 25% elapsed → 120%, 1.2x);
  runway `Empty` / `LastsToReset` (both 0% used and a slow rate) /
  `Until`; Monthly → `None`; missing `resets_at` → `None`.
- `icon.rs`: four zones for default marks; zone skipped when two marks are
  equal to an end; no emitted arc spans over 90°.
- `format.rs`: Burn Rate strings for every metric × state.
- `hub.rs`: moved tests still pass; a `View::Burn` and a `View::Gauge` in
  the same registry both render on one poll.
- Manifest/layout consistency tests extended to the Burn Rate action.

Manual: add README smoke-checklist items for the colors section, pace mode,
and each Burn Rate metric on both a key and a dial.
