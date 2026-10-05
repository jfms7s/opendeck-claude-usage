//! Usage Gauge: one usage window (Session, Weekly or Monthly) as a dial's
//! touch-strip bar or a keypad tile in one of six styles; a short keypad
//! press cycles the ticked styles.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

use crate::format::{build_display, error_display, feedback_for_display};
use crate::gauge_style::{GaugeStyle, StyleSettings, next_style};
use crate::history::Reading;
use crate::hub::{HubView, UsageHub, View};
use crate::hub_action::{HubActionCore, HubSettings};
use crate::level::ColorSettings;
use crate::settings::lenient;
use crate::source::{UsageSnapshot, WindowKind};
use crate::styles::build_styled_icon;
use crate::surface::Output;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageGaugeSettings {
    /// Lenient like every other field: an unknown window (a downgrade, a
    /// future window kind) falls back to Session instead of resetting the
    /// key's colors and styles too (KI-10).
    #[serde(default, deserialize_with = "lenient")]
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

/// What a gauge instance shows.
#[derive(Debug, Clone, PartialEq)]
pub struct GaugeView {
    pub window: WindowKind,
    pub colors: ColorSettings,
    pub style: GaugeStyle,
}

impl HubView for GaugeView {
    fn output(
        &self,
        snapshot: Option<&UsageSnapshot>,
        _history: &[Reading],
        keypad: bool,
        now: DateTime<Utc>,
    ) -> Output {
        let display = match snapshot {
            Some(s) => build_display(s, self.window, &self.colors, now),
            None => error_display(),
        };
        if keypad {
            Output::Image(build_styled_icon(&display, self.style))
        } else {
            Output::Feedback(feedback_for_display(&display))
        }
    }
}

impl HubSettings for UsageGaugeSettings {
    fn view(&self) -> View {
        Arc::new(GaugeView {
            window: self.window,
            colors: self.colors.clone(),
            style: self.styles.style,
        })
    }

    /// The next ticked style, or `None` when fewer than two are ticked.
    /// (A long press refreshes, which is what a tap did before styles
    /// existed.)
    fn next(&self) -> Option<Self> {
        let next = next_style(self.styles.style, &self.styles.cycle)?;
        let mut updated = self.clone();
        updated.styles.style = next;
        Some(updated)
    }

    const SWITCHED: &'static str = "gauge style";
}

#[derive(Clone)]
pub struct UsageGaugeAction {
    core: Arc<HubActionCore<UsageGaugeSettings>>,
}

impl UsageGaugeAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            core: Arc::new(HubActionCore::new(hub)),
        }
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

    /// A dial has no styles: pressing it refreshes.
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

    /// Short press cycles the ticked styles; a long one (>= 500 ms)
    /// refreshes.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.core.release(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gauge_style::ALL_STYLES;
    use crate::level::{DEFAULT_WATCH, Marks};
    use crate::source::{MonthlyUsage, WindowUsage};
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
                enabled: true,
                percent: Some(25.0),
                used_dollars: Some(12.5),
                limit_dollars: Some(50.0),
            },
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, 20, 30, 0).unwrap()
    }

    fn view(style: GaugeStyle) -> GaugeView {
        GaugeView {
            window: WindowKind::Session,
            colors: ColorSettings::default(),
            style,
        }
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let feedback = feedback_for_display(&error_display());
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/usage.json"),
            &feedback,
            &[],
        );
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<UsageGaugeAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/index.html"
        );
    }

    #[test]
    fn a_gauge_on_a_keypad_is_an_image() {
        let out = view(GaugeStyle::Speedometer).output(Some(&snapshot()), &[], true, now());
        assert!(matches!(out, Output::Image(ref s) if s.starts_with("data:image/svg+xml;base64,")));
    }

    #[test]
    fn a_gauge_on_a_dial_is_feedback() {
        let Output::Feedback(f) =
            view(GaugeStyle::Speedometer).output(Some(&snapshot()), &[], false, now())
        else {
            panic!("expected feedback");
        };
        assert_eq!(f["percent"], "33%");
    }

    #[test]
    fn no_snapshot_renders_no_data() {
        let Output::Feedback(f) = view(GaugeStyle::Bar).output(None, &[], false, now()) else {
            panic!("expected feedback");
        };
        assert_eq!(f["detail"], "no data");
    }

    #[test]
    fn every_gauge_style_is_an_image_on_a_keypad_and_unchanged_on_a_dial() {
        let dial = view(GaugeStyle::Speedometer).output(Some(&snapshot()), &[], false, now());
        for style in ALL_STYLES {
            assert!(matches!(
                view(style).output(Some(&snapshot()), &[], true, now()),
                Output::Image(_)
            ));
            assert_eq!(
                view(style).output(Some(&snapshot()), &[], false, now()),
                dial
            );
        }
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

    /// KI-10 for the gauge's own `window`: an unknown window (a future
    /// "opus" window, a downgrade) falls back alone.
    #[test]
    fn bad_window_does_not_reset_colors_or_styles() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r##"{"window":"hourly","colorNormal":"#123456","style":"bar","cycleStyles":["bar"]}"##,
        )
        .unwrap();
        assert_eq!(s.window, WindowKind::Session);
        assert_eq!(s.colors.palette.normal, "#123456");
        assert_eq!(s.styles.style, GaugeStyle::Bar);
        assert_eq!(s.styles.cycle, vec![GaugeStyle::Bar]);
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
        assert!(format!("{:?}", s.view()).contains("ThinRing"));
    }

    #[test]
    fn next_moves_to_the_next_style_and_keeps_everything_else() {
        let s: UsageGaugeSettings = serde_json::from_str(
            r#"{"window":"weekly","watch":40,"style":"bar","cycleStyles":["bar","openDonut"]}"#,
        )
        .unwrap();
        let next = s.next().unwrap();
        assert_eq!(next.styles.style, GaugeStyle::OpenDonut);
        assert_eq!(next.window, WindowKind::Weekly);
        assert_eq!(next.colors, s.colors);
        assert_eq!(next.styles.cycle, s.styles.cycle);
    }

    #[test]
    fn next_is_none_with_one_style() {
        let s: UsageGaugeSettings = serde_json::from_str(r#"{"cycleStyles":["bar"]}"#).unwrap();
        assert!(s.next().is_none());
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

    /// KI-14: the PI passes the press-picked `style` through its save (the
    /// pass-through itself is exercised by `tests/pi/pi-common.test.mjs`).
    #[test]
    fn property_inspector_passes_the_style_through() {
        let html = include_str!("../assets/propertyInspector/index.html");
        assert!(html.contains(r#"<script src="pi-common.js"></script>"#));
        assert!(html.contains(r#"passThrough: ["style"]"#));
    }
}
