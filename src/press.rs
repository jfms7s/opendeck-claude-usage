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
}
