//! Session + Weekly: both usage windows on one key (two rows or two tall
//! bars, flipped by a short press) or one dial strip (two bars).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

use crate::combo::{ComboLayout, combo_feedback};
use crate::format::{build_display, error_display};
use crate::history::Reading;
use crate::hub::{HubView, UsageHub, View};
use crate::hub_action::{HubActionCore, HubSettings};
use crate::level::ColorSettings;
use crate::settings::lenient;
use crate::source::{UsageSnapshot, WindowKind};
use crate::styles::combo::render as combo_key;
use crate::surface::Output;
use crate::tile;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComboSettings {
    /// One set of marks/colors for both bars; each bar still gets its own
    /// level from its own window's usage.
    #[serde(flatten)]
    pub colors: ColorSettings,
    /// Changed only by a short press on the key.
    #[serde(default, deserialize_with = "lenient")]
    pub layout: ComboLayout,
}

/// What a combo instance shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ComboView {
    pub colors: ColorSettings,
    pub layout: ComboLayout,
}

impl HubView for ComboView {
    fn output(
        &self,
        snapshot: Option<&UsageSnapshot>,
        _history: &[Reading],
        keypad: bool,
        now: DateTime<Utc>,
    ) -> Output {
        let (session, weekly) = match snapshot {
            Some(s) => (
                build_display(s, WindowKind::Session, &self.colors, now),
                build_display(s, WindowKind::Weekly, &self.colors, now),
            ),
            None => (error_display(), error_display()),
        };
        if keypad {
            Output::Image(tile::data_uri(&combo_key(&session, &weekly, self.layout)))
        } else {
            Output::Feedback(combo_feedback(&session, &weekly))
        }
    }
}

impl HubSettings for ComboSettings {
    fn view(&self) -> View {
        Arc::new(ComboView {
            colors: self.colors.clone(),
            layout: self.layout,
        })
    }

    /// These settings with the other layout - everything else unchanged.
    fn next(&self) -> Option<Self> {
        let mut updated = self.clone();
        updated.layout = self.layout.flipped();
        Some(updated)
    }

    const SWITCHED: &'static str = "combo layout";
}

/// Goes through `HubActionCore` like the other press-cycling actions, so
/// it gets the same KI-06 / KI-08 press-race handling.
#[derive(Clone)]
pub struct ComboAction {
    core: Arc<HubActionCore<ComboSettings>>,
}

impl ComboAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            core: Arc::new(HubActionCore::new(hub)),
        }
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

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.refresh(instance, settings).await
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.core.press(&instance.instance_id);
        Ok(())
    }

    /// Short press flips horizontal/vertical; a long press refreshes.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.core.release(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::test_support::idle_hub;
    use crate::source::{MonthlyUsage, WindowUsage};
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::{assert_feedback_matches_layout, manifest_entry};
    use chrono::TimeZone;

    fn snapshot() -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: 33.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 13, 22, 40, 0).unwrap()),
            },
            weekly: WindowUsage {
                percent: 29.0,
                resets_at: Some(Utc.with_ymd_and_hms(2026, 9, 17, 6, 0, 0).unwrap()),
            },
            monthly: MonthlyUsage {
                enabled: false,
                percent: None,
                used_dollars: None,
                limit_dollars: None,
            },
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 20, 30, 0).unwrap()
    }

    fn view(layout: ComboLayout) -> ComboView {
        ComboView {
            colors: ColorSettings::default(),
            layout,
        }
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<ComboAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/combo.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/combo.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let feedback = combo_feedback(&error_display(), &error_display());
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/combo.json"),
            &feedback,
            &["s_label", "w_label"],
        );
    }

    #[test]
    fn a_combo_on_a_keypad_is_an_image_per_layout() {
        let h = view(ComboLayout::Horizontal).output(Some(&snapshot()), &[], true, now());
        let v = view(ComboLayout::Vertical).output(Some(&snapshot()), &[], true, now());
        assert!(matches!(h, Output::Image(_)));
        assert_ne!(h, v);
    }

    #[test]
    fn a_combo_on_a_dial_is_two_bar_feedback() {
        let Output::Feedback(f) =
            view(ComboLayout::Horizontal).output(Some(&snapshot()), &[], false, now())
        else {
            panic!("expected feedback");
        };
        assert_eq!(f["s_bar"]["value"], 33.0);
        assert_eq!(f["w_bar"]["value"], 29.0);
    }

    #[test]
    fn a_combo_without_data_is_dashes() {
        let Output::Feedback(f) = view(ComboLayout::Vertical).output(None, &[], false, now())
        else {
            panic!("expected feedback");
        };
        assert_eq!(f["s_value"], "\u{2014}");
    }

    #[test]
    fn empty_settings_are_horizontal_with_default_colors() {
        let s: ComboSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.layout, ComboLayout::Horizontal);
        assert_eq!(s.colors, ColorSettings::default());
    }

    #[test]
    fn colors_only_settings_keep_colors() {
        let s: ComboSettings =
            serde_json::from_str(r##"{"watch":40,"colorNormal":"#112233","layout":"sideways"}"##)
                .unwrap();
        assert_eq!(s.colors.marks.watch, 40.0);
        assert_eq!(s.colors.palette.normal, "#112233");
        assert_eq!(s.layout, ComboLayout::Horizontal);
    }

    #[test]
    fn next_changes_only_the_layout() {
        let s: ComboSettings =
            serde_json::from_str(r#"{"critical":95,"colorMode":"pace"}"#).unwrap();
        let f = s.next().unwrap();
        assert_eq!(f.layout, ComboLayout::Vertical);
        assert_eq!(f.colors, s.colors);
        assert_eq!(f.next().unwrap().layout, ComboLayout::Horizontal);
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

    /// KI-08 on the Combo key itself (it used to start every press from
    /// OpenDeck's possibly stale settings): two fast presses flip twice.
    #[tokio::test]
    async fn two_fast_presses_flip_twice() {
        let action = ComboAction::new(idle_hub());
        let key = FakeSurface::new("ctx1", true);
        let stale = ComboSettings::default();
        action.core.appear(&key, &stale).await.unwrap();
        action.core.release(&key, &stale).await.unwrap();
        action.core.release(&key, &stale).await.unwrap();
        let layouts: Vec<_> = key
            .persisted
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["layout"].clone())
            .collect();
        assert_eq!(layouts, ["vertical", "horizontal"]);
    }

    /// The PI loads the shared colors section and passes the press-picked
    /// layout through its save (exercised in `tests/pi/`).
    #[test]
    fn property_inspector_mounts_colors_and_keeps_layout() {
        let html = include_str!("../assets/propertyInspector/combo.html");
        assert!(html.contains(r#"<script src="colors.js"></script>"#));
        assert!(html.contains("showMode: true"));
        assert!(html.contains(r#"passThrough: ["layout"]"#));
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
