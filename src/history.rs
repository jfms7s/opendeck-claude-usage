//! Recorded %-of-limit history. The usage API only ever returns the
//! current session/weekly percentages, so trends need readings kept over
//! time: an in-memory list mirrored to a small append-only JSONL file.
//! It holds percentages (session, weekly and extra usage) and reset times only - no tokens, credentials or
//! account data - and anything older than `retention()` is dropped. Still,
//! it's nobody else's business how much someone uses Claude, so the file
//! and any directory created for it are owner-only.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::source::UsageSnapshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: DateTime<Utc>,
    pub session: f64,
    pub session_resets_at: Option<DateTime<Utc>>,
    pub weekly: f64,
    pub weekly_resets_at: Option<DateTime<Utc>>,
    /// Extra usage (% of the monthly cap); `None` when it isn't enabled,
    /// or in lines saved before it was recorded.
    #[serde(default)]
    pub monthly: Option<f64>,
}

/// A weekly window is 7 days; one extra day lets "today" and the whole
/// current week always be covered.
pub fn retention() -> Duration {
    Duration::days(8)
}

/// Pruning only drops readings from memory; the file keeps them until it
/// is rewritten. Rewriting once this many have been pruned keeps it to
/// about `retention()` of readings without rewriting on every poll.
const COMPACT_AFTER: usize = 100;

pub struct HistoryStore {
    readings: Mutex<Vec<Reading>>,
    /// `None` for `in_memory()` - never touches disk.
    path: Option<PathBuf>,
    /// Readings pruned from memory but still in the file (see
    /// `COMPACT_AFTER`). Only changed while holding the `readings` lock.
    stale_lines: AtomicUsize,
    /// False when the file existed but couldn't be read: rewriting it from
    /// memory would wipe readings that are still on disk.
    rewritable: bool,
    /// Set after the first failure of each kind, so a read-only disk warns
    /// once instead of on every poll - and a failed load doesn't hide a
    /// later write failure.
    read_warned: AtomicBool,
    write_warned: AtomicBool,
}

impl Reading {
    fn from_snapshot(snapshot: &UsageSnapshot, at: DateTime<Utc>) -> Self {
        Self {
            at,
            session: snapshot.session.percent,
            session_resets_at: snapshot.session.resets_at,
            weekly: snapshot.weekly.percent,
            weekly_resets_at: snapshot.weekly.resets_at,
            monthly: snapshot
                .monthly
                .enabled
                .then_some(snapshot.monthly.percent)
                .flatten(),
        }
    }

    /// Same values, ignoring when it was taken - an unchanged poll adds
    /// nothing to a trend.
    fn same_values(&self, other: &Reading) -> bool {
        self.session == other.session
            && same_reset(self.session_resets_at, other.session_resets_at)
            && self.weekly == other.weekly
            && same_reset(self.weekly_resets_at, other.weekly_resets_at)
            && self.monthly == other.monthly
    }
}

/// Real resets are hours apart, but the API stamps `resets_at` with each
/// request's own sub-second fraction, so the same reset reads slightly
/// differently on every fetch. Anything within this is the same reset.
const RESET_TOLERANCE: Duration = Duration::minutes(5);

/// Whether two reset times are the same reset (see `RESET_TOLERANCE`).
pub fn same_reset(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < RESET_TOLERANCE,
        (None, None) => true,
        _ => false,
    }
}

/// `$XDG_STATE_HOME` when absolute (per the XDG spec), else the platform's
/// place for app state: `~/.local/state` on Linux, `~/Library/Application
/// Support` on macOS.
fn state_base(xdg_state_home: Option<std::ffi::OsString>, home: &str, macos: bool) -> PathBuf {
    xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = PathBuf::from(home);
            if macos {
                home.join("Library/Application Support")
            } else {
                home.join(".local/state")
            }
        })
}

impl HistoryStore {
    /// `state_base()` plus the plugin's own directory.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        state_base(
            std::env::var_os("XDG_STATE_HOME"),
            &home,
            cfg!(target_os = "macos"),
        )
        .join("opendeck-claude-usage")
        .join("history.jsonl")
    }

    /// A store that never touches disk - for tests of anything that
    /// records history.
    #[cfg(test)]
    pub fn in_memory() -> Arc<Self> {
        Arc::new(Self {
            readings: Mutex::new(Vec::new()),
            path: None,
            stale_lines: AtomicUsize::new(0),
            rewritable: true,
            read_warned: AtomicBool::new(false),
            write_warned: AtomicBool::new(false),
        })
    }

    /// Loads, prunes and sorts the recorded readings, rewriting the file
    /// pruned so it never grows beyond a few days of readings across
    /// restarts. A missing file just starts empty; unreadable lines (e.g.
    /// a crash mid-append) are skipped. A file that can't be read at all,
    /// or in which not one line parses (a newer version's format), is
    /// never rewritten: that would wipe readings this version can't see.
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self> {
        let mut store = Self {
            readings: Mutex::new(Vec::new()),
            path: Some(path.clone()),
            stale_lines: AtomicUsize::new(0),
            rewritable: true,
            read_warned: AtomicBool::new(false),
            write_warned: AtomicBool::new(false),
        };
        tighten_dir(&path);
        // Bytes, decoded lossily: one corrupt byte must only cost its own
        // line, not fail the whole read.
        let read = match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                let parsed: Vec<Reading> = text
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                if lines > 0 && parsed.is_empty() {
                    log::warn!(
                        "usage history {} has {lines} lines but none this version can read \
                         (written by a newer version?); leaving it untouched",
                        path.display()
                    );
                    store.rewritable = false;
                    None
                } else {
                    Some(parsed)
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                if !store.read_warned.swap(true, Ordering::Relaxed) {
                    log::warn!(
                        "could not read usage history {}: {e}; starting with an empty history",
                        path.display()
                    );
                }
                store.rewritable = false;
                None
            }
        };
        let loaded = read.is_some();
        let mut readings = read.unwrap_or_default();
        let cutoff = now - retention();
        readings.retain(|r| r.at >= cutoff);
        readings.sort_by_key(|r| r.at);
        // Only rewrite what was actually read - rewriting after a failed
        // read would wipe the file.
        if loaded && let Err(e) = store.rewrite(&readings) {
            store.warn_write(&path, &e);
        }
        *store
            .readings
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = readings;
        Arc::new(store)
    }

    /// The readings, even after a panic elsewhere while they were locked:
    /// a reading is inserted in one step, so the list is never half-done.
    fn lock(&self) -> MutexGuard<'_, Vec<Reading>> {
        self.readings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds a reading and appends it to the file - or, once enough old
    /// readings have been pruned, rewrites the file with only the kept
    /// ones. File I/O happens under the lock so an append can't slip in
    /// between a rewrite's snapshot and its rename.
    ///
    /// An unchanged reading is skipped - except the first one on a new day
    /// in `now`'s time zone, which marks the usage at midnight for the
    /// sparkline's "Today".
    pub fn record<Tz: TimeZone>(&self, snapshot: &UsageSnapshot, now: DateTime<Tz>) {
        let tz = now.timezone();
        let day = |at: DateTime<Utc>| at.with_timezone(&tz).date_naive();
        let now = now.with_timezone(&Utc);
        let reading = Reading::from_snapshot(snapshot, now);
        let mut readings = self.lock();
        // Inserted in time order: two racing refreshes or a clock jump
        // can hand us a reading older than the last one kept. The file
        // is still appended to; `load` sorts it.
        let index = readings.partition_point(|r| r.at <= reading.at);
        if index.checked_sub(1).is_some_and(|before| {
            let before = &readings[before];
            before.same_values(&reading) && day(before.at) == day(reading.at)
        }) {
            return;
        }
        readings.insert(index, reading.clone());
        let before = readings.len();
        let cutoff = now - retention();
        readings.retain(|r| r.at >= cutoff);
        let pruned = before - readings.len();
        let stale = self.stale_lines.fetch_add(pruned, Ordering::Relaxed) + pruned;
        if stale >= COMPACT_AFTER && self.rewritable {
            match self.rewrite(&readings) {
                Ok(()) => self.stale_lines.store(0, Ordering::Relaxed),
                Err(e) => {
                    // The old file is intact: add this reading to it, and
                    // try compacting again next time.
                    if let Some(path) = &self.path {
                        self.warn_write(path, &e);
                    }
                    self.append(&reading);
                }
            }
        } else {
            self.append(&reading);
        }
    }

    pub fn readings(&self) -> Vec<Reading> {
        self.lock().clone()
    }

    fn append(&self, reading: &Reading) {
        let Some(path) = &self.path else {
            return;
        };
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
            let line = serde_json::to_string(reading).map_err(std::io::Error::other)?;
            // O_NOFOLLOW: a symlink planted at the path is an error, not a
            // redirect (same as the rewrite's O_EXCL temp file, KI-27).
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)?;
            writeln!(file, "{line}")
        })();
        if let Err(e) = result {
            self.warn_write(path, &e);
        }
    }

    /// Write-then-rename, so a crash mid-rewrite leaves the old file; the
    /// data is synced before the rename, so a power loss can't leave an
    /// empty one. The temp file is always freshly created (`create_new`,
    /// i.e. `O_EXCL`), so a symlink left at its path is never followed; a
    /// leftover from a crash is removed first (removing a symlink removes
    /// only the link).
    fn rewrite(&self, readings: &[Reading]) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let body: String = readings
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .map(|l| l + "\n")
            .collect();
        let tmp = path.with_extension("jsonl.tmp");
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    }

    fn warn_write(&self, path: &Path, e: &std::io::Error) {
        if !self.write_warned.swap(true, Ordering::Relaxed) {
            log::warn!(
                "could not save usage history to {}: {e}; new readings are kept in memory \
                 until the plugin restarts (further save errors are not logged)",
                path.display()
            );
        }
    }
}

/// Makes an existing state directory owner-only. A directory created
/// before KI-26 kept its umask mode; one created now already is 0700.
fn tighten_dir(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    if let Ok(meta) = std::fs::metadata(dir)
        && meta.is_dir()
        && meta.permissions().mode() & 0o077 != 0
        && let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    {
        log::warn!("could not make {} owner-only: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use chrono::TimeZone;

    #[test]
    fn history_dir_per_platform() {
        use std::ffi::OsString;
        let abs = Some(OsString::from("/xdg/state"));
        let rel = Some(OsString::from("relative/state"));
        // An absolute $XDG_STATE_HOME wins on both platforms.
        assert_eq!(
            state_base(abs.clone(), "/home/jf", false),
            PathBuf::from("/xdg/state")
        );
        assert_eq!(
            state_base(abs, "/Users/jf", true),
            PathBuf::from("/xdg/state")
        );
        // Otherwise each platform's convention; a relative one is ignored.
        assert_eq!(
            state_base(None, "/home/jf", false),
            PathBuf::from("/home/jf/.local/state")
        );
        assert_eq!(
            state_base(rel, "/home/jf", false),
            PathBuf::from("/home/jf/.local/state")
        );
        assert_eq!(
            state_base(None, "/Users/jf", true),
            PathBuf::from("/Users/jf/Library/Application Support")
        );
    }

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 30, hour, 0, 0).unwrap()
    }

    fn snapshot(session: f64, weekly: f64) -> UsageSnapshot {
        UsageSnapshot {
            session: WindowUsage {
                percent: session,
                resets_at: Some(at(12)),
            },
            weekly: WindowUsage {
                percent: weekly,
                resets_at: Some(at(23)),
            },
            monthly: MonthlyUsage {
                enabled: false,
                percent: None,
                used_dollars: None,
                limit_dollars: None,
            },
        }
    }

    fn line(r: &Reading) -> String {
        serde_json::to_string(r).unwrap()
    }

    fn reading(at: DateTime<Utc>, session: f64) -> Reading {
        Reading {
            at,
            session,
            session_resets_at: None,
            weekly: 1.0,
            weekly_resets_at: None,
            monthly: None,
        }
    }

    #[test]
    fn missing_file_starts_empty_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        let store = HistoryStore::load(path.clone(), at(10));
        assert!(store.readings().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn load_skips_garbage_and_prunes_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let old = reading(at(10) - Duration::days(9), 5.0);
        let recent = reading(at(9), 20.0);
        std::fs::write(
            &path,
            format!("{}\n{{not json\n{}\n{{\"at\":", line(&old), line(&recent)),
        )
        .unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        assert_eq!(store.readings(), vec![recent.clone()]);
        // Rewritten pruned.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", line(&recent))
        );
    }

    #[test]
    fn load_sorts_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let (a, b) = (reading(at(8), 1.0), reading(at(9), 2.0));
        std::fs::write(&path, format!("{}\n{}\n", line(&b), line(&a))).unwrap();
        assert_eq!(HistoryStore::load(path, at(10)).readings(), vec![a, b]);
    }

    #[test]
    fn record_appends_and_creates_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        let store = HistoryStore::load(path.clone(), at(10));
        store.record(&snapshot(40.0, 20.0), at(10));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let back: Reading = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(back.session, 40.0);
        assert_eq!(back.weekly_resets_at, Some(at(23)));
    }

    #[test]
    fn unchanged_snapshot_is_not_recorded() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(40.0, 20.0), at(9));
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(store.readings().len(), 1);
        store.record(&snapshot(41.0, 20.0), at(11));
        assert_eq!(store.readings().len(), 2);
    }

    /// Two refreshes racing, or the clock jumping back, can hand `record`
    /// an older reading than the last one kept.
    #[test]
    fn an_out_of_order_reading_is_kept_in_time_order() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(10.0, 1.0), at(8));
        store.record(&snapshot(30.0, 1.0), at(10));
        store.record(&snapshot(20.0, 1.0), at(9));
        let times: Vec<_> = store.readings().iter().map(|r| r.at).collect();
        assert_eq!(times, vec![at(8), at(9), at(10)]);
    }

    /// Dedup compares with the reading just before the new one in time,
    /// not with the last one pushed.
    #[test]
    fn an_out_of_order_reading_equal_to_its_predecessor_is_not_recorded() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(10.0, 1.0), at(8));
        store.record(&snapshot(30.0, 1.0), at(10));
        store.record(&snapshot(10.0, 1.0), at(9));
        assert_eq!(store.readings().len(), 2);
    }

    /// The first reading of each local day is kept even when nothing
    /// changed, so "Today" knows the usage at (within a poll of) midnight.
    #[test]
    fn the_first_reading_of_a_local_day_is_kept_even_if_unchanged() {
        let plus2 = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        let store = HistoryStore::in_memory();
        // 23:00 on the 30th local, then 00:01 and 00:05 on the 1st.
        store.record(&snapshot(40.0, 20.0), at(21).with_timezone(&plus2));
        let after_midnight = at(22) + Duration::minutes(1);
        store.record(&snapshot(40.0, 20.0), after_midnight.with_timezone(&plus2));
        store.record(
            &snapshot(40.0, 20.0),
            (at(22) + Duration::minutes(5)).with_timezone(&plus2),
        );
        let times: Vec<_> = store.readings().iter().map(|r| r.at).collect();
        assert_eq!(times, vec![at(21), after_midnight]);
        // The same instants are all on the 30th in UTC: nothing new is kept.
        let utc = HistoryStore::in_memory();
        utc.record(&snapshot(40.0, 20.0), at(21));
        utc.record(&snapshot(40.0, 20.0), after_midnight);
        assert_eq!(utc.readings().len(), 1);
    }

    #[test]
    fn a_reading_keeps_the_monthly_percent_only_when_enabled() {
        let mut s = snapshot(40.0, 20.0);
        s.monthly = MonthlyUsage {
            enabled: true,
            percent: Some(12.5),
            used_dollars: Some(6.25),
            limit_dollars: Some(50.0),
        };
        assert_eq!(Reading::from_snapshot(&s, at(10)).monthly, Some(12.5));
        let off = snapshot(40.0, 20.0);
        assert_eq!(Reading::from_snapshot(&off, at(10)).monthly, None);
    }

    #[test]
    fn a_monthly_only_change_is_recorded() {
        let store = HistoryStore::in_memory();
        let mut s = snapshot(40.0, 20.0);
        s.monthly.enabled = true;
        s.monthly.percent = Some(10.0);
        store.record(&s, at(9));
        s.monthly.percent = Some(11.0);
        store.record(&s, at(10));
        assert_eq!(store.readings().len(), 2);
    }

    #[test]
    fn a_line_saved_before_monthly_was_recorded_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(
            &path,
            "{\"at\":\"2026-09-30T09:00:00Z\",\"session\":20.0,\"session_resets_at\":null,\
             \"weekly\":1.0,\"weekly_resets_at\":null}\n",
        )
        .unwrap();
        let store = HistoryStore::load(path, at(10));
        assert_eq!(store.readings(), vec![reading(at(9), 20.0)]);
    }

    #[test]
    fn record_prunes_old_readings_in_memory() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(1.0, 1.0), at(10) - Duration::days(9));
        store.record(&snapshot(2.0, 1.0), at(10));
        assert_eq!(store.readings().len(), 1);
    }

    #[test]
    fn unwritable_path_keeps_readings_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the directory should be: create_dir_all fails.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let store = HistoryStore::load(blocker.join("history.jsonl"), at(10));
        store.record(&snapshot(40.0, 20.0), at(10));
        store.record(&snapshot(41.0, 20.0), at(11));
        assert_eq!(store.readings().len(), 2);
    }

    #[test]
    fn in_memory_never_writes() {
        let store = HistoryStore::in_memory();
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(store.readings().len(), 1);
        assert!(store.path.is_none());
    }

    #[test]
    fn default_path_ends_in_the_plugin_state_dir() {
        let p = HistoryStore::default_path();
        assert!(p.ends_with("opendeck-claude-usage/history.jsonl"), "{p:?}");
    }

    #[test]
    fn reset_times_that_differ_by_request_jitter_are_the_same_reset() {
        // The API stamps resets_at with the request's sub-second fraction,
        // so the same reset reads differently on every fetch.
        let a = Some(
            "2026-09-13T22:40:00.186282Z"
                .parse::<DateTime<Utc>>()
                .unwrap(),
        );
        let b = Some(
            "2026-09-13T22:39:59.552839Z"
                .parse::<DateTime<Utc>>()
                .unwrap(),
        );
        assert!(same_reset(a, b));
        assert!(!same_reset(a, Some(at(12))));
        assert!(same_reset(None, None));
        assert!(!same_reset(a, None));
    }

    #[test]
    fn jittered_reset_is_still_an_unchanged_reading() {
        let store = HistoryStore::in_memory();
        let mut s = snapshot(40.0, 20.0);
        store.record(&s, at(9));
        s.session.resets_at = s.session.resets_at.map(|r| r + Duration::milliseconds(634));
        store.record(&s, at(10));
        assert_eq!(store.readings().len(), 1);
    }

    #[test]
    fn a_non_utf8_byte_does_not_wipe_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let good = reading(at(9), 20.0);
        let mut bytes = format!("{}\n", line(&good)).into_bytes();
        bytes.extend_from_slice(b"\xff\xfe garbage\n");
        std::fs::write(&path, bytes).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        assert_eq!(store.readings(), vec![good.clone()]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", line(&good))
        );
    }

    #[test]
    fn enough_pruned_readings_compact_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let start = at(10) - Duration::days(7);
        let old: String = (0..COMPACT_AFTER)
            .map(|i| line(&reading(start + Duration::minutes(i as i64), i as f64)) + "\n")
            .collect();
        std::fs::write(&path, old).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        assert_eq!(store.readings().len(), COMPACT_AFTER);
        // Two days on, every loaded reading has aged out.
        let later = at(10) + Duration::days(2);
        store.record(&snapshot(40.0, 20.0), later);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "got: {text}");
        assert_eq!(store.readings().len(), 1);
    }

    #[test]
    fn a_few_pruned_readings_only_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let old = reading(at(10) - Duration::days(7), 1.0);
        std::fs::write(&path, format!("{}\n", line(&old))).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        store.record(&snapshot(40.0, 20.0), at(10) + Duration::days(2));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "got: {text}");
    }

    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn an_appended_file_and_its_directory_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/history.jsonl");
        let store = HistoryStore::load(path.clone(), at(10));
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    /// A file saved by an older version with the default umask becomes
    /// owner-only on the next load, since loading rewrites it.
    #[test]
    fn a_rewritten_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, format!("{}\n", line(&reading(at(9), 20.0)))).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        HistoryStore::load(path.clone(), at(10));
        assert_eq!(mode(&path), 0o600);
    }

    /// A symlink planted at the temp path must not be followed: the
    /// rewrite would otherwise overwrite whatever it points at.
    #[test]
    fn a_rewrite_does_not_follow_a_symlink_at_the_temp_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        std::os::unix::fs::symlink(&victim, path.with_extension("jsonl.tmp")).unwrap();
        let kept = reading(at(9), 20.0);
        std::fs::write(&path, format!("{}\n", line(&kept))).unwrap();
        HistoryStore::load(path.clone(), at(10));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", line(&kept))
        );
        assert!(!std::fs::symlink_metadata(&path).unwrap().is_symlink());
    }

    #[test]
    fn a_load_warning_does_not_silence_write_warnings() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: reading and writing fail.
        let path = dir.path().join("history.jsonl");
        std::fs::create_dir(&path).unwrap();
        let store = HistoryStore::load(path, at(10));
        assert!(store.read_warned.load(Ordering::Relaxed));
        assert!(!store.write_warned.load(Ordering::Relaxed));
        store.record(&snapshot(40.0, 20.0), at(10));
        assert!(store.write_warned.load(Ordering::Relaxed));
    }

    /// A file in which no line parses (a newer version's format, after a
    /// downgrade) is not this version's to rewrite: it's left as it is.
    #[test]
    fn a_file_where_no_line_parses_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let future = "{\"at\":\"2026-09-30T09:00:00Z\",\"v\":2}\n";
        std::fs::write(&path, future).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        assert!(store.readings().is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
        // And nothing compacts it later either.
        assert!(!store.rewritable);
    }

    /// The `rewritable` guard: a file that couldn't be read is never
    /// overwritten from memory, however many readings get pruned.
    #[test]
    fn an_unreadable_file_is_never_compacted_over() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let original = format!("{}\n", line(&reading(at(9), 20.0)));
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        // Each reading is a day after the last, so after the first eight
        // every one prunes one - well past COMPACT_AFTER.
        for i in 0..COMPACT_AFTER + 10 {
            store.record(&snapshot(i as f64, 1.0), at(10) + Duration::days(i as i64));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// A compaction that can't write must not lose the reading that
    /// triggered it: it's appended instead, and compaction is retried.
    #[test]
    fn a_failed_compaction_still_saves_the_reading() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let path = state.join("history.jsonl");
        let start = at(10) - Duration::days(7);
        let old: String = (0..COMPACT_AFTER)
            .map(|i| line(&reading(start + Duration::minutes(i as i64), i as f64)) + "\n")
            .collect();
        std::fs::write(&path, old).unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        // The temp file can't be created next to the history file.
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap();
        store.record(&snapshot(55.5, 20.0), at(10) + Duration::days(2));
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"session\":55.5")
        );
        // Still due: the next reading compacts.
        store.record(
            &snapshot(56.5, 20.0),
            at(10) + Duration::days(2) + Duration::hours(1),
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
    }

    /// A state directory created by a version before KI-26 (with the
    /// default umask) is made owner-only on load.
    #[test]
    fn an_existing_state_directory_becomes_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        HistoryStore::load(state.join("history.jsonl"), at(10));
        assert_eq!(mode(&state), 0o700);
    }

    /// Appending never follows a symlink planted at the history path
    /// (the KI-27 hardening, on the more frequent write path).
    #[test]
    fn an_append_does_not_follow_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        let store = HistoryStore::load(path.clone(), at(10));
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        store.record(&snapshot(40.0, 20.0), at(10));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        assert_eq!(store.readings().len(), 1, "still kept in memory");
    }
}
