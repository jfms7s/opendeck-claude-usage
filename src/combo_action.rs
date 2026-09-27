use crate::combo::LayoutSettings;
use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::press::{Press, PressTimer, Release, on_release};
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComboSettings {
    /// One set of marks/colors for both bars; each bar still gets its own
    /// level from its own window's usage.
    #[serde(flatten)]
    pub colors: ColorSettings,
    #[serde(flatten)]
    pub layout: LayoutSettings,
}

impl ComboSettings {
    fn view(&self) -> View {
        View::Combo {
            colors: self.colors.clone(),
            layout: self.layout.layout,
        }
    }

    /// These settings with the other layout - everything else unchanged.
    fn flipped(&self) -> ComboSettings {
        let mut updated = self.clone();
        updated.layout.layout = self.layout.layout.flipped();
        updated
    }

    /// Short press flips horizontal/vertical; a long press refreshes.
    fn release(&self, press: Press) -> Release<ComboSettings> {
        on_release(press, || Some(self.flipped()))
    }
}

#[derive(Clone)]
pub struct ComboAction {
    hub: Arc<UsageHub>,
    /// Tells a short press (flip layout) from a long one (refresh).
    presses: Arc<PressTimer>,
}

impl ComboAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: Arc::new(PressTimer::default()),
        }
    }

    /// Switches to the other layout: persists it (survives restarts),
    /// re-tracks the view for the poll loop, redraws from the cache.
    async fn show_layout(
        &self,
        instance: &Instance,
        updated: ComboSettings,
    ) -> OpenActionResult<()> {
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist combo layout: {e}");
        }
        let view = updated.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }
}

#[async_trait]
impl Action for ComboAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.combo";
    type Settings = ComboSettings;

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
            Release::Switch(updated) => self.show_layout(instance, updated).await,
            Release::Stay => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combo::{ComboLayout, combo_feedback};
    use crate::format::error_display;

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][4];
        assert_eq!(
            entry["UUID"].as_str().unwrap(),
            <ComboAction as Action>::UUID
        );
        assert_eq!(entry["Encoder"]["layout"], "layouts/combo.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/combo.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let feedback = combo_feedback(&error_display(), &error_display());
        crate::test_support::assert_feedback_matches_layout(
            include_str!("../assets/layouts/combo.json"),
            &feedback,
            &["s_label", "w_label"],
        );
    }

    #[test]
    fn empty_settings_are_horizontal_with_default_colors() {
        let s: ComboSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.layout.layout, ComboLayout::Horizontal);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn colors_only_settings_keep_colors() {
        let s: ComboSettings =
            serde_json::from_str(r##"{"watch":40,"colorNormal":"#112233","layout":"sideways"}"##)
                .unwrap();
        assert_eq!(s.colors.marks.watch, 40.0);
        assert_eq!(s.colors.palette.normal, "#112233");
        assert_eq!(s.layout.layout, ComboLayout::Horizontal);
    }

    #[test]
    fn flipped_changes_only_the_layout() {
        let s: ComboSettings =
            serde_json::from_str(r#"{"critical":95,"colorMode":"pace"}"#).unwrap();
        let f = s.flipped();
        assert_eq!(f.layout.layout, ComboLayout::Vertical);
        assert_eq!(f.colors, s.colors);
        assert!(matches!(
            f.view(),
            View::Combo {
                layout: ComboLayout::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn a_release_flips_when_short_and_refreshes_when_long() {
        let s = ComboSettings::default();
        let Release::Switch(next) = s.release(Press::Short) else {
            panic!("expected a switch");
        };
        assert_eq!(next.layout.layout, ComboLayout::Vertical);
        assert!(matches!(s.release(Press::Long), Release::Refresh));
    }

    #[test]
    fn settings_round_trip_flat() {
        let s: ComboSettings = serde_json::from_str(r#"{"layout":"vertical","risk":70}"#).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["layout"], "vertical");
        assert_eq!(v["risk"], 70.0);
        let back: ComboSettings = serde_json::from_value(v).unwrap();
        assert_eq!(back.layout, s.layout);
        assert_eq!(back.colors, s.colors);
    }

    /// The PI must load the shared colors section and pass the stored
    /// layout through, or saving it would reset the layout.
    #[test]
    fn property_inspector_mounts_colors_and_keeps_layout() {
        let html = include_str!("../assets/propertyInspector/combo.html");
        assert!(html.contains(r#"<script src="colors.js"></script>"#));
        assert!(html.contains("showMode: true"));
        assert!(html.contains("storedLayout"));
    }

    /// KI-14: the PI re-reads the stored settings before saving, in case
    /// OpenDeck didn't forward the plugin's press-driven `setSettings`.
    #[test]
    fn property_inspector_refreshes_before_saving() {
        let html = include_str!("../assets/propertyInspector/combo.html");
        assert!(html.contains(r#"event: "getSettings""#));
        assert!(html.contains("pendingSave"));
    }

    /// KI-15: the hint uses the spec's wording.
    #[test]
    fn property_inspector_hint_matches_the_spec() {
        let html = include_str!("../assets/propertyInspector/combo.html");
        assert!(html.contains(
            r#"<p class="hint">Short press flips between horizontal and vertical. Hold to refresh.</p>"#
        ));
    }
}
