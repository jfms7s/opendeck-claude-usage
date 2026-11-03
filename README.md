# OpenDeck Claude Usage

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin that shows your
Claude usage on Stream Deck keys, with eight actions - **Usage Gauge**,
**Session + Weekly**, **Burn Rate**, **Usage Heatmap**, **Usage Sparkline**,
**Peak Clock**, **Metric Tile**, and **Sessions**. Usage Gauge shows percent used and time
until reset for one of Claude's usage windows - **Session** (5 hour) or
**Weekly** (7 day).

On a key, the data renders as a generated icon on a dark card: a
speedometer-style gauge - a four-zone semicircle drawn from the key's own
Watch/Risk/Critical marks and colors (see below) - with a light needle
pointing at the current percent - above the percent and a compact countdown
(e.g. `3h 54m`, `6d 10h`). The text is drawn into the icon itself rather than
the key's native title, so it looks the same whatever title font/size/alignment
the key is set to, and needs no custom background color to stay readable.

Built for Stream Deck keys: put one key per window (or a Session + Weekly key)
anywhere on the deck for an always-visible usage readout. Most actions also
work on a dial (Stream Deck + / + XL), where the touch strip shows a live bar,
percent, and detail text; Metric Tile, Peak Clock and Sessions are key-only.

## Where the data comes from

The plugin asks Anthropic directly: it calls
`https://api.anthropic.com/api/oauth/usage` (the same endpoint Claude Code's
`/usage` reads) with the OAuth login Claude Code stores - in
`~/.claude/.credentials.json` on Linux, and in the login Keychain on macOS - and
keeps the answer in memory. On macOS it reads the Keychain item ("Claude
Code-credentials") through `/usr/bin/security`, the same way Claude Code does, so
no Keychain prompt is expected; if there is no item it falls back to
`~/.claude/.credentials.json`. The only file
the plugin writes is the usage history for **Usage Sparkline** (see below).
It works the same whether you use Claude Code from the
CLI, the desktop app, or an IDE extension, as long as one of them has logged
in on this machine.

The token is only read and sent in the request's `Authorization` header. The
plugin never refreshes it or writes the credentials itself (a second
refresher racing Claude Code could log it out). On Linux, when the token has
expired, the plugin asks Claude Code to renew it by running
`claude auth status --json` - found on `PATH`, else in `~/.local/bin` or
`~/.claude/local` - which refreshes the login through Claude Code's own code
path; its output (your account email and org) is discarded. That runs at most
once every 10 minutes, is stopped after 10 seconds, and is skipped when the
login has no refresh token (Claude Code has signed out). If there is no
`claude`, or it can't renew the login (a revoked login is signed out, as
opening Claude Code would), the keys show "no data" until Claude
Code next runs (on macOS this is still the only behaviour).

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

Extra usage (pay-as-you-go spend against a monthly cap) is not shown on any
key. Keys saved with the old **Monthly** window (before 0.21) show Session.

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
are read. The first scan after OpenDeck starts reads every transcript
(seconds on a large `~/.claude/projects`). It runs in the background, so
the other keys are not held up: a Metric Tile shows "no data" and a
Heatmap shows an empty grid until it finishes, then both redraw.

The tile's **Session** range reuses the Usage Gauge's 5-hour rate-limit
window (the same `resets_at` the gauge fetches) rather than a fixed
rolling window, so it lines up with what "session" means elsewhere in
this plugin. If that fetch fails or has no `resets_at`, it falls back to a rolling last-5-hour window instead of
erroring.

## Installing

Runs in OpenDeck on Linux (x86_64 and aarch64) and on macOS with Apple Silicon.

Download the latest `.streamDeckPlugin` and `SHA256SUMS` from
[Releases](https://github.com/jfms7s/opendeck-claude-usage/releases), then
either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into OpenDeck's plugin folder and restart OpenDeck
(plugins are only loaded at startup):

- Linux: `~/.config/opendeck/plugins/`
- macOS: `~/Library/Application Support/opendeck/plugins/`

On macOS, a bundle unzipped by hand (e.g. in Finder) is marked as downloaded
and Gatekeeper refuses to start the binary. Clear the mark once:

```bash
xattr -dr com.apple.quarantine ~/Library/Application\ Support/opendeck/plugins/com.jfms7s.claudeusage.sdPlugin
```

The plugin can read your Claude login, so check the download is the one CI
built before installing it - either against the checksum, or against the
build provenance GitHub recorded for it, which only the Release workflow
can produce:

```bash
sha256sum -c SHA256SUMS                    # macOS: shasum -a 256 -c SHA256SUMS
gh attestation verify opendeck-claude-usage.streamDeckPlugin --repo jfms7s/opendeck-claude-usage --signer-workflow jfms7s/opendeck-claude-usage/.github/workflows/release.yml
```

Add `--source-ref refs/tags/vX.Y.Z` (the release you downloaded) to also pin
it to that tag.

**Upgrading from 0.6.0:** existing Usage Gauge keys switch from
green/yellow/red at 50/80% to copper with Watch 50, Risk 75 and Critical
90. Change this in a key's **Thresholds** and **Colors** settings.

## Using a dial or tile

1. Add a **Usage Gauge** key on a dial or a keypad tile.
2. Pick which window to show: Session or Weekly.
3. It redraws every minute (the numbers behind it are fetched about every
   three minutes - see above). On a dial, press for an immediate refresh.
   On a keypad tile, a short press switches to the next style (see below)
   and holding for half a second refreshes. A refresh still goes through
   the shared throttle; if a fetch takes longer than half a second, the key
   shows the cached numbers and redraws as soon as the fetch lands.

## Using a Metric Tile

1. Add a **Metric Tile** key on a keypad tile (no dial/Encoder variant).
2. Pick the metric (Tokens or Cost) and the range. **Today** is since local midnight (the same day as
   the Heatmap's and the Sparkline's "today"); **7 days** is the last 7×24
   hours; **Session** is the current 5-hour usage window.
   **Scope** limits the count to one model family (**Opus**, **Sonnet** or
   **Haiku**, matched in the model id) or to one **Project**, or keeps
   **All usage**. The projects are listed by the plugin, most recently
   used first, named after the repository folder; a Claude Code worktree
   (`<repo>/.claude/worktrees/<branch>`, `<repo>/.worktrees/<branch>`)
   counts as its repository. A scoped tile names the family or project
   under the number (`today · opus`, `today · my-repo`) and shows `0`, not
   "no data", when it has no usage in range.
3. It redraws every minute; tap the tile for an immediate refresh. A
   Session tile's tap uses the last fetched session reset time; when none
   has been fetched yet, it shows a rolling 5-hour window and redraws
   again right after with the fetched reset time.

## Using Peak Clock

1. Add a **Peak Clock** key on a keypad tile.
2. Set the peak hours (start and end, `HH:MM`; a window may cross
   midnight) and the days they apply to (weekdays by default; unticking
   every day means no peak hours at all).
3. The tile shows a 24-hour clock face with the peak hours marked in red
   (dimmed on a day they don't apply to) and a pointer at the current
   time, and under it **Peak** (red, with `ends in HH:MM`) or **Off-peak**
   (green, with `peak in HH:MM`, or `no peak set`). It redraws every
   minute; tap it to redraw now. The clock uses local time.
4. Inside the face, a faint blue ring shows when in the day you use
   Claude: one segment per local hour, shaded by the tokens used in that
   hour over the last 28 days (from the same transcripts as Metric Tile and
   Usage Heatmap), the busiest hour the most opaque. Hours with no usage
   are left out. The ring is refreshed with each minute's redraw, so it
   appears up to a minute after OpenDeck starts. With no transcripts there
   is no ring, and the clock never says "no data".

## Using Sessions

1. Add a **Sessions** key on a keypad tile. It has no settings.
2. The key shows how many Claude Code sessions are running (`3 sessions`),
   how long the longest-waiting one has waited for you (`12m`), and a
   breakdown (`1 wait · 1 idle · 1 busy`). A session waits for you when it
   is **idle** (done, your turn) or **waiting** (a permission prompt or
   another question is open). The time is copper under 2 minutes, yellow
   from 2 minutes and red from 10 (fixed marks). While every session is
   busy the key says `busy` in copper; with none open it says `0 sessions`.
3. A short press cycles through the sessions one at a time - name, time
   in its current state, the state and `2/3` - and then back to the
   summary. The choice isn't saved. Hold the key to re-read now; otherwise
   it re-reads every 15 seconds.

Where the data comes from: Claude Code keeps one `~/.claude/sessions/<pid>.json`
file per running process, with its state (`busy`, `idle` or `waiting`) and
when that last changed. The key shows those states as Claude Code writes
them. The format is undocumented and changes between Claude Code releases,
so the key reads it leniently: a file it can't make sense of counts as an
unknown session (`?`) instead of blanking the key. A file whose process has
exited (or, on Linux, whose pid now belongs to another process) is ignored. Only the
`*.json` files are read, never the `*.key` files next to them. If OpenDeck
runs in a sandbox with its own process ids (e.g. a Flatpak), the plugin
can't check which processes are alive and trusts the files.

## Colors & thresholds

Usage Gauge, Burn Rate, Session + Weekly and Usage Sparkline keys stay a
calm copper until usage crosses one of three marks you set per key (in %
used): **Watch** (default 50), **Risk** (75), **Critical** (90), each with
its own editable color. Marks must increase; if they don't, the defaults
are used (the settings page warns about it).

Usage Gauge, Session + Weekly and Usage Sparkline keys color by current
usage. Burn Rate colors by **pace**: whichever level is worse, current usage
or the usage you'd reach at reset if you keep burning at the current rate.

## Styles (keypad)

A Usage Gauge key can be drawn as a **Speedometer**, **Bar**, **Soft pill**,
**Open donut**, **Tracked donut**, or **Thin ring**; every style except the
speedometer shows your Watch/Risk/Critical marks as tick marks. A short
press moves to the next style, in that order, and the key remembers its
style across restarts. Hold the key for half a second to refresh instead. Dials keep the
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
   local Claude Code logs as Metric Tile — and a color. **Scope** limits
   it to one model family or one project as on Metric Tile; the caption
   then names it (`OPUS · 1.2M`, `my-repo · 1.2M`) instead of the view.
3. Each cell is one local calendar day, shaded relative to the busiest day
   shown; days with no usage stay grey. The caption shows the total.
4. A short press (key or dial) flips between **7 days** (today on the
   right) and a **4-week** grid (oldest week on top), and it's remembered.
   Hold half a second to refresh. On a dial the same chart fills the touch
   strip.

## Using Usage Sparkline

1. Add a **Usage Sparkline** key on a dial or a keypad tile and pick the
   window: **Session** or **Weekly**.
2. A short press (key or dial) switches between two series; hold to refresh:
   - **Trend** — % of limit over the current window.
   - **Today** — running increase since local midnight (e.g. `8.4pp`). If
     OpenDeck wasn't running (or couldn't read usage) around midnight, it
     counts from the day's first reading instead of guessing how much of
     the gap was yesterday's.

   Burn Rate's **Even burn** shows pace against an even burn.
3. The line takes the key's level color (**Colors & thresholds**). A new key
   says "collecting…" until at least two readings exist.

Anthropic's usage endpoint only reports the current percentages, so the
plugin records them itself: each successful poll whose numbers changed (plus the
first poll of each day, to mark midnight) is appended to `~/.local/state/opendeck-claude-usage/history.jsonl`
on Linux or `~/Library/Application Support/opendeck-claude-usage/history.jsonl` on macOS (or under
`$XDG_STATE_HOME` when that is set). It holds only session/weekly/extra-usage percentages and reset
times - no tokens, credentials or account data - and anything older than 8
days is dropped as it goes. The file and its folder are owner-only. Delete
the file any time to reset the history. If the folder can't be written, the
plugin logs one warning and keeps the history in memory until OpenDeck
restarts. A file with lines this version can't read (say, written by a newer
version before a downgrade) is left untouched rather than overwritten;
the lines it can read still load.

## Alerts (Usage Gauge, Usage Sparkline)

Tick **Flash when the window resets or reaches Critical** on a Usage Gauge
or Usage Sparkline key and it flashes once, with OpenDeck's own signs:
a check mark when its window (Session or Weekly) resets and a new one
starts, and a warning sign when usage crosses that key's own **Critical**
mark. It never flashes for the first reading after OpenDeck starts, never
twice for the same change, and never for a press's refresh. Alerts come
from the regular poll, so they can lag the real reset or crossing by up to
about 3 minutes. Off by default.

## Manual smoke-test checklist

The unit tests (Rust) and the settings-page tests (`node --test tests/pi/*.test.mjs`)
cover the logic; these are the things only a live OpenDeck + Stream Deck
session can show, on keys only (the dial strips are frozen and no longer
smoke-tested - see `CLAUDE.md`'s Scope section). Run them before cutting a
release and record the version you checked them on:

| Check | Last verified |
|---|---|
| Every action's key is readable (text not clipped or overlapping, colors right) for each style/view/series it offers. | not recorded |
| Keys redraw every minute without interaction, and a key that appears shows numbers straight away. | not recorded |
| A short press on Usage Gauge, Session + Weekly, Usage Heatmap or Usage Sparkline switches what it shows, and the choice survives an OpenDeck restart; two fast presses advance twice. | not recorded |
| Holding a key for half a second refreshes without switching. | not recorded |
| A Usage Gauge with **Flash when…** ticked shows OpenDeck's check mark once after its session resets, and its warning sign once when usage crosses its Critical mark; an unticked key shows neither. | not recorded |
| Editing a settings page while a key's style/layout/view/series was changed by a press doesn't undo the press. | not recorded |
| Removing a key causes no error in the plugin log on the next minute's redraw. | not recorded |
| With every Claude Code client closed past the access token's expiry, usage keys recover within about 10 minutes (plugin log: "Claude Code renewed the expired OAuth access token"). | not recorded |
| Usage keeps updating with only the Claude desktop app open (no CLI session, no editor extension). | not recorded |
| Metric Tile and Usage Heatmap totals match `ccusage` (or another transcript tool) for the same day. | not recorded |
| The Metric Tile and Usage Heatmap settings pages list your projects under **Scope** (worktrees folded into their repository), and a project-scoped key counts only that project. | not recorded |
| A Sessions key counts the open Claude Code sessions, turns yellow 2 minutes after one finishes or asks for permission (red after 10), and a short press steps through each session. | not recorded |
| Peak Clock's usage ring is darkest at the hours you actually work, and appears within a minute of OpenDeck starting. | not recorded |
| `~/.local/state/opendeck-claude-usage/history.jsonl` is created owner-only, only grows when usage changes, and survives a restart. | not recorded |

On a Mac (Apple Silicon), with the release bundle installed through OpenDeck:

- [ ] `xattr -l` on the installed `opendeck-claude-usage-aarch64-apple-darwin`
      shows no `com.apple.quarantine`. *(not yet verified)*
- [ ] Usage Gauge, Session + Weekly, Burn Rate and Usage Sparkline show usage
      from the Keychain login, with no Keychain prompt - also after updating the
      plugin. *(not yet verified)*
- [ ] Metric Tile, Usage Heatmap and Peak Clock read the transcripts in
      `~/.claude/projects`. *(not yet verified)*
- [ ] `~/Library/Application Support/opendeck-claude-usage/history.jsonl` is
      created owner-only. *(not yet verified)*
- [ ] `cargo test -- --ignored live_` passes on the Mac. *(not yet verified)*

## Development

```bash
cargo test --locked                          # unit tests (no live OpenDeck needed)
node --test tests/pi/*.test.mjs                  # settings-page (Property Inspector) tests
cargo test -- --ignored live_ --nocapture    # one real usage-API request + a scan of your transcripts
cargo build --release --locked
node build.mjs                               # assembles dist/<uuid>.sdPlugin from what was built
cp -r dist/com.jfms7s.claudeusage.sdPlugin ~/.config/opendeck/plugins/
# (macOS: ~/Library/Application\ Support/opendeck/plugins/)
# restart OpenDeck, then work through the smoke-test checklist above
```

Releases are cut by pushing a `vX.Y.Z` tag matching the version in
`Cargo.toml` and `assets/manifest.json`: the Release workflow tests, builds
both Linux architectures and the macOS one (on a macOS runner), and creates a
**draft** release with the bundle, its `SHA256SUMS` and a build-provenance
attestation. The draft is published by hand only after the smoke-test checklist
above has been run on its bundle and recorded in that release's smoke log
(`releases/vX.Y.Z-smoke.md` in the maintainer's project notes, kept outside this
repository); a release that skips a row says so in its notes. Before the release PR, `cargo update` refreshes
the lockfile (except `openaction`, which moves in its own PR) and open Dependabot
PRs are merged or closed.

## License

MIT — see [LICENSE](LICENSE).
