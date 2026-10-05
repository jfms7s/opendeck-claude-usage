use crate::metric::{MetricDisplay, MetricKind, MetricSource, RangeKind};
use crate::serde_util::or_default;
use crate::source::UsageSource;
use crate::source::console::ConsoleData;
use crate::source::logs::{LogEntry, LogUsageSource};
use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct MetricTileSettings {
    pub metric: MetricKind,
    pub range: RangeKind,
    pub refresh_seconds: u64,
    /// Unknown values fall back to Logs alone (see `serde_util`).
    #[serde(deserialize_with = "or_default")]
    pub source: MetricSource,
}

impl Default for MetricTileSettings {
    fn default() -> Self {
        Self {
            metric: MetricKind::default(),
            range: RangeKind::default(),
            refresh_seconds: 60,
            source: MetricSource::Logs,
        }
    }
}

struct TrackedInstance {
    settings: MetricTileSettings,
    next_due: Instant,
}

#[derive(Clone)]
pub struct MetricTileAction {
    log_source: Arc<LogUsageSource>,
    session_source: Arc<dyn UsageSource>,
    console: Arc<dyn ConsoleData>,
    registry: Arc<DashMap<String, TrackedInstance>>,
}

impl MetricTileAction {
    pub fn new(
        log_source: Arc<LogUsageSource>,
        session_source: impl UsageSource + 'static,
        console: Arc<dyn ConsoleData>,
    ) -> Self {
        Self {
            log_source,
            session_source: Arc::new(session_source),
            console,
            registry: Arc::new(DashMap::new()),
        }
    }

    /// Tracks (or re-tracks, overwriting prior settings) an instance,
    /// due immediately so it renders on the very next tick rather than
    /// waiting a full `refresh_seconds` interval.
    fn track(&self, instance_id: &str, settings: MetricTileSettings) {
        self.registry.insert(
            instance_id.to_string(),
            TrackedInstance {
                settings,
                next_due: Instant::now(),
            },
        );
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Log entries for `settings` - none are scanned for a Console tile.
    async fn entries_for(&self, settings: &MetricTileSettings) -> Arc<Vec<LogEntry>> {
        match settings.source {
            MetricSource::Logs => self.log_source.entries().await,
            MetricSource::Console => Arc::new(Vec::new()),
        }
    }

    /// The tile's numbers from the logs, as before the Console source.
    async fn log_display(
        &self,
        entries: &[LogEntry],
        settings: &MetricTileSettings,
    ) -> MetricDisplay {
        if entries.is_empty() {
            return crate::metric::error_display();
        }
        let session_resets_at = if settings.range == RangeKind::Session {
            self.session_source
                .read()
                .await
                .ok()
                .and_then(|s| s.session.resets_at)
        } else {
            None
        };
        crate::metric::build_metric_display(
            entries,
            settings.metric,
            settings.range,
            chrono::Utc::now(),
            session_resets_at,
        )
    }

    /// Renders one instance from already-scanned log entries (so a tick
    /// scans once for all its due instances), or from the Console cache
    /// for a Console-sourced tile - used by `will_appear` /
    /// `did_receive_settings`, `key_up` (tap-to-refresh) and the tick loop.
    async fn render(
        &self,
        instance: &Instance,
        entries: &[LogEntry],
        settings: &MetricTileSettings,
    ) -> OpenActionResult<()> {
        let display = match settings.source {
            MetricSource::Logs => self.log_display(entries, settings).await,
            MetricSource::Console => crate::metric::build_console_metric_display(
                self.console.snapshot().await.as_ref(),
                settings.metric,
                settings.range,
                chrono::Utc::now().date_naive(),
            ),
        };
        let title = format!(
            "{}\n{}\n{}",
            display.label, display.value_text, display.subtitle
        );
        instance.set_title(Some(title), None).await?;
        instance
            .set_image(
                Some(crate::metric_icon::build_metric_icon(display.accent_color)),
                None,
            )
            .await
    }

    /// Runs forever: every 1s, checks every registered instance's
    /// `next_due` and re-renders (then reschedules) only the ones that
    /// have elapsed. A 1s tick is cheap - just an `Instant` comparison
    /// per instance - and is what lets each instance honor its own
    /// `refresh_seconds` independently, unlike the other two tiles'
    /// single shared fixed-interval loop.
    pub async fn tick_loop(&self) {
        loop {
            self.tick_once().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn tick_once(&self) {
        let now = Instant::now();
        let snapshot: Vec<(String, Instant)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), e.next_due))
            .collect();

        // Scanned at most once per tick, however many instances are due.
        let mut scanned = None;
        for instance_id in due_instance_ids(&snapshot, now) {
            let Some(settings) = self.registry.get(&instance_id).map(|t| t.settings.clone()) else {
                continue; // removed between the snapshot and now
            };
            if let Some(mut tracked) = self.registry.get_mut(&instance_id) {
                tracked.next_due =
                    Instant::now() + Duration::from_secs(settings.refresh_seconds.max(1));
            }
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // instance disappeared between the snapshot and now
            };
            let entries = match settings.source {
                MetricSource::Console => Arc::new(Vec::new()),
                MetricSource::Logs => match &scanned {
                    Some(entries) => Arc::clone(entries),
                    None => Arc::clone(scanned.insert(self.log_source.entries().await)),
                },
            };
            if let Err(e) = self.render(&instance, &entries, &settings).await {
                log::warn!("metric tile render failed: {e}");
            }
        }
    }
}

/// Pure due-time filter, extracted from `tick_once` so the scheduling
/// decision is unit-testable without touching the DashMap/openaction
/// instance machinery.
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
        self.track(&instance.instance_id, settings.clone());
        let entries = self.entries_for(settings).await;
        self.render(instance, &entries, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.clone());
        let entries = self.entries_for(settings).await;
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

    /// A tap forces an immediate refresh of just that tile, same as the
    /// other two tiles' `key_up` - and does not reset its scheduled
    /// `next_due`, since a tap is a bonus refresh, not a reason to skip
    /// the next one.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        let entries = self.entries_for(settings).await;
        self.render(instance, &entries, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::api::ApiUsageSource;

    #[test]
    fn default_settings_are_tokens_today_at_sixty_seconds() {
        let settings = MetricTileSettings::default();
        assert_eq!(settings.metric, MetricKind::Tokens);
        assert_eq!(settings.range, RangeKind::Today);
        assert_eq!(settings.refresh_seconds, 60);
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // Same footgun UsageGaugeSettings' own test guards against: openaction
        // falls back to Default::default() when settings JSON fails to
        // deserialize at all, not just on missing fields - confirm both
        // paths land on the same value.
        let from_missing_keys: MetricTileSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, MetricTileSettings::default());
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
        action.track(
            "ctx1",
            MetricTileSettings {
                metric: MetricKind::Tokens,
                range: RangeKind::Today,
                refresh_seconds: 60,
                ..MetricTileSettings::default()
            },
        );
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

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let manifest_uuid = manifest["Actions"][2]["UUID"].as_str().unwrap();
        assert_eq!(manifest_uuid, <MetricTileAction as Action>::UUID);
    }

    use crate::metric::MetricSource;
    use crate::source::console::{ConsoleData, ConsoleError, ConsoleSnapshot};

    struct NoConsole;

    #[async_trait]
    impl ConsoleData for NoConsole {
        async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError> {
            Err(ConsoleError::NoKey)
        }
    }

    fn action() -> MetricTileAction {
        MetricTileAction::new(
            Arc::new(LogUsageSource::default()),
            ApiUsageSource::default(),
            Arc::new(NoConsole),
        )
    }

    #[test]
    fn existing_tiles_without_a_source_stay_on_logs() {
        let s: MetricTileSettings =
            serde_json::from_str(r#"{"metric": "cost", "range": "sevenday"}"#).unwrap();
        assert_eq!(s.source, MetricSource::Logs);
        assert_eq!(s.metric, MetricKind::Cost);
    }

    #[test]
    fn an_unknown_source_falls_back_without_resetting_the_rest() {
        let s: MetricTileSettings = serde_json::from_str(
            r#"{"source": "billing", "metric": "cost", "refresh_seconds": 30}"#,
        )
        .unwrap();
        assert_eq!(s.source, MetricSource::Logs);
        assert_eq!(s.metric, MetricKind::Cost);
        assert_eq!(s.refresh_seconds, 30);
    }

    #[test]
    fn console_source_round_trips() {
        let s: MetricTileSettings = serde_json::from_str(r#"{"source": "console"}"#).unwrap();
        assert_eq!(s.source, MetricSource::Console);
        assert_eq!(serde_json::to_value(&s).unwrap()["source"], "console");
    }

    #[test]
    fn property_inspector_offers_console_and_disables_session_for_it() {
        let html = include_str!("../assets/propertyInspector/metrictile.html");
        assert!(html.contains(r#"<option value="logs">"#));
        assert!(html.contains(r#"<option value="console">"#));
        assert!(html.contains("syncSource"));
        assert!(html.contains(".disabled = consoleSource"));
    }

    /// Opening the PI must show the saved range: a saved (console, session)
    /// tile draws "5H N/A", so the PI keeps Session selected and only moves
    /// off it - and saves - when the user switches the source.
    #[test]
    fn property_inspector_only_moves_off_session_when_the_user_switches_source() {
        let html = include_str!("../assets/propertyInspector/metrictile.html");
        let apply = &html[html.find("function applySettings").unwrap()..];
        let apply = &apply[..apply.find("\n\t\t}").unwrap()];
        assert!(apply.contains("syncSource(false);"), "{apply}");
        assert!(html.contains("syncSource(true);"));
        assert!(!html.contains("syncSource();"));
    }
}
