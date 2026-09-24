use chrono::{Datelike, NaiveDateTime, Weekday};

pub const DEFAULT_PEAK_START: &str = "13:00";
pub const DEFAULT_PEAK_END: &str = "18:00";

/// Day keys in Monday-first order, as stored in the `peak_days` setting.
pub const DAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
/// Weekdays only - the weekend is off-peak unless the user opts it in.
pub const DEFAULT_PEAK_DAYS: [&str; 5] = ["mon", "tue", "wed", "thu", "fri"];

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

/// Which weekdays the peak window applies on, indexed Monday-first. A window
/// that crosses midnight belongs to the day it *starts* on, so Friday's
/// 22:00-06:00 window is still peak at 03:00 Saturday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakDays([bool; 7]);

impl PeakDays {
    /// Unknown keys are ignored rather than rejected, same fallback posture as
    /// `PeakWindow::from_settings`.
    pub fn from_settings<S: AsRef<str>>(days: &[S]) -> Self {
        let mut active = [false; 7];
        for day in days {
            if let Some(i) = DAY_KEYS
                .iter()
                .position(|k| k.eq_ignore_ascii_case(day.as_ref().trim()))
            {
                active[i] = true;
            }
        }
        Self(active)
    }

    pub fn includes(&self, day: Weekday) -> bool {
        self.0[day.num_days_from_monday() as usize]
    }

    fn is_empty(&self) -> bool {
        !self.0.iter().any(|&d| d)
    }
}

/// A peak window plus the weekdays it applies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeakSchedule {
    pub window: PeakWindow,
    pub days: PeakDays,
}

impl PeakSchedule {
    /// Whether `now` is inside a window that started on a selected day -
    /// today's, or (for an overnight window) yesterday's spilling past
    /// midnight.
    pub fn is_peak(&self, now: NaiveDateTime) -> bool {
        use chrono::Timelike;
        let minute = now.time().num_seconds_from_midnight() / 60;
        let today = now.weekday();
        let PeakWindow {
            start_minutes: start,
            end_minutes: end,
        } = self.window;
        if start <= end {
            self.days.includes(today) && self.window.contains(minute)
        } else {
            (minute >= start && self.days.includes(today))
                || (minute < end && self.days.includes(today.pred()))
        }
    }

    /// Minutes from `now` to the next window start on a selected day, or
    /// `None` when no day is selected (or the window is zero-length and so
    /// never actually starts a peak).
    fn minutes_until_next_start(&self, now: NaiveDateTime) -> Option<u32> {
        use chrono::Timelike;
        if self.days.is_empty() || self.window.start_minutes == self.window.end_minutes {
            return None;
        }
        let minute = now.time().num_seconds_from_midnight() / 60;
        let mut day = now.weekday();
        // Up to 8 so today's start is reachable again a full week later.
        for offset in 0..=7u32 {
            let until = (offset * 1440 + self.window.start_minutes) as i64 - minute as i64;
            if until > 0 && self.days.includes(day) {
                return Some(until as u32);
            }
            day = day.succ();
        }
        None
    }
}

pub struct PeakStatus {
    pub is_peak: bool,
    pub status_text: &'static str,
    pub countdown_text: String,
}

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

/// Computes whether `now` falls inside `schedule`, plus a countdown to
/// whichever boundary comes next (the end if currently peak, the next start
/// on a selected day if currently off-peak - possibly days away, so the hours
/// can exceed 24).
pub fn peak_status(schedule: PeakSchedule, now: NaiveDateTime) -> PeakStatus {
    use chrono::Timelike;
    let now_minutes = now.time().num_seconds_from_midnight() / 60;
    let is_peak = schedule.is_peak(now);
    let (status_text, countdown_text) = if is_peak {
        let until = minutes_until(now_minutes, schedule.window.end_minutes);
        ("Peak", format!("ends in {}", format_hhmm(until)))
    } else {
        let countdown = match schedule.minutes_until_next_start(now) {
            Some(until) => format!("peak in {}", format_hhmm(until)),
            None => "no peak set".to_string(),
        };
        ("Off-peak", countdown)
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

    use chrono::NaiveDate;

    /// 2026-09-23 is a Wednesday; `day_offset` walks forward from it
    /// (0 = Wed, 3 = Sat, 4 = Sun, 5 = Mon).
    fn on(day_offset: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23 + day_offset)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn at(h: u32, m: u32) -> NaiveDateTime {
        on(0, h, m)
    }

    fn every_day(window: PeakWindow) -> PeakSchedule {
        PeakSchedule {
            window,
            days: PeakDays::from_settings(&DAY_KEYS),
        }
    }

    fn weekdays(window: PeakWindow) -> PeakSchedule {
        PeakSchedule {
            window,
            days: PeakDays::from_settings(&DEFAULT_PEAK_DAYS),
        }
    }

    const NINE_TO_FIVE: PeakWindow = PeakWindow {
        start_minutes: 540,
        end_minutes: 1020,
    };

    const TEN_PM_TO_SIX_AM: PeakWindow = PeakWindow {
        start_minutes: 1320,
        end_minutes: 360,
    };

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
        let status = peak_status(every_day(window), at(10, 0));
        assert!(status.is_peak);
        assert_eq!(status.status_text, "Peak");
        assert_eq!(status.countdown_text, "ends in 07:00");
    }

    #[test]
    fn status_outside_normal_window_counts_down_to_start() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(every_day(window), at(8, 0));
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
        let status = peak_status(every_day(window), at(23, 0));
        assert!(status.is_peak);
        assert_eq!(status.countdown_text, "ends in 07:00");
    }

    #[test]
    fn status_outside_overnight_window_counts_down_to_start_next_night() {
        let window = PeakWindow {
            start_minutes: 1320,
            end_minutes: 360,
        };
        let status = peak_status(every_day(window), at(12, 0));
        assert!(!status.is_peak);
        assert_eq!(status.countdown_text, "peak in 10:00");
    }

    #[test]
    fn status_exactly_at_start_boundary_is_peak() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(every_day(window), at(9, 0));
        assert!(status.is_peak);
    }

    #[test]
    fn status_exactly_at_end_boundary_is_off_peak() {
        let window = PeakWindow {
            start_minutes: 540,
            end_minutes: 1020,
        };
        let status = peak_status(every_day(window), at(17, 0));
        assert!(!status.is_peak);
    }

    #[test]
    fn peak_days_parse_known_keys_and_ignore_unknown_ones() {
        let days = PeakDays::from_settings(&["mon", " SAT ", "funday"]);
        assert!(days.includes(Weekday::Mon));
        assert!(days.includes(Weekday::Sat));
        assert!(!days.includes(Weekday::Tue));
        assert!(!days.includes(Weekday::Sun));
    }

    #[test]
    fn default_days_are_weekdays_only() {
        let days = PeakDays::from_settings(&DEFAULT_PEAK_DAYS);
        assert!(days.includes(Weekday::Mon));
        assert!(days.includes(Weekday::Fri));
        assert!(!days.includes(Weekday::Sat));
        assert!(!days.includes(Weekday::Sun));
    }

    #[test]
    fn unselected_day_is_off_peak_inside_the_window() {
        let status = peak_status(weekdays(NINE_TO_FIVE), on(3, 10, 0)); // Sat
        assert!(!status.is_peak);
        // Next start is Monday 09:00: 14h left of Sat + 24h Sun + 9h Mon.
        assert_eq!(status.countdown_text, "peak in 47:00");
    }

    #[test]
    fn friday_evening_counts_down_across_the_weekend() {
        let status = peak_status(weekdays(NINE_TO_FIVE), on(2, 18, 0)); // Fri
        assert!(!status.is_peak);
        assert_eq!(status.countdown_text, "peak in 63:00");
    }

    #[test]
    fn selected_day_is_peak_inside_the_window() {
        let status = peak_status(weekdays(NINE_TO_FIVE), on(5, 10, 0)); // Mon
        assert!(status.is_peak);
        assert_eq!(status.countdown_text, "ends in 07:00");
    }

    #[test]
    fn overnight_window_belongs_to_the_day_it_starts_on() {
        let schedule = weekdays(TEN_PM_TO_SIX_AM);
        // Friday's window spills into Saturday morning...
        assert!(peak_status(schedule, on(3, 3, 0)).is_peak);
        // ...but Saturday's own window never starts...
        assert!(!peak_status(schedule, on(3, 23, 0)).is_peak);
        // ...so Monday morning (Sunday's spill-over) is off-peak too.
        assert!(!peak_status(schedule, on(5, 3, 0)).is_peak);
    }

    #[test]
    fn only_today_selected_counts_down_a_full_week_after_the_window() {
        let schedule = PeakSchedule {
            window: NINE_TO_FIVE,
            days: PeakDays::from_settings(&["wed"]),
        };
        let status = peak_status(schedule, at(18, 0));
        assert_eq!(status.countdown_text, "peak in 159:00");
    }

    #[test]
    fn no_days_selected_is_never_peak() {
        let schedule = PeakSchedule {
            window: NINE_TO_FIVE,
            days: PeakDays::from_settings::<&str>(&[]),
        };
        let status = peak_status(schedule, at(10, 0));
        assert!(!status.is_peak);
        assert_eq!(status.countdown_text, "no peak set");
    }
}
