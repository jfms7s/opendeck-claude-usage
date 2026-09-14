use crate::clock_icon::build_clock_icon;
use crate::peak::{PeakWindow, peak_status};
use async_trait::async_trait;
use chrono::{Local, Timelike};
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PeakClockSettings {
    pub peak_start: String,
    pub peak_end: String,
}

impl Default for PeakClockSettings {
    fn default() -> Self {
        Self {
            peak_start: crate::peak::DEFAULT_PEAK_START.to_string(),
            peak_end: crate::peak::DEFAULT_PEAK_END.to_string(),
        }
    }
}

fn window_from(settings: &PeakClockSettings) -> PeakWindow {
    PeakWindow::from_settings(&settings.peak_start, &settings.peak_end)
}

#[derive(Clone)]
pub struct PeakClockAction {
    registry: Arc<DashMap<String, PeakWindow>>,
}

impl PeakClockAction {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(DashMap::new()),
        }
    }

    fn track(&self, instance_id: &str, settings: &PeakClockSettings) {
        self.registry
            .insert(instance_id.to_string(), window_from(settings));
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Renders one instance from `window` and the current local time - a
    /// clock tile has no external data source, so unlike the usage gauge
    /// this is always computed fresh, never cached.
    async fn render(instance: &Instance, window: PeakWindow) -> OpenActionResult<()> {
        let now = Local::now().time();
        let now_minutes = now.num_seconds_from_midnight() / 60;
        let status = peak_status(window, now);
        let title = format!("{}\n{}", status.status_text, status.countdown_text);
        instance.set_title(Some(title), None).await?;
        instance
            .set_image(
                Some(build_clock_icon(window, now_minutes, status.is_peak)),
                None,
            )
            .await
    }

    /// Runs forever: every ~20s, re-renders every currently-registered
    /// instance so its pointer and countdown stay current. Mirrors
    /// `UsageGaugeAction::poll_loop`'s cadence but is fully independent -
    /// this action has no shared state or data source with the usage gauge.
    pub async fn tick_loop(&self) {
        loop {
            self.refresh_all().await;
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    }

    async fn refresh_all(&self) {
        let entries: Vec<(String, PeakWindow)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect();

        for (instance_id, window) in entries {
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue;
            };
            if let Err(e) = Self::render(&instance, window).await {
                log::warn!("clock render failed: {e}");
            }
        }
    }
}

#[async_trait]
impl Action for PeakClockAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.peakclock";
    type Settings = PeakClockSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings);
        Self::render(instance, window_from(settings)).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings);
        Self::render(instance, window_from(settings)).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    /// A tap forces an immediate refresh of just that tile, same as
    /// `UsageGaugeAction::key_up`.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        Self::render(instance, window_from(settings)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_use_the_default_peak_window() {
        let settings = PeakClockSettings::default();
        assert_eq!(settings.peak_start, "13:00");
        assert_eq!(settings.peak_end, "18:00");
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // Same footgun UsageGaugeSettings' own test guards against: openaction
        // falls back to Default::default() when settings JSON fails to
        // deserialize at all, not just on missing fields - confirm both
        // paths land on the same value.
        let from_missing_keys: PeakClockSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, PeakClockSettings::default());
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = PeakClockAction::new();
        let settings = PeakClockSettings {
            peak_start: "10:00".to_string(),
            peak_end: "20:00".to_string(),
        };
        action.track("ctx1", &settings);
        assert_eq!(
            *action.registry.get("ctx1").unwrap(),
            PeakWindow {
                start_minutes: 600,
                end_minutes: 1200
            }
        );

        action.untrack("ctx1");
        assert!(action.registry.get("ctx1").is_none());
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_window() {
        let action = PeakClockAction::new();
        action.track(
            "ctx1",
            &PeakClockSettings {
                peak_start: "01:00".to_string(),
                peak_end: "02:00".to_string(),
            },
        );
        action.track(
            "ctx1",
            &PeakClockSettings {
                peak_start: "03:00".to_string(),
                peak_end: "04:00".to_string(),
            },
        );
        assert_eq!(
            *action.registry.get("ctx1").unwrap(),
            PeakWindow {
                start_minutes: 180,
                end_minutes: 240
            }
        );
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let manifest_uuid = manifest["Actions"][1]["UUID"].as_str().unwrap();
        assert_eq!(manifest_uuid, <PeakClockAction as Action>::UUID);
    }
}
