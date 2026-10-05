use crate::hub::Output;
use crate::press::{LatestSettings, Press, PressTimer, Release, on_release};
use crate::source::console::{ConsoleData, ConsoleError, ConsoleSnapshot};
use crate::spend::{ApiSpendSettings, build_spend_display, spend_feedback};
use crate::spend_icon::render_key;
use crate::surface::{Surface, for_each_tracked};
use crate::tile;
use async_trait::async_trait;
use chrono::NaiveDate;
use openaction::{Action, Instance, OpenActionResult};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

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

/// Redraws read the shared 5-minute cache, so a minute keeps keys in step
/// with it (and picks up a newly added key file) at no request cost.
const REFRESH: Duration = Duration::from_secs(60);

fn cycled(settings: &ApiSpendSettings) -> ApiSpendSettings {
    let mut updated = settings.clone();
    updated.range = settings.range.next();
    updated
}

/// What a key or dial release does: a short press cycles the range (the
/// only way to change it), a long press refreshes.
fn release(press: Press, settings: &ApiSpendSettings) -> Release<ApiSpendSettings> {
    on_release(press, || Some(cycled(settings)))
}

#[derive(Clone)]
pub struct ApiSpendAction {
    console: Arc<dyn ConsoleData>,
    /// Each visible instance's last-set settings: what the tick redraws,
    /// and what a press starts from (see `LatestSettings`).
    registry: Arc<LatestSettings<ApiSpendSettings>>,
    /// Tells a short press (next range) from a long one (refresh).
    presses: Arc<PressTimer>,
}

impl ApiSpendAction {
    pub fn new(console: Arc<dyn ConsoleData>) -> Self {
        Self {
            console,
            registry: Arc::new(LatestSettings::default()),
            presses: Arc::new(PressTimer::default()),
        }
    }

    async fn output(&self, settings: &ApiSpendSettings, keypad: bool) -> Output {
        output_from(
            &self.console.snapshot().await,
            settings,
            keypad,
            today_utc(),
        )
    }

    async fn render(
        &self,
        instance: &impl Surface,
        settings: &ApiSpendSettings,
    ) -> OpenActionResult<()> {
        instance
            .push(self.output(settings, instance.is_keypad()).await)
            .await
    }

    fn track(&self, instance_id: &str, settings: &ApiSpendSettings) {
        self.registry.set(instance_id, settings);
    }

    /// Runs forever: every minute, re-renders every visible API Spend key
    /// and dial. Spawned once from `main.rs`.
    pub async fn tick_loop(&self) {
        loop {
            tokio::time::sleep(REFRESH).await;
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
        let outcome = self.console.snapshot().await;
        let outcome = &outcome;
        let today = today_utc();
        for_each_tracked(
            self.registry.ids(),
            |id| self.registry.get(id),
            lookup,
            |instance, settings| async move {
                let output = output_from(outcome, &settings, instance.is_keypad(), today);
                if let Err(e) = instance.push(output).await {
                    log::warn!("api spend render failed: {e}");
                }
            },
        )
        .await;
    }

    fn release(
        &self,
        instance_id: &str,
        settings: &ApiSpendSettings,
        press: Press,
    ) -> Release<ApiSpendSettings> {
        self.registry
            .release(instance_id, settings, |s| release(press, s))
    }

    /// Short press (key or dial) moves to the next range; a long press
    /// redraws from the cache.
    async fn released(
        &self,
        instance: &Instance,
        settings: &ApiSpendSettings,
    ) -> OpenActionResult<()> {
        let press = self.presses.up(&instance.instance_id);
        match self.release(&instance.instance_id, settings, press) {
            Release::Refresh => {
                let current = self.registry.current(&instance.instance_id, settings);
                self.render(instance, &current).await
            }
            Release::Switch(updated) => self.show_range(instance, updated).await,
            Release::Stay => Ok(()),
        }
    }

    async fn show_range(
        &self,
        instance: &Instance,
        updated: ApiSpendSettings,
    ) -> OpenActionResult<()> {
        // Already kept by `release`, so a tick meanwhile draws it.
        if let Err(e) = instance.set_settings(&updated).await {
            log::warn!("could not persist api spend range: {e}");
        }
        self.render(instance, &updated).await
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
    use crate::spend::SpendRange;
    use base64::Engine as _;
    use serde_json::{Value, json};

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
        let manifest: Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let entry = &manifest["Actions"][7];
        assert_eq!(
            entry["UUID"].as_str().unwrap(),
            <ApiSpendAction as Action>::UUID
        );
        assert_eq!(entry["Controllers"], json!(["Encoder", "Keypad"]));
        assert_eq!(entry["Encoder"]["layout"], "layouts/usage.json");
        assert_eq!(
            entry["PropertyInspectorPath"],
            "propertyInspector/apispend.html"
        );
        assert_eq!(entry["Icon"], "icons/apispend");
    }

    #[test]
    fn the_action_icon_is_shipped() {
        assert!(!include_bytes!("../assets/icons/apispend.png").is_empty());
    }

    #[test]
    fn feedback_keys_match_the_shipped_layout() {
        let d = build_spend_display(
            Ok(&ConsoleSnapshot::default()),
            &ApiSpendSettings::default(),
            today_utc(),
        );
        crate::test_support::assert_feedback_matches_layout(
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
    fn a_release_cycles_when_short_and_refreshes_when_long() {
        let s = ApiSpendSettings::default();
        assert_eq!(release(Press::Short, &s), Release::Switch(cycled(&s)));
        assert_eq!(cycled(&s).range, SpendRange::Today);
        assert_eq!(release(Press::Long, &s), Release::Refresh);
    }

    /// KI-08: the second of two fast presses gets OpenDeck's settings from
    /// before the first - it must move on again, not repeat the first.
    #[test]
    fn two_fast_presses_cycle_twice() {
        let a = action(Ok(ConsoleSnapshot::default()));
        let stale = ApiSpendSettings::default();
        a.track("ctx1", &stale);
        let Release::Switch(first) = a.release("ctx1", &stale, Press::Short) else {
            panic!("expected a switch");
        };
        let Release::Switch(second) = a.release("ctx1", &stale, Press::Short) else {
            panic!("expected a switch");
        };
        assert_eq!(first.range, SpendRange::Today);
        assert_eq!(second.range, SpendRange::SevenDay);
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

    /// KI-07: a range changed while the tick awaits the instance lookup
    /// must not be drawn over with the old range.
    #[tokio::test]
    async fn a_range_cycled_during_the_lookup_is_the_one_drawn() {
        let action = action(Ok(ConsoleSnapshot::default()));
        let month = ApiSpendSettings::default();
        let today = cycled(&month);
        action.track("ctx1", &month);
        let surface = FakeSurface::default();
        action
            .render_tracked(|id| {
                action.track(&id, &today); // the press lands here
                std::future::ready(Some(surface.clone()))
            })
            .await;
        let expected = action.output(&today, true).await;
        assert!(
            *surface.pushed.lock().unwrap() == vec![expected],
            "drew the old range"
        );
    }

    #[test]
    fn property_inspector_has_budget_colors_and_key_help() {
        let html = include_str!("../assets/propertyInspector/apispend.html");
        assert!(html.contains(r#"id="budgetDollars""#));
        assert!(html.contains(r#"<script src="colors.js"></script>"#));
        assert!(html.contains("~/.config/opendeck-claude-usage/admin-key"));
        assert!(html.contains("chmod 600"));
        assert!(html.contains("Short press (key or dial)"));
    }

    /// KI-14: the range changes only by pressing, so the PI passes the
    /// stored one through and re-reads it before saving.
    #[test]
    fn property_inspector_keeps_the_pressed_range() {
        let html = include_str!("../assets/propertyInspector/apispend.html");
        assert!(html.contains("storedRange"));
        assert!(html.contains(r#"event: "getSettings""#));
        assert!(html.contains("pendingSave"));
    }
}
