//! Recorded %-of-limit history. The usage API only ever returns the
//! current session/weekly percentages, so trends need readings kept over
//! time: an in-memory list mirrored to a small append-only JSONL file.
//! It holds percentages and reset times only - no tokens, credentials or
//! account data - and anything older than `retention()` is dropped. Still,
//! it's nobody else's business how much someone uses Claude, so the file
//! and any directory created for it are owner-only.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::source::UsageSnapshot;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    pub at: DateTime<Utc>,
    pub session: f64,
    pub session_resets_at: Option<DateTime<Utc>>,
    pub weekly: f64,
    pub weekly_resets_at: Option<DateTime<Utc>>,
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
        }
    }

    /// Same values, ignoring when it was taken - an unchanged poll adds
    /// nothing to a trend.
    fn same_values(&self, other: &Reading) -> bool {
        self.session == other.session
            && same_reset(self.session_resets_at, other.session_resets_at)
            && self.weekly == other.weekly
            && same_reset(self.weekly_resets_at, other.weekly_resets_at)
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

impl HistoryStore {
    /// `$XDG_STATE_HOME` (when absolute, per the XDG spec) or
    /// `~/.local/state`, plus the plugin's own directory.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
                PathBuf::from(home).join(".local/state")
            });
        base.join("opendeck-claude-usage").join("history.jsonl")
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
    /// a crash mid-append) are skipped.
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self> {
        let mut store = Self {
            readings: Mutex::new(Vec::new()),
            path: Some(path.clone()),
            stale_lines: AtomicUsize::new(0),
            rewritable: true,
            read_warned: AtomicBool::new(false),
            write_warned: AtomicBool::new(false),
        };
        // Bytes, decoded lossily: one corrupt byte must only cost its own
        // line, not fail the whole read.
        let read = match std::fs::read(&path) {
            Ok(bytes) => Some(
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect::<Vec<Reading>>(),
            ),
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
        if loaded {
            store.rewrite(&readings);
        }
        *store.readings.lock().unwrap() = readings;
        Arc::new(store)
    }

    /// Adds a reading and appends it to the file - or, once enough old
    /// readings have been pruned, rewrites the file with only the kept
    /// ones. File I/O happens under the lock so an append can't slip in
    /// between a rewrite's snapshot and its rename.
    pub fn record(&self, snapshot: &UsageSnapshot, now: DateTime<Utc>) {
        let reading = Reading::from_snapshot(snapshot, now);
        let mut readings = self.readings.lock().unwrap();
        // Inserted in time order: two racing refreshes or a clock jump
        // can hand us a reading older than the last one kept. The file
        // is still appended to; `load` sorts it.
        let index = readings.partition_point(|r| r.at <= reading.at);
        if index
            .checked_sub(1)
            .is_some_and(|before| readings[before].same_values(&reading))
        {
            return;
        }
        readings.insert(index, reading.clone());
        let before = readings.len();
        let cutoff = now - retention();
        readings.retain(|r| r.at >= cutoff);
        let pruned = before - readings.len();
        let stale = self.stale_lines.fetch_add(pruned, Ordering::Relaxed) + pruned;
        if stale >= COMPACT_AFTER && self.rewritable {
            self.stale_lines.store(0, Ordering::Relaxed);
            self.rewrite(&readings);
        } else {
            self.append(&reading);
        }
    }

    pub fn readings(&self) -> Vec<Reading> {
        self.readings.lock().unwrap().clone()
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
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
            writeln!(file, "{line}")
        })();
        if let Err(e) = result {
            self.warn_write(path, &e);
        }
    }

    /// Write-then-rename, so a crash mid-rewrite leaves the old file. The
    /// temp file is always freshly created (`create_new`, i.e. `O_EXCL`),
    /// so a symlink left at its path is never followed; a leftover from a
    /// crash is removed first (removing a symlink removes only the link).
    fn rewrite(&self, readings: &[Reading]) {
        let Some(path) = &self.path else {
            return;
        };
        let body: String = readings
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .map(|l| l + "\n")
            .collect();
        let tmp = path.with_extension("jsonl.tmp");
        let result = match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
        .and_then(|_| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
        })
        .and_then(|mut file| file.write_all(body.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            self.warn_write(path, &e);
        }
    }

    fn warn_write(&self, path: &std::path::Path, e: &std::io::Error) {
        if !self.write_warned.swap(true, Ordering::Relaxed) {
            log::warn!(
                "could not save usage history to {}: {e}; new readings are kept in memory \
                 until the plugin restarts (further save errors are not logged)",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{MonthlyUsage, WindowUsage};
    use chrono::TimeZone;

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
}
