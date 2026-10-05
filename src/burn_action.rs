//! Burn Rate: Pace, Even burn or Runway for the Session or Weekly window,
//! on a key or a dial. A press refreshes.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};

use crate::burn::{BurnMetric, build_burn_display, burn_error_display, burn_feedback, burn_window};
use crate::burn_icon::build_burn_icon;
use crate::history::Reading;
use crate::hub::{HubView, UsageHub, View};
use crate::hub_action::{HubActionCore, HubSettings};
use crate::level::ColorSettings;
use crate::settings::lenient;
use crate::source::{UsageSnapshot, WindowKind};
use crate::surface::Output;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BurnRateSettings {
    #[serde(default, deserialize_with = "lenient")]
    pub window: WindowKind,
    #[serde(default, deserialize_with = "lenient")]
    pub metric: BurnMetric,
    /// Only marks and palette matter here - Burn Rate always colors
    /// pace-based, so a stored `colorMode` is ignored.
    #[serde(flatten)]
    pub colors: ColorSettings,
}

/// What a Burn Rate instance shows.
#[derive(Debug, Clone, PartialEq)]
pub struct BurnView {
    pub window: WindowKind,
    pub metric: BurnMetric,
    pub colors: ColorSettings,
}

impl HubView for BurnView {
    fn output(
        &self,
        snapshot: Option<&UsageSnapshot>,
        _history: &[Reading],
        keypad: bool,
        now: DateTime<Utc>,
    ) -> Output {
        let display = match snapshot {
            Some(s) => build_burn_display(s, self.window, self.metric, &self.colors, now),
            None => burn_error_display(self.metric),
        };
        if keypad {
            Output::Image(build_burn_icon(&display))
        } else {
            Output::Feedback(burn_feedback(&display))
        }
    }
}

impl HubSettings for BurnRateSettings {
    fn view(&self) -> View {
        Arc::new(BurnView {
            window: burn_window(self.window),
            metric: self.metric,
            colors: self.colors.clone(),
        })
    }

    // Nothing to switch: a press only refreshes.
    const SWITCHED: &'static str = "burn rate settings";
}

#[derive(Clone)]
pub struct BurnRateAction {
    core: Arc<HubActionCore<BurnRateSettings>>,
}

impl BurnRateAction {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            core: Arc::new(HubActionCore::new(hub)),
        }
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

    /// A tap forces an immediate refresh of just that tile.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.core.refresh(instance, settings).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn view() -> BurnView {
        BurnView {
            window: WindowKind::Session,
            metric: BurnMetric::EvenBurn,
            colors: ColorSettings::default(),
        }
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<BurnRateAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/burnrate.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let feedback = burn_feedback(&burn_error_display(BurnMetric::Pace));
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/usage.json"),
            &feedback,
            &[],
        );
    }

    #[test]
    fn burn_on_a_dial_is_feedback() {
        let Output::Feedback(f) = view().output(Some(&snapshot()), &[], false, now()) else {
            panic!("expected feedback");
        };
        assert!(f["detail"].as_str().unwrap().ends_with("session"));
    }

    #[test]
    fn burn_on_a_keypad_is_an_image() {
        assert!(matches!(
            view().output(Some(&snapshot()), &[], true, now()),
            Output::Image(_)
        ));
    }

    #[test]
    fn no_snapshot_renders_no_data() {
        let Output::Feedback(f) = view().output(None, &[], false, now()) else {
            panic!("expected feedback");
        };
        assert_eq!(f["detail"], "no data");
    }

    #[test]
    fn a_press_has_nothing_to_switch_to() {
        assert!(BurnRateSettings::default().next().is_none());
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

    /// KI-10: the PI shows a real option for a stored value it doesn't
    /// know (`selectKnown`, exercised in `tests/pi/`), or the next edit
    /// would send "".
    #[test]
    fn property_inspector_falls_back_to_known_options() {
        let html = include_str!("../assets/propertyInspector/burnrate.html");
        assert!(html.contains(r#"selectKnown(document.getElementById("metric")"#));
        assert!(html.contains(r#"selectKnown(document.getElementById("window")"#));
    }

    #[test]
    fn monthly_setting_views_as_session() {
        let s: BurnRateSettings = serde_json::from_str(r#"{"window":"monthly"}"#).unwrap();
        assert!(format!("{:?}", s.view()).contains("window: Session"));
    }
}
