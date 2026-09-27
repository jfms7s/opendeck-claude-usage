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

    pub fn forget(&self, instance_id: &str) {
        self.by_instance.remove(instance_id);
    }

    /// Applies `change` to the kept settings (`fallback` when none are
    /// kept) and keeps the result, all under one entry lock - so two
    /// presses handled at once can't both start from the same settings.
    /// `None` keeps them as they were.
    pub fn update(
        &self,
        instance_id: &str,
        fallback: &S,
        change: impl FnOnce(&S) -> Option<S>,
    ) -> Option<S> {
        let mut entry = self
            .by_instance
            .entry(instance_id.to_string())
            .or_insert_with(|| fallback.clone());
        let updated = change(&entry)?;
        *entry = updated.clone();
        Some(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn update_starts_from_the_fallback_then_from_the_kept_settings() {
        let latest = LatestSettings::default();
        assert_eq!(latest.update("a", &1, |n| Some(n + 1)), Some(2));
        assert_eq!(latest.update("a", &1, |n| Some(n + 1)), Some(3));
        assert_eq!(latest.get("a"), Some(3));
    }

    #[test]
    fn a_none_update_keeps_the_settings() {
        let latest = LatestSettings::default();
        latest.set("a", &5);
        assert_eq!(latest.update("a", &1, |_| None), None);
        assert_eq!(latest.current("a", &1), 5);
    }

    #[test]
    fn set_overrides_and_forget_falls_back_to_the_event() {
        let latest = LatestSettings::default();
        latest.update("a", &1, |n| Some(n + 1));
        latest.set("a", &10);
        assert_eq!(latest.current("a", &1), 10);
        latest.forget("a");
        assert_eq!(latest.current("a", &1), 1);
        assert!(latest.ids().is_empty());
    }
}
