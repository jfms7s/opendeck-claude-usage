# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust [OpenDeck](https://github.com/nekename/OpenDeck) plugin (Linux only, built on the `openaction` crate) that shows Claude usage on Stream Deck dials and keypad tiles. Seven actions: Usage Gauge, Session + Weekly (combo), Burn Rate, Usage Heatmap, Usage Sparkline, Peak Clock, Metric Tile. The README is the user-facing spec for every action's behavior.

## Commands

```bash
cargo fmt --check                                      # CI gate (rustfmt.toml: edition 2024)
cargo clippy --all-targets --locked -- -D warnings     # CI gate - warnings fail the build
cargo test --locked                                    # all unit tests; no OpenDeck needed
cargo test heatmap::                                   # tests in one module
node --test tests/pi/*.test.mjs                        # Property Inspector tests (runs the real PI pages against a stub DOM)
cargo test -- --ignored live_ --nocapture              # one real usage-API request + a scan of your real transcripts
cargo build --release --locked                         # or --target <triple>
node build.mjs                                         # bundles every built target into dist/com.jfms7s.claudeusage.sdPlugin
```

`node build.mjs <triple>...` bundles exactly those targets; `node build.mjs --all` (what the Release workflow runs) requires every target in the manifest's `CodePaths`. MSRV is `rust-version` in `Cargo.toml` (1.88), checked by CI; CI and Release pin the toolchain they build with.

Install locally by copying `dist/com.jfms7s.claudeusage.sdPlugin` into `~/.config/opendeck/plugins/` and restarting OpenDeck (plugins load only at startup). No real hardware is available in the dev environment; the README's "Manual smoke-test checklist" lists what only a device can confirm.

## Architecture

**Two data sources**, both built once in `main.rs::wire` and shared by all actions:

- `source/api.rs` → `source/cached.rs`: calls the undocumented `api.anthropic.com/api/oauth/usage` with the OAuth token from `~/.claude/.credentials.json` (read-only; never refresh it - that logs Claude Code out; no redirects, https only, and errors never quote the credentials file). `CachedUsageSource` throttles to **one request per ~3 minutes (±10% jitter) across all actions** (the rate limit is shared with Claude Code's own `/usage`), backs off exponentially on failure (honouring `Retry-After`), and serves the last good snapshot until it's 15 min stale. Actions only ever get it as `Arc<dyn SharedUsage>`, which only `CachedUsageSource` implements, so a raw source can't be wired in; `main.rs`'s test checks every reader shares one budget. Never add a code path that bypasses this cache.
- `source/logs.rs`: `LogUsageSource` reads Claude Code transcripts (`~/.claude/projects/**/*.jsonl`, including `<session>/subagents/`), deduplicating on `message.id` + `requestId` (a message is written once per content block), reading each append-only file incrementally and keeping one shared copy of the entries. Used by Metric Tile and Heatmap. Cost is estimated via the exact model-ID price table in `pricing.rs` (unknown models are flagged, not guessed); `<synthetic>` entries are excluded.

**`hub.rs` - `UsageHub`**: every API-driven action (Gauge, Burn Rate, Combo, Sparkline) registers instances with `track(id, View)`, where a `View` is an `Arc<dyn HubView>` that turns a snapshot (and, if `needs_history`, the recorded readings) into an `Output`. One `poll_loop` - just after each minute boundary, parked while nothing is tracked - reads the shared cache once and re-renders every tracked instance; `render_cached` draws on appear/press from `SharedUsage::peek` (same staleness rule). The hub also records new readings to `HistoryStore` (`history.rs`, `~/.local/state/opendeck-claude-usage/history.jsonl`, trimmed to 8 days) which the Sparkline draws from. Metric Tile, Heatmap and Peak Clock have their own tick loops (deadline- or minute-based, parked while empty).

**Per-action module split**:
- `<feature>.rs` - pure logic/data shaping (`burn.rs`, `combo.rs`, `heatmap.rs`, `sparkline.rs`, `metric.rs`, `peak.rs`, `pace.rs`, `level.rs`, `gauge_style.rs`), unit-tested without OpenDeck. `format.rs` is the shared display layer (`UsageDisplay`, `build_display`, `usage_feedback`) used by the gauge, combo and the gauge styles.
- `<feature>_action.rs` - the `openaction::Action` impl and its settings struct (serde, `#[serde(flatten)]` shared pieces like `ColorSettings`/`StyleSettings` so the Property Inspector writes flat top-level fields). Hub actions also define their `HubView` here (`GaugeView`, `BurnView`, `ComboView`, `SparkView`) and implement `HubSettings` (`view()`, and `next()` for what a short press switches to); `hub_action.rs::HubActionCore` supplies the whole lifecycle.
- Renderers: `styles/` returns bare SVG (the gauge's six styles via `styles::build_styled_icon`, and the Combo/Heatmap/Sparkline key and strip renderers); single-layout tiles live in `<feature>_icon.rs` (`burn_icon`, `clock_icon`, `metric_icon`) and return the data URI. `tile.rs` is the shared toolkit (card, text lines, `strip_svg` for 200x100 dial strips, `data_uri`). Text is always drawn inside the SVG; the native title is cleared once per appearance.

Shared plumbing:
- `surface.rs` - `Output`, `Surface` (abstracts an `Instance` so render loops are testable; `FakeSurface` in its `test_support`), `Frames` (skips frames identical to the last one sent), `for_each_tracked` (the KI-06/KI-07 race-safe re-render loop).
- `press.rs` - short press = switch view/style and persist via settings, ≥500 ms long press = refresh. `PressCycler` decides every release from the instance's own last settings (KI-08); every press-switching action goes through it (hub actions via `HubActionCore`, which also tracks the new view before awaiting the save - KI-06).
- `settings.rs` - `lenient`: every settings field falls back alone, because openaction resets the *whole* struct on any deserialize error (KI-10). Fields with their own fallback rules use a `*Wire` struct (`level::ColorSettingsWire`, `gauge_style::StyleSettingsWire`). `settings.rs`'s table test feeds garbage to every action's settings - add new settings structs to it.
- `tasks.rs` - `spawn_supervised` (background loops are restarted, with a log line, if they panic), `sleep_to_next_minute`, `park_while_empty`.
- `level.rs` (Watch/Risk/Critical marks + colors).

**Assets** (`assets/`, copied verbatim by `build.mjs`): `manifest.json` (action UUIDs `com.jfms7s.claudeusage.<action>`, controllers, PI paths), `layouts/*.json` (dial touch-strip layouts), `propertyInspector/*.html` (+ shared `pi-common.js` - socket, save, and pass-through of press-picked fields - and `colors.js`, the Colors & thresholds cards). Dial feedback keys must match the layout's items in both directions - use `test_support::assert_feedback_matches_layout` in action tests; find manifest entries with `test_support::manifest_entry(uuid)`. PI behaviour is tested under node in `tests/pi/` (`fake-dom.mjs` runs the real pages). `assets/icon-src/*.svg` are the icon sources; see its README for the regenerate command.

## Conventions

- **Version lives in two places**: `Cargo.toml` and `assets/manifest.json` must match (`build.mjs` refuses to build otherwise); a release bumps both plus `Cargo.lock` in a `chore: release X.Y.Z` commit, then pushes the `vX.Y.Z` tag. The Release workflow checks the tag matches, runs fmt/clippy/tests, builds x86_64 + aarch64 in a read-only job, then a separate job attests the bundle and creates the GitHub release as a draft with the bundle and `SHA256SUMS`, publishing it only once both are attached.
- Workflow actions are pinned to full commit SHAs (with a `# vX.Y.Z` comment) and every cargo command runs `--locked`; Dependabot proposes monthly grouped updates for both.
- Conventional Commits (`feat:`, `fix:`, `chore:`, `test:`); PRs squash-merged to `master`.
- Project docs live in the Obsidian vault at `~/git/obsidian-vault/personal/projects/opendeck-claude-usage/`, not in this repo. Follow the vault's `CLAUDE.md` conventions (frontmatter `title` + `tags` including `projects` and `opendeck-claude-usage`; add each new note to the folder note it lives in).
- `~/git/obsidian-vault/personal/projects/opendeck-claude-usage/known-issues.md` tracks deferred issues by ID (`KI-NN`) with a status column; record new ones there, reference the ID in fix commits, and update its status when it lands.
- New features are designed in the vault's `specs/` and planned in its `plans/` (dated filenames) before implementation.
- User-visible behavior changes should be reflected in the README (action usage section and smoke-test checklist).
- When a new Claude model ships, add its exact id to `pricing.rs` (and to the test of published rates) - Cost shows a trailing `+` until then.
