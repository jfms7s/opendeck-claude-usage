//! The lifecycle every hub-driven action shares - appear, new settings,
//! disappear, press, release, refresh - written once, so its ordering
//! rules (KI-06, KI-08) hold for every action instead of being re-applied
//! to each by hand. An action supplies only its settings type: what view
//! the settings draw, and what a short press switches to.

use std::sync::Arc;

use openaction::OpenActionResult;
use serde::Serialize;

use crate::hub::{UsageHub, View};
use crate::press::{PressCycler, Release};
use crate::surface::Surface;

pub trait HubSettings: Clone + Serialize + Send + Sync + 'static {
    /// What an instance with these settings shows.
    fn view(&self) -> View;

    /// The settings a short press switches to; `None` when there's nothing
    /// to switch to (the press does nothing).
    fn next(&self) -> Option<Self> {
        None
    }

    /// Named in the warning when a switched setting can't be saved.
    const SWITCHED: &'static str;
}

pub struct HubActionCore<S> {
    hub: Arc<UsageHub>,
    presses: PressCycler<S>,
}

impl<S: HubSettings> HubActionCore<S> {
    pub fn new(hub: Arc<UsageHub>) -> Self {
        Self {
            hub,
            presses: PressCycler::default(),
        }
    }

    pub async fn appear<T: Surface + ?Sized>(
        &self,
        surface: &T,
        settings: &S,
    ) -> OpenActionResult<()> {
        self.presses.set(surface.id(), settings);
        let view = settings.view();
        self.hub.appear(surface.id(), Arc::clone(&view));
        self.hub.render_cached(surface, &view).await
    }

    /// New settings from OpenDeck (the PI saved, or a press's own save
    /// echoed back).
    pub async fn settings_changed<T: Surface + ?Sized>(
        &self,
        surface: &T,
        settings: &S,
    ) -> OpenActionResult<()> {
        self.presses.set(surface.id(), settings);
        let view = settings.view();
        self.hub.track(surface.id(), Arc::clone(&view));
        self.hub.render_cached(surface, &view).await
    }

    pub fn disappear(&self, instance_id: &str) {
        self.presses.forget(instance_id);
        self.hub.untrack(instance_id);
    }

    pub fn press(&self, instance_id: &str) {
        self.presses.down(instance_id);
    }

    /// A key or dial release: a long press refreshes, a short one switches
    /// to `S::next` of the instance's own last settings (KI-08).
    pub async fn release<T: Surface + ?Sized>(
        &self,
        surface: &T,
        event_settings: &S,
    ) -> OpenActionResult<()> {
        match self.presses.up(surface.id(), event_settings, S::next) {
            Release::Refresh => self.refresh(surface, event_settings).await,
            Release::Switch(updated) => self.switch(surface, updated).await,
            Release::Stay => Ok(()),
        }
    }

    /// Reads and redraws now, from the instance's own last settings.
    pub async fn refresh<T: Surface + ?Sized>(
        &self,
        surface: &T,
        event_settings: &S,
    ) -> OpenActionResult<()> {
        let view = self.presses.current(surface.id(), event_settings).view();
        self.hub.refresh_one(surface, &view).await
    }

    /// Shows `updated` and saves it, so it survives an OpenDeck restart.
    /// The view is tracked *before* the save is awaited, so a poll landing
    /// meanwhile already draws it (KI-06); then it's redrawn from the
    /// cache - instant, no API call.
    async fn switch<T: Surface + ?Sized>(&self, surface: &T, updated: S) -> OpenActionResult<()> {
        let view = updated.view();
        self.hub.track(surface.id(), Arc::clone(&view));
        let saved = match serde_json::to_value(&updated) {
            Ok(value) => surface.persist(value).await,
            Err(e) => Err(e.into()),
        };
        if let Err(e) = saved {
            log::warn!("could not save the {}: {e}", S::SWITCHED);
        }
        self.hub.render_cached(surface, &view).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::history::Reading;
    use crate::hub::HubView;
    use crate::hub::test_support::idle_hub;
    use crate::source::UsageSnapshot;
    use crate::surface::Output;
    use crate::surface::test_support::FakeSurface;
    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    /// A view that draws its own number.
    #[derive(Debug)]
    struct NumberView(u8);

    impl HubView for NumberView {
        fn output(
            &self,
            _: Option<&UsageSnapshot>,
            _: &[Reading],
            _: bool,
            _: DateTime<Utc>,
        ) -> Output {
            Output::Image(self.0.to_string())
        }
    }

    /// Settings that count up by one per short press, wrapping at 3.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Counter(u8);

    impl HubSettings for Counter {
        fn view(&self) -> View {
            Arc::new(NumberView(self.0))
        }

        fn next(&self) -> Option<Self> {
            Some(Counter((self.0 + 1) % 3))
        }

        const SWITCHED: &'static str = "counter";
    }

    fn drawn(surface: &FakeSurface) -> Vec<String> {
        surface
            .frames()
            .into_iter()
            .map(|o| match o {
                Output::Image(s) => s,
                Output::Feedback(f) => f.to_string(),
            })
            .collect()
    }

    /// KI-08 for every hub action: OpenDeck hands the second of two fast
    /// presses the settings from before the first; both presses count.
    #[tokio::test]
    async fn two_fast_presses_switch_twice() {
        let core = HubActionCore::<Counter>::new(idle_hub());
        let key = FakeSurface::new("ctx1", true);
        core.appear(&key, &Counter(0)).await.unwrap();
        core.release(&key, &Counter(0)).await.unwrap();
        core.release(&key, &Counter(0)).await.unwrap();
        assert_eq!(drawn(&key), ["0", "1", "2"]);
        let saved: Vec<_> = key.persisted.lock().unwrap().clone();
        assert_eq!(saved, [serde_json::json!(1), serde_json::json!(2)]);
    }

    /// KI-06 for every hub action: while the switched settings are being
    /// saved, the hub already tracks the new view.
    #[tokio::test]
    async fn the_new_view_is_tracked_before_the_save_is_awaited() {
        let hub = idle_hub();
        let core = Arc::new(HubActionCore::<Counter>::new(Arc::clone(&hub)));
        let key = FakeSurface::new("ctx1", true);
        core.appear(&key, &Counter(0)).await.unwrap();
        let seen = Arc::new(std::sync::Mutex::new(None));
        {
            let (hub, seen) = (Arc::clone(&hub), Arc::clone(&seen));
            *key.on_persist.lock().unwrap() = Some(Box::new(move || {
                let view = hub.tracked_view("ctx1").unwrap();
                *seen.lock().unwrap() = Some(format!("{view:?}"));
            }));
        }
        core.release(&key, &Counter(0)).await.unwrap();
        assert_eq!(seen.lock().unwrap().as_deref(), Some("NumberView(1)"));
    }

    #[tokio::test]
    async fn a_press_after_new_settings_starts_from_them() {
        let core = HubActionCore::<Counter>::new(idle_hub());
        let key = FakeSurface::new("ctx1", true);
        core.appear(&key, &Counter(0)).await.unwrap();
        core.release(&key, &Counter(0)).await.unwrap(); // -> 1
        core.settings_changed(&key, &Counter(0)).await.unwrap(); // the PI saved 0
        core.release(&key, &Counter(0)).await.unwrap(); // -> 1 again, not 2
        assert_eq!(drawn(&key).last().map(String::as_str), Some("1"));
    }

    #[tokio::test]
    async fn disappearing_untracks_and_forgets() {
        let hub = idle_hub();
        let core = HubActionCore::<Counter>::new(Arc::clone(&hub));
        let key = FakeSurface::new("ctx1", true);
        core.appear(&key, &Counter(2)).await.unwrap();
        core.disappear("ctx1");
        assert!(hub.tracked_view("ctx1").is_none());
        // Starts from the event's settings again.
        core.release(&key, &Counter(0)).await.unwrap();
        assert_eq!(
            key.persisted.lock().unwrap().last(),
            Some(&serde_json::json!(1))
        );
    }
}
