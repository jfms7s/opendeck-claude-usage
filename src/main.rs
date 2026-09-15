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
use source::file::FileUsageSource;
use source::logs::LogUsageSource;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    let action = UsageGaugeAction::new(FileUsageSource::default());
    let poller = action.clone();
    tokio::spawn(async move { poller.poll_loop().await });

    let clock = PeakClockAction::new();
    let ticker = clock.clone();
    tokio::spawn(async move { ticker.tick_loop().await });

    let metric_tile = MetricTileAction::new(LogUsageSource::default(), FileUsageSource::default());
    let metric_ticker = metric_tile.clone();
    tokio::spawn(async move { metric_ticker.tick_loop().await });

    register_action(action).await;
    register_action(clock).await;
    register_action(metric_tile).await;
    run(std::env::args().collect()).await
}
