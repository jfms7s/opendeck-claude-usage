//! Usage Sparkline: recorded usage history as a line on a key or a dial
//! strip; a short press (key or dial) cycles the series.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

use crate::history::Reading;
use crate::hub::{HubView, UsageHub, View};
use crate::hub_action::{HubActionCore, HubSettings};
use crate::level::ColorSettings;
use crate::source::UsageSnapshot;
use crate::sparkline::{SparkSettings, build_sparkline};
use crate::styles::sparkline::{render_key, sparkline_feedback};
use crate::surface::Output;
use crate::tile;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SparklineSettings {
    /// `window` + `series`.
    #[serde(flatten)]
    pub spark: SparkSettings,
    /// Marks/palette/mode for the line's level color.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

/// What a sparkline instance shows. Drawn from the recorded history, not
/// the snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SparkView {
    pub settings: SparkSettings,
    pub colors: ColorSettings,
}

impl HubView for SparkView {
    fn output(
        &self,
        _snapshot: Option<&UsageSnapshot>,
        history: &[Reading],
        keypad: bool,
        now: DateTime<Utc>,
    ) -> Output {
        let display = build_sparkline(
            history,
            &self.settings,
            &self.colors,
            now.with_timezone(&Local),
        );
        if keypad {
            Output::Image(tile::data_uri(&render_key(&display)))
        } else {
            Output::Feedback(sparkline_feedback(&display))
        }
    }

    fn needs_history(&self) -> bool {
        true
    }
}

impl HubSettings for SparklineSettings {
    fn view(&self) -> View {
        Arc::new(SparkView {
            settings: self.spark.clone(),
            colors: self.colors.clone(),
        })
    }

    /// These settings with the next series - everything else unchanged.
    fn next(&self) -> Option<Self> {
        let mut updated = self.clone();
        updated.spark.series = self.spark.series.next();
        Some(updated)
    }

    const SWITCHED: &'static str = "sparkline series";
}

#[derive(Clone)]
pub struct SparklineAction {
    core: Arc<HubActionCore<SparklineSettings>>,
}

impl SparklineAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            core: Arc::new(HubActionCore::new(hub)),
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
        self.core.appear(instance, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.settings_changed(instance, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.disappear(&instance.instance_id);
        Ok(())
    }

    async fn dial_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.press(&instance.instance_id);
        Ok(())
    }

    /// Same gesture as the key: a dial that only refreshed could never
    /// change series.
    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.release(instance, settings).await
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.press(&instance.instance_id);
        Ok(())
    }

    /// Short press cycles the series; a long one refreshes.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.core.release(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::WindowKind;
    use crate::sparkline::SparkSeries;
    use crate::test_support::{assert_feedback_matches_layout, manifest_entry};

    fn view() -> SparkView {
        SparkView {
            settings: SparkSettings::default(),
            colors: ColorSettings::default(),
        }
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<SparklineAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/chart.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/sparkline.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let d = build_sparkline(
            &[],
            &SparkSettings::default(),
            &ColorSettings::default(),
            Local::now(),
        );
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/chart.json"),
            &sparkline_feedback(&d),
            &[],
        );
    }

    #[test]
    fn a_sparkline_on_a_keypad_is_an_image() {
        assert!(matches!(
            view().output(None, &[], true, Utc::now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn a_sparkline_on_a_dial_is_chart_feedback() {
        let Output::Feedback(f) = view().output(None, &[], false, Utc::now()) else {
            panic!("expected feedback");
        };
        assert!(
            f["chart"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );
    }

    #[test]
    fn only_the_sparkline_view_needs_history() {
        assert!(view().needs_history());
    }

    #[test]
    fn next_moves_to_the_next_series_and_keeps_the_rest() {
        let s: SparklineSettings =
            serde_json::from_str(r#"{"window":"weekly","series":"today","critical":95}"#).unwrap();
        let c = s.next().unwrap();
        assert_eq!(c.spark.series, SparkSeries::EvenBurn);
        assert_eq!(c.spark.window, WindowKind::Weekly);
        assert_eq!(c.colors, s.colors);
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
        assert!(html.contains(r#"passThrough: ["series"]"#));
        assert!(html.contains("key or dial"));
    }
}
