mod action;
mod burn;
mod burn_action;
mod burn_icon;
mod clock_action;
mod clock_icon;
mod combo;
mod combo_action;
mod format;
mod heatmap;
mod heatmap_action;
mod history;
mod hub;
mod level;
mod metric;
mod metric_action;
mod metric_icon;
mod pace;
mod peak;
mod press;
mod pricing;
mod source;
mod sparkline;
mod sparkline_action;
mod style;
mod styles;
mod surface;
#[cfg(test)]
mod test_support;
mod tile;

use action::UsageGaugeAction;
use burn_action::BurnRateAction;
use clock_action::PeakClockAction;
use combo_action::ComboAction;
use heatmap_action::HeatmapAction;
use history::HistoryStore;
use hub::UsageHub;
use metric_action::MetricTileAction;
use openaction::{OpenActionResult, register_action, run};
use source::api::ApiUsageSource;
use source::cached::{CachePolicy, CachedUsageSource};
use source::logs::LogUsageSource;
use sparkline_action::SparklineAction;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    // One throttled in-memory source shared by every action, so the gauges'
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

    // Every usage-driven action (gauge, burn rate, combo, sparkline) registers its instances
    // in this one hub, so a single poll loop serves them all.
    // Recorded %-of-limit readings for Usage Sparkline, kept in a small
    // file under ~/.local/state so trends survive restarts.
    let history = HistoryStore::load(HistoryStore::default_path(), chrono::Utc::now());
    let hub = UsageHub::new(usage.clone(), history);
    tokio::spawn(hub.clone().poll_loop());

    let action = UsageGaugeAction::new(hub.clone());
    let burn_rate = BurnRateAction::new(hub.clone());
    let combo = ComboAction::new(hub.clone());
    let sparkline = SparklineAction::new(hub.clone());

    let clock = PeakClockAction::new();
    let ticker = clock.clone();
    tokio::spawn(async move { ticker.tick_loop().await });

    // One log scanner (and mtime cache) shared by every log-reading action.
    let logs = Arc::new(LogUsageSource::default());
    let metric_tile = MetricTileAction::new(logs.clone(), usage);
    let metric_ticker = metric_tile.clone();
    tokio::spawn(async move { metric_ticker.tick_loop().await });

    let heatmap = HeatmapAction::new(logs);
    let heatmap_ticker = heatmap.clone();
    tokio::spawn(async move { heatmap_ticker.tick_loop().await });

    register_action(action).await;
    register_action(clock).await;
    register_action(metric_tile).await;
    register_action(burn_rate).await;
    register_action(combo).await;
    register_action(heatmap).await;
    register_action(sparkline).await;
    run(std::env::args().collect()).await
}
