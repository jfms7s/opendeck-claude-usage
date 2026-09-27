# OpenDeck Claude Usage

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with seven actions
- **Usage Gauge**, **Session + Weekly**, **Burn Rate**, **Usage Heatmap**,
**Usage Sparkline**, **Peak Clock**, and **Metric Tile**. Usage Gauge is
assignable to a Stream Deck dial or a keypad tile and shows percent used and
time until reset for one of Claude's usage windows - **Session** (5 hour),
**Weekly** (7 day), or **Monthly** (pay-as-you-go extra usage spend, if
enabled on your account).

On a dial, the touch strip shows a live bar, percent, and detail text. On a
keypad tile (no touch strip), the same data renders as a generated icon on
a dark card: a speedometer-style gauge - a four-zone semicircle drawn from the key's own
Watch/Risk/Critical marks and colors (see below) - with a light needle pointing at the current percent - above the percent and a
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
`~/.claude/.credentials.json`, and keeps the answer in memory. The only file
the plugin writes is the usage history for **Usage Sparkline** (see below).
It works the same whether you use Claude Code from the
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

**Upgrading from 0.6.0:** existing Usage Gauge keys switch from
green/yellow/red at 50/80% to copper with Watch 50, Risk 75 and Critical
90. Open a key's **Colors & thresholds** section to change this.

## Using a dial or tile

1. Add a **Usage Gauge** key on a dial or a keypad tile.
2. Pick which window to show: Session, Weekly, or Monthly (extra usage).
3. It updates automatically roughly every 20 seconds. On a dial, press for
   an immediate refresh. On a keypad tile, a short press switches to the
   next style (see below) and holding for half a second refreshes.

## Using a Metric Tile

1. Add a **Metric Tile** key on a keypad tile (no dial/Encoder variant).
2. Pick the metric (Tokens or Cost), the range (Today/7 days/Session),
   and how often it refreshes (in seconds).
3. It updates automatically on that schedule; tap the tile for an
   immediate refresh (this doesn't reset the schedule - the next
   automatic refresh still happens on time).

## Colors & thresholds

Usage Gauge and Burn Rate keys stay a calm copper until usage crosses one
of three marks you set per key (in % used): **Watch** (default 50),
**Risk** (75), **Critical** (90), each with its own editable color. Marks
must increase; if they don't, the defaults are used.

On a Usage Gauge you can also color by **pace**: the key warns at
whichever level is worse, current usage or the usage you'd reach at reset
if you keep burning at the current rate. Pace is only computed after 10%
of the window has passed (earlier projections are noise), and never for
Monthly.

## Styles (keypad)

A Usage Gauge key can be drawn as a **Speedometer**, **Bar**, **Soft pill**,
**Open donut**, **Tracked donut**, or **Thin ring**; every style except the
speedometer shows your Watch/Risk/Critical marks as tick marks. Tick the
styles you want under **Cycle styles**; a short press moves to the next
ticked one (you need at least two) and the key remembers its style across
restarts. Hold the key for half a second to refresh instead. Dials keep the
touch-strip bar.

## Using Burn Rate

1. Add a **Burn Rate** key on a dial or a keypad tile.
2. Pick the window (Session or Weekly) and what to show:
   - **Pace**: % used per hour (Session) or per day (Weekly) so far.
   - **Even burn**: projected usage at reset ÷ 100%, so `1.0x` is exactly on track.
   - **Runway**: time until 100% at the current rate, or ✓ if it lasts to the reset.
3. Its color always follows pace. It shows "too early" for the first 10% of a window.

## Using Session + Weekly

1. Add a **Session + Weekly** key on a dial or a keypad tile.
2. On a key it shows both windows at once: as two rows with bars and reset
   times (horizontal) or as two tall bars (vertical). A short press flips
   between them and the key remembers its layout; hold half a second to
   refresh.
3. On a dial the touch strip shows two bars, 5h and 7d, each with its
   percent and time until reset; press the dial to refresh.
4. Both bars share one set of marks and colors (**Colors & thresholds**),
   but each is colored by its own usage.

## Using Usage Heatmap

1. Add a **Usage Heatmap** key on a dial or a keypad tile.
2. Pick what to measure — **Tokens** or estimated **Cost**, from the same
   local Claude Code logs as Metric Tile — and a color.
3. Each cell is one local calendar day, shaded relative to the busiest day
   shown; days with no usage stay grey. The caption shows the total.
4. A short press (key or dial) flips between **7 days** (today on the
   right) and a **4-week** grid (oldest week on top), and it's remembered.
   Hold half a second to refresh. On a dial the same chart fills the touch
   strip.

## Using Usage Sparkline

1. Add a **Usage Sparkline** key on a dial or a keypad tile and pick the
   window: **Session** or **Weekly**.
2. A short press (key or dial) cycles the series; hold to refresh:
   - **Trend** — % of limit over the current window.
   - **Per poll** — how much each reading added (e.g. `+2.1pp`).
   - **Today** — running increase since local midnight (e.g. `8.4pp`).
   - **Vs even** — pace vs an even burn over the window (`1.0x` = on track).
3. The line takes the key's level color (**Colors & thresholds**). A new key
   says "collecting…" until at least two readings exist.

Anthropic's usage endpoint only reports the current percentages, so the
plugin records them itself: each successful poll whose numbers changed is
appended to `~/.local/state/opendeck-claude-usage/history.jsonl` (or under
`$XDG_STATE_HOME`). It holds only session/weekly percentages and reset
times - no tokens, credentials or account data - and anything older than 8
days is dropped when OpenDeck starts. Delete the file any time to reset the
history. If the folder can't be written, the plugin logs one warning and
keeps the history in memory until OpenDeck restarts.

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
- [ ] Changing a Usage Gauge's marks/colors updates both a dial's bar
      color and a tile's speedometer zones immediately. *(not yet verified)*
- [ ] Color-by-pace on a Usage Gauge turns it Watch/Risk earlier during a
      fast burn, and not during the first 10% of a window. *(not yet verified)*
- [ ] Burn Rate shows Pace / Even burn / Runway on a key and on a dial for
      Session and Weekly. *(not yet verified)*
- [ ] A key upgraded from 0.6.0 keeps its window setting. *(not yet verified)*
- [ ] Each of the six styles renders on a keypad tile with tick marks at
      the key's marks. *(not yet verified)*
- [ ] A short press cycles only the ticked styles and the chosen style
      survives an OpenDeck restart. *(not yet verified)*
- [ ] Holding a key ~0.5s refreshes it without changing its style.
      *(not yet verified)*
- [ ] With fewer than two styles ticked, a short press does nothing.
      *(not yet verified)*
- [ ] Session + Weekly shows both layouts on a key with each bar in its
      own level color and tick marks at the marks. *(not yet verified)*
- [ ] A short press flips the layout and it survives an OpenDeck restart;
      holding refreshes. *(not yet verified)*
- [ ] On a dial the touch strip shows the 5h and 7d bars with percent and
      reset time. *(not yet verified)*
- [ ] Usage Heatmap shows 7 days and 4 weeks on a key with today's cell on
      the right and the correct weekday letters. *(not yet verified)*
- [ ] A short press flips the view and it survives an OpenDeck restart.
      *(not yet verified)*
- [ ] On a dial the heatmap image fills the touch strip, including the
      caption text (confirms OpenDeck renders an SVG image with text in a
      pixmap item), and a dial press flips the view. *(not yet verified)*
- [ ] Usage Sparkline says "collecting…" at first, then draws a line after
      a few polls; a short press on the key or dial cycles all four series.
      *(not yet verified)*
- [ ] `~/.local/state/opendeck-claude-usage/history.jsonl` is created, only
      grows when usage changes, and survives an OpenDeck restart (the line
      is still there). *(not yet verified)*
- [ ] On a dial the sparkline image fills the touch strip, headline and
      caption visible. *(not yet verified)*

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
