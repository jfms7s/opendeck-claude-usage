use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::press::{LatestSettings, Press, PressTimer, Release, on_release};
use crate::sparkline::SparkSettings;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SparklineSettings {
    /// `window` + `series`.
    #[serde(flatten)]
    pub spark: SparkSettings,
    /// Marks/palette/mode for the line's level color.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

impl SparklineSettings {
    fn view(&self) -> View {
        View::Sparkline {
            settings: self.spark.clone(),
            colors: self.colors.clone(),
        }
    }

    /// These settings with the next series - everything else unchanged.
    fn cycled(&self) -> SparklineSettings {
        let mut updated = self.clone();
        updated.spark.series = self.spark.series.next();
        updated
    }

    /// Short press (key or dial) cycles the series; a long one refreshes.
    fn release(&self, press: Press) -> Release<SparklineSettings> {
        on_release(press, || Some(self.cycled()))
    }
}

#[derive(Clone)]
pub struct SparklineAction {
    hub: Arc<UsageHub>,
    /// Tells a short press (next series) from a long one (refresh), on
    /// keys and dials alike.
    presses: Arc<PressTimer>,
    /// What each instance was last set to, so a press starts from it.
    latest: Arc<LatestSettings<SparklineSettings>>,
}

impl SparklineAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: Arc::new(PressTimer::default()),
            latest: Arc::new(LatestSettings::default()),
        }
    }

    /// What a release does, decided from the last-set settings rather than
    /// the event's, and kept when it switches (see `LatestSettings`).
    fn release(
        &self,
        instance_id: &str,
        settings: &SparklineSettings,
        press: Press,
    ) -> Release<SparklineSettings> {
        self.latest
            .release(instance_id, settings, |s| s.release(press))
    }

    async fn released(
        &self,
        instance: &Instance,
        settings: &SparklineSettings,
    ) -> OpenActionResult<()> {
        let press = self.presses.up(&instance.instance_id);
        match self.release(&instance.instance_id, settings, press) {
            Release::Refresh => {
                let view = self.latest.current(&instance.instance_id, settings).view();
                self.hub.refresh_one(instance, &view).await
            }
            Release::Stay => Ok(()),
            Release::Switch(updated) => {
                // Tracked before the await, so a poll meanwhile draws it.
                let view = updated.view();
                self.hub.track(&instance.instance_id, view.clone());
                if let Err(e) = instance.set_settings(&updated).await {
                    log::warn!("could not persist sparkline series: {e}");
                }
                self.hub.render_cached(instance, &view).await
            }
        }
    }
}

#[async_trait]
impl Action for SparklineAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.sparkline";
    type Settings = SparklineSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.latest.set(&instance.instance_id, settings);
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.latest.set(&instance.instance_id, settings);
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.forget(&instance.instance_id);
        self.latest.forget(&instance.instance_id);
        self.hub.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    /// Same gesture as the key: a dial that only refreshed could never
    /// change series.
    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.released(instance, settings).await
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.released(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::WindowKind;
    use crate::sparkline::SparkSeries;
    use serde_json::Value;

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][6];
        assert_eq!(
            entry["UUID"].as_str().unwrap(),
            <SparklineAction as Action>::UUID
        );
        assert_eq!(entry["Encoder"]["layout"], "layouts/chart.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/sparkline.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let d = crate::sparkline::build_sparkline(
            &[],
            &SparkSettings::default(),
            &ColorSettings::default(),
            chrono::Local::now(),
        );
        crate::test_support::assert_feedback_matches_layout(
            include_str!("../assets/layouts/chart.json"),
            &crate::styles::sparkline::sparkline_feedback(&d),
            &[],
        );
    }

    #[test]
    fn cycled_moves_to_the_next_series_and_keeps_the_rest() {
        let s: SparklineSettings =
            serde_json::from_str(r#"{"window":"weekly","series":"today","critical":95}"#).unwrap();
        let c = s.cycled();
        assert_eq!(c.spark.series, SparkSeries::EvenBurn);
        assert_eq!(c.spark.window, WindowKind::Weekly);
        assert_eq!(c.colors, s.colors);
        assert!(matches!(c.view(), View::Sparkline { .. }));
    }

    #[test]
    fn a_release_cycles_when_short_and_refreshes_when_long() {
        let s = SparklineSettings::default();
        let Release::Switch(next) = s.release(Press::Short) else {
            panic!("expected a switch");
        };
        assert_eq!(next.spark.series, s.spark.series.next());
        assert!(matches!(s.release(Press::Long), Release::Refresh));
    }

    #[test]
    fn settings_round_trip_flat() {
        let s: SparklineSettings =
            serde_json::from_str(r#"{"window":"weekly","series":"betweenPolls","watch":40}"#)
                .unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["series"], "betweenPolls");
        assert_eq!(v["watch"], 40.0);
        let back: SparklineSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back.spark, s.spark);
        assert_eq!(back.colors, s.colors);
    }

    #[test]
    fn empty_settings_are_session_trend() {
        let s: SparklineSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.spark, SparkSettings::default());
        assert_eq!(s.colors, ColorSettings::default());
    }

    /// The PI must load the shared colors section, offer every window
    /// (and keep a stored Monthly), and pass the stored series through, or
    /// saving it would reset the series.
    #[test]
    fn property_inspector_offers_windows_and_keeps_series() {
        let html = include_str!("../assets/propertyInspector/sparkline.html");
        assert!(html.contains(r#"<script src="colors.js"></script>"#));
        assert!(html.contains(r#"<option value="weekly">"#));
        assert!(html.contains(r#"<option value="monthly">"#));
        assert!(!html.contains(r#"settings.window === "weekly" ? "weekly" : "session""#));
        assert!(html.contains("storedSeries"));
        assert!(html.contains("key or dial"));
    }

    /// KI-14: the PI re-reads the stored settings before saving, in case
    /// OpenDeck didn't forward the plugin's press-driven `setSettings`.
    #[test]
    fn property_inspector_refreshes_before_saving() {
        let html = include_str!("../assets/propertyInspector/sparkline.html");
        assert!(html.contains(r#"event: "getSettings""#));
        assert!(html.contains("pendingSave"));
    }

    fn action() -> SparklineAction {
        SparklineAction::new(crate::hub::UsageHub::new(
            crate::hub::test_support::NeverCalled,
            crate::history::HistoryStore::in_memory(),
        ))
    }

    fn switched(r: Release<SparklineSettings>) -> SparkSeries {
        let Release::Switch(s) = r else {
            panic!("expected a switch");
        };
        s.spark.series
    }

    /// KI-08: the second of two fast presses gets OpenDeck's settings from
    /// before the first.
    #[test]
    fn two_fast_presses_advance_two_series() {
        let a = action();
        let stale = SparklineSettings::default();
        let first = switched(a.release("ctx1", &stale, Press::Short));
        let second = switched(a.release("ctx1", &stale, Press::Short));
        assert_eq!(first, SparkSeries::BetweenPolls);
        assert_eq!(second, SparkSeries::Today);
    }

    #[test]
    fn a_press_after_new_settings_starts_from_them() {
        let a = action();
        a.release("ctx1", &SparklineSettings::default(), Press::Short);
        let pi: SparklineSettings = serde_json::from_str(r#"{"series":"evenBurn"}"#).unwrap();
        a.latest.set("ctx1", &pi);
        assert_eq!(
            switched(a.release("ctx1", &pi, Press::Short)),
            SparkSeries::Trend
        );
    }
}
