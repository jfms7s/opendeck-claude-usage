use chrono::NaiveTime;

pub const DEFAULT_PEAK_START: &str = "13:00";
pub const DEFAULT_PEAK_END: &str = "18:00";

/// Parses "HH:MM" into minutes since midnight (0..=1439). Malformed input
/// (wrong shape, non-numeric, or an out-of-range hour/minute) yields `None`
/// rather than an error - callers fall back to a default per field, the same
/// "malformed becomes a fallback, not a hard failure" pattern `source/file.rs`
/// uses for `resets_at`.
pub fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

/// A peak-hours window as minutes-since-midnight. `start_minutes >
/// end_minutes` means the window crosses midnight (e.g. 22:00-06:00).
/// `start_minutes == end_minutes` is a zero-length window - always off-peak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakWindow {
    pub start_minutes: u32,
    pub end_minutes: u32,
}

impl PeakWindow {
    /// Parses each field independently, falling back to the built-in default
    /// for whichever field fails to parse - a malformed start doesn't also
    /// discard a valid end.
    pub fn from_settings(start: &str, end: &str) -> Self {
        let default_start = parse_hhmm(DEFAULT_PEAK_START).unwrap();
        let default_end = parse_hhmm(DEFAULT_PEAK_END).unwrap();
        Self {
            start_minutes: parse_hhmm(start).unwrap_or(default_start),
            end_minutes: parse_hhmm(end).unwrap_or(default_end),
        }
    }

    /// Start inclusive, end exclusive - so a window never overlaps itself at
    /// the boundary minute.
    pub fn contains(&self, minute_of_day: u32) -> bool {
        if self.start_minutes <= self.end_minutes {
            minute_of_day >= self.start_minutes && minute_of_day < self.end_minutes
        } else {
            minute_of_day >= self.start_minutes || minute_of_day < self.end_minutes
        }
    }
}

pub struct PeakStatus {
    pub is_peak: bool,
    pub status_text: &'static str,
    pub countdown_text: String,
}

/// Computes whether `now` falls inside `window`, plus a countdown to
/// whichever boundary comes next (the end if currently peak, the start if
/// currently off-peak).
/// Minutes to advance clockwise from `from` to reach `to`, wrapping across
/// midnight - always the forward distance, never negative or backward.
fn minutes_until(from: u32, to: u32) -> u32 {
    (to + 1440 - from) % 1440
}

fn format_hhmm(total_minutes: u32) -> String {
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;
    format!("{hours:02}:{minutes:02}")
}

pub fn peak_status(window: PeakWindow, now: NaiveTime) -> PeakStatus {
    use chrono::Timelike;
    let now_minutes = now.num_seconds_from_midnight() / 60;
    let is_peak = window.contains(now_minutes);
    let (status_text, until) = if is_peak {
        ("Peak", minutes_until(now_minutes, window.end_minutes))
    } else {
        ("Off-peak", minutes_until(now_minutes, window.start_minutes))
    };
    let countdown_text = if is_peak {
        format!("off-peak in {}", format_hhmm(until))
    } else {
        format!("peak in {}", format_hhmm(until))
    };
    PeakStatus {
        is_peak,
        status_text,
        countdown_text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_valid_hhmm() {
        assert_eq!(parse_hhmm("09:00"), Some(540));
        assert_eq!(parse_hhmm("23:59"), Some(1439));
        assert_eq!(parse_hhmm("00:00"), Some(0));
    }

    #[test]
    fn rejects_out_of_range_hhmm() {
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
    }

    #[test]
    fn rejects_malformed_hhmm() {
        assert_eq!(parse_hhmm("abc"), None);
        assert_eq!(parse_hhmm("9"), None);
        assert_eq!(parse_hhmm(""), None);
    }

    #[test]
    fn from_settings_falls_back_per_field_on_malformed_input() {
        let window = PeakWindow::from_settings("bad", "20:00");
        assert_eq!(window.start_minutes, 780); // DEFAULT_PEAK_START
        assert_eq!(window.end_minutes, 1200); // parsed, not defaulted
    }

    #[test]
    fn contains_normal_window_is_start_inclusive_end_exclusive() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        assert!(window.contains(540));
        assert!(window.contains(1019));
        assert!(!window.contains(1020));
        assert!(!window.contains(0));
    }

    #[test]
    fn contains_overnight_window_wraps_across_midnight() {
        let window = PeakWindow {
            start_minutes: 1320,
            end_minutes: 360,
        };
        assert!(window.contains(1320));
        assert!(window.contains(0));
        assert!(window.contains(359));
        assert!(!window.contains(360));
        assert!(!window.contains(700));
    }

    #[test]
    fn status_inside_normal_window_counts_down_to_end() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(window, time(10, 0));
        assert!(status.is_peak);
        assert_eq!(status.status_text, "Peak");
        assert_eq!(status.countdown_text, "off-peak in 07:00");
    }

    #[test]
    fn status_outside_normal_window_counts_down_to_start() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(window, time(8, 0));
        assert!(!status.is_peak);
        assert_eq!(status.status_text, "Off-peak");
        assert_eq!(status.countdown_text, "peak in 01:00");
    }

    #[test]
    fn status_inside_overnight_window_counts_down_to_end_next_day() {
        let window = PeakWindow {
            start_minutes: 1320,
            end_minutes: 360,
        };
        let status = peak_status(window, time(23, 0));
        assert!(status.is_peak);
        assert_eq!(status.countdown_text, "off-peak in 07:00");
    }

    #[test]
    fn status_outside_overnight_window_counts_down_to_start_next_night() {
        let window = PeakWindow {
            start_minutes: 1320,
            end_minutes: 360,
        };
        let status = peak_status(window, time(12, 0));
        assert!(!status.is_peak);
        assert_eq!(status.countdown_text, "peak in 10:00");
    }

    #[test]
    fn status_exactly_at_start_boundary_is_peak() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(window, time(9, 0));
        assert!(status.is_peak);
    }

    #[test]
    fn status_exactly_at_end_boundary_is_off_peak() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(window, time(17, 0));
        assert!(!status.is_peak);
    }
}
