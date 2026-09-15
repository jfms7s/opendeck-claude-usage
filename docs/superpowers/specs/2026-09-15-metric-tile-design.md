# Metric Tile (Tokens / Cost) — Design

## Goal

Add a new keypad-only tile, **Metric Tile**, showing either total **Tokens**
or total **Cost** parsed from Claude Code's local transcript logs, for one
of three ranges: **Today** (rolling 24h), **7 days** (rolling 168h), or
**Session** (the current 5-hour rate-limit window). One configurable action
(metric + range + refresh interval), added multiple times to reproduce the
three-tile mockup the user provided.

Non-goals: no dial/Encoder variant (matches the mockup, which only shows
keypad-style cards); no historical charting; no per-project breakdown.

## Where the data comes from

Claude Code writes one JSONL transcript file per session under
`~/.claude/projects/<project-dir>/<session-id>.jsonl`. Each line with
`"type": "assistant"` and a `usage` object carries the token counts for that
turn:

```json
{
  "type": "assistant",
  "timestamp": "2026-09-13T12:59:01.971Z",
  "message": {
    "model": "claude-opus-5",
    "usage": {
      "input_tokens": 2,
      "output_tokens": 346,
      "cache_creation_input_tokens": 19929,
      "cache_read_input_tokens": 29011
    }
  }
}
```

There is **no cost field anywhere in these logs** (confirmed by grepping a
real transcript on this machine). Cost must be computed from tokens × a
hardcoded per-model price table.

Model name in the `model` field is inconsistent across entries seen on this
machine: `claude-opus-5`, `claude-sonnet-5`, and also bare `opus`, `sonnet`,
`haiku`, plus a synthetic placeholder `<synthetic>` (used for
locally-generated content like compaction summaries — no real API call, no
real cost, excluded entirely).

## Components

### `src/source/logs.rs` — transcript scanning

```rust
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

pub struct LogUsageSource {
    projects_dir: PathBuf,
    cache: Mutex<HashMap<PathBuf, (SystemTime /* mtime */, Vec<LogEntry>)>>,
}

impl LogUsageSource {
    pub fn default_path() -> PathBuf; // ~/.claude/projects
    /// Returns every LogEntry across all transcript files, using the mtime
    /// cache: a file whose mtime is unchanged since the last scan is not
    /// re-read or re-parsed. Runs inside spawn_blocking (directory walk +
    /// many file reads is real, avoidable blocking work).
    pub async fn entries(&self) -> Result<Vec<LogEntry>, LogSourceError>;
}
```

- Discovers files via `~/.claude/projects/*/*.jsonl` (one level of project
  dirs, jsonl files directly inside — matches the layout on this machine).
- A line that fails to parse as JSON, or parses but has `type != "assistant"`
  or no `usage` object, is skipped — not a hard error for the whole file
  (same "malformed becomes a fallback" convention `source/file.rs` uses for
  `resets_at`).
- Lines where `model == "<synthetic>"` are skipped entirely (excluded from
  both tokens and cost).
- The mtime cache is keyed by file path; on each `entries()` call, files
  with an unchanged mtime reuse their cached `Vec<LogEntry>` instead of
  being re-read. This means multiple tile instances (different
  metric/range) sharing the same poll tick don't multiply disk I/O, and an
  instance's own refresh doesn't force a full rescan if nothing changed.
- "Total tokens" for a `LogEntry` = `input + output + cache_creation_input
  + cache_read_input` (the usual convention token-usage tools use).

### `src/pricing.rs` — cost estimation

```rust
pub struct PriceTable { input, output, cache_write, cache_read: f64 } // $ per 1M tokens

pub fn price_for_model(model: &str) -> Option<PriceTable>;
pub fn cost_for_entry(entry: &LogEntry) -> Option<f64>; // None if model unrecognized
```

- Matches by substring against the normalized model string (lowercased):
  contains `"opus"` → opus rates, `"sonnet"` → sonnet rates, `"haiku"` →
  haiku rates. Handles both `claude-opus-5` and bare `opus` identically.
- Rates are a **best-effort, unverified snapshot** — same caveat as the
  existing `extra_usage` dollar-amount assumption already documented in
  the README. Documented inline as a comment and in a new README section.
- An entry whose model matches no known family contributes to the Tokens
  total but is skipped for Cost (rather than guessing).

### Range resolution

```rust
pub enum RangeKind { Today, SevenDay, Session }

pub fn range_bounds(
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>, // from statusline-usage.json
) -> (DateTime<Utc>, DateTime<Utc>);
```

- `Today` = `[now - 24h, now]` (rolling, not calendar-day).
- `SevenDay` = `[now - 168h, now]` (rolling).
- `Session` = `[resets_at - 5h, resets_at]` when `session_resets_at` is
  `Some` (read from the same `~/.claude/statusline-usage.json` the
  existing `UsageGaugeAction` reads); falls back to `[now - 5h, now]` if
  the file is missing, unparseable, or has no `resets_at` — a fallback,
  not an error, per the plugin's existing convention.

### `src/metric.rs` — aggregation & display

```rust
pub enum MetricKind { Tokens, Cost }

pub struct MetricDisplay {
    pub label: &'static str,     // "Tokens" | "Cost"
    pub value_text: String,      // "1.2M" | "$8.40"
    pub subtitle: &'static str,  // "today" | "7 days" | "session"
    pub accent_color: &'static str,
}

pub fn build_metric_display(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>,
) -> MetricDisplay;
```

- Filters `entries` to `range_bounds`, then sums tokens or cost
  (skipping unpriced entries for `Cost`, as above).
- Token formatting: `< 1000` → exact integer; `>= 1000` → one decimal
  place with `K`/`M` suffix (`1.2M`, `318K`), matching the mockup.
- Cost formatting: always `$X.XX` (2 decimals).
- `accent_color` is a **fixed color per metric**, not value-driven (unlike
  the gauge's green/yellow/red threshold colors) — Tokens and Cost each
  get their own constant accent so multiple tiles are visually
  distinguishable at a glance, but the color never changes based on the
  number shown.
- No log data available (empty scan, or `LogUsageSource::entries()`
  fails) renders the same "no data" state pattern as
  `format::error_display()`.

### `src/metric_icon.rs` — rendering

Mirrors `icon.rs`'s SVG-background + native-title split used by the
existing keypad tiles:
- SVG draws a dark rounded-rect card background plus a short colored
  underline bar (`accent_color`) positioned under where the label line
  will sit.
- The tile's native title is 3 lines: `"{label}\n{value_text}\n{subtitle}"`
  — kept native (not drawn in SVG) for the same crispness reason
  `icon.rs` already documents for the gauge's percent/detail text.
- Not a pixel-exact match for the mockup (native title can't underline
  just one line, or vary font weight per line) but keeps this codebase's
  established "text stays native" convention instead of introducing a
  second, inconsistent rendering style.

### `src/metric_action.rs` — the action

```rust
struct MetricTileSettings {
    metric: MetricKind,
    range: RangeKind,
    refresh_seconds: u64, // default 60; PI-constrained to e.g. 5..=3600
}
```

- `Controllers: ["Keypad"]` only.
- Registry: `DashMap<instance_id, (MetricTileSettings, next_due: Instant)>`
  — replaces the existing tiles' `DashMap<instance_id, WindowKind>` /
  `DashMap<instance_id, PeakWindow>` shape, since this action needs
  per-instance cadence rather than one shared fixed interval.
- A single loop ticks every 1s (cheap: just compares `Instant`s), and for
  each instance whose `next_due` has elapsed: reads/aggregates via the
  shared `LogUsageSource` + `FileUsageSource` (for session bounds),
  renders, then reschedules `next_due = now + refresh_seconds`.
- `key_up` (tap) renders that one instance immediately, exactly like the
  existing tiles' tap-to-refresh, and does **not** reset its `next_due`
  schedule (a tap is a bonus refresh, not a reason to skip the next
  scheduled one).
- `will_appear` / `did_receive_settings` render from cache immediately
  (same `render_cached` pattern as `UsageGaugeAction`) and (re)track the
  instance with `next_due = now` so it refreshes on the very next tick
  rather than waiting a full interval.

### Property Inspector — `assets/propertyInspector/metrictile.html`

Same structural pattern as the existing `index.html`/`peakclock.html`:
one `<select id="metric">` (Tokens / Cost), one `<select id="range">`
(Today / 7 days / Session), one `<input type="number" id="refresh_seconds"
min="5" max="3600">`. Same websocket wiring, copy-pasted structure.

### Manifest & wiring

- `assets/manifest.json`: new `Actions[2]` entry,
  `UUID: "com.jfms7s.claudeusage.metrictile"`, `Name: "Metric Tile"`,
  `Controllers: ["Keypad"]`, own `PropertyInspectorPath`.
- `src/main.rs`: construct `LogUsageSource::default()`, build
  `MetricTileAction::new(log_source, file_source_for_session_bounds)`,
  spawn its tick loop alongside the other two, `register_action`.

## Error handling

Every failure mode falls back to a visible-but-clear state rather than a
crash or silent blank tile, matching the existing plugin's convention:

| Failure | Behavior |
|---|---|
| `~/.claude/projects` missing/unreadable | Empty entry list → "no data" display (not an error) |
| One transcript file unreadable/corrupt | That file skipped, others still scanned |
| One line malformed / not JSON | That line skipped |
| Unknown model for Cost | Entry counted in Tokens, excluded from Cost |
| `statusline-usage.json` missing (Session range) | Falls back to rolling last-5h window |
| `resets_at` missing/malformed (Session range) | Falls back to rolling last-5h window |

## Testing

- `logs.rs`: tempdir JSONL fixtures (mirrors `source/file.rs`'s
  `tempfile` pattern) — correct token sums, `<synthetic>` exclusion,
  malformed-line skipping, mtime-cache reuse (second call with an
  untouched file doesn't re-parse it — assert via a call counter or by
  mutating the file and confirming the cache *does* pick up the change).
- `pricing.rs`: family-matching for known model strings (`claude-opus-5`,
  bare `sonnet`, etc.), `None` for unrecognized models.
- `metric.rs`: range filtering at exact boundaries, token/cost
  formatting (`999` vs `1000` vs `1234567`, `$0.00` vs `$8.40`),
  fixed-vs-value-driven accent color.
- `metric_action.rs`: track/untrack round-trip, due-time scheduling
  (an instance ticked before its `next_due` is not re-rendered; one
  ticked after is, and reschedules), `action_uuid_matches_the_shipped_manifest`
  test mirroring the existing two actions' tests.

## Documentation

README gets a new section alongside the existing "Where the data comes
from" one, disclosing: the transcript-log source path, the
`<synthetic>`-exclusion rule, and — most importantly — that Cost is a
**best-effort, unverified estimate** from a hardcoded price table, not
sourced from Claude Code itself. Manual smoke-test checklist gets three
new unchecked items for the Metric Tile (Tokens/Cost × the three ranges,
tap-to-refresh, configurable interval taking effect).
