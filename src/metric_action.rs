//! Metric Tile: total Tokens or Cost over Today / 7 days / the current
//! session - estimated from Claude Code's transcripts, or billed from the
//! Console Admin API - each tile on its own refresh interval. Keypad only;
//! a tap refreshes.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::metric::{
    MetricDisplay, MetricKind, MetricSource, RangeKind, build_console_metric_display,
    build_metric_display, error_display,
};
use crate::metric_icon::build_metric_icon;
use crate::settings::lenient;
use crate::source::cached::SharedUsage;
use crate::source::console::ConsoleData;
use crate::source::logs::{LogEntry, LogUsageSource};
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::park_while_empty;

/// The PI offers 5..=3600 seconds; anything stored outside that is clamped.
const MIN_REFRESH: u64 = 5;
const MAX_REFRESH: u64 = 3600;

/// Each field falls back alone (see `settings::lenient`).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MetricTileSettings {
    #[serde(default, deserialize_with = "lenient")]
    pub metric: MetricKind,
    #[serde(default, deserialize_with = "lenient")]
    pub range: RangeKind,
    #[serde(default = "default_refresh", deserialize_with = "lenient_refresh")]
    pub refresh_seconds: u64,
    /// Existing tiles have no `source`, so they stay on the logs.
    #[serde(default, deserialize_with = "lenient")]
    pub source: MetricSource,
}

fn default_refresh() -> u64 {
    60
}

fn lenient_refresh<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_u64().unwrap_or_else(default_refresh))
}

impl Default for MetricTileSettings {
    fn default() -> Self {
        Self {
            metric: MetricKind::default(),
            range: RangeKind::default(),
            refresh_seconds: default_refresh(),
            source: MetricSource::default(),
        }
    }
}

impl MetricTileSettings {
    fn interval(&self) -> Duration {
        Duration::from_secs(self.refresh_seconds.clamp(MIN_REFRESH, MAX_REFRESH))
    }
}

struct TrackedInstance {
    settings: MetricTileSettings,
    next_due: Instant,
}

#[derive(Clone)]
pub struct MetricTileAction {
    log_source: Arc<LogUsageSource>,
    /// Only read for the Session range's reset time - through the plugin's
    /// one shared, throttled usage cache.
    usage: Arc<dyn SharedUsage>,
    /// Billed numbers for Console-sourced tiles, through their own cache.
    console: Arc<dyn ConsoleData>,
    registry: Arc<DashMap<String, TrackedInstance>>,
    frames: Arc<Frames>,
    /// Pinged on every (re)track, so the tick loop re-plans its next
    /// wake-up (or leaves its park).
    wake: Arc<Notify>,
}

impl MetricTileAction {
    pub fn new(
        log_source: Arc<LogUsageSource>,
        usage: Arc<dyn SharedUsage>,
        console: Arc<dyn ConsoleData>,
    ) -> Self {
        Self {
            log_source,
            usage,
            console,
            registry: Arc::new(DashMap::new()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
        }
    }

    /// Tracks (or re-tracks, overwriting prior settings) an instance that
    /// was just drawn, so it's next due one interval from now.
    fn track(&self, instance_id: &str, settings: MetricTileSettings) {
        let next_due = Instant::now() + settings.interval();
        self.registry.insert(
            instance_id.to_string(),
            TrackedInstance { settings, next_due },
        );
        self.wake.notify_one();
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
        self.frames.forget(instance_id);
    }

    /// What one instance shows: from the Console cache for a Console tile,
    /// otherwise from already-scanned entries (so a tick scans once for all
    /// its due instances).
    async fn display(&self, entries: &[LogEntry], settings: &MetricTileSettings) -> MetricDisplay {
        if settings.source == MetricSource::Console {
            return build_console_metric_display(
                self.console.snapshot().await.as_ref(),
                settings.metric,
                settings.range,
                chrono::Utc::now().date_naive(),
            );
        }
        if entries.is_empty() {
            return error_display();
        }
        let session_resets_at = if settings.range == RangeKind::Session {
            self.usage
                .read()
                .await
                .ok()
                .and_then(|s| s.session.resets_at)
        } else {
            None
        };
        build_metric_display(
            entries,
            settings.metric,
            settings.range,
            chrono::Local::now(),
            session_resets_at,
        )
    }

    pub(crate) async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        entries: &[LogEntry],
        settings: &MetricTileSettings,
    ) -> OpenActionResult<()> {
        let display = self.display(entries, settings).await;
        self.frames
            .push(surface, Output::Image(build_metric_icon(&display)))
            .await
    }

    /// When the soonest instance is due, if any is tracked.
    fn next_deadline(&self) -> Option<Instant> {
        self.registry.iter().map(|t| t.next_due).min()
    }

    /// Runs forever: sleeps until the soonest instance is due, renders the
    /// due ones (each on its own `refresh_seconds`), and re-plans whenever
    /// an instance is tracked. Parks while none is visible. Spawned once
    /// from `main.rs`, supervised.
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.registry.is_empty(), &self.wake).await;
            let Some(deadline) = self.next_deadline() else {
                continue;
            };
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    self.tick_once(openaction::get_instance).await;
                }
                () = self.wake.notified() => {}
            }
        }
    }

    async fn tick_once<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        let now = Instant::now();
        let snapshot: Vec<(String, Instant)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), e.next_due))
            .collect();
        let due = due_instance_ids(&snapshot, now);
        if due.is_empty() {
            return;
        }
        for id in &due {
            if let Some(mut tracked) = self.registry.get_mut(id) {
                tracked.next_due = now + tracked.settings.interval();
            }
        }
        // Scanned once per tick, however many instances are due.
        let entries = self.log_source.entries().await;
        let entries = &entries;
        for_each_tracked(
            due,
            |id| self.registry.get(id).map(|t| t.settings.clone()),
            lookup,
            |surface, settings| async move {
                if let Err(e) = self.render(&surface, entries, &settings).await {
                    log::warn!("metric tile render failed: {e}");
                }
            },
        )
        .await;
    }
}

/// Pure due-time filter, so the scheduling decision is unit-testable.
fn due_instance_ids(tracked: &[(String, Instant)], now: Instant) -> Vec<String> {
    tracked
        .iter()
        .filter(|(_, due)| *due <= now)
        .map(|(id, _)| id.clone())
        .collect()
}

#[async_trait]
impl Action for MetricTileAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.metrictile";
    type Settings = MetricTileSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.frames.forget(&instance.instance_id);
        self.track(&instance.instance_id, settings.clone());
        let entries = self.log_source.entries().await;
        self.render(instance, &entries, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.clone());
        let entries = self.log_source.entries().await;
        self.render(instance, &entries, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    /// A tap refreshes just that tile - and doesn't move its next scheduled
    /// refresh, since a tap is a bonus, not a reason to skip one.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let entries = self.log_source.entries().await;
        self.render(instance, &entries, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::cached::test_support::shared;
    use crate::source::console::{ConsoleDay, ConsoleError, ConsoleSnapshot};
    use crate::source::{MonthlyUsage, UsageSnapshot, UsageSource, UsageSourceError, WindowUsage};
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::manifest_entry;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts reads of the usage API.
    #[derive(Clone, Default)]
    struct Counting(Arc<AtomicUsize>);

    #[async_trait]
    impl UsageSource for Counting {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(UsageSnapshot {
                session: WindowUsage {
                    percent: 10.0,
                    resets_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                },
                weekly: WindowUsage {
                    percent: 10.0,
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

    fn action_with(source: Counting) -> MetricTileAction {
        MetricTileAction::new(
            Arc::new(LogUsageSource::new("/nonexistent".into())),
            shared(source),
            Arc::new(NoConsole),
        )
    }

    fn action() -> MetricTileAction {
        action_with(Counting::default())
    }

    fn entry() -> LogEntry {
        LogEntry {
            timestamp: chrono::Utc::now(),
            model: "claude-sonnet-5".into(),
            input_tokens: 1500,
            ..LogEntry::default()
        }
    }

    #[test]
    fn default_settings_are_tokens_today_at_sixty_seconds() {
        let settings = MetricTileSettings::default();
        assert_eq!(settings.metric, MetricKind::Tokens);
        assert_eq!(settings.range, RangeKind::Today);
        assert_eq!(settings.refresh_seconds, 60);
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // openaction falls back to Default::default() when settings JSON
        // fails to deserialize at all, not just on missing fields - confirm
        // both paths land on the same value.
        let from_missing_keys: MetricTileSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, MetricTileSettings::default());
    }

    /// KI-10 class: an unknown metric falls back alone.
    #[test]
    fn a_bad_field_keeps_the_others() {
        let s: MetricTileSettings =
            serde_json::from_str(r#"{"metric":"bogus","range":"sevenday","refresh_seconds":"30"}"#)
                .unwrap();
        assert_eq!(s.metric, MetricKind::Tokens);
        assert_eq!(s.range, RangeKind::SevenDay);
        assert_eq!(s.refresh_seconds, 60);
    }

    #[test]
    fn the_interval_is_clamped_to_what_the_pi_offers() {
        let at = |refresh_seconds| MetricTileSettings {
            refresh_seconds,
            ..MetricTileSettings::default()
        };
        assert_eq!(at(0).interval(), Duration::from_secs(5));
        assert_eq!(at(30).interval(), Duration::from_secs(30));
        assert_eq!(at(1_000_000).interval(), Duration::from_secs(3600));
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = action();
        action.track("ctx1", MetricTileSettings::default());
        assert!(action.registry.contains_key("ctx1"));

        action.untrack("ctx1");
        assert!(!action.registry.contains_key("ctx1"));
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_settings() {
        let action = action();
        action.track("ctx1", MetricTileSettings::default());
        action.track(
            "ctx1",
            MetricTileSettings {
                metric: MetricKind::Cost,
                range: RangeKind::Session,
                refresh_seconds: 30,
                ..MetricTileSettings::default()
            },
        );
        let tracked = action.registry.get("ctx1").unwrap();
        assert_eq!(tracked.settings.metric, MetricKind::Cost);
        assert_eq!(tracked.settings.refresh_seconds, 30);
    }

    #[test]
    fn due_instance_ids_includes_only_elapsed_or_exactly_due_instances() {
        let now = Instant::now();
        let tracked = vec![
            ("already_due".to_string(), now - Duration::from_secs(1)),
            ("not_due_yet".to_string(), now + Duration::from_secs(30)),
            ("exactly_due".to_string(), now),
        ];
        let mut due = due_instance_ids(&tracked, now);
        due.sort();
        assert_eq!(
            due,
            vec!["already_due".to_string(), "exactly_due".to_string()]
        );
    }

    /// Each tile is drawn on its own interval: a 30s tile twice a minute, a
    /// 60s tile once.
    #[tokio::test(start_paused = true)]
    async fn each_instance_is_rendered_on_its_own_interval() {
        let action = action();
        let at = |refresh_seconds| MetricTileSettings {
            refresh_seconds,
            ..MetricTileSettings::default()
        };
        action.track("fast", at(30));
        action.track("slow", at(60));
        let (fast, slow) = (
            FakeSurface::new("fast", true),
            FakeSurface::new("slow", true),
        );
        let mut drawn = Vec::new();
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(15)).await;
            action
                .tick_once(|id| {
                    drawn.push(id.clone());
                    std::future::ready(Some(if id == "fast" {
                        fast.clone()
                    } else {
                        slow.clone()
                    }))
                })
                .await;
        }
        drawn.sort();
        assert_eq!(drawn, ["fast", "fast", "slow"]);
    }

    /// The Session range reads the shared, throttled usage cache - several
    /// Session tiles refreshing never multiply API requests.
    #[tokio::test(start_paused = true)]
    async fn session_tiles_share_one_throttled_usage_read() {
        let source = Counting::default();
        let action = action_with(source.clone());
        let session = MetricTileSettings {
            range: RangeKind::Session,
            ..MetricTileSettings::default()
        };
        for id in ["a", "b", "c"] {
            action
                .render(&FakeSurface::new(id, true), &[entry()], &session)
                .await
                .unwrap();
        }
        assert_eq!(source.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_log_data_shows_no_data_without_reading_usage() {
        let source = Counting::default();
        let action = action_with(source.clone());
        let key = FakeSurface::new("a", true);
        let session = MetricTileSettings {
            range: RangeKind::Session,
            ..MetricTileSettings::default()
        };
        action.render(&key, &[], &session).await.unwrap();
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            key.frames(),
            vec![Output::Image(build_metric_icon(&error_display()))]
        );
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<MetricTileAction as Action>::UUID);
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/metrictile.html"
        );
    }

    struct NoConsole;

    #[async_trait]
    impl ConsoleData for NoConsole {
        async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError> {
            Err(ConsoleError::NoKey)
        }
    }

    struct FixedConsole(ConsoleSnapshot);

    #[async_trait]
    impl ConsoleData for FixedConsole {
        async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn existing_tiles_without_a_source_stay_on_logs() {
        let s: MetricTileSettings =
            serde_json::from_str(r#"{"metric": "cost", "range": "sevenday"}"#).unwrap();
        assert_eq!(s.source, MetricSource::Logs);
        assert_eq!(s.metric, MetricKind::Cost);
    }

    #[test]
    fn console_source_round_trips() {
        let s: MetricTileSettings = serde_json::from_str(r#"{"source": "console"}"#).unwrap();
        assert_eq!(s.source, MetricSource::Console);
        assert_eq!(serde_json::to_value(&s).unwrap()["source"], "console");
    }

    /// A Console tile draws the billed number from the Console cache - and
    /// never reads the logs' usage source, even on the Session range.
    #[tokio::test]
    async fn a_console_tile_draws_billed_numbers() {
        let today = chrono::Utc::now().date_naive();
        let snapshot = ConsoleSnapshot {
            days: vec![ConsoleDay {
                date: today,
                cost_dollars: 12.34,
                tokens: 0,
            }],
        };
        let usage = Counting::default();
        let action = MetricTileAction::new(
            Arc::new(LogUsageSource::new("/nonexistent".into())),
            shared(usage.clone()),
            Arc::new(FixedConsole(snapshot.clone())),
        );
        let settings = MetricTileSettings {
            metric: MetricKind::Cost,
            source: MetricSource::Console,
            ..MetricTileSettings::default()
        };
        let tile = FakeSurface::new("tile", true);
        action.render(&tile, &[], &settings).await.unwrap();
        let expected =
            build_console_metric_display(Ok(&snapshot), MetricKind::Cost, RangeKind::Today, today);
        assert_eq!(expected.value_text, "$12.34");
        assert_eq!(
            tile.frames(),
            vec![Output::Image(build_metric_icon(&expected))]
        );

        let session = MetricTileSettings {
            range: RangeKind::Session,
            ..settings
        };
        action.render(&tile, &[], &session).await.unwrap();
        assert_eq!(usage.0.load(Ordering::SeqCst), 0);
    }
}
