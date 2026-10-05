# OpenDeck Claude Usage

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with eight actions
- **Usage Gauge**, **Session + Weekly**, **Burn Rate**, **Usage Heatmap**,
**Usage Sparkline**, **Peak Clock**, **Metric Tile**, and **API Spend**. Usage Gauge is
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
extensions that show your limits). The plugin makes at most one request
every three minutes or so (randomly ±10%, so it doesn't stay in step with
anything else that polls) however many dials, tiles, and taps are involved,
and none at all while no usage key or dial is on screen. When a request
fails (a 429 because something else used the allowance, a network blip, an
expired token), it retries after 6 minutes, then every 10 (or later, if the
429 says so) and keeps showing the last good numbers meanwhile; only once
those are more than 15 minutes old does each dial switch to a "no data"
state - never a crash or a blank display.

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

## Where the Tokens/Cost data comes from

**Metric Tile** and **Usage Heatmap** read a different source: Claude
Code's own transcripts under `~/.claude/projects/` - one file per session in
each project's folder, plus the transcripts of subagents
(`<session>/subagents/*.jsonl`), across every project. Each assistant
message carries token usage (input/output/cache-read/cache-write) but **no
cost figure at all**.

- **Tokens** counts every API message once. Claude Code writes a message as
  several lines (one per thinking/text/tool-use block), each repeating the
  message and request ids and its usage, and a resumed session can copy
  messages into a new file; the plugin keeps one copy of each message (the
  fullest one), across files.
- **Cost** is estimated from those tokens with an exact per-model price
  table in `src/pricing.rs`: Anthropic's published API list prices (input,
  output, 5-minute and 1-hour cache writes, cache reads), checked on
  2026-10-05. It's what the same tokens would cost on the API - not what a
  Pro/Max subscription is billed. A model missing from the table (a model
  released after this version) is counted in Tokens but not in Cost, and
  the Cost shows a trailing `+` (e.g. `$12.40+`): the real figure is at
  least that. Update the table when that happens.

Entries with `model == "<synthetic>"` (Claude Code's placeholder for
locally-generated content like compaction summaries) are excluded
entirely, since they represent no real API call.

Transcripts only grow, so after the first scan only the lines added since
are read.

The tile's **Session** range reuses the Usage Gauge's 5-hour rate-limit
window (the same `resets_at` the gauge fetches) rather than a fixed
rolling window, so it lines up with what "session" means elsewhere in
this plugin. If that fetch fails or has no `resets_at`, it falls back to a rolling last-5-hour window instead of
erroring.

## Where the API Spend data comes from

**API Spend** (and a Metric Tile set to **Source: Console**) shows *billed*
spend for your Claude Console organization - API usage paid per token, not
your Pro/Max subscription. It comes from Anthropic's
[Usage & Cost Admin API](https://platform.claude.com/docs/en/manage-claude/usage-cost-api)
(`/v1/organizations/cost_report` and `/v1/organizations/usage_report/messages`).

That API needs an **Admin key** (`sk-ant-admin01-…`), created in the Console
under Settings → Admin keys. A regular API key (`sk-ant-api…`) can't read
usage, and individual (non-organization) accounts can't create Admin keys
at all - the key then shows **NOT ADMIN**.

Put the key on one line in `~/.config/opendeck-claude-usage/admin-key` and
make it private:

    chmod 600 ~/.config/opendeck-claude-usage/admin-key

The plugin refuses a key file that other users could read (**KEY PERMS**),
shows **NO KEY** when the file is missing, and picks up a new or changed key
within a minute - no restart. The key is only ever sent, as the `x-api-key`
header, to `api.anthropic.com`; it's never stored in OpenDeck's settings,
shown in a settings page, or logged.

The numbers cover the **whole organization**, every API key and workspace.
The API buckets cost by **UTC day**, so "today" starts at 00:00 UTC, and
data lags real usage by about 5 minutes. Cost includes web search and code
execution charges (but not Priority Tier). The plugin fetches at most twice
every 5 minutes, however many keys show it; on failures it backs off up to
30 minutes and keeps showing the last good numbers for up to an hour.

## Installing

Download the latest `.streamDeckPlugin` and `SHA256SUMS` from
[Releases](https://github.com/jfms7s/opendeck-claude-usage/releases), then
either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into `~/.config/opendeck/plugins/` and restart OpenDeck
(plugins are only loaded at startup).

The plugin can read your Claude login, so check the download is the one CI
built before installing it - either against the checksum, or against the
build provenance GitHub recorded for it:

```bash
sha256sum -c SHA256SUMS
gh attestation verify opendeck-claude-usage.streamDeckPlugin --repo jfms7s/opendeck-claude-usage
```

**Upgrading from 0.6.0:** existing Usage Gauge keys switch from
green/yellow/red at 50/80% to copper with Watch 50, Risk 75 and Critical
90. Change this in a key's **Thresholds** and **Colors** settings.

## Using a dial or tile

1. Add a **Usage Gauge** key on a dial or a keypad tile.
2. Pick which window to show: Session, Weekly, or Monthly (extra usage).
3. It redraws every minute (the numbers behind it are fetched about every
   three minutes - see above). On a dial, press for an immediate refresh.
   On a keypad tile, a short press switches to the next style (see below)
   and holding for half a second refreshes.

## Using a Metric Tile

1. Add a **Metric Tile** key on a keypad tile (no dial/Encoder variant).
2. Pick the metric (Tokens or Cost), the range, and how often it refreshes
   (5 to 3600 seconds). **Today** is since local midnight (the same day as
   the Heatmap's and the Sparkline's "today"); **7 days** is the last 7×24
   hours; **Session** is the current 5-hour usage window.
   Pick the **source** too: Claude Code's logs (estimated cost), or the
   Console API (billed; see **Where the API Spend data comes from**).
   Console data comes in whole UTC days, so its Today is the UTC day and
   Session isn't offered.
3. It updates automatically on that schedule; tap the tile for an
   immediate refresh (this doesn't reset the schedule - the next
   automatic refresh still happens on time).

## Using API Spend

1. Set up the Admin key file (see **Where the API Spend data comes from**).
2. Add an **API Spend** key on a dial or a keypad tile.
3. It shows billed dollars for **This month** first. A short press (key or
   dial) cycles **Today (UTC) → 7 days → This month**, and the choice is
   remembered; hold half a second to refresh.
4. Optionally set a **Monthly budget**. On This month the key then draws a
   bar of the budget used and colors it with the **Colors & thresholds**
   marks; on a dial the touch-strip bar fills the same way.

## Using Peak Clock

1. Add a **Peak Clock** key on a keypad tile.
2. Set the peak hours (start and end, `HH:MM`; a window may cross
   midnight) and the days they apply to (weekdays by default; unticking
   every day means no peak hours at all).
3. The tile shows a 24-hour clock face with the peak hours marked in red
   (dimmed on a day they don't apply to) and a pointer at the current
   time, and under it **Peak** (red, with `ends in HH:MM`) or **Off-peak**
   (green, with `peak in HH:MM`, or `no peak set`). It redraws every
   minute; tap it to redraw now. The clock uses local time and has no data
   source, so it never says "no data".

## Colors & thresholds

Usage Gauge, Burn Rate, Session + Weekly and Usage Sparkline keys stay a
calm copper until usage crosses one of three marks you set per key (in %
used): **Watch** (default 50), **Risk** (75), **Critical** (90), each with
its own editable color. Marks must increase; if they don't, the defaults
are used (the settings page warns about it).

On a Usage Gauge, Session + Weekly or Usage Sparkline you can also color by
**pace**: the key warns at
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
   - **Runway**: time until 100% at the current rate, with "lasts to reset" or
     "runs out early" underneath (∞ when nothing has been used yet).
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
   window: **Session**, **Weekly**, or **Monthly** (extra usage).
2. A short press (key or dial) cycles the series; hold to refresh:
   - **Trend** — % of limit over the current window.
   - **Per poll** — how much each reading added (e.g. `+2.1pp`).
   - **Today** — running increase since local midnight (e.g. `8.4pp`). If
     OpenDeck wasn't running (or couldn't read usage) around midnight, it
     counts from the day's first reading instead of guessing how much of
     the gap was yesterday's.
   - **Vs even** — pace vs an even burn over the window (`1.0x` = on track).
3. The line takes the key's level color (**Colors & thresholds**). A new key
   says "collecting…" until at least two readings exist.
4. **Monthly** plots extra usage as a % of your monthly cap, but only as far
   back as the recorded history (8 days); a drop in the percentage is taken
   as the start of a new month. **Vs even** shows "—" for Monthly, since the
   usage data has no monthly reset time. If extra usage isn't enabled, the
   key says "off · not enabled".

Anthropic's usage endpoint only reports the current percentages, so the
plugin records them itself: each successful poll whose numbers changed (plus the
first poll of each day, to mark midnight) is appended to `~/.local/state/opendeck-claude-usage/history.jsonl` (or under
`$XDG_STATE_HOME`). It holds only session/weekly/extra-usage percentages and reset
times - no tokens, credentials or account data - and anything older than 8
days is dropped as it goes. The file and its folder are owner-only. Delete
the file any time to reset the history. If the folder can't be written, the
plugin logs one warning and keeps the history in memory until OpenDeck
restarts. A file this version can't read (say, written by a newer version
before a downgrade) is left untouched rather than overwritten.

## Manual smoke-test checklist

The unit tests (Rust) and the settings-page tests (`node --test tests/pi/*.test.mjs`)
cover the logic; these are the things only a live OpenDeck + Stream Deck
XL+ session can show. Run them before cutting a release and record the
version you checked them on:

| Check | Last verified |
|---|---|
| Every action's keypad tile and dial strip is readable (text not clipped or overlapping, colors right) for each style/view/series. | not recorded |
| Keys and dials redraw every minute without interaction, and a key that appears shows numbers straight away. | not recorded |
| A short press on Usage Gauge, Session + Weekly, Usage Heatmap or Usage Sparkline switches what it shows, and the choice survives an OpenDeck restart; two fast presses advance twice. | not recorded |
| Holding a key for half a second (or pressing a dial on Gauge/Burn Rate/Combo) refreshes without switching. | not recorded |
| Editing a settings page while a key's style/layout/view/series was changed by a press doesn't undo the press. | not recorded |
| Removing a key or dial causes no error in the plugin log on the next minute's redraw. | not recorded |
| Usage keeps updating with only the Claude desktop app open (no CLI session, no editor extension). | not recorded |
| Metric Tile and Usage Heatmap totals match `ccusage` (or another transcript tool) for the same day. | not recorded |
| `~/.local/state/opendeck-claude-usage/history.jsonl` is created owner-only, only grows when usage changes, and survives a restart. | not recorded |

- [ ] API Spend with no key file shows NO KEY; after creating a 0600 key
      file it shows dollars within a minute, without restarting OpenDeck.
      *(not yet verified)*
- [ ] A 0644 key file shows KEY PERMS; a regular API key shows NOT ADMIN.
      *(not yet verified)*
- [ ] A short press on an API Spend key or dial cycles Today UTC → 7 days
      → This month, and the range survives an OpenDeck restart; holding
      refreshes. *(not yet verified)*
- [ ] With a monthly budget, This month shows the bar and changes color at
      the marks; the dial's bar fills the same way. *(not yet verified)*
- [ ] A Metric Tile on Source = Console shows billed Tokens/Cost with a
      "billed" subtitle, and Session is greyed out in its settings.
      *(not yet verified)*
- [ ] `cargo test -- --ignored live_console` passes with a real Admin key
      (today's bucket is present). *(not yet verified)*

## Development

```bash
cargo test --locked                          # unit tests (no live OpenDeck needed)
node --test tests/pi/*.test.mjs                  # settings-page (Property Inspector) tests
cargo test -- --ignored live_ --nocapture    # one real usage-API request + a scan of your transcripts (+ the Console API with an Admin key)
cargo build --release --locked
node build.mjs                               # assembles dist/<uuid>.sdPlugin from what was built
cp -r dist/com.jfms7s.claudeusage.sdPlugin ~/.config/opendeck/plugins/
# restart OpenDeck, then work through the smoke-test checklist above
```

Releases are cut by pushing a `vX.Y.Z` tag matching the version in
`Cargo.toml` and `assets/manifest.json`: the Release workflow tests, builds
both architectures, and publishes the release with the bundle, its
`SHA256SUMS` and a build-provenance attestation.

## License

MIT — see [LICENSE](LICENSE).
