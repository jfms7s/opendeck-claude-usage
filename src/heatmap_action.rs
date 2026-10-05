//! Usage Heatmap: daily Tokens or Cost from Claude Code's transcripts, as
//! 7 days or a 4-week grid; a short press (key or dial) flips the view.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::heatmap::{HeatmapDisplay, HeatmapSettings, build_heatmap};
use crate::press::{PressCycler, Release};
use crate::source::logs::{LogEntry, LogUsageSource};
use crate::styles::heatmap::{render_key, render_strip};
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};
use crate::tile;

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

/// These settings with the other view - what a short press (key or dial)
/// switches to. Both controllers share the gesture: the view is otherwise
/// only reachable by pressing, so a dial that only refreshed would be
/// stuck on 7 days.
fn flipped(settings: &HeatmapSettings) -> Option<HeatmapSettings> {
    let mut updated = settings.clone();
    updated.view = settings.view.flipped();
    Some(updated)
}

#[derive(Clone)]
pub struct HeatmapAction {
    logs: Arc<LogUsageSource>,
    /// Each visible instance's last-set settings - what the tick redraws,
    /// and what a press starts from (KI-07, KI-08).
    presses: Arc<PressCycler<HeatmapSettings>>,
    frames: Arc<Frames>,
    /// Pinged when an instance appears, so a parked tick loop resumes.
    wake: Arc<Notify>,
}

impl HeatmapAction {
    pub fn new(logs: Arc<LogUsageSource>) -> Self {
        Self {
            logs,
            presses: Arc::new(PressCycler::default()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
        }
    }

    /// The frame for `settings`, reading the logs.
    async fn output(&self, settings: &HeatmapSettings, keypad: bool) -> Output {
        output_from(&self.logs.entries().await, settings, keypad)
    }

    async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        let output = self.output(settings, surface.is_keypad()).await;
        self.frames.push(surface, output).await
    }

    fn track(&self, instance_id: &str, settings: &HeatmapSettings) {
        self.presses.set(instance_id, settings);
        self.wake.notify_one();
    }

    /// Runs forever: just after every minute boundary, re-renders every
    /// visible heatmap (the logs only change as Claude Code writes them,
    /// and only appended lines are read; unchanged frames aren't resent).
    /// Parks while none is visible. Spawned once from `main.rs`,
    /// supervised.
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.presses.is_empty(), &self.wake).await;
            sleep_to_next_minute().await;
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
            self.presses.ids(),
            |id| self.presses.get(id),
            lookup,
            |surface, settings| async move {
                let output = output_from(entries, &settings, surface.is_keypad());
                if let Err(e) = self.frames.push(&surface, output).await {
                    log::warn!("heatmap render failed: {e}");
                }
            },
        )
        .await;
    }

    /// Short press (key or dial) flips 7 days / 4 weeks; a long press
    /// re-reads the logs.
    async fn released<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        match self.presses.up(surface.id(), settings, flipped) {
            Release::Refresh => {
                let current = self.presses.current(surface.id(), settings);
                self.render(surface, &current).await
            }
            Release::Switch(updated) => {
                // Already kept by `up`, so a tick meanwhile draws it.
                let saved = match serde_json::to_value(&updated) {
                    Ok(value) => surface.persist(value).await,
                    Err(e) => Err(e.into()),
                };
                if let Err(e) = saved {
                    log::warn!("could not save the heatmap view: {e}");
                }
                self.render(surface, &updated).await
            }
            Release::Stay => Ok(()),
        }
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
        self.frames.forget(&instance.instance_id);
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
        self.frames.forget(&instance.instance_id);
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
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::{assert_feedback_matches_layout, manifest_entry};

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<HeatmapAction as Action>::UUID);
        assert_eq!(entry["Encoder"]["layout"], "layouts/chart.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/heatmap.html"
        );
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let d = build_heatmap(&[], &HeatmapSettings::default(), chrono::Utc::now());
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/chart.json"),
            &heatmap_feedback(&d),
            &[],
        );
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
        let f = flipped(&s).unwrap();
        assert_eq!(f.view, HeatmapView::FourWeeks);
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
        assert!(html.contains(r#"passThrough: ["view"]"#));
    }

    #[test]
    fn property_inspector_hint_covers_dials() {
        let html = include_str!("../assets/propertyInspector/heatmap.html");
        assert!(
            html.contains("Short press (key or dial)"),
            "hint must mention dials"
        );
    }

    fn action() -> HeatmapAction {
        let dir = tempfile::tempdir().unwrap();
        HeatmapAction::new(Arc::new(LogUsageSource::new(dir.path().join("none"))))
    }

    /// KI-07: a flip that lands while the tick awaits the instance lookup
    /// must not be drawn over with the old view.
    #[tokio::test]
    async fn a_view_flipped_during_the_lookup_is_the_one_drawn() {
        let action = action();
        let seven = HeatmapSettings::default();
        let four = flipped(&seven).unwrap();
        action.track("ctx1", &seven);
        let surface = FakeSurface::new("ctx1", true);
        action
            .render_tracked(|id| {
                action.track(&id, &four); // the press lands here
                std::future::ready(Some(surface.clone()))
            })
            .await;
        let expected = action.output(&four, true).await;
        assert!(
            surface.frames() == vec![expected],
            "drew the old 7-day view"
        );
    }

    /// KI-08: the second of two fast presses gets OpenDeck's settings from
    /// before the first - it must flip back, not land on 4 weeks again.
    #[tokio::test]
    async fn two_fast_presses_flip_twice() {
        let a = action();
        let stale = HeatmapSettings::default();
        a.track("ctx1", &stale);
        let key = FakeSurface::new("ctx1", true);
        a.released(&key, &stale).await.unwrap();
        a.released(&key, &stale).await.unwrap();
        let views: Vec<_> = key
            .persisted
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["view"].clone())
            .collect();
        assert_eq!(views, ["fourWeeks", "sevenDays"]);
    }

    /// While the flipped view is being saved, the tick already draws it.
    #[tokio::test]
    async fn a_flip_is_kept_before_its_save_is_awaited() {
        let a = action();
        let stale = HeatmapSettings::default();
        a.track("ctx1", &stale);
        let key = FakeSurface::new("ctx1", true);
        let presses = Arc::clone(&a.presses);
        let seen = Arc::new(std::sync::Mutex::new(None));
        let seen_in_hook = Arc::clone(&seen);
        *key.on_persist.lock().unwrap() = Some(Box::new(move || {
            *seen_in_hook.lock().unwrap() = presses.get("ctx1").map(|s| s.view);
        }));
        a.released(&key, &stale).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), Some(HeatmapView::FourWeeks));
    }
}
