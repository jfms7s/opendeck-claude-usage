//! How fast a usage window is burning: elapsed fraction, projected % at
//! reset, even-burn ratio, and runway. Pure, with `now` passed in.

use chrono::{DateTime, Duration, Utc};

use crate::source::{WindowKind, WindowUsage};

/// Below this fraction of a window elapsed, projections are noise (3% used
/// five minutes into a session "projects" to 180%) - report nothing
/// rather than a false alarm.
pub const MIN_ELAPSED_FRACTION: f64 = 0.10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Runway {
    Empty,
    LastsToReset,
    Until(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    pub elapsed_fraction: f64,
    pub projected: f64,
    pub even_burn: f64,
    pub rate_per_hour: f64,
    pub runway: Runway,
}

/// Monthly (`extra_usage`) has no rolling window, so it has no length.
pub fn window_length(kind: WindowKind) -> Option<Duration> {
    match kind {
        WindowKind::Session => Some(Duration::hours(5)),
        WindowKind::Weekly => Some(Duration::days(7)),
        WindowKind::Monthly => None,
    }
}

pub fn pace(window: &WindowUsage, kind: WindowKind, now: DateTime<Utc>) -> Option<Pace> {
    let length_secs = window_length(kind)?.num_seconds() as f64;
    let remaining = window.resets_at? - now;
    // (length - remaining) / length rather than 1 - remaining/length: the
    // latter lands a hair under 0.10 at exactly 10% elapsed in f64.
    let elapsed_fraction =
        ((length_secs - remaining.num_seconds() as f64) / length_secs).clamp(0.0, 1.0);
    if elapsed_fraction < MIN_ELAPSED_FRACTION {
        return None;
    }
    let used = window.percent.max(0.0);
    let rate_per_hour = used / (elapsed_fraction * length_secs / 3600.0);
    let projected = used / elapsed_fraction;
    let runway = if used >= 100.0 {
        Runway::Empty
    } else if rate_per_hour <= 0.0 {
        Runway::LastsToReset
    } else {
        let until = Duration::seconds(((100.0 - used) / rate_per_hour * 3600.0).round() as i64);
        if until >= remaining {
            Runway::LastsToReset
        } else {
            Runway::Until(until)
        }
    };
    Some(Pace {
        elapsed_fraction,
        projected,
        even_burn: projected / 100.0,
        rate_per_hour,
        runway,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    /// Session resetting at 22:40 started at 17:40.
    fn session(percent: f64) -> WindowUsage {
        WindowUsage {
            percent,
            resets_at: Some(at(22, 40)),
        }
    }

    #[test]
    fn window_lengths() {
        assert_eq!(window_length(WindowKind::Session), Some(Duration::hours(5)));
        assert_eq!(window_length(WindowKind::Weekly), Some(Duration::days(7)));
        assert_eq!(window_length(WindowKind::Monthly), None);
    }

    #[test]
    fn projects_a_known_case() {
        // 25% elapsed (1h15m of 5h), 30% used.
        let p = pace(&session(30.0), WindowKind::Session, at(18, 55)).unwrap();
        assert!((p.elapsed_fraction - 0.25).abs() < 1e-9);
        assert!((p.projected - 120.0).abs() < 1e-9);
        assert!((p.even_burn - 1.2).abs() < 1e-9);
        assert!((p.rate_per_hour - 24.0).abs() < 1e-9);
        // 70% left at 24%/h = 2h55m, before the 3h45m until reset.
        assert_eq!(p.runway, Runway::Until(Duration::minutes(175)));
    }

    #[test]
    fn too_early_returns_none() {
        // 29m of 5h = 9.67% elapsed.
        assert_eq!(pace(&session(3.0), WindowKind::Session, at(18, 9)), None);
    }

    #[test]
    fn exactly_ten_percent_elapsed_is_enough() {
        assert!(pace(&session(3.0), WindowKind::Session, at(18, 10)).is_some());
    }

    #[test]
    fn slow_burn_lasts_to_reset() {
        // 50% elapsed, 10% used: 4%/h needs 22.5h for the remaining 90%.
        let p = pace(&session(10.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::LastsToReset);
    }

    #[test]
    fn zero_usage_lasts_to_reset() {
        let p = pace(&session(0.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::LastsToReset);
        assert_eq!(p.projected, 0.0);
    }

    #[test]
    fn full_usage_is_empty() {
        let p = pace(&session(100.0), WindowKind::Session, at(20, 10)).unwrap();
        assert_eq!(p.runway, Runway::Empty);
    }

    #[test]
    fn monthly_has_no_pace() {
        assert_eq!(pace(&session(30.0), WindowKind::Monthly, at(20, 10)), None);
    }

    #[test]
    fn missing_reset_has_no_pace() {
        let w = WindowUsage {
            percent: 30.0,
            resets_at: None,
        };
        assert_eq!(pace(&w, WindowKind::Session, at(20, 10)), None);
    }

    #[test]
    fn now_past_reset_clamps_and_lasts_to_reset() {
        let p = pace(&session(40.0), WindowKind::Session, at(23, 30)).unwrap();
        assert_eq!(p.elapsed_fraction, 1.0);
        assert!((p.projected - 40.0).abs() < 1e-9);
        assert_eq!(p.runway, Runway::LastsToReset);
    }
}
