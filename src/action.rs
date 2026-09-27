use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::source::WindowKind;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
    /// Flattened so the Property Inspector writes plain top-level fields
    /// (`watch`, `colorNormal`, ...). Its lenient wire format means a bad
    /// color can't make openaction reset `window` too.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

impl UsageGaugeSettings {
    fn view(&self) -> View {
        View::Gauge {
            window: self.window,
            colors: self.colors.clone(),
            style: crate::style::GaugeStyle::default(),
        }
    }
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    hub: Arc<UsageHub>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self { hub }
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

    /// Keypad's equivalent of `dial_up` - a tap forces an immediate refresh
    /// of just that tile.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.hub.refresh_one(instance, &settings.view()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{error_display, feedback_for_display};
    use crate::level::{DEFAULT_WATCH, Marks};

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
        let feedback = feedback_for_display(&error_display());
        for k in feedback.as_object().unwrap().keys() {
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
    fn default_matches_missing_key_deserialization() {
        // openaction falls back to Default::default() when settings JSON
        // fails to deserialize at all - both paths must agree.
        let from_missing_keys: UsageGaugeSettings = serde_json::from_str("{}").unwrap();
        let from_default = UsageGaugeSettings::default();
        assert_eq!(from_missing_keys.window, from_default.window);
        assert_eq!(from_missing_keys.colors, from_default.colors);
        assert_eq!(from_default.window, WindowKind::Session);
    }

    #[test]
    fn old_settings_keep_window_and_get_default_colors() {
        // What a v0.6.0 key has stored.
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"window":"weekly"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn bad_color_field_does_not_reset_window() {
        let s: UsageGaugeSettings =
            serde_json::from_str(r#"{"window":"monthly","colorWatch":42,"watch":"abc"}"#).unwrap();
        assert_eq!(s.window, WindowKind::Monthly);
        assert_eq!(s.colors.palette.watch, DEFAULT_WATCH);
        assert_eq!(s.colors.marks, Marks::default());
    }

    #[test]
    fn settings_round_trip_through_json() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"weekly","watch":40,"risk":60,"critical":80,"colorMode":"pace"}"#,
        )
        .unwrap();
        let back: UsageGaugeSettings =
            serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back.window, WindowKind::Weekly);
        assert_eq!(back.colors, s.colors);
    }
}
