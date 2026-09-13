# OpenDeck Claude Usage

An [OpenDeck](https://github.com/nekename/OpenDeck) plugin with one action,
**Usage Gauge**, for a Stream Deck dial: its touch strip shows a live bar,
percent used, and time until reset for one of Claude's usage windows -
**Session** (5 hour), **Weekly** (7 day), or **Monthly** (pay-as-you-go extra
usage spend, if enabled on your account).

Built for a Stream Deck XL+'s 6 dials and 1200x100 touch strip - assign up
to three dials (one per window) for an always-visible usage readout.

## Where the data comes from

Claude Code itself maintains `~/.claude/statusline-usage.json`, refreshed
whenever its statusLine hook fires during an active session. This plugin
reads that file every ~20 seconds; it does not call any API directly. If
you've never run Claude Code, or haven't in a while, the file may not exist
or may be stale - each dial then shows a "no data" state rather than a
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

## Installing

Download the latest `.streamDeckPlugin` from
[Releases](https://github.com/jfms7s/opendeck-claude-usage/releases), then
either double-click it (if your file manager associates the extension with
OpenDeck) or unzip it into `~/.config/opendeck/plugins/` and restart OpenDeck
(plugins are only loaded at startup).

## Using a dial

1. Add a **Usage Gauge** key on a dial.
2. Pick which window to show: Session, Weekly, or Monthly (extra usage).
3. The touch strip updates automatically roughly every 20 seconds; press the
   dial for an immediate refresh.

## Manual smoke-test checklist

Run this against a live OpenDeck + Stream Deck XL+ session before cutting a
release. None of these have been verified on real hardware as of this
version - the manual smoke test was deliberately not run in this
development environment, which has no OpenDeck/Stream Deck to test against:

- [ ] Session/Weekly/Monthly dials each show a percent, bar, and detail line
      shortly after appearing. *(not yet verified)*
- [ ] Display refreshes within ~20s without any interaction. *(not yet verified)*
- [ ] Pressing a dial refreshes it immediately. *(not yet verified)*
- [ ] Monthly dial shows "not enabled" cleanly when extra usage is off. *(not yet verified)*
- [ ] Removing a dial doesn't error on the next poll tick. *(not yet verified)*

## Development

```bash
cargo test                                   # unit tests (no live OpenDeck needed)
cargo build --release --target <triple>
node build.mjs <triple>                      # assembles dist/<uuid>.sdPlugin
cp -r dist/com.jfms7s.claudeusage.sdPlugin ~/.config/opendeck/plugins/
# restart OpenDeck, then work through the smoke-test checklist above
```

## License

MIT — see [LICENSE](LICENSE).
