use crate::heatmap::{HeatmapDisplay, HeatmapSettings, build_heatmap};
use crate::hub::Output;
use crate::press::{LatestSettings, Press, PressTimer};
use crate::source::logs::{LogEntry, LogUsageSource};
use crate::styles::heatmap::{render_key, render_strip};
use crate::surface::{Surface, for_each_tracked};
use crate::tile;
use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Dial payload for `layouts/chart.json`: the whole strip is one image.
pub fn heatmap_feedback(display: &HeatmapDisplay) -> Value {
    json!({ "chart": tile::data_uri(&render_strip(display)) })
}

/// The frame for `settings` on a key or a dial strip.
fn output_from(entries: &[LogEntry], settings: &HeatmapSettings, keypad: bool) -> Output {
    let display = build_heatmap(entries, settings, chrono::Local::now());
    if keypad {
        // Text is drawn inside the image (see tile.rs).
        Output::Image(tile::data_uri(&render_key(&display)))
    } else {
        Output::Feedback(heatmap_feedback(&display))
    }
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
    /// Each visible instance's last-set settings: what the tick redraws,
    /// and what a press starts from (see `LatestSettings`).
    registry: Arc<LatestSettings<HeatmapSettings>>,
    /// Tells a short press (flip 7 days / 4 weeks) from a long one (refresh).
    presses: Arc<PressTimer>,
}

impl HeatmapAction {
    pub fn new(logs: Arc<LogUsageSource>) -> Self {
        Self {
            logs,
            registry: Arc::new(LatestSettings::default()),
            presses: Arc::new(PressTimer::default()),
        }
    }

    /// The frame for `settings` on a key or a dial strip, reading the logs.
    async fn output(&self, settings: &HeatmapSettings, keypad: bool) -> Output {
        output_from(&self.logs.entries().await, settings, keypad)
    }

    async fn render(
        &self,
        instance: &impl Surface,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        instance
            .push(self.output(settings, instance.is_keypad()).await)
            .await
    }

    fn track(&self, instance_id: &str, settings: &HeatmapSettings) {
        self.registry.set(instance_id, settings);
    }

    /// Runs forever: every minute, re-renders every visible heatmap.
    /// Spawned once from `main.rs`.
    pub async fn tick_loop(&self) {
        loop {
            tokio::time::sleep(REFRESH).await;
            self.render_tracked(openaction::get_instance).await;
        }
    }

    /// Re-renders every tracked heatmap, reading each one's settings only
    /// once its instance has been looked up (see `for_each_tracked`).
    async fn render_tracked<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        // Scanned once, up front: an await between reading an instance's
        // settings and pushing its frame would reopen the race.
        let entries = self.logs.entries().await;
        let entries = &entries;
        for_each_tracked(
            self.registry.ids(),
            |id| self.registry.get(id),
            lookup,
            |instance, settings| async move {
                let output = output_from(entries, &settings, instance.is_keypad());
                if let Err(e) = instance.push(output).await {
                    log::warn!("heatmap render failed: {e}");
                }
            },
        )
        .await;
    }

    /// Short press (key or dial) flips 7 days / 4 weeks; a long press
    /// re-reads the logs.
    async fn released(
        &self,
        instance: &Instance,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        match on_release(self.presses.up(&instance.instance_id)) {
            Release::Refresh => {
                let current = self.registry.current(&instance.instance_id, settings);
                self.render(instance, &current).await
            }
            Release::Flip => self.flip_view(instance, settings).await,
        }
    }

    /// The settings a short press switches to, kept as the instance's
    /// latest. Starts from those, not the event's (see `LatestSettings`).
    fn next_settings(&self, instance_id: &str, settings: &HeatmapSettings) -> HeatmapSettings {
        self.registry
            .update(instance_id, settings, |s| Some(flipped(s)))
            .unwrap_or_else(|| flipped(settings))
    }

    async fn flip_view(
        &self,
        instance: &Instance,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        let updated = self.next_settings(&instance.instance_id, settings);
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist heatmap view: {e}");
        }
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
        self.registry.forget(&instance.instance_id);
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
    use crate::heatmap::HeatmapView;

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

    #[derive(Clone, Default)]
    struct FakeSurface {
        pushed: Arc<std::sync::Mutex<Vec<Output>>>,
    }

    #[async_trait]
    impl Surface for FakeSurface {
        fn is_keypad(&self) -> bool {
            true
        }

        async fn push(&self, output: Output) -> OpenActionResult<()> {
            self.pushed.lock().unwrap().push(output);
            Ok(())
        }
    }

    /// KI-07: a flip that lands while the tick awaits the instance lookup
    /// must not be drawn over with the old view.
    #[tokio::test]
    async fn a_view_flipped_during_the_lookup_is_the_one_drawn() {
        let dir = tempfile::tempdir().unwrap();
        let action = HeatmapAction::new(Arc::new(LogUsageSource::new(dir.path().to_path_buf())));
        let seven = HeatmapSettings::default();
        let four = flipped(&seven);
        action.track("ctx1", &seven);
        let surface = FakeSurface::default();
        action
            .render_tracked(|id| {
                action.track(&id, &four); // the press lands here
                std::future::ready(Some(surface.clone()))
            })
            .await;
        let expected = action.output(&four, true).await;
        assert!(
            *surface.pushed.lock().unwrap() == vec![expected],
            "drew the old 7-day view"
        );
    }

    /// KI-08: the second of two fast presses gets OpenDeck's settings from
    /// before the first - it must flip back, not land on 4 weeks again.
    #[test]
    fn two_fast_presses_flip_twice() {
        let dir = tempfile::tempdir().unwrap();
        let a = HeatmapAction::new(Arc::new(LogUsageSource::new(dir.path().to_path_buf())));
        let stale = HeatmapSettings::default();
        a.track("ctx1", &stale);
        assert_eq!(a.next_settings("ctx1", &stale).view, HeatmapView::FourWeeks);
        assert_eq!(a.next_settings("ctx1", &stale).view, HeatmapView::SevenDays);
    }
}
