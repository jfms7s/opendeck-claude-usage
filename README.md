# OpenDeck Claude Usage

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with three actions
- **Usage Gauge**, **Peak Clock**, and **Metric Tile**. Usage Gauge is
assignable to a Stream Deck dial or a keypad tile and shows percent used and
time until reset for one of Claude's usage windows - **Session** (5 hour),
**Weekly** (7 day), or **Monthly** (pay-as-you-go extra usage spend, if
enabled on your account).

On a dial, the touch strip shows a live bar, percent, and detail text. On a
keypad tile (no touch strip), the same data renders as a generated icon on
a dark card: a speedometer-style gauge - a three-zone semicircle
(green/yellow/red, at the same 50%/80% thresholds as the dial's bar) with a
light needle pointing at the current percent - above the percent and a
compact countdown (e.g. `3h 54m`, `6d 10h`). The text is drawn into the
icon itself rather than the key's native title, so it looks the same
whatever title font/size/alignment the key is set to, and needs no custom
background color to stay readable.

Built for a Stream Deck XL+'s 6 dials, 1200x100 touch strip, and 32 keys -
assign up to three dials or tiles (one per window) for an always-visible
usage readout.

## Where the data comes from

The plugin asks Anthropic directly: it calls
`https://api.anthropic.com/api/oauth/usage` (the same endpoint Claude Code's
`/usage` reads) with the OAuth login Claude Code stores in
`~/.claude/.credentials.json`, and keeps the answer in memory only - nothing
is written to disk. It works the same whether you use Claude Code from the
CLI, the desktop app, or an IDE extension, as long as one of them has logged
in on this machine.

The token is only read and sent in the request's `Authorization` header. The
plugin never refreshes it (that would log Claude Code out); if it has
expired, the dials show "no data" until Claude Code next runs and renews it.

That endpoint is undocumented and rate-limited per account - and the limit
is shared with anything else that calls it (Claude Code's `/usage`, editor
extensions that show your limits). The plugin makes at most one request a
minute however many dials, tiles, and taps are involved. When a request fails
(a 429 because something else used the minute's allowance, a network blip,
an expired token), it retries after 2, 4, 8... minutes (capped at 10) and
keeps showing the last good numbers meanwhile; only once those are more
than 15 minutes old does each dial switch to a "no data" state - never a
crash or a blank display.

There is no native monthly rate-limit window in Claude's usage data - only
session and weekly exist. The "Monthly" setting shows `extra_usage`
(pay-as-you-go overage spend against a monthly cap) instead, since it's the
closest real metric to what "monthly" usually means. It renders "not
enabled" if you haven't turned on pay-as-you-go overage credits.

The dollar amounts shown for Monthly (`used_credits`/`monthly_limit` in the
source file) are assumed to already be decimal dollars (e.g. `12.5` means
$12.50), not minor units needing further scaling - this is unverified,
since the account used to build this plugin has never had `extra_usage`
enabled. The `currency` field in `extra_usage` is currently ignored
entirely; the `$` sign is hardcoded regardless of account currency.

## Where the Tokens/Cost tile's data comes from

The **Metric Tile** action reads a different source: Claude Code's own
per-session transcript logs at `~/.claude/projects/<project>/<session-id>.jsonl`,
one file per session, across every project. Each assistant turn in these
files carries token usage (input/output/cache-read/cache-write) but
**no cost figure at all** - Cost is estimated by multiplying tokens by a
hardcoded per-model-family price table in `src/pricing.rs`.

That price table is a **best-effort, unverified snapshot** - it isn't
sourced from Claude Code, isn't fetched from any live pricing API, and
hasn't been re-checked against a real invoice. If Anthropic changes
pricing, the Cost tile's numbers will drift until the table is updated
by hand. Treat Cost as an estimate; treat Tokens (a direct sum from the
logs) as exact.

Entries with `model == "<synthetic>"` (Claude Code's placeholder for
locally-generated content like compaction summaries) are excluded
entirely, since they represent no real API call.

The tile's **Session** range reuses the Usage Gauge's 5-hour rate-limit
window (the same `resets_at` the gauge fetches) rather than a fixed
rolling window, so it lines up with what "session" means elsewhere in
this plugin. If that fetch fails or has no `resets_at`, it falls back to a rolling last-5-hour window instead of
erroring.

## Installing

Download the latest `.streamDeckPlugin` from
[Releases](https://github.com/jfms7s/opendeck-claude-usage/releases), then
either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into `~/.config/opendeck/plugins/` and restart OpenDeck
(plugins are only loaded at startup).

## Using a dial or tile

1. Add a **Usage Gauge** key on a dial or a keypad tile.
2. Pick which window to show: Session, Weekly, or Monthly (extra usage).
3. It updates automatically roughly every 20 seconds; press the dial or tap
   the tile for an immediate refresh.

## Using a Metric Tile

1. Add a **Metric Tile** key on a keypad tile (no dial/Encoder variant).
2. Pick the metric (Tokens or Cost), the range (Today/7 days/Session),
   and how often it refreshes (in seconds).
3. It updates automatically on that schedule; tap the tile for an
   immediate refresh (this doesn't reset the schedule - the next
   automatic refresh still happens on time).

## Manual smoke-test checklist

Run this against a live OpenDeck + Stream Deck XL+ session before cutting a
release. None of these have been verified on real hardware as of this
version - the manual smoke test was deliberately not run in this
development environment, which has no OpenDeck/Stream Deck to test against:

- [ ] Session/Weekly/Monthly dials each show a percent, bar, and detail line
      shortly after appearing. *(not yet verified)*
- [ ] Session/Weekly/Monthly keypad tiles each show a gauge icon with the
      percent and countdown drawn in with the needle at the right position shortly
      after appearing. *(not yet verified)*
- [ ] Display refreshes within ~20s without any interaction, on both a dial
      and a tile. *(not yet verified)*
- [ ] Pressing a dial or tapping a tile refreshes it immediately. *(not yet verified)*
- [ ] Monthly dial/tile shows "not enabled" cleanly when extra usage is off. *(not yet verified)*
- [ ] Removing a dial or tile doesn't error on the next poll tick. *(not yet verified)*
- [ ] Dials keep updating with only the Claude desktop app open (no CLI
      session, no editor extension). *(not yet verified)*
- [ ] Metric Tile shows the right label/value/subtitle for each metric
      (Tokens/Cost) × range (Today/7 days/Session) combination.
      *(not yet verified)*
- [ ] Metric Tile's configured refresh interval actually changes how
      often it updates (e.g. set to 5s, confirm faster updates than the
      default 60s). *(not yet verified)*
- [ ] Tapping a Metric Tile refreshes it immediately without disrupting
      its next scheduled refresh. *(not yet verified)*

## Development

```bash
cargo test                                   # unit tests (no live OpenDeck needed)
cargo test -- --ignored live_                # one real request to the usage API with your login
cargo build --release --target <triple>
node build.mjs <triple>                      # assembles dist/<uuid>.sdPlugin
cp -r dist/com.jfms7s.claudeusage.sdPlugin ~/.config/opendeck/plugins/
# restart OpenDeck, then work through the smoke-test checklist above
```

## License

MIT — see [LICENSE](LICENSE).
