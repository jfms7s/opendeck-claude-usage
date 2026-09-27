use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::press::{Press, PressTimer, Release, on_release};
use crate::source::WindowKind;
use crate::style::{StyleSettings, next_style};
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    #[serde(default)]
    pub window: WindowKind,
    /// Flattened so the Property Inspector writes plain top-level fields
    /// (`watch`, `colorNormal`, ...). Its lenient wire format means a bad
    /// color can't make openaction reset `window` too.
    #[serde(flatten)]
    pub colors: ColorSettings,
    /// `style` + `cycleStyles`, lenient in the same way.
    #[serde(flatten)]
    pub styles: StyleSettings,
}

impl UsageGaugeSettings {
    fn view(&self) -> View {
        View::Gauge {
            window: self.window,
            colors: self.colors.clone(),
            style: self.styles.style,
        }
    }

    /// These settings with the next ticked style, or `None` when fewer than
    /// two styles are ticked.
    fn cycled(&self) -> Option<UsageGaugeSettings> {
        let next = next_style(self.styles.style, &self.styles.cycle)?;
        let mut updated = self.clone();
        updated.styles.style = next;
        Some(updated)
    }

    /// Short press cycles the ticked styles (nothing to cycle with one);
    /// a long press (>= 500 ms) forces a refresh, which is what a tap did
    /// before styles existed.
    fn release(&self, press: Press) -> Release<UsageGaugeSettings> {
        on_release(press, || self.cycled())
    }
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    hub: Arc<UsageHub>,
    /// Tells a short press (cycle style) from a long one (refresh).
    presses: Arc<PressTimer>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: Arc::new(PressTimer::default()),
        }
    }

    /// Switches to the next ticked style: persists it (so it survives an
    /// OpenDeck restart), re-tracks the view for the poll loop, and redraws
    /// from the cached snapshot - instant, no API call.
    async fn show_style(
        &self,
        instance: &Instance,
        updated: UsageGaugeSettings,
    ) -> OpenActionResult<()> {
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist gauge style: {e}");
        }
        let view = updated.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
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
        self.presses.forget(&instance.instance_id);
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

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        match settings.release(self.presses.up(&instance.instance_id)) {
            Release::Refresh => self.hub.refresh_one(instance, &settings.view()).await,
            Release::Switch(updated) => self.show_style(instance, updated).await,
            Release::Stay => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{error_display, feedback_for_display};
    use crate::level::{DEFAULT_WATCH, Marks};

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let feedback = feedback_for_display(&error_display());
        crate::test_support::assert_feedback_matches_layout(
            include_str!("../assets/layouts/usage.json"),
            &feedback,
            &[],
        );
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

    use crate::style::{ALL_STYLES, GaugeStyle, StyleSettings};

    #[test]
    fn v070_settings_load_as_speedometer_with_all_styles() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r##"{"window":"weekly","watch":40,"risk":60,"critical":80,"colorNormal":"#112233"}"##,
        )
        .unwrap();
        assert_eq!(s.window, WindowKind::Weekly);
        assert_eq!(s.colors.marks.watch, 40.0);
        assert_eq!(s.colors.palette.normal, "#112233");
        assert_eq!(s.styles, StyleSettings::default());
        assert_eq!(s.styles.cycle, ALL_STYLES.to_vec());
    }

    #[test]
    fn view_carries_the_style() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"style":"thinRing"}"#).unwrap();
        assert!(matches!(
            s.view(),
            View::Gauge {
                style: GaugeStyle::ThinRing,
                ..
            }
        ));
    }

    #[test]
    fn cycled_moves_to_the_next_style_and_keeps_everything_else() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"weekly","watch":40,"style":"bar","cycleStyles":["bar","openDonut"]}"#,
        )
        .unwrap();
        let next = s.cycled().unwrap();
        assert_eq!(next.styles.style, GaugeStyle::OpenDonut);
        assert_eq!(next.window, WindowKind::Weekly);
        assert_eq!(next.colors, s.colors);
        assert_eq!(next.styles.cycle, s.styles.cycle);
    }

    #[test]
    fn cycled_is_none_with_one_style() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"cycleStyles":["bar"]}"#).unwrap();
        assert!(s.cycled().is_none());
    }

    #[test]
    fn a_short_release_switches_to_the_next_style() {
        let s: UsageGaugeSettings =
            serde_json::from_str(r#"{"style":"bar","cycleStyles":["bar","openDonut"]}"#).unwrap();
        let Release::Switch(next) = s.release(Press::Short) else {
            panic!("expected a switch");
        };
        assert_eq!(next.styles.style, GaugeStyle::OpenDonut);
    }

    #[test]
    fn a_short_release_with_one_style_stays() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"cycleStyles":["bar"]}"#).unwrap();
        assert!(matches!(s.release(Press::Short), Release::Stay));
    }

    #[test]
    fn a_long_release_refreshes() {
        let s = UsageGaugeSettings::default();
        assert!(matches!(s.release(Press::Long), Release::Refresh));
    }

    #[test]
    fn full_settings_round_trip() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"monthly","critical":95,"style":"softPill","cycleStyles":["softPill","thinRing"]}"#,
        )
        .unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["window"], "monthly");
        assert_eq!(v["style"], "softPill");
        assert_eq!(v["critical"], 95.0);
        let back: UsageGaugeSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back.styles, s.styles);
        assert_eq!(back.colors, s.colors);
    }

    /// KI-14: the PI re-reads the stored settings before saving, in case
    /// OpenDeck didn't forward the plugin's press-driven `setSettings`.
    #[test]
    fn property_inspector_refreshes_before_saving() {
        let html = include_str!("../assets/propertyInspector/index.html");
        assert!(html.contains(r#"event: "getSettings""#));
        assert!(html.contains("pendingSave"));
    }
}
