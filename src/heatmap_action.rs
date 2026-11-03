//! Usage Heatmap: daily Tokens or Cost from Claude Code's transcripts, as
//! 7 days or a 4-week grid; a short press (key or dial) flips the view.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use openaction::{Action, Instance, OpenActionResult};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::heatmap::{HeatmapDisplay, HeatmapSettings, build_scoped_heatmap, scope_caption};
use crate::press::{PressCycler, Release};
use crate::project_list::{is_projects_request, send_projects};
use crate::source::logs::{LogEntry, LogUsageSource};
use crate::styles::heatmap::{render_key, render_strip};
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};
use crate::tile;

/// Dial payload for `layouts/chart.json`: the whole strip is one image.
pub fn heatmap_feedback(display: &HeatmapDisplay) -> Value {
    json!({ "chart": tile::data_uri(&render_strip(display)) })
}

/// The frame for `settings` on a key or a dial strip (`logs` names a
/// project scope).
fn output_from(
    logs: &LogUsageSource,
    entries: &[LogEntry],
    settings: &HeatmapSettings,
    keypad: bool,
) -> Output {
    let caption = scope_caption(&settings.scope, |id| Some(logs.project_name(id)));
    let display = build_scoped_heatmap(entries, settings, caption.as_deref(), chrono::Local::now());
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
    /// Set once an event has spawned the first transcript scan, so a burst
    /// of appears at start-up spawns one.
    first_scan_spawned: Arc<AtomicBool>,
}

impl HeatmapAction {
    pub fn new(logs: Arc<LogUsageSource>) -> Self {
        Self {
            logs,
            presses: Arc::new(PressCycler::default()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
            first_scan_spawned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The frame for `settings`, reading the logs.
    async fn output(&self, settings: &HeatmapSettings, keypad: bool) -> Output {
        output_from(&self.logs, &self.logs.entries().await, settings, keypad)
    }

    /// Draws one instance for an event (appear, new settings, press).
    /// Until the first transcript scan (seconds, on a big
    /// `~/.claude/projects`) has completed, it draws what is already
    /// scanned instead of waiting - openaction handles events one at a
    /// time - and spawns that scan, which redraws every tracked heatmap.
    async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &HeatmapSettings,
    ) -> OpenActionResult<()> {
        self.render_with(surface, settings, openaction::get_instance)
            .await
    }

    /// `render`, with the lookup the first scan's redraw uses passed in
    /// (tests pass a fake).
    async fn render_with<S, T, L, LF>(
        &self,
        surface: &S,
        settings: &HeatmapSettings,
        lookup: L,
    ) -> OpenActionResult<()>
    where
        S: Surface + ?Sized,
        T: Surface + 'static,
        L: FnMut(String) -> LF + Send + 'static,
        LF: Future<Output = Option<T>> + Send,
    {
        if self.logs.scanned() {
            // A warm rescan only tails the files.
            let output = self.output(settings, surface.is_keypad()).await;
            return self.frames.push(surface, output).await;
        }
        let output = output_from(&self.logs, &self.logs.peek(), settings, surface.is_keypad());
        let pushed = self.frames.push(surface, output).await;
        if !self.first_scan_spawned.swap(true, Ordering::SeqCst) {
            let this = self.clone();
            tokio::spawn(async move {
                this.render_tracked(lookup).await;
                // A scan that panicked leaves the next event to retry.
                if !this.logs.scanned() {
                    this.first_scan_spawned.store(false, Ordering::SeqCst);
                }
            });
        }
        pushed
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
                let output = output_from(&self.logs, entries, &settings, surface.is_keypad());
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

    async fn property_inspector_did_appear(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        send_projects(Arc::clone(&self.logs), instance.instance_id.clone());
        Ok(())
    }

    async fn send_to_plugin(
        &self,
        instance: &Instance,
        _settings: &Self::Settings,
        payload: &Value,
    ) -> OpenActionResult<()> {
        if is_projects_request(payload) {
            send_projects(Arc::clone(&self.logs), instance.instance_id.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heatmap::{HeatmapView, build_heatmap};
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::{assert_feedback_matches_layout, manifest_entry};
    use std::time::Duration;

    /// A projects dir with one transcript line written now.
    fn one_transcript() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let at = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            project.join("session.jsonl"),
            format!(
                r#"{{"type":"assistant","timestamp":"{at}","message":{{"model":"claude-sonnet-5","usage":{{"input_tokens":2000,"output_tokens":3000}}}}}}"#
            ),
        )
        .unwrap();
        root
    }

    /// Before the first transcript scan an event draws at once, without
    /// waiting on the scan; the scan it spawns redraws every tracked
    /// heatmap with the data. A second event meanwhile spawns no second
    /// scan.
    #[tokio::test]
    async fn an_event_before_the_first_scan_draws_at_once_and_the_scan_redraws() {
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        let action = HeatmapAction::new(Arc::clone(&logs));
        let settings = HeatmapSettings::default();
        let key = FakeSurface::new("a", true);
        action.track("a", &settings);
        let lookup = {
            let key = key.clone();
            move |_: String| std::future::ready(Some(key.clone()))
        };
        action
            .render_with(&key, &settings, lookup.clone())
            .await
            .unwrap();
        let empty = output_from(&logs, &[], &settings, true);
        assert_eq!(key.frames(), vec![empty.clone()]);
        assert!(action.first_scan_spawned.load(Ordering::SeqCst));

        tokio::time::timeout(Duration::from_secs(5), async {
            while key.frames().len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first scan never redrew the heatmap");
        assert!(logs.scanned());
        let frames = key.frames();
        assert_eq!(frames[1], output_from(&logs, &logs.peek(), &settings, true));
        assert_ne!(frames[1], empty);
    }

    /// Once scanned, an event reads the (tailed) transcripts directly:
    /// a line written since the scan is drawn.
    #[tokio::test]
    async fn an_event_after_the_first_scan_draws_fresh_entries() {
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        let before = logs.entries().await;
        let line = std::fs::read_to_string(root.path().join("project/session.jsonl")).unwrap();
        let newer = line.replace("\"input_tokens\":2000", "\"input_tokens\":9000");
        std::fs::write(
            root.path().join("project/later.jsonl"),
            format!("{newer}\n"),
        )
        .unwrap();
        let action = HeatmapAction::new(Arc::clone(&logs));
        let settings = HeatmapSettings::default();
        let key = FakeSurface::new("a", true);
        action
            .render_with(&key, &settings, |_: String| {
                std::future::ready(None::<FakeSurface>)
            })
            .await
            .unwrap();
        let after = logs.peek();
        assert_eq!(after.len(), before.len() + 1);
        assert_eq!(
            key.frames(),
            vec![output_from(&logs, &after, &settings, true)]
        );
        assert_ne!(
            key.frames()[0],
            output_from(&logs, &before, &settings, true)
        );
        assert!(!action.first_scan_spawned.load(Ordering::SeqCst));
    }

    /// A heatmap removed before the first scan finishes isn't redrawn.
    #[tokio::test]
    async fn the_first_scan_does_not_redraw_a_removed_heatmap() {
        let root = one_transcript();
        let logs = Arc::new(LogUsageSource::new(root.path().to_path_buf()));
        let action = HeatmapAction::new(Arc::clone(&logs));
        let settings = HeatmapSettings::default();
        let key = FakeSurface::new("a", true);
        action.track("a", &settings);
        let lookup = {
            let key = key.clone();
            move |_: String| std::future::ready(Some(key.clone()))
        };
        action.render_with(&key, &settings, lookup).await.unwrap();
        // Removed before the spawned scan gets to run (one-thread runtime).
        action.presses.forget("a");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !logs.scanned() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(key.frames().len(), 1, "a removed key was redrawn");
    }

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
