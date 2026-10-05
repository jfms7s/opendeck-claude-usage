mod burn;
mod burn_action;
mod burn_icon;
mod clock_action;
mod clock_icon;
mod combo;
mod combo_action;
mod format;
mod gauge_action;
mod gauge_style;
mod heatmap;
mod heatmap_action;
mod history;
mod hub;
mod hub_action;
mod level;
mod metric;
mod metric_action;
mod metric_icon;
mod pace;
mod peak;
mod press;
mod pricing;
mod settings;
mod source;
mod sparkline;
mod sparkline_action;
mod styles;
mod surface;
mod tasks;
#[cfg(test)]
mod test_support;
mod tile;

use std::sync::Arc;
use std::time::Duration;

use burn_action::BurnRateAction;
use clock_action::PeakClockAction;
use combo_action::ComboAction;
use gauge_action::UsageGaugeAction;
use heatmap_action::HeatmapAction;
use history::HistoryStore;
use hub::UsageHub;
use metric_action::MetricTileAction;
use openaction::{OpenActionResult, register_action, run};
use source::UsageSource;
use source::api::ApiUsageSource;
use source::cached::{CachePolicy, CachedUsageSource, SharedUsage};
use source::logs::LogUsageSource;
use sparkline_action::SparklineAction;
use tasks::spawn_supervised;

/// The usage endpoint's rate limit is per account and shared with Claude
/// Code's own `/usage`, so the plugin asks rarely: every 3 minutes (±10%,
/// so it doesn't stay in step with anything else polling), backing off to
/// 6 then 10 minutes on failure, and showing the last good numbers for up
/// to 15 minutes before "no data".
const USAGE_POLICY: CachePolicy = CachePolicy {
    min_interval: Duration::from_secs(3 * 60),
    jitter: 0.1,
    max_backoff: Duration::from_secs(10 * 60),
    stale_after: Duration::from_secs(15 * 60),
};

/// Every action, wired to the plugin's shared state.
struct Actions {
    hub: Arc<UsageHub>,
    gauge: UsageGaugeAction,
    burn_rate: BurnRateAction,
    combo: ComboAction,
    sparkline: SparklineAction,
    clock: PeakClockAction,
    metric_tile: MetricTileAction,
    heatmap: HeatmapAction,
}

/// Builds every action around one throttled usage cache and one transcript
/// scanner. This is the only place a usage source is wrapped, so every
/// action - the hub's four and Metric Tile's Session range - shares one
/// request budget (tested below).
fn wire(
    api: impl UsageSource + 'static,
    history: Arc<HistoryStore>,
    logs: LogUsageSource,
) -> Actions {
    let usage: Arc<dyn SharedUsage> = Arc::new(CachedUsageSource::new(api, USAGE_POLICY));
    let hub = UsageHub::new(Arc::clone(&usage), history);
    let logs = Arc::new(logs);
    Actions {
        gauge: UsageGaugeAction::new(Arc::clone(&hub)),
        burn_rate: BurnRateAction::new(Arc::clone(&hub)),
        combo: ComboAction::new(Arc::clone(&hub)),
        sparkline: SparklineAction::new(Arc::clone(&hub)),
        clock: PeakClockAction::new(),
        metric_tile: MetricTileAction::new(Arc::clone(&logs), usage),
        heatmap: HeatmapAction::new(logs),
        hub,
    }
}

// Two workers are plenty: the plugin's work is a few renders a minute, and
// file I/O goes to the blocking pool.
#[tokio::main(worker_threads = 2)]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    // Recorded %-of-limit readings for Usage Sparkline, kept in a small
    // file under ~/.local/state so trends survive restarts. Loading reads
    // and rewrites the file - keep that off the async threads.
    let history = tokio::task::spawn_blocking(|| {
        HistoryStore::load(HistoryStore::default_path(), chrono::Utc::now())
    })
    .await
    .expect("loading usage history panicked");

    let actions = wire(
        ApiUsageSource::default(),
        history,
        LogUsageSource::default(),
    );

    // Each background loop restarts (and logs why) if it ever panics.
    let hub = Arc::clone(&actions.hub);
    spawn_supervised("usage poll loop", move || Arc::clone(&hub).poll_loop());
    let clock = actions.clock.clone();
    spawn_supervised("peak clock tick loop", move || clock.clone().tick_loop());
    let metric_tile = actions.metric_tile.clone();
    spawn_supervised("metric tile tick loop", move || {
        metric_tile.clone().tick_loop()
    });
    let heatmap = actions.heatmap.clone();
    spawn_supervised("heatmap tick loop", move || heatmap.clone().tick_loop());

    register_action(actions.gauge).await;
    register_action(actions.clock).await;
    register_action(actions.metric_tile).await;
    register_action(actions.burn_rate).await;
    register_action(actions.combo).await;
    register_action(actions.heatmap).await;
    register_action(actions.sparkline).await;
    run(std::env::args().collect()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub_action::HubSettings;
    use crate::metric::RangeKind;
    use crate::metric_action::MetricTileSettings;
    use crate::source::{MonthlyUsage, UsageSnapshot, UsageSourceError, WindowUsage};
    use crate::surface::test_support::FakeSurface;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Default)]
    struct Counting(Arc<AtomicUsize>);

    #[async_trait]
    impl UsageSource for Counting {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(UsageSnapshot {
                session: WindowUsage {
                    percent: 1.0,
                    resets_at: None,
                },
                weekly: WindowUsage {
                    percent: 1.0,
                    resets_at: None,
                },
                monthly: MonthlyUsage {
                    enabled: false,
                    percent: None,
                    used_dollars: None,
                    limit_dollars: None,
                },
            })
        }
    }

    /// The rule CLAUDE.md calls critical: every action that reads usage
    /// goes through one cache. Wiring the hub and Metric Tile to separate
    /// (or no) caches fails this.
    #[tokio::test]
    async fn every_usage_reader_shares_one_request_budget() {
        let api = Counting::default();
        let actions = wire(
            api.clone(),
            HistoryStore::in_memory(),
            LogUsageSource::new("/nonexistent".into()),
        );
        let dial = FakeSurface::dial("gauge");
        actions
            .hub
            .refresh_one(&dial, &gauge_action::UsageGaugeSettings::default().view())
            .await
            .unwrap();
        let session = MetricTileSettings {
            range: RangeKind::Session,
            ..MetricTileSettings::default()
        };
        let entry = crate::source::logs::LogEntry {
            timestamp: chrono::Utc::now(),
            ..Default::default()
        };
        actions
            .metric_tile
            .render(&FakeSurface::new("tile", true), &[entry], &session)
            .await
            .unwrap();
        assert_eq!(api.0.load(Ordering::SeqCst), 1);
    }

    /// Every file the manifest points at ships in `assets/` (icons are named
    /// without their extension), so a renamed asset fails here rather than
    /// in a release.
    #[test]
    fn every_manifest_path_exists_in_assets() {
        let manifest = crate::test_support::manifest();
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let image_exists = |name: &str| {
            ["png", "svg"]
                .iter()
                .any(|ext| assets.join(format!("{name}.{ext}")).exists())
        };
        for key in ["Icon", "CategoryIcon"] {
            let name = manifest[key].as_str().unwrap();
            assert!(image_exists(name), "{key}: {name}");
        }
        for action in manifest["Actions"].as_array().unwrap() {
            let uuid = action["UUID"].as_str().unwrap();
            let icon = action["Icon"].as_str().unwrap();
            assert!(image_exists(icon), "{uuid} Icon: {icon}");
            for state in action["States"].as_array().into_iter().flatten() {
                let image = state["Image"].as_str().unwrap();
                assert!(image_exists(image), "{uuid} state image: {image}");
            }
            for path in [
                &action["PropertyInspectorPath"],
                &action["Encoder"]["layout"],
            ] {
                if let Some(path) = path.as_str() {
                    assert!(assets.join(path).exists(), "{uuid}: {path}");
                }
            }
        }
    }

    #[test]
    fn the_usage_api_is_asked_at_most_every_three_minutes() {
        assert!(
            USAGE_POLICY.min_interval.mul_f64(1.0 - USAGE_POLICY.jitter)
                >= Duration::from_secs(160)
        );
    }
}
