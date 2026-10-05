//! Short vs long key presses, shared by every action whose keypad press
//! does two things: a short press changes what the key shows, a long one
//! forces a refresh.

use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Held at least this long, a press refreshes instead of changing the view.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Short,
    Long,
}

/// `None` means `key_up` arrived with no recorded `key_down` (e.g. the
/// plugin restarted mid-press) - treated as the cheaper, reversible short
/// press.
pub fn classify_press(held: Option<Duration>) -> Press {
    match held {
        Some(d) if d >= LONG_PRESS => Press::Long,
        _ => Press::Short,
    }
}

/// What a key or dial release does on an action whose short press
/// switches what it shows: a long press refreshes; a short one switches to
/// `next`'s settings, or stays put when there is nothing to switch to.
/// Pure, so each action's press behaviour is testable without an
/// `Instance`.
#[derive(Debug, Clone, PartialEq)]
pub enum Release<S> {
    Refresh,
    Switch(S),
    Stay,
}

pub fn on_release<S>(press: Press, next: impl FnOnce() -> Option<S>) -> Release<S> {
    match press {
        Press::Long => Release::Refresh,
        Press::Short => next().map_or(Release::Stay, Release::Switch),
    }
}

/// When each key went down, so `key_up` can classify the press.
#[derive(Default)]
pub struct PressTimer {
    pressed_at: DashMap<String, Instant>,
}

impl PressTimer {
    pub fn down(&self, instance_id: &str) {
        self.pressed_at
            .insert(instance_id.to_string(), Instant::now());
    }

    pub fn up(&self, instance_id: &str) -> Press {
        let held = self
            .pressed_at
            .remove(instance_id)
            .map(|(_, down)| down.elapsed());
        classify_press(held)
    }

    /// Drops a pending press - on `will_disappear`, so a key removed
    /// mid-press leaves nothing behind.
    pub fn forget(&self, instance_id: &str) {
        self.pressed_at.remove(instance_id);
    }
}

/// The settings each instance was last given, by OpenDeck or by a press.
/// A press starts from these rather than from its event's: OpenDeck can
/// hand the second of two fast presses the settings from before the
/// first, so both would land on the same choice (KI-08).
pub struct LatestSettings<S> {
    by_instance: DashMap<String, S>,
}

impl<S> Default for LatestSettings<S> {
    fn default() -> Self {
        Self {
            by_instance: DashMap::new(),
        }
    }
}

impl<S: Clone> LatestSettings<S> {
    pub fn set(&self, instance_id: &str, settings: &S) {
        self.by_instance
            .insert(instance_id.to_string(), settings.clone());
    }

    pub fn get(&self, instance_id: &str) -> Option<S> {
        self.by_instance.get(instance_id).map(|s| s.clone())
    }

    /// The kept settings, or `fallback` (the event's) when none are kept.
    pub fn current(&self, instance_id: &str, fallback: &S) -> S {
        self.get(instance_id).unwrap_or_else(|| fallback.clone())
    }

    pub fn ids(&self) -> Vec<String> {
        self.by_instance.iter().map(|e| e.key().clone()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.by_instance.is_empty()
    }

    pub fn forget(&self, instance_id: &str) {
        self.by_instance.remove(instance_id);
    }

    /// Decides a release from the kept settings (`fallback` when none are
    /// kept) and keeps what it switches to, all under one entry lock - so
    /// two presses handled at once can't both start from the same
    /// settings.
    pub fn release(
        &self,
        instance_id: &str,
        fallback: &S,
        decide: impl FnOnce(&S) -> Release<S>,
    ) -> Release<S> {
        let mut entry = self
            .by_instance
            .entry(instance_id.to_string())
            .or_insert_with(|| fallback.clone());
        let release = decide(&entry);
        if let Release::Switch(updated) = &release {
            *entry = updated.clone();
        }
        release
    }
}

/// The press handling every action whose short press switches what it
/// shows: when each key went down, and the settings each instance was last
/// given. `up` classifies the press and decides from - and keeps - the
/// plugin's own last settings, so two fast presses advance twice (KI-08)
/// and a periodic redraw meanwhile already draws the new choice (KI-06,
/// KI-07). Every such action goes through this, so the race fixes live in
/// one place.
pub struct PressCycler<S> {
    presses: PressTimer,
    latest: LatestSettings<S>,
}

impl<S> Default for PressCycler<S> {
    fn default() -> Self {
        Self {
            presses: PressTimer::default(),
            latest: LatestSettings::default(),
        }
    }
}

impl<S: Clone> PressCycler<S> {
    /// An instance appeared or got new settings (from OpenDeck or its PI).
    pub fn set(&self, instance_id: &str, settings: &S) {
        self.latest.set(instance_id, settings);
    }

    /// An instance disappeared: forget its settings and any pending press.
    pub fn forget(&self, instance_id: &str) {
        self.presses.forget(instance_id);
        self.latest.forget(instance_id);
    }

    pub fn down(&self, instance_id: &str) {
        self.presses.down(instance_id);
    }

    /// What a release does: a long press refreshes; a short one switches
    /// to `next` of the kept settings (`event_settings` when none are kept),
    /// keeping the switched-to settings before returning.
    pub fn up(
        &self,
        instance_id: &str,
        event_settings: &S,
        next: impl FnOnce(&S) -> Option<S>,
    ) -> Release<S> {
        let press = self.presses.up(instance_id);
        self.latest.release(instance_id, event_settings, |s| {
            on_release(press, || next(s))
        })
    }

    /// The kept settings, or `fallback` (the event's) when none are kept.
    pub fn current(&self, instance_id: &str, fallback: &S) -> S {
        self.latest.current(instance_id, fallback)
    }

    pub fn get(&self, instance_id: &str) -> Option<S> {
        self.latest.get(instance_id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.latest.ids()
    }

    pub fn is_empty(&self) -> bool {
        self.latest.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_up(n: &i32) -> Option<i32> {
        Some(n + 1)
    }

    /// KI-08: OpenDeck can hand the second of two fast presses the settings
    /// from before the first; both presses must still count.
    #[test]
    fn a_cycler_advances_once_per_press_from_stale_event_settings() {
        let cycler = PressCycler::default();
        cycler.set("a", &1);
        assert_eq!(cycler.up("a", &1, next_up), Release::Switch(2));
        assert_eq!(cycler.up("a", &1, next_up), Release::Switch(3));
        assert_eq!(cycler.current("a", &1), 3);
    }

    #[test]
    fn a_cycler_refreshes_on_a_long_press_without_switching() {
        let cycler = PressCycler::default();
        cycler.set("a", &1);
        cycler
            .presses
            .pressed_at
            .insert("a".to_string(), Instant::now() - Duration::from_millis(600));
        assert_eq!(
            cycler.up("a", &1, |_| unreachable!("a long press never switches")),
            Release::Refresh
        );
        assert_eq!(cycler.current("a", &1), 1);
    }

    #[test]
    fn a_cycler_stays_when_there_is_nothing_to_switch_to() {
        let cycler = PressCycler::default();
        assert_eq!(cycler.up("a", &1, |_| None), Release::Stay);
    }

    #[test]
    fn forgetting_drops_the_settings_and_a_pending_press() {
        let cycler = PressCycler::default();
        cycler.set("a", &5);
        cycler.down("a");
        cycler.forget("a");
        assert!(cycler.is_empty());
        assert!(cycler.presses.pressed_at.is_empty());
        assert_eq!(cycler.current("a", &1), 1);
    }

    #[test]
    fn press_threshold_is_500ms() {
        assert_eq!(
            classify_press(Some(Duration::from_millis(499))),
            Press::Short
        );
        assert_eq!(
            classify_press(Some(Duration::from_millis(500))),
            Press::Long
        );
    }

    #[test]
    fn no_key_down_is_short() {
        assert_eq!(classify_press(None), Press::Short);
    }

    #[test]
    fn a_long_release_refreshes_without_computing_the_switch() {
        let r: Release<u8> = on_release(Press::Long, || unreachable!());
        assert_eq!(r, Release::Refresh);
    }

    #[test]
    fn a_short_release_switches_or_stays() {
        assert_eq!(on_release(Press::Short, || Some(7)), Release::Switch(7));
        assert_eq!(on_release(Press::Short, || None::<u8>), Release::Stay);
    }

    #[test]
    fn timer_up_without_down_is_short() {
        assert_eq!(PressTimer::default().up("a"), Press::Short);
    }

    #[test]
    fn timer_quick_press_is_short() {
        let t = PressTimer::default();
        t.down("a");
        assert_eq!(t.up("a"), Press::Short);
        assert!(t.pressed_at.is_empty());
    }

    #[test]
    fn timer_held_press_is_long() {
        let t = PressTimer::default();
        t.pressed_at
            .insert("a".to_string(), Instant::now() - Duration::from_millis(600));
        assert_eq!(t.up("a"), Press::Long);
    }

    #[test]
    fn timer_forget_clears_the_entry() {
        let t = PressTimer::default();
        t.down("a");
        t.forget("a");
        assert!(t.pressed_at.is_empty());
    }

    fn inc(n: &i32) -> Release<i32> {
        Release::Switch(n + 1)
    }

    #[test]
    fn release_starts_from_the_fallback_then_from_the_kept_settings() {
        let latest = LatestSettings::default();
        assert_eq!(latest.release("a", &1, inc), Release::Switch(2));
        assert_eq!(latest.release("a", &1, inc), Release::Switch(3));
        assert_eq!(latest.get("a"), Some(3));
    }

    #[test]
    fn a_release_that_does_not_switch_keeps_the_settings() {
        let latest = LatestSettings::default();
        latest.set("a", &5);
        assert_eq!(
            latest.release("a", &1, |_| Release::Refresh),
            Release::Refresh
        );
        assert_eq!(latest.release("a", &1, |_| Release::Stay), Release::Stay);
        assert_eq!(latest.current("a", &1), 5);
    }

    #[test]
    fn set_overrides_and_forget_falls_back_to_the_event() {
        let latest = LatestSettings::default();
        latest.release("a", &1, inc);
        latest.set("a", &10);
        assert_eq!(latest.current("a", &1), 10);
        latest.forget("a");
        assert_eq!(latest.current("a", &1), 1);
        assert!(latest.ids().is_empty());
    }
}
