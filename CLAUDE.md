# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust [OpenDeck](https://github.com/nekename/OpenDeck) plugin (Linux only, built on the `openaction` crate) that shows Claude usage on Stream Deck dials and keypad tiles. Eight actions: Usage Gauge, Session + Weekly (combo), Burn Rate, Usage Heatmap, Usage Sparkline, Peak Clock, Metric Tile, API Spend. The README is the user-facing spec for every action's behavior.

## Commands

```bash
cargo fmt --check                              # CI gate (rustfmt.toml: edition 2024)
cargo clippy --all-targets -- -D warnings      # CI gate - warnings fail the build
cargo test                                     # all unit tests; no OpenDeck needed
cargo test heatmap::                           # tests in one module
cargo test -- --ignored live_                  # real requests: usage API (your ~/.claude login) + Console Admin API (skips without ~/.config/opendeck-claude-usage/admin-key)
cargo build --release --target x86_64-unknown-linux-gnu
node build.mjs x86_64-unknown-linux-gnu        # assembles dist/com.jfms7s.claudeusage.sdPlugin
```

Install locally by copying `dist/com.jfms7s.claudeusage.sdPlugin` into `~/.config/opendeck/plugins/` and restarting OpenDeck (plugins load only at startup). No real hardware is available in the dev environment; the README's "Manual smoke-test checklist" lists what's unverified on-device.

## Architecture

**Three data sources**, both built once in `src/main.rs` and shared by all actions:

- `source/api.rs` → `source/cached.rs`: calls the undocumented `api.anthropic.com/api/oauth/usage` with the OAuth token from `~/.claude/.credentials.json` (read-only; never refresh it - that logs Claude Code out). `CachedSource` throttles to **at most one request per minute across all actions** (the rate limit is shared with Claude Code's own `/usage`), backs off exponentially on failure, and serves the last good snapshot until it's 15 min stale. Never add a code path that bypasses this cache.
- `source/logs.rs`: `LogUsageSource` scans Claude Code transcripts (`~/.claude/projects/*/*.jsonl`) with an mtime cache. Used by Metric Tile and Heatmap. Cost is estimated from tokens via the hand-maintained table in `pricing.rs`; `<synthetic>` model entries are excluded.
- `source/console.rs` → `CachedSource`: billed Console org spend from the Usage & Cost Admin API (`cost_report` + `usage_report/messages`, daily UTC buckets, month to date plus the last 7 days) with an Admin key read from `~/.config/opendeck-claude-usage/admin-key` (refused unless 0600/0400; never put it in settings, a PI, or a log). Its own cache: 5 min interval, 30 min max backoff, 60 min stale; a missing/insecure key file is re-checked every read without a request (`Fetch::is_local`). API Spend and Console-sourced Metric Tiles read it through the `ConsoleData` trait.

**`hub.rs` - `UsageHub`**: every API-driven action (Gauge, Burn Rate, Combo, Sparkline) registers instances with `track(id, View)`; one 20s `poll_loop` reads the cached source once and re-renders every tracked instance. `View` says what an instance wants; `output_for` maps (View, snapshot) → `Output` (keypad SVG image vs. dial feedback JSON). The hub also appends changed readings to `HistoryStore` (`history.rs`, `~/.local/state/opendeck-claude-usage/history.jsonl`, trimmed to 8 days) which the Sparkline draws from. Log-driven actions (Metric Tile, Heatmap), API Spend and Peak Clock have their own `tick_loop`s instead.

**Per-action module split** - most features follow the same three-layer shape:
- `<feature>.rs` - pure logic/data shaping (e.g. `burn.rs`, `combo.rs`, `heatmap.rs`, `sparkline.rs`, `pace.rs`, `level.rs`), unit-tested without OpenDeck.
- `<feature>_action.rs` - the `openaction::Action` impl: settings struct (serde, `#[serde(flatten)]` shared pieces like `ColorSettings`/`StyleSettings` so the Property Inspector writes flat top-level fields), key/dial event handlers.
- `styles/<name>.rs` or `<feature>_icon.rs` / `tile.rs` - SVG renderers returning strings; `styles/mod.rs::svg` + data-URI wrapping for `setImage`.

Shared plumbing: `surface.rs` (`Surface` trait abstracts an `Instance` so render loops are testable; `for_each_tracked`), `press.rs` (short press = cycle view/style and persist via settings, ≥500 ms long press = refresh; `PressTimer`, `LatestSettings` handle press races), `level.rs` (Watch/Risk/Critical marks + colors), `format.rs` (display structs and dial feedback).

**Assets** (`assets/`, copied verbatim by `build.mjs`): `manifest.json` (action UUIDs `com.jfms7s.claudeusage.<action>`, controllers, PI paths), `layouts/*.json` (dial touch-strip layouts), `propertyInspector/*.html` (+ shared `colors.js`). Dial feedback keys must match the layout's items in both directions - use `test_support::assert_feedback_matches_layout` in action tests. `assets/icon-src/*.svg` are the icon sources; see its README for the regenerate command.

## Conventions

- **Version lives in two places**: `Cargo.toml` and `assets/manifest.json` must match (`build.mjs` refuses to build otherwise); a release bumps both plus `Cargo.lock` in a `chore: release X.Y.Z` commit. The Release workflow builds x86_64 + aarch64 on a published GitHub release.
- Conventional Commits (`feat:`, `fix:`, `chore:`, `test:`); PRs squash-merged to `master`.
- Project docs live in the Obsidian vault at `~/git/obsidian-vault/personal/projects/opendeck-claude-usage/`, not in this repo. Follow the vault's `CLAUDE.md` conventions (frontmatter `title` + `tags` including `projects` and `opendeck-claude-usage`; add each new note to the folder note it lives in).
- `~/git/obsidian-vault/personal/projects/opendeck-claude-usage/known-issues.md` tracks deferred issues by ID (`KI-NN`) with a status column; record new ones there, reference the ID in fix commits, and update its status when it lands.
- New features are designed in the vault's `specs/` and planned in its `plans/` (dated filenames) before implementation.
- User-visible behavior changes should be reflected in the README (action usage section and smoke-test checklist).
