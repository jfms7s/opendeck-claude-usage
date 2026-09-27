use crate::hub::{UsageHub, View};
use crate::level::ColorSettings;
use crate::press::{LatestSettings, Press, PressTimer};
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
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    hub: Arc<UsageHub>,
    /// Tells a short press (cycle style) from a long one (refresh).
    presses: Arc<PressTimer>,
    /// What each key was last set to, so a press starts from it.
    latest: Arc<LatestSettings<UsageGaugeSettings>>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: Arc::new(PressTimer::default()),
            latest: Arc::new(LatestSettings::default()),
        }
    }

    /// Keeps the settings OpenDeck just sent, for the next press.
    fn remember(&self, instance_id: &str, settings: &UsageGaugeSettings) {
        self.latest.set(instance_id, settings);
    }

    /// The settings a short press switches to, or `None` when fewer than
    /// two styles are ticked. Starts from the last-set settings, not the
    /// event's (see `LatestSettings`).
    fn next_settings(
        &self,
        instance_id: &str,
        settings: &UsageGaugeSettings,
    ) -> Option<UsageGaugeSettings> {
        self.latest
            .update(instance_id, settings, UsageGaugeSettings::cycled)
    }

    /// The view a refresh draws - from the last-set settings too.
    fn current_view(&self, instance_id: &str, settings: &UsageGaugeSettings) -> View {
        self.latest.current(instance_id, settings).view()
    }

    /// Switches to the next ticked style: re-tracks the view for the poll
    /// loop, persists it (so it survives an OpenDeck restart), and redraws
    /// from the cached snapshot - instant, no API call.
    async fn cycle_style(
        &self,
        instance: &Instance,
        settings: &UsageGaugeSettings,
    ) -> OpenActionResult<()> {
        let Some(updated) = self.next_settings(&instance.instance_id, settings) else {
            return Ok(());
        };
        // Tracked before the await, so a poll meanwhile draws the new view.
        let view = updated.view();
        self.hub.track(&instance.instance_id, view.clone());
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist gauge style: {e}");
        }
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
        self.remember(&instance.instance_id, settings);
        let view = settings.view();
        self.hub.track(&instance.instance_id, view.clone());
        self.hub.render_cached(instance, &view).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.remember(&instance.instance_id, settings);
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

    async fn dial_up(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        let view = self.current_view(&instance.instance_id, settings);
        self.hub.refresh_one(instance, &view).await
    }

    async fn key_down(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.down(&instance.instance_id);
        Ok(())
    }

    /// Short press cycles the ticked styles; a long press (>= 500 ms)
    /// forces a refresh, which is what a tap did before styles existed.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        match self.presses.up(&instance.instance_id) {
            Press::Long => {
                let view = self.current_view(&instance.instance_id, settings);
                self.hub.refresh_one(instance, &view).await
            }
            Press::Short => self.cycle_style(instance, settings).await,
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

    fn action() -> UsageGaugeAction {
        UsageGaugeAction::new(UsageHub::new(
            crate::hub::test_support::NeverCalled,
            crate::history::HistoryStore::in_memory(),
        ))
    }

    /// KI-08: OpenDeck hands the second of two fast presses the settings
    /// from before the first, so the press must start from the plugin's own
    /// last-set ones.
    #[test]
    fn two_fast_presses_advance_two_styles() {
        let a = action();
        let stale: UsageGaugeSettings =
            serde_json::from_str(r#"{"style":"bar","cycleStyles":["bar","openDonut","thinRing"]}"#)
                .unwrap();
        let first = a.next_settings("ctx1", &stale).unwrap();
        let second = a.next_settings("ctx1", &stale).unwrap();
        assert_eq!(first.styles.style, GaugeStyle::OpenDonut);
        assert_eq!(second.styles.style, GaugeStyle::ThinRing);
    }

    #[test]
    fn a_press_after_new_settings_starts_from_them() {
        let a = action();
        let old: UsageGaugeSettings =
            serde_json::from_str(r#"{"style":"bar","cycleStyles":["bar","openDonut","thinRing"]}"#)
                .unwrap();
        a.next_settings("ctx1", &old);
        let pi: UsageGaugeSettings = serde_json::from_str(
            r#"{"style":"thinRing","cycleStyles":["bar","openDonut","thinRing"]}"#,
        )
        .unwrap();
        a.remember("ctx1", &pi);
        let next = a.next_settings("ctx1", &pi).unwrap();
        assert_eq!(next.styles.style, GaugeStyle::Bar);
    }
}
