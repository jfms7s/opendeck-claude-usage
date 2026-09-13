use crate::format::{build_feedback, error_feedback};
use crate::source::{UsageSnapshot, UsageSource, WindowKind};
use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
}

struct SharedState {
    source: Box<dyn UsageSource>,
    latest: watch::Sender<Option<UsageSnapshot>>,
    registry: DashMap<String, WindowKind>,
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    shared: Arc<SharedState>,
}

impl UsageGaugeAction {
    pub fn new(source: impl UsageSource + 'static) -> Self {
        let (latest, _) = watch::channel(None);
        Self {
            shared: Arc::new(SharedState {
                source: Box::new(source),
                latest,
                registry: DashMap::new(),
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
        let snapshot = self.shared.latest.borrow().clone();
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
            let _ = self.shared.latest.send(Some(snapshot.clone()));
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
        if let Err(e) = &read_result {
            log::warn!("usage source read failed: {e}");
        }

        for entry in self.shared.registry.iter() {
            let instance_id = entry.key().clone();
            let window = *entry.value();
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // dial disappeared between the registry snapshot and now
            };
            let feedback = match &read_result {
                Ok(snapshot) => build_feedback(snapshot, window, chrono::Utc::now()),
                Err(_) => error_feedback(),
            };
            let _ = instance.set_feedback(&feedback).await;
        }
    }
}

#[async_trait]
impl Action for UsageGaugeAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.usagegauge";
    type Settings = UsageGaugeSettings;

    async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.window);
        self.render_cached(instance, settings.window).await
    }

    async fn did_receive_settings(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.window);
        self.render_cached(instance, settings.window).await
    }

    async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.refresh_one(instance, settings.window).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NeverCalled;

    #[async_trait]
    impl UsageSource for NeverCalled {
        async fn read(&self) -> Result<UsageSnapshot, crate::source::UsageSourceError> {
            unreachable!("this task's tests never trigger a read")
        }
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = UsageGaugeAction::new(NeverCalled);
        action.track("ctx1", WindowKind::Weekly);
        assert_eq!(*action.shared.registry.get("ctx1").unwrap(), WindowKind::Weekly);

        action.untrack("ctx1");
        assert!(action.shared.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_window() {
        let action = UsageGaugeAction::new(NeverCalled);
        action.track("ctx1", WindowKind::Session);
        action.track("ctx1", WindowKind::Monthly);
        assert_eq!(*action.shared.registry.get("ctx1").unwrap(), WindowKind::Monthly);
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
