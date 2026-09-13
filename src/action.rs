use crate::format::{build_feedback, error_feedback};
use crate::source::{UsageSnapshot, UsageSource, WindowKind};
use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
}

struct SharedState {
    source: Box<dyn UsageSource>,
    latest: RwLock<Option<UsageSnapshot>>,
    registry: DashMap<String, WindowKind>,
    /// Tracks whether the poll loop's most recent `refresh_all` read
    /// succeeded, so it can log a `warn!` only on the transition into
    /// failing (and an `info!` only on the transition back to succeeding)
    /// instead of every ~20s tick forever. Starts `true` so the very first
    /// failure is logged. Not touched by `refresh_one`/`dial_up` - a single
    /// manual dial-press failure isn't part of the "every 20s forever"
    /// noise pattern this is fixing.
    poll_last_read_ok: AtomicBool,
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    shared: Arc<SharedState>,
}

impl UsageGaugeAction {
    pub fn new(source: impl UsageSource + 'static) -> Self {
        Self {
            shared: Arc::new(SharedState {
                source: Box::new(source),
                latest: RwLock::new(None),
                registry: DashMap::new(),
                poll_last_read_ok: AtomicBool::new(true),
            }),
        }
    }

    fn track(&self, instance_id: &str, window: WindowKind) {
        self.shared.registry.insert(instance_id.to_string(), window);
    }

    fn untrack(&self, instance_id: &str) {
        self.shared.registry.remove(instance_id);
    }

    /// Renders from the last cached snapshot (no fresh read) - used when a
    /// dial appears or its settings change, so it shows *something*
    /// immediately rather than waiting for the next poll tick.
    async fn render_cached(&self, instance: &Instance, window: WindowKind) -> OpenActionResult<()> {
        let snapshot = self.shared.latest.read().await.clone();
        let feedback = match snapshot {
            Some(s) => build_feedback(&s, window, chrono::Utc::now()),
            None => error_feedback(),
        };
        instance.set_feedback(&feedback).await
    }

    /// Reads the source, and on success caches it in `latest` for
    /// `render_cached` - shared by `refresh_one` and `refresh_all` so the
    /// "read, then cache on success" step exists in exactly one place.
    async fn read_and_cache(&self) -> Result<UsageSnapshot, crate::source::UsageSourceError> {
        let result = self.shared.source.read().await;
        if let Ok(snapshot) = &result {
            *self.shared.latest.write().await = Some(snapshot.clone());
        }
        result
    }

    /// Reads the source directly and renders just this one dial immediately
    /// - used on a dial press, without waiting for the next scheduled tick.
    async fn refresh_one(&self, instance: &Instance, window: WindowKind) -> OpenActionResult<()> {
        let feedback = match self.read_and_cache().await {
            Ok(snapshot) => build_feedback(&snapshot, window, chrono::Utc::now()),
            Err(e) => {
                log::warn!("usage source read failed: {e}");
                error_feedback()
            }
        };
        instance.set_feedback(&feedback).await
    }

    /// Logs the poll loop's read outcome, but only on a transition (first
    /// failure after a success, or the recovery back to success) - not on
    /// every ~20s tick, which would otherwise warn forever while the source
    /// stays unavailable. See `SharedState::poll_last_read_ok`.
    fn log_poll_read_transition(
        &self,
        read_ok: bool,
        error: Option<&crate::source::UsageSourceError>,
    ) {
        let was_ok = self
            .shared
            .poll_last_read_ok
            .swap(read_ok, Ordering::Relaxed);
        if was_ok && !read_ok {
            if let Some(e) = error {
                log::warn!("usage source read failed: {e}");
            }
        } else if !was_ok && read_ok {
            log::info!("usage source read recovered");
        }
    }

    /// Runs forever: every ~20s, reads the usage source once and pushes a
    /// fresh render to every currently-registered dial. Spawned once from
    /// `main.rs` alongside `register_action`.
    pub async fn poll_loop(&self) {
        loop {
            self.refresh_all().await;
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    }

    async fn refresh_all(&self) {
        let read_result = self.read_and_cache().await;
        self.log_poll_read_transition(read_result.is_ok(), read_result.as_ref().err());

        // Collect registry entries into a Vec first, releasing the DashMap
        // shard lock before awaiting `get_instance`/`set_feedback` below -
        // holding a DashMap iterator guard across an await point per entry
        // would keep that shard locked for the whole loop.
        let entries: Vec<(String, WindowKind)> = self
            .shared
            .registry
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect();

        for (instance_id, window) in entries {
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // dial disappeared between the registry snapshot and now
            };
            let feedback = match &read_result {
                Ok(snapshot) => build_feedback(snapshot, window, chrono::Utc::now()),
                Err(_) => error_feedback(),
            };
            if let Err(e) = instance.set_feedback(&feedback).await {
                log::warn!("set_feedback failed: {e}");
            }
        }
    }
}

#[async_trait]
impl Action for UsageGaugeAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.usagegauge";
    type Settings = UsageGaugeSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.window);
        self.render_cached(instance, settings.window).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.window);
        self.render_cached(instance, settings.window).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.refresh_one(instance, settings.window).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::source::{MonthlyUsage, WindowUsage};
    use chrono::{TimeZone, Utc};

    struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, crate::source::UsageSourceError> {
            unreachable!("this task's tests never trigger a read")
        }
    }

    /// Always succeeds with a fixed, realistic snapshot - used by tests that
    /// need `read_and_cache` to actually populate the cache, unlike
    /// `NeverCalled` above.
    struct AlwaysOk;

    #[async_trait]
    impl UsageSource for AlwaysOk {
        async fn read(&self) -> Result<UsageSnapshot, crate::source::UsageSourceError> {
            Ok(UsageSnapshot {
                session: WindowUsage {
                    percent: 33.0,
                    resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()),
                },
                weekly: WindowUsage {
                    percent: 29.0,
                    resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 17, 6, 0, 0).unwrap()),
                },
                monthly: MonthlyUsage {
                    enabled: true,
                    percent: Some(25.0),
                    used_dollars: Some(12.5),
                    limit_dollars: Some(50.0),
                },
            })
        }
    }

    #[tokio::test]
    async fn read_and_cache_populates_the_cached_snapshot() {
        let action = UsageGaugeAction::new(AlwaysOk);
        action.read_and_cache().await.unwrap();
        assert!(action.shared.latest.read().await.is_some());
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let layout: serde_json::Value =
            serde_json::from_str(include_str!("../assets/layouts/usage.json")).unwrap();
        let keys: Vec<&str> = layout["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        for k in crate::format::error_feedback().as_object().unwrap().keys() {
            assert!(keys.contains(&k.as_str()), "layout has no item keyed {k}");
        }
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let manifest_uuid = manifest["Actions"][0]["UUID"].as_str().unwrap();
        assert_eq!(manifest_uuid, <UsageGaugeAction as Action>::UUID);
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = UsageGaugeAction::new(NeverCalled);
        action.track("ctx1", WindowKind::Weekly);
        assert_eq!(
            *action.shared.registry.get("ctx1").unwrap(),
            WindowKind::Weekly
        );

        action.untrack("ctx1");
        assert!(action.shared.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_window() {
        let action = UsageGaugeAction::new(NeverCalled);
        action.track("ctx1", WindowKind::Session);
        action.track("ctx1", WindowKind::Monthly);
        assert_eq!(
            *action.shared.registry.get("ctx1").unwrap(),
            WindowKind::Monthly
        );
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // Same footgun opendeck-focus-launcher's settings hit: openaction
        // falls back to Default::default() when settings JSON fails to
        // deserialize at all, not just on missing fields - confirm both
        // paths land on the same value.
        let from_missing_keys: UsageGaugeSettings = serde_json::from_str("{}").unwrap();
        let from_default = UsageGaugeSettings::default();
        assert_eq!(from_missing_keys.window, from_default.window);
        assert_eq!(from_default.window, WindowKind::Session);
    }
}
