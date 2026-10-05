//! API Spend: billed Console spend for Today (UTC), 7 days or this month,
//! with an optional monthly budget; a short press (key or dial) cycles the
//! range.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::NaiveDate;
use openaction::{Action, Instance, OpenActionResult};
use tokio::sync::Notify;

use crate::press::{PressCycler, Release};
use crate::source::console::{ConsoleData, ConsoleError, ConsoleSnapshot};
use crate::spend::{ApiSpendSettings, build_spend_display, spend_feedback};
use crate::spend_icon::render_key;
use crate::surface::{Frames, Output, Surface, for_each_tracked};
use crate::tasks::{park_while_empty, sleep_to_next_minute};
use crate::tile;

/// The frame for `settings` on a key or a dial strip.
fn output_from(
    outcome: &Result<ConsoleSnapshot, ConsoleError>,
    settings: &ApiSpendSettings,
    keypad: bool,
    today: NaiveDate,
) -> Output {
    let display = build_spend_display(outcome.as_ref(), settings, today);
    if keypad {
        // Text is drawn inside the image (see tile.rs).
        Output::Image(tile::data_uri(&render_key(&display)))
    } else {
        Output::Feedback(spend_feedback(&display))
    }
}

/// The Admin API buckets by UTC day, so "today" is the UTC date.
fn today_utc() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

/// These settings with the next range - what a short press (key or dial)
/// switches to. The range is otherwise only reachable by pressing.
fn cycled(settings: &ApiSpendSettings) -> Option<ApiSpendSettings> {
    let mut updated = settings.clone();
    updated.range = settings.range.next();
    Some(updated)
}

#[derive(Clone)]
pub struct ApiSpendAction {
    console: Arc<dyn ConsoleData>,
    /// Each visible instance's last-set settings - what the tick redraws,
    /// and what a press starts from (KI-07, KI-08).
    presses: Arc<PressCycler<ApiSpendSettings>>,
    frames: Arc<Frames>,
    /// Pinged when an instance appears, so a parked tick loop resumes.
    wake: Arc<Notify>,
}

impl ApiSpendAction {
    pub fn new(console: Arc<dyn ConsoleData>) -> Self {
        Self {
            console,
            presses: Arc::new(PressCycler::default()),
            frames: Arc::new(Frames::default()),
            wake: Arc::new(Notify::new()),
        }
    }

    /// The frame for `settings`, reading the Console cache.
    async fn output(&self, settings: &ApiSpendSettings, keypad: bool) -> Output {
        output_from(
            &self.console.snapshot().await,
            settings,
            keypad,
            today_utc(),
        )
    }

    async fn render<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &ApiSpendSettings,
    ) -> OpenActionResult<()> {
        let output = self.output(settings, surface.is_keypad()).await;
        self.frames.push(surface, output).await
    }

    fn track(&self, instance_id: &str, settings: &ApiSpendSettings) {
        self.presses.set(instance_id, settings);
        self.wake.notify_one();
    }

    /// Runs forever: just after every minute boundary, re-renders every
    /// visible API Spend key and dial from the shared 5-minute cache (which
    /// also picks up a newly added key file) - no extra requests, and
    /// unchanged frames aren't resent. Parks while none is visible.
    /// Spawned once from `main.rs`, supervised.
    pub async fn tick_loop(self) {
        loop {
            park_while_empty(|| self.presses.is_empty(), &self.wake).await;
            sleep_to_next_minute().await;
            self.render_tracked(openaction::get_instance).await;
        }
    }

    /// Re-renders every tracked instance from one snapshot read, reading
    /// each one's settings only once its instance has been looked up (see
    /// `for_each_tracked`).
    async fn render_tracked<S, L, LF>(&self, lookup: L)
    where
        S: Surface,
        L: FnMut(String) -> LF,
        LF: Future<Output = Option<S>>,
    {
        // Read once, up front: an await between reading an instance's
        // settings and pushing its frame would reopen the race.
        let outcome = self.console.snapshot().await;
        let outcome = &outcome;
        let today = today_utc();
        for_each_tracked(
            self.presses.ids(),
            |id| self.presses.get(id),
            lookup,
            |surface, settings| async move {
                let output = output_from(outcome, &settings, surface.is_keypad(), today);
                if let Err(e) = self.frames.push(&surface, output).await {
                    log::warn!("api spend render failed: {e}");
                }
            },
        )
        .await;
    }

    /// Short press (key or dial) moves to the next range; a long press
    /// redraws from the cache (it can't bypass the 5-minute throttle).
    async fn released<S: Surface + ?Sized>(
        &self,
        surface: &S,
        settings: &ApiSpendSettings,
    ) -> OpenActionResult<()> {
        match self.presses.up(surface.id(), settings, cycled) {
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
                    log::warn!("could not save the api spend range: {e}");
                }
                self.render(surface, &updated).await
            }
            Release::Stay => Ok(()),
        }
    }
}

#[async_trait]
impl Action for ApiSpendAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.apispend";
    type Settings = ApiSpendSettings;

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
    use crate::spend::SpendRange;
    use crate::surface::test_support::FakeSurface;
    use crate::test_support::{assert_feedback_matches_layout, manifest_entry};
    use base64::Engine as _;
    use serde_json::json;

    struct FixedConsole(Result<ConsoleSnapshot, ConsoleError>);

    #[async_trait]
    impl ConsoleData for FixedConsole {
        async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError> {
            self.0.clone()
        }
    }

    fn action(outcome: Result<ConsoleSnapshot, ConsoleError>) -> ApiSpendAction {
        ApiSpendAction::new(Arc::new(FixedConsole(outcome)))
    }

    fn decoded(output: Output) -> String {
        let Output::Image(uri) = output else {
            panic!("expected a keypad image, got {output:?}");
        };
        let b64 = uri.strip_prefix("data:image/svg+xml;base64,").unwrap();
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let entry = manifest_entry(<ApiSpendAction as Action>::UUID);
        assert_eq!(entry["Controllers"], json!(["Encoder", "Keypad"]));
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/apispend.html"
        );
        assert_eq!(entry["Icon"], "icons/apispend");
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let d = build_spend_display(
            Ok(&ConsoleSnapshot::default()),
            &ApiSpendSettings::default(),
            today_utc(),
        );
        assert_feedback_matches_layout(
            include_str!("../assets/layouts/usage.json"),
            &spend_feedback(&d),
            &[],
        );
    }

    #[test]
    fn a_missing_key_draws_no_key_on_a_key() {
        let svg = decoded(output_from(
            &Err(ConsoleError::NoKey),
            &ApiSpendSettings::default(),
            true,
            today_utc(),
        ));
        assert!(svg.contains(">NO KEY</text>"), "{svg}");
        assert!(svg.contains(">API SPEND</text>"), "{svg}");
    }

    #[test]
    fn a_dial_gets_feedback_not_an_image() {
        let output = output_from(
            &Ok(ConsoleSnapshot::default()),
            &ApiSpendSettings::default(),
            false,
            today_utc(),
        );
        let Output::Feedback(f) = output else {
            panic!("expected dial feedback");
        };
        assert_eq!(f["percent"], "$0.00");
        assert_eq!(f["detail"], "API · MONTH");
    }

    #[test]
    fn cycled_changes_only_the_range() {
        let s = ApiSpendSettings {
            budget_dollars: Some(40.0),
            ..ApiSpendSettings::default()
        };
        let c = cycled(&s).unwrap();
        assert_eq!(c.range, SpendRange::Today);
        assert_eq!(c.budget_dollars, Some(40.0));
    }

    /// KI-08: the second of two fast presses gets OpenDeck's settings from
    /// before the first - it must move on again, not repeat the first.
    #[tokio::test]
    async fn two_fast_presses_cycle_twice() {
        let a = action(Ok(ConsoleSnapshot::default()));
        let stale = ApiSpendSettings::default();
        a.track("ctx1", &stale);
        let key = FakeSurface::new("ctx1", true);
        a.released(&key, &stale).await.unwrap();
        a.released(&key, &stale).await.unwrap();
        let ranges: Vec<_> = key
            .persisted
            .lock()
            .unwrap()
            .iter()
            .map(|v| v["range"].clone())
            .collect();
        assert_eq!(ranges, ["today", "sevenday"]);
    }

    /// KI-07: a range changed while the tick awaits the instance lookup
    /// must not be drawn over with the old range.
    #[tokio::test]
    async fn a_range_cycled_during_the_lookup_is_the_one_drawn() {
        let action = action(Ok(ConsoleSnapshot::default()));
        let month = ApiSpendSettings::default();
        let today = cycled(&month).unwrap();
        action.track("ctx1", &month);
        let surface = FakeSurface::new("ctx1", true);
        action
            .render_tracked(|id| {
                action.track(&id, &today); // the press lands here
                std::future::ready(Some(surface.clone()))
            })
            .await;
        let expected = action.output(&today, true).await;
        assert!(surface.frames() == vec![expected], "drew the old range");
    }
}
