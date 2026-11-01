//! Metric Tile: total Tokens or estimated Cost from Claude Code's
//! transcripts over Today / 7 days / the current session, redrawn every
//! minute like the other actions. Keypad only; a tap refreshes.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::metric::{
    MetricDisplay, MetricKind, ModelScope, RangeKind, build_scoped_metric_display, error_display,
};
use crate::metric_icon::build_metric_icon;
use crate::settings::lenient;
use crate::source::cached::SharedUsage;
use crate::source::logs::{LogEntry, LogUsageSource};
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};

/// Each field falls back alone (see `settings::lenient`). A stored
/// `refresh_seconds` (cut in 0.20) is ignored.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq)]
pub struct MetricTileSettings {
    #[serde(default, deserialize_with = "lenient")]
    pub metric: MetricKind,
    #[serde(default, deserialize_with = "lenient")]
    pub range: RangeKind,
    /// Which models count (all, or one family).
    #[serde(default, deserialize_with = "lenient")]
    pub scope: ModelScope,
}

/// Where the Session range takes its reset time from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageRead {
    /// Through the throttled cache, which may fetch - only the tick loop,
    /// which runs off openaction's serial event loop.
    Fetch,
    /// Only what the cache already holds, never a fetch - event handlers,
    /// so a slow usage request can't stall every key on the deck.
    Cached,
}

#[derive(Clone)]
pub struct MetricTileAction {
    log_source: Arc<LogUsageSource>,
    /// Only read for the Session range's reset time - through the plugin's
    /// one shared, throttled usage cache.
    usage: Arc<dyn SharedUsage>,
    /// Each visible instance's settings - what the tick redraws.
    registry: Arc<DashMap<String, MetricTileSettings>>,
    frames: Arc<Frames>,
    /// Pinged on every (re)track, so a parked tick loop resumes.
    wake: Arc<Notify>,
    /// Pinged by an event that drew without data it couldn't wait for (no
    /// transcript scan yet, or no cached session reset time): the tick
    /// loop then redraws now instead of at the next minute.
    refresh_soon: Arc<Notify>,
}

impl MetricTileAction {
    pub fn new(log_source: Arc<LogUsageSource>, usage: Arc<dyn SharedUsage>) -> Self {
        Self {
            log_source,
            usage,
            registry: Arc::new(DashMap::new()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
            refresh_soon: Arc::new(Notify::new()),
        }
    }

    /// Tracks (or re-tracks, overwriting prior settings) an instance.
    fn track(&self, instance_id: &str, settings: MetricTileSettings) {
        self.registry.insert(instance_id.to_string(), settings);
        self.wake.notify_one();
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
        self.frames.forget(instance_id);
    }

    /// What one instance shows, from already-scanned entries (so a tick
    /// scans once for every tile).
    async fn display(
        &self,
        entries: &[LogEntry],
        settings: &MetricTileSettings,
        read: UsageRead,
    ) -> MetricDisplay {
        if entries.is_empty() {
            return error_display();
        }
        // Without a reset time the Session range falls back to a rolling
        // 5-hour window (see `build_metric_display`).
        let session_resets_at = if settings.range == RangeKind::Session {
            let snapshot = match read {
                UsageRead::Fetch => self.usage.read().await.ok(),
                UsageRead::Cached => self.usage.peek().and_then(Result::ok),
            };
            snapshot.and_then(|s| s.session.resets_at)
        } else {
            None
        };
        build_scoped_metric_display(
            entries,
            settings.metric,
            settings.range,
            settings.scope,
            chrono::Local::now(),
            session_resets_at,
        )
    }

    pub(crate) async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        entries: &[LogEntry],
        settings: &MetricTileSettings,
        read: UsageRead,
    ) -> OpenActionResult<()> {
        let display = self.display(entries, settings, read).await;
        self.frames
            .push(surface, Output::Image(build_metric_icon(&display)))
            .await
    }

    /// Draws one instance for an event (appear, new settings, tap) without
    /// waiting on anything slow, since openaction handles events one at a
    /// time: the Session range takes its reset time from the usage cache
    /// without fetching, and until the first transcript scan (seconds, on
    /// a big `~/.claude/projects`) has completed the tile draws what is
    /// already scanned (nothing: NO DATA) instead of waiting for it.
    async fn redraw<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &MetricTileSettings,
    ) -> OpenActionResult<()> {
        let scanned = self.log_source.scanned();
        let entries = if scanned {
            // A warm rescan only tails the files.
            self.log_source.entries().await
        } else {
            self.log_source.peek()
        };
        let drawn = self
            .render(surface, &entries, settings, UsageRead::Cached)
            .await;
        // Either half missing (no scan yet, or no usable usage snapshot
        // for the Session range): ask the tick loop to redraw now - it
        // scans and reads off the event loop, once for every tile, and the
        // usage cache bounds what a read costs.
        let usage_cold = settings.range == RangeKind::Session
            && self.usage.peek().and_then(Result::ok).is_none();
        if !scanned || usage_cold {
            self.refresh_soon.notify_one();
        }
        drawn
    }

    /// Runs forever: just after every minute boundary (or at once when an
    /// event asked for it) scans the transcripts once and redraws every
    /// visible tile. Parks while none is visible. Spawned once from
    /// `main.rs`, supervised.
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.registry.is_empty(), &self.wake).await;
            tokio::select! {
                () = sleep_to_next_minute() => {}
                () = self.refresh_soon.notified() => {}
            }
            self.tick_once(openaction::get_instance).await;
        }
    }

    async fn tick_once<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        let ids: Vec<String> = self.registry.iter().map(|e| e.key().clone()).collect();
        if ids.is_empty() {
            return;
        }
        // Scanned once per tick, however many tiles are visible.
        let entries = self.log_source.entries().await;
        let entries = &entries;
        for_each_tracked(
            ids,
            |id| self.registry.get(id).map(|s| s.clone()),
            lookup,
            |surface, settings| async move {
                if let Err(e) = self
                    .render(&surface, entries, &settings, UsageRead::Fetch)
                    .await
                {
                    log::warn!("metric tile render failed: {e}");
                }
            },
        )
        .await;
    }
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
        self.redraw(instance, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.clone());
        self.redraw(instance, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    /// A tap refreshes just that tile at once.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.redraw(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::build_metric_display;
    use crate::source::cached::test_support::shared;
    use crate::source::{MonthlyUsage, UsageSnapshot, UsageSource, UsageSourceError, WindowUsage};
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::manifest_entry;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
                details: Default::default(),
            })
        }
    }

    fn action_with(source: Counting) -> MetricTileAction {
        MetricTileAction::new(
            Arc::new(LogUsageSource::new("/nonexistent".into())),
            shared(source),
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
    fn default_settings_are_tokens_today_all_models() {
        let settings = MetricTileSettings::default();
        assert_eq!(settings.metric, MetricKind::Tokens);
        assert_eq!(settings.range, RangeKind::Today);
        assert_eq!(settings.scope, ModelScope::All);
    }

    /// A tile saved with the cut `refresh_seconds` loads with its other
    /// settings; the stored refresh is ignored.
    #[test]
    fn a_saved_refresh_seconds_is_ignored() {
        for json in [
            r#"{"metric":"cost","refresh_seconds":30}"#,
            r#"{"metric":"cost","refresh_seconds":"x"}"#,
        ] {
            let s: MetricTileSettings = serde_json::from_str(json).unwrap();
            assert_eq!(s.metric, MetricKind::Cost, "{json}");
            let saved = serde_json::to_value(&s).unwrap();
            assert!(saved.get("refresh_seconds").is_none());
        }
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
            serde_json::from_str(r#"{"metric":"bogus","range":"sevenday","scope":"opus"}"#)
                .unwrap();
        assert_eq!(s.metric, MetricKind::Tokens);
        assert_eq!(s.range, RangeKind::SevenDay);
        assert_eq!(s.scope, ModelScope::Opus);
    }

    /// A tile saved on v0.16 with the removed Console source still loads with
    /// its metric and range, and now reads the transcripts.
    #[test]
    fn a_saved_console_source_is_ignored() {
        let s: MetricTileSettings = serde_json::from_str(
            r#"{"metric":"cost","range":"sevenday","refresh_seconds":120,"source":"console"}"#,
        )
        .unwrap();
        assert_eq!(
            s,
            MetricTileSettings {
                metric: MetricKind::Cost,
                range: RangeKind::SevenDay,
                ..MetricTileSettings::default()
            }
        );
    }

    /// A tile scoped to a family that has no usage shows a real zero
    /// with the family named, not "no data".
    #[tokio::test]
    async fn a_scoped_tile_with_no_matching_usage_shows_zero() {
        let action = action();
        let key = FakeSurface::new("a", true);
        let haiku = MetricTileSettings {
            scope: ModelScope::Haiku,
            ..MetricTileSettings::default()
        };
        action
            .render(&key, &[entry()], &haiku, UsageRead::Cached)
            .await
            .unwrap();
        let [Output::Image(frame)] = &key.frames()[..] else {
            panic!("expected one frame");
        };
        let display = build_scoped_metric_display(
            &[entry()],
            MetricKind::Tokens,
            RangeKind::Today,
            ModelScope::Haiku,
            chrono::Local::now(),
            None,
        );
        assert_eq!(display.value_text, "0");
        assert_eq!(display.subtitle, "today \u{b7} haiku");
        assert_eq!(*frame, build_metric_icon(&display));
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
                ..MetricTileSettings::default()
            },
        );
        let tracked = action.registry.get("ctx1").unwrap();
        assert_eq!(tracked.metric, MetricKind::Cost);
        assert_eq!(tracked.range, RangeKind::Session);
    }

    /// One tick redraws every visible tile, whatever its settings, from
    /// one scan; a tile removed meanwhile isn't looked up.
    #[tokio::test]
    async fn one_tick_redraws_every_tracked_tile() {
        let action = action();
        action.track("tokens", MetricTileSettings::default());
        action.track(
            "cost",
            MetricTileSettings {
                metric: MetricKind::Cost,
                ..MetricTileSettings::default()
            },
        );
        action.track("gone", MetricTileSettings::default());
        action.untrack("gone");
        let mut drawn = Vec::new();
        action
            .tick_once(|id| {
                drawn.push(id.clone());
                std::future::ready(Some(FakeSurface::new(&id, true)))
            })
            .await;
        drawn.sort();
        assert_eq!(drawn, ["cost", "tokens"]);
    }

    /// Asked for a redraw now (`refresh_soon` has a permit)?
    async fn refresh_requested(action: &MetricTileAction) -> bool {
        tokio::time::timeout(Duration::from_millis(10), action.refresh_soon.notified())
            .await
            .is_ok()
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
                .render(
                    &FakeSurface::new(id, true),
                    &[entry()],
                    &session,
                    UsageRead::Fetch,
                )
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
        action
            .render(&key, &[], &session, UsageRead::Fetch)
            .await
            .unwrap();
        assert_eq!(source.0.load(Ordering::SeqCst), 0);
        assert_eq!(
            key.frames(),
            vec![Output::Image(build_metric_icon(&error_display()))]
        );
    }

    /// A usage source whose fetch never answers.
    struct Hanging;

    #[async_trait]
    impl UsageSource for Hanging {
        async fn read(&self) -> Result<UsageSnapshot, UsageSourceError> {
            std::future::pending().await
        }
    }

    /// A usage cache holding one good snapshot (from `Counting`) whose
    /// next fetch is already due - so a `read()` would fetch.
    async fn cache_due_for_a_fetch(source: Counting) -> Arc<dyn SharedUsage> {
        let usage = shared(source);
        usage.read().await.unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        usage
    }

    /// A projects dir with one transcript line, 4.5 hours old: inside the
    /// rolling 5-hour fallback, but outside the window `Counting`'s reset
    /// time (an hour from now) implies.
    fn one_transcript() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let at = (chrono::Utc::now() - chrono::Duration::minutes(270)).to_rfc3339();
        std::fs::write(
            project.join("session.jsonl"),
            format!(
                r#"{{"type":"assistant","timestamp":"{at}","message":{{"model":"claude-sonnet-5","usage":{{"input_tokens":2,"output_tokens":3}}}}}}"#
            ),
        )
        .unwrap();
        root
    }

    fn session() -> MetricTileSettings {
        MetricTileSettings {
            range: RangeKind::Session,
            ..MetricTileSettings::default()
        }
    }

    /// What a Session tile over `logs` shows for `resets_at`.
    async fn expected(
        logs: &LogUsageSource,
        resets_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Output {
        let entries = logs.entries().await;
        Output::Image(build_metric_icon(&build_metric_display(
            &entries,
            MetricKind::Tokens,
            RangeKind::Session,
            chrono::Local::now(),
            resets_at,
        )))
    }

    /// An event never waits on a usage fetch: with the upstream hanging
    /// and nothing cached, a Session tile's appear/settings/tap path
    /// still draws (the rolling window) and brings its refresh forward.
    #[tokio::test]
    async fn an_event_draws_a_session_tile_without_fetching_usage() {
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        logs.entries().await; // the first scan is done
        let action = MetricTileAction::new(Arc::clone(&logs), shared(Hanging));
        action.track("a", session());
        let key = FakeSurface::new("a", true);
        tokio::time::timeout(Duration::from_millis(100), action.redraw(&key, &session()))
            .await
            .expect("an event waited on the usage fetch")
            .unwrap();
        assert_eq!(key.frames(), vec![expected(&logs, None).await]);
        assert!(refresh_requested(&action).await);
    }

    /// An event uses the cached reset time, and makes no request even when
    /// one is due; the tick loop's read is the one that fetches.
    #[tokio::test(start_paused = true)]
    async fn an_event_reads_the_cached_reset_time_and_only_a_tick_fetches() {
        let source = Counting::default();
        let usage = cache_due_for_a_fetch(source.clone()).await;
        let resets_at = usage.peek().unwrap().unwrap().session.resets_at;
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        logs.entries().await; // the first scan is done
        let action = MetricTileAction::new(Arc::clone(&logs), usage);
        let key = FakeSurface::new("a", true);
        action.redraw(&key, &session()).await.unwrap();
        assert_eq!(source.0.load(Ordering::SeqCst), 1, "an event fetched");
        let cached = expected(&logs, resets_at).await;
        assert_ne!(cached, expected(&logs, None).await, "the test can't tell");
        assert_eq!(key.frames(), vec![cached]);

        action.track("a", session());
        tokio::time::advance(Duration::from_secs(3600)).await;
        action
            .tick_once(|_| std::future::ready(Some(key.clone())))
            .await;
        assert_eq!(source.0.load(Ordering::SeqCst), 2, "the tick did not fetch");
    }

    /// With a usable snapshot cached and the transcripts scanned, an event
    /// asks for no extra redraw.
    #[tokio::test(start_paused = true)]
    async fn a_warm_tile_asks_for_no_extra_redraw() {
        let usage = shared(Counting::default());
        usage.read().await.unwrap();
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        logs.entries().await; // the first scan is done
        let action = MetricTileAction::new(logs, usage);
        action.track("a", session());
        action
            .redraw(&FakeSurface::new("a", true), &session())
            .await
            .unwrap();
        assert!(!refresh_requested(&action).await);
    }

    /// Before the first transcript scan an event draws at once, without
    /// scanning, and asks for a redraw now; the tick loop then scans and
    /// draws the data.
    #[tokio::test(start_paused = true)]
    async fn an_event_before_the_first_scan_draws_at_once_and_the_tick_fills_it_in() {
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        let action = MetricTileAction::new(Arc::clone(&logs), shared(Counting::default()));
        let today = MetricTileSettings::default();
        action.track("a", today.clone());
        let key = FakeSurface::new("a", true);
        action.redraw(&key, &today).await.unwrap();
        assert!(!logs.scanned(), "an event waited on the first scan");
        assert_eq!(
            key.frames(),
            vec![Output::Image(build_metric_icon(&error_display()))]
        );
        assert!(refresh_requested(&action).await);

        action
            .tick_once(|_| std::future::ready(Some(key.clone())))
            .await;
        assert!(logs.scanned());
        let frames = key.frames();
        assert_eq!(frames.len(), 2, "the tick did not redraw: {frames:?}");
        let entries = logs.peek();
        assert!(!entries.is_empty());
        assert_ne!(frames[1], frames[0]);
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<MetricTileAction as Action>::UUID);
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/metrictile.html"
        );
    }
}
