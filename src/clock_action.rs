//! Peak Clock: a 24-hour clock face with the configured peak hours marked,
//! whether it's peak now, and how long until that changes. Keypad only; a
//! tap redraws it.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::clock_icon::build_clock_icon;
use crate::peak::{
    DEFAULT_PEAK_DAYS, DEFAULT_PEAK_END, DEFAULT_PEAK_START, PeakDays, PeakSchedule, PeakWindow,
    peak_status,
};
use crate::press::LatestSettings;
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};

/// Each field falls back alone (see `settings::lenient`); a *missing* key
/// gets its default from `Default`, so an empty `peak_days` list (every box
/// unticked) is kept.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PeakClockSettings {
    #[serde(deserialize_with = "lenient_or_start")]
    pub peak_start: String,
    #[serde(deserialize_with = "lenient_or_end")]
    pub peak_end: String,
    /// Lowercase three-letter day keys (see `peak::DAY_KEYS`).
    #[serde(deserialize_with = "lenient_or_weekdays")]
    pub peak_days: Vec<String>,
}

impl Default for PeakClockSettings {
    fn default() -> Self {
        Self {
            peak_start: DEFAULT_PEAK_START.to_string(),
            peak_end: DEFAULT_PEAK_END.to_string(),
            peak_days: DEFAULT_PEAK_DAYS.iter().map(|d| d.to_string()).collect(),
        }
    }
}

/// `settings::lenient`, but falling back to the field's real default
/// rather than an empty string or list.
fn lenient_or<'de, D, T>(deserializer: D, fallback: impl FnOnce() -> T) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_else(|_| fallback()))
}

fn lenient_or_start<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    lenient_or(d, || DEFAULT_PEAK_START.to_string())
}

fn lenient_or_end<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    lenient_or(d, || DEFAULT_PEAK_END.to_string())
}

fn lenient_or_weekdays<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    lenient_or(d, || PeakClockSettings::default().peak_days)
}

fn schedule_from(settings: &PeakClockSettings) -> PeakSchedule {
    PeakSchedule {
        window: PeakWindow::from_settings(&settings.peak_start, &settings.peak_end),
        days: PeakDays::from_settings(&settings.peak_days),
    }
}

/// The tile for `schedule` at local time `now`. A clock tile has no
/// external data source, so it's always computed fresh.
fn output_at(schedule: PeakSchedule, now: NaiveDateTime) -> Output {
    let now_minutes = now.time().num_seconds_from_midnight() / 60;
    let status = peak_status(schedule, now);
    // Dim the arcs on a day the window doesn't apply to, unless a
    // previous day's overnight window is still running.
    let arcs_active = status.is_peak || schedule.days.includes(now.weekday());
    Output::Image(build_clock_icon(
        schedule.window,
        now_minutes,
        arcs_active,
        &status,
    ))
}

#[derive(Clone)]
pub struct PeakClockAction {
    registry: Arc<LatestSettings<PeakSchedule>>,
    frames: Arc<Frames>,
    wake: Arc<Notify>,
}

impl PeakClockAction {
    pub fn new() -> Self {
        Self {
            registry: Arc::new(LatestSettings::default()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
        }
    }

    fn track(&self, instance_id: &str, settings: &PeakClockSettings) {
        self.registry.set(instance_id, &schedule_from(settings));
        self.wake.notify_one();
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.forget(instance_id);
        self.frames.forget(instance_id);
    }

    async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        schedule: PeakSchedule,
    ) -> OpenActionResult<()> {
        let output = output_at(schedule, Local::now().naive_local());
        self.frames.push(surface, output).await
    }

    /// Runs forever: just after every minute boundary - the face has
    /// minute resolution - redraws every visible clock. Parks while none is
    /// visible. Spawned once from `main.rs`, supervised; independent of the
    /// usage hub (a clock has no data source).
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.registry.is_empty(), &self.wake).await;
            sleep_to_next_minute().await;
            self.render_tracked(openaction::get_instance).await;
        }
    }

    /// Redraws every tracked clock, reading each one's schedule only once
    /// its instance has been looked up (see `for_each_tracked`), so a
    /// schedule saved meanwhile isn't drawn over with the old one.
    async fn render_tracked<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        for_each_tracked(
            self.registry.ids(),
            |id| self.registry.get(id),
            lookup,
            |surface, schedule| async move {
                if let Err(e) = self.render(&surface, schedule).await {
                    log::warn!("clock render failed: {e}");
                }
            },
        )
        .await;
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
        self.frames.forget(&instance.instance_id);
        self.track(&instance.instance_id, settings);
        self.render(instance, schedule_from(settings)).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings);
        self.render(instance, schedule_from(settings)).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    /// A tap redraws just that tile now.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.render(instance, schedule_from(settings)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::manifest_entry;

    #[test]
    fn default_settings_use_the_default_peak_window() {
        let settings = PeakClockSettings::default();
        assert_eq!(settings.peak_start, "13:00");
        assert_eq!(settings.peak_end, "18:00");
        assert_eq!(settings.peak_days, ["mon", "tue", "wed", "thu", "fri"]);
    }

    #[test]
    fn empty_day_list_is_kept_rather_than_defaulted() {
        // Unticking every box must stick - only a *missing* key gets the
        // weekday default.
        let settings: PeakClockSettings = serde_json::from_str(r#"{"peak_days":[]}"#).unwrap();
        assert!(settings.peak_days.is_empty());
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // openaction falls back to Default::default() when settings JSON
        // fails to deserialize at all, not just on missing fields - confirm
        // both paths land on the same value.
        let from_missing_keys: PeakClockSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, PeakClockSettings::default());
    }

    /// KI-10 class: a bad value falls back to that field's default alone.
    #[test]
    fn a_bad_field_keeps_the_others() {
        let s: PeakClockSettings =
            serde_json::from_str(r#"{"peak_days":5,"peak_start":"09:30","peak_end":null}"#)
                .unwrap();
        assert_eq!(s.peak_start, "09:30");
        assert_eq!(s.peak_end, "18:00");
        assert_eq!(s.peak_days, PeakClockSettings::default().peak_days);
    }

    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = PeakClockAction::new();
        let settings = PeakClockSettings {
            peak_start: "10:00".to_string(),
            peak_end: "20:00".to_string(),
            ..Default::default()
        };
        action.track("ctx1", &settings);
        assert_eq!(
            action.registry.get("ctx1").unwrap(),
            PeakSchedule {
                window: PeakWindow {
                    start_minutes: 600,
                    end_minutes: 1200
                },
                days: PeakDays::from_settings(&DEFAULT_PEAK_DAYS),
            }
        );

        action.untrack("ctx1");
        assert!(action.registry.get("ctx1").is_none());
    }

    /// The KI-07 race, on the clock: a schedule saved while the tick awaits
    /// the instance lookup is the one drawn.
    #[tokio::test]
    async fn a_schedule_saved_during_the_lookup_is_the_one_drawn() {
        let action = PeakClockAction::new();
        let old = PeakClockSettings::default();
        let new = PeakClockSettings {
            peak_start: "01:00".to_string(),
            peak_end: "02:00".to_string(),
            ..Default::default()
        };
        action.track("ctx1", &old);
        let key = FakeSurface::new("ctx1", true);
        action
            .render_tracked(|id| {
                action.track(&id, &new); // the PI saves here
                std::future::ready(Some(key.clone()))
            })
            .await;
        let expected = output_at(schedule_from(&new), Local::now().naive_local());
        assert_eq!(key.frames(), vec![expected]);
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<PeakClockAction as Action>::UUID);
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/peakclock.html"
        );
    }
}
