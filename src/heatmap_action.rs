use crate::heatmap::{HeatmapDisplay, HeatmapSettings, build_heatmap};
use crate::hub::KEYPAD_CONTROLLER;
use crate::press::{Press, PressTimer};
use crate::source::logs::LogUsageSource;
use crate::styles::heatmap::{render_key, render_strip};
use crate::tile;
use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// Dial payload for `layouts/chart.json`: the whole strip is one image.
pub fn heatmap_feedback(display: &HeatmapDisplay) -> Value {
    json!({ "chart": tile::data_uri(&render_strip(display)) })
}

/// Logs only change as Claude Code writes them; a minute keeps "today"
/// fresh without rescanning constantly (unchanged files come from the
/// shared mtime cache anyway).
const REFRESH: Duration = Duration::from_secs(60);

fn flipped(settings: &HeatmapSettings) -> HeatmapSettings {
    let mut updated = settings.clone();
    updated.view = settings.view.flipped();
    updated
}

/// What a key or dial release does. Both controllers share one gesture:
/// the view is otherwise only reachable by pressing, so a dial that only
/// refreshed would be stuck on 7 days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Release {
    Flip,
    Refresh,
}

fn on_release(press: Press) -> Release {
    match press {
        Press::Short => Release::Flip,
        Press::Long => Release::Refresh,
    }
}

#[derive(Clone)]
pub struct HeatmapAction {
    logs: Arc<LogUsageSource>,
    registry: Arc<DashMap<String, HeatmapSettings>>,
    /// Tells a short press (flip 7 days / 4 weeks) from a long one (refresh).
    presses: Arc<PressTimer>,
}

impl HeatmapAction {
    pub fn new(logs: Arc<LogUsageSource>) -> Self {
        Self {
            logs,
            registry: Arc::new(DashMap::new()),
            presses: Arc::new(PressTimer::default()),
        }
    }

    async fn render(
        &self,
        instance: &Instance,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        let entries = self.logs.entries().await;
        let display = build_heatmap(&entries, settings, chrono::Local::now());
        if instance.controller == KEYPAD_CONTROLLER {
            // Text is drawn inside the image (see tile.rs).
            instance.set_title(Some(String::new()), None).await?;
            instance
                .set_image(Some(tile::data_uri(&render_key(&display))), None)
                .await
        } else {
            instance.set_feedback(&heatmap_feedback(&display)).await
        }
    }

    fn track(&self, instance_id: &str, settings: &HeatmapSettings) {
        self.registry
            .insert(instance_id.to_string(), settings.clone());
    }

    /// Runs forever: every minute, re-renders every visible heatmap.
    /// Spawned once from `main.rs`.
    pub async fn tick_loop(&self) {
        loop {
            tokio::time::sleep(REFRESH).await;
            let entries: Vec<(String, HeatmapSettings)> = self
                .registry
                .iter()
                .map(|e| (e.key().clone(), e.value().clone()))
                .collect();
            for (instance_id, settings) in entries {
                let Some(instance) = openaction::get_instance(instance_id).await else {
                    continue;
                };
                if let Err(e) = self.render(&instance, &settings).await {
                    log::warn!("heatmap render failed: {e}");
                }
            }
        }
    }

    /// Short press (key or dial) flips 7 days / 4 weeks; a long press
    /// re-reads the logs.
    async fn released(
        &self,
        instance: &Instance,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        match on_release(self.presses.up(&instance.instance_id)) {
            Release::Refresh => self.render(instance, settings).await,
            Release::Flip => self.flip_view(instance, settings).await,
        }
    }

    async fn flip_view(
        &self,
        instance: &Instance,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        let updated = flipped(settings);
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist heatmap view: {e}");
        }
        self.track(&instance.instance_id, &updated);
        self.render(instance, &updated).await
    }
}

#[async_trait]
impl Action for HeatmapAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.heatmap";
    type Settings = HeatmapSettings;

    async fn will_appear(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings);
        self.render(instance, settings).await
    }

    async fn did_receive_settings(
        &self,
        instance: &Instance,
        settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings);
        self.render(instance, settings).await
    }

    async fn will_disappear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        self.presses.forget(&instance.instance_id);
        self.registry.remove(&instance.instance_id);
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

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][5];
        assert_eq!(
            entry["UUID"].as_str().unwrap(),
            <HeatmapAction as Action>::UUID
        );
        assert_eq!(entry["Encoder"]["layout"], "layouts/chart.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/heatmap.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let layout: Value =
            serde_json::from_str(include_str!("../assets/layouts/chart.json")).unwrap();
        let keys: Vec<&str> = layout["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        let d = build_heatmap(&[], &HeatmapSettings::default(), chrono::Utc::now());
        for k in heatmap_feedback(&d).as_object().unwrap().keys() {
            assert!(keys.contains(&k.as_str()), "layout has no item keyed {k}");
        }
    }

    #[test]
    fn feedback_is_an_svg_data_uri() {
        let d = build_heatmap(&[], &HeatmapSettings::default(), chrono::Utc::now());
        assert!(
            heatmap_feedback(&d)["chart"]
                .as_str()
                .unwrap()
                .starts_with("data:image/svg+xml;base64,")
        );
    }

    #[test]
    fn flipped_changes_only_the_view() {
        let s = HeatmapSettings {
            color: "#123456".to_string(),
            ..HeatmapSettings::default()
        };
        let f = flipped(&s);
        assert_eq!(f.view, crate::heatmap::HeatmapView::FourWeeks);
        assert_eq!(f.color, "#123456");
        assert_eq!(f.metric, s.metric);
    }

    /// The PI must offer both metrics and pass the stored view through, or
    /// saving it would reset the view.
    #[test]
    fn property_inspector_offers_metrics_and_keeps_view() {
        let html = include_str!("../assets/propertyInspector/heatmap.html");
        assert!(html.contains(r#"<option value="tokens">"#));
        assert!(html.contains(r#"<option value="cost">"#));
        assert!(html.contains(r#"type="color""#));
        assert!(html.contains("storedView"));
    }

    /// Keys and dials share one gesture - a dial used to only refresh, so
    /// a dial could never reach the 4-week view.
    #[test]
    fn a_release_flips_when_short_and_refreshes_when_long() {
        assert_eq!(on_release(Press::Short), Release::Flip);
        assert_eq!(on_release(Press::Long), Release::Refresh);
    }

    #[test]
    fn property_inspector_hint_covers_dials() {
        let html = include_str!("../assets/propertyInspector/heatmap.html");
        assert!(
            html.contains("Short press (key or dial)"),
            "hint must mention dials"
        );
    }
}
