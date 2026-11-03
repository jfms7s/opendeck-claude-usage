//! When a usage key flashes (opt-in per key): its window reset since the
//! previous poll (`Alert::Fresh`, OpenDeck's check mark), or its usage
//! crossed the key's own Critical mark (`Alert::Critical`, the warning
//! triangle). Pure: the hub compares the previous poll's snapshot with the
//! new one and asks each view.

use crate::history::same_reset;
use crate::level::{ColorSettings, Level};
use crate::source::{UsageSnapshot, WindowKind, WindowUsage};
use crate::surface::Alert;

fn window(snapshot: &UsageSnapshot, kind: WindowKind) -> &WindowUsage {
    match kind {
        WindowKind::Session => &snapshot.session,
        WindowKind::Weekly => &snapshot.weekly,
    }
}

/// What `kind`'s window did between two polls, as `colors` marks it.
/// Crossing Critical wins over a reset (a fresh window is never Critical in
/// practice). A reset is a move of `resets_at` beyond the API's per-request
/// jitter (`same_reset`); a window that expired idle (no reset time) and
/// one that starts later are not resets the key saw happen.
pub fn window_alert(
    prev: &UsageSnapshot,
    cur: &UsageSnapshot,
    kind: WindowKind,
    colors: &ColorSettings,
) -> Option<Alert> {
    let (before, now) = (window(prev, kind), window(cur, kind));
    if colors.level(before.percent) < Level::Critical
        && colors.level(now.percent) == Level::Critical
    {
        return Some(Alert::Critical);
    }
    let reset = before.resets_at.is_some()
        && now.resets_at.is_some()
        && !same_reset(before.resets_at, now.resets_at);
    reset.then_some(Alert::Fresh)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::source::MonthlyUsage;
    use chrono::{DateTime, Duration, Utc};

    pub const R: &str = "2026-09-30T10:00:00Z";

    pub fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    /// A snapshot with this session percent and reset time; weekly fixed.
    pub fn snap(session: f64, resets_at: Option<DateTime<Utc>>) -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: session,
                resets_at,
            },
            weekly: WindowUsage {
                percent: 20.0,
                resets_at: Some(at(R) + Duration::days(3)),
            },
            monthly: MonthlyUsage {
                enabled: false,
                percent: None,
                used_dollars: None,
                limit_dollars: None,
            },
            details: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use chrono::Duration;

    fn alert(prev: &UsageSnapshot, cur: &UsageSnapshot) -> Option<Alert> {
        window_alert(prev, cur, WindowKind::Session, &ColorSettings::default())
    }

    #[test]
    fn the_same_snapshot_or_reset_jitter_is_nothing() {
        let s = snap(30.0, Some(at(R)));
        assert_eq!(alert(&s, &s), None);
        let jittered = snap(31.0, Some(at(R) + Duration::minutes(4)));
        assert_eq!(alert(&s, &jittered), None);
    }

    #[test]
    fn a_moved_reset_is_a_fresh_window() {
        let before = snap(70.0, Some(at(R)));
        let after = snap(2.0, Some(at(R) + Duration::hours(5)));
        assert_eq!(alert(&before, &after), Some(Alert::Fresh));
        // Weekly didn't move: a weekly key sees nothing.
        let weekly = window_alert(
            &before,
            &after,
            WindowKind::Weekly,
            &ColorSettings::default(),
        );
        assert_eq!(weekly, None);
    }

    #[test]
    fn an_idle_expiry_or_a_later_start_is_not_a_reset() {
        let active = snap(40.0, Some(at(R)));
        let idle = snap(0.0, None);
        assert_eq!(alert(&active, &idle), None);
        assert_eq!(alert(&idle, &active), None);
    }

    #[test]
    fn crossing_the_keys_critical_mark_once() {
        let mut colors = ColorSettings::default();
        colors.marks.critical = 90.0;
        let run = |a: f64, b: f64| {
            window_alert(
                &snap(a, Some(at(R))),
                &snap(b, Some(at(R))),
                WindowKind::Session,
                &colors,
            )
        };
        assert_eq!(run(85.0, 92.0), Some(Alert::Critical));
        assert_eq!(run(92.0, 95.0), None, "already Critical");
        assert_eq!(run(80.0, 85.0), None);
        colors.marks.critical = 95.0;
        let higher = window_alert(
            &snap(85.0, Some(at(R))),
            &snap(92.0, Some(at(R))),
            WindowKind::Session,
            &colors,
        );
        assert_eq!(higher, None, "the key's own mark");
    }
}
