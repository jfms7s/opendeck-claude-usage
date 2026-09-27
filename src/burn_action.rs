use crate::burn::{BurnMetric, burn_window};
use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::source::WindowKind;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BurnRateSettings {
    #[serde(default, deserialize_with = "or_default")]
    pub window: WindowKind,
    #[serde(default, deserialize_with = "or_default")]
    pub metric: BurnMetric,
    /// Only marks and palette matter here - Burn Rate always colors
    /// pace-based, so a stored `colorMode` is ignored.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

/// An unknown value falls back to that field's default. A hard error
/// would make openaction drop every setting, colors included.
fn or_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

impl BurnRateSettings {
    fn view(&self) -> View {
        View::Burn {
            window: burn_window(self.window),
            metric: self.metric,
            colors: self.colors.clone(),
        }
    }
}

#[derive(Clone)]
pub struct BurnRateAction {
    hub: Arc<UsageHub>,
}

impl BurnRateAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl Action for BurnRateAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.burnrate";
    type Settings = BurnRateSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.hub.untrack(&instance.instance_id);
        Ok(())
    }

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }

    /// A tap forces an immediate refresh of just that tile.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::{burn_error_display, burn_feedback};

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][3];
        assert_eq!(
            entry["UUID"].as_str().unwrap(),
            <BurnRateAction as Action>::UUID
        );
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/burnrate.html"
        );
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
        let feedback = burn_feedback(&burn_error_display(BurnMetric::Pace));
        for k in feedback.as_object().unwrap().keys() {
            assert!(keys.contains(&k.as_str()), "layout has no item keyed {k}");
        }
    }

    #[test]
    fn empty_settings_are_session_pace() {
        let s: BurnRateSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.metric, BurnMetric::Pace);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn parses_window_metric_and_colors_together() {
        let s: BurnRateSettings =
            serde_json::from_str(r#"{"window":"weekly","metric":"runway","critical":95}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.metric, BurnMetric::Runway);
        assert_eq!(s.colors.marks.critical, 95.0);
    }

    /// KI-10: an unknown value in one field must not throw away the others
    /// (openaction falls back to all defaults when settings fail to parse).
    #[test]
    fn an_unknown_metric_or_window_keeps_the_other_settings() {
        let s: BurnRateSettings =
            serde_json::from_str(r#"{"window":"weekly","metric":"","critical":95}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.metric, BurnMetric::Pace);
        assert_eq!(s.colors.marks.critical, 95.0);
        let s: BurnRateSettings =
            serde_json::from_str(r#"{"window":7,"metric":"runway","critical":95}"#).unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.metric, BurnMetric::Runway);
        assert_eq!(s.colors.marks.critical, 95.0);
    }

    /// KI-10: the PI must show a real option for a stored value it doesn't
    /// know, or the next edit would send "".
    #[test]
    fn property_inspector_falls_back_to_a_known_metric() {
        let html = include_str!("../assets/propertyInspector/burnrate.html");
        assert!(
            html.contains("knownMetric"),
            "burnrate.html must validate the stored metric"
        );
    }

    #[test]
    fn monthly_setting_views_as_session() {
        let s: BurnRateSettings = serde_json::from_str(r#"{"window":"monthly"}"#).unwrap();
        assert!(matches!(
            s.view(),
            View::Burn {
                window: WindowKind::Session,
                ..
            }
        ));
    }
}
