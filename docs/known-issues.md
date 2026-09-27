# Known issues

Small issues deferred by the final reviews of v0.7.0–v0.11.0. None of them
block normal use. Each one has an ID, so a fix (commit, PR or session) can
point back here. Remove the entry, or mark it fixed, when it lands.

⭐ = something a user can actually notice. Starred items are the first to
fix.

**Status:** `open` · `in progress` · `fixed in vX.Y.Z` · `fixed (internal)`
(code or tests only, so no release carries it) · `accepted` (by design; no
fix planned)

## On the device

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-01 ⭐ | On a Sparkline dial strip, the caption and a long headline can overlap (e.g. `PER POLL · 5H` with `+12.3pp`). | `src/styles/sparkline.rs` `render_strip` | fixed in v0.11.1 |
| KI-02 ⭐ | A big cost on the Heatmap key caption (`4 WEEKS · $5177.06`) is squeezed to ~74% width. Drop the cents at $1000 and above. | `src/heatmap.rs` `caption_cost` | fixed in v0.11.1 |
| KI-03 ⭐ | A Combo key with no data still draws bright white ticks on empty grey tracks. | `src/styles/combo.rs` / `src/combo.rs` | fixed in v0.11.1 |
| KI-04 | The Soft pill's 12px minimum width makes 1–15% look alike (the spec asked for this). | `src/styles/` soft pill | fixed in v0.12.3 |
| KI-05 | Once the reset time has passed with no new reading, the Sparkline keeps showing the old window. | `src/sparkline.rs` `current_window` | fixed in v0.12.3 |

## Presses during a refresh (races)

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-06 | A short press that lands during a poll can show the old gauge style for up to ~20 s (the saved setting is correct). Fix: re-read the registry per instance in `refresh_all`. | `src/hub.rs` | fixed in v0.12.2 |
| KI-07 | A press during the Heatmap's once-a-minute tick can redraw the old view for up to 60 s. Same fix, in the tick loop. | `src/heatmap_action.rs` | fixed in v0.12.2 |
| KI-08 | Two very fast short presses can both start from OpenDeck's not-yet-updated settings and land on the same style. | gauge / sparkline / heatmap actions | fixed in v0.12.2 |
| KI-09 | History readings can be stored out of order if two refreshes race or the clock jumps (`record` pushes without sorting). | `src/history.rs` `record` | fixed in v0.12.2 |

## Settings page (property inspector)

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-10 ⭐ | An unknown saved Burn Rate `metric` leaves the select empty. The next edit sends `""`, and openaction then resets every setting, colors included. | `assets/propertyInspector/burnrate.html` | fixed in v0.11.1 |
| KI-11 | An invalid saved color shows as `#000000` in `<input type=color>` and is saved as black on the next edit. | `assets/propertyInspector/colors.js` | fixed in v0.12.1 |
| KI-12 | The first edit saves all eight color fields, which pins today's defaults if a later release changes them. | `assets/propertyInspector/colors.js` | fixed in v0.12.1 |
| KI-13 | Threshold marks that aren't increasing get no inline warning; the key quietly uses the defaults. | `assets/propertyInspector/colors.js` | fixed in v0.12.1 |
| KI-14 | The PI passes a stale value through if OpenDeck doesn't forward the plugin's `setSettings` to an open PI. That value is the gauge `style`, Heatmap `view` or Sparkline `series`. Saving the PI then undoes the press-picked choice. It is visual only, and one press fixes it. | gauge / heatmap / sparkline PIs | fixed in v0.12.1 |
| KI-15 | The Combo PI hint is longer than the spec's wording. | `assets/propertyInspector/combo.html` | fixed in v0.12.1 |

## History file

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-16 | The file is only trimmed to 8 days at startup, so it grows until the next restart (~KB–MB). | `src/history.rs` | fixed in v0.12.3 |
| KI-17 | A warning while loading the file silences later write warnings (one shared `warn_once`). The message wording could also be clearer. | `src/history.rs` | fixed in v0.12.3 |

## Performance

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-18 | Every Heatmap and Metric Tile instance rescans and clones all log entries on each tick. | `src/heatmap_action.rs`, `src/metric_action.rs` | fixed in v0.12.3 |
| KI-19 | The history is cloned on every render for every view, not just sparklines. | `src/hub.rs` | fixed in v0.12.3 |

## Code and tests

| ID | Issue | Where | Status |
|---|---|---|---|
| KI-20 | Fully qualified `crate::` paths are used where imports already exist. | `src/hub.rs` | fixed (internal) |
| KI-21 | The feedback-keys vs layout test only checks one direction. | `*_action.rs` tests | fixed (internal) |
| KI-22 | The mixed Gauge + Burn Rate hub test only checks registration, not that both render in one poll. | `src/hub.rs` tests | fixed (internal) |
| KI-23 | `key_down`/`key_up` handlers aren't tested directly (they need an `Instance`); the README smoke items cover them. | actions | fixed (internal) |
| KI-24 | There's no daylight-saving-time test for the Heatmap's local days. | `src/heatmap.rs` tests | fixed (internal) |
| KI-25 | There's no test for the `7 DAYS · 0` caption when every entry falls outside the window. | `src/heatmap.rs` tests | fixed (internal) |

## Accepted by design

| ID | Issue | Status |
|---|---|---|
| KI-26 | The history file uses the default umask permissions. | fixed in v0.12.4 |
| KI-27 | The rewrite's temp file has a theoretical symlink risk. | fixed in v0.12.4 |
| KI-28 | History file I/O blocks briefly inside async code. | fixed in v0.12.4 |
| KI-29 | The Sparkline's "Today" baseline is an approximation (the last reading before midnight). | fixed in v0.12.4 |
| KI-30 | A saved Monthly window on a Sparkline falls back to Session. | accepted |
