mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod metric;
mod metric_action;
mod metric_icon;
mod peak;
mod pricing;
mod source;

use action::UsageGaugeAction;
use clock_action::PeakClockAction;
use metric_action::MetricTileAction;
use openaction::{OpenActionResult, register_action, run};
use source::api::ApiUsageSource;
use source::cached::{CachePolicy, CachedUsageSource};
use source::logs::LogUsageSource;
use std::time::Duration;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    // One throttled in-memory source shared by both actions, so the gauges'
    // ~20s polls, taps and the Metric Tile's Session range together hit
    // Anthropic's usage endpoint at most once a minute - its rate limit is
    // per account and shared with Claude Code's own `/usage`, so failures
    // back off and keep showing the last good numbers for a while.
    let usage = CachedUsageSource::new(
        ApiUsageSource::default(),
        CachePolicy {
            min_interval: Duration::from_secs(60),
            max_backoff: Duration::from_secs(10 * 60),
            stale_after: Duration::from_secs(15 * 60),
        },
    );

    let action = UsageGaugeAction::new(usage.clone());
    let poller = action.clone();
    tokio::spawn(async move { poller.poll_loop().await });

    let clock = PeakClockAction::new();
    let ticker = clock.clone();
    tokio::spawn(async move { ticker.tick_loop().await });

    let metric_tile = MetricTileAction::new(LogUsageSource::default(), usage);
    let metric_ticker = metric_tile.clone();
    tokio::spawn(async move { metric_ticker.tick_loop().await });

    register_action(action).await;
    register_action(clock).await;
    register_action(metric_tile).await;
    run(std::env::args().collect()).await
}
