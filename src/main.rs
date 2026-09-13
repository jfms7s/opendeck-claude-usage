mod action;
mod format;
mod icon;
mod source;

use action::UsageGaugeAction;
use openaction::{OpenActionResult, register_action, run};
use source::file::FileUsageSource;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    let action = UsageGaugeAction::new(FileUsageSource::default());
    let poller = action.clone();
    tokio::spawn(async move { poller.poll_loop().await });

    register_action(action).await;
    run(std::env::args().collect()).await
}
