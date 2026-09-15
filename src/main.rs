mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod peak;
mod pricing;
mod source;

use action::UsageGaugeAction;
use clock_action::PeakClockAction;
use openaction::{OpenActionResult, register_action, run};
use source::file::FileUsageSource;

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

    register_action(action).await;
    register_action(clock).await;
    run(std::env::args().collect()).await
}
