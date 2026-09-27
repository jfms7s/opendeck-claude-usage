//! Recorded %-of-limit history. The usage API only ever returns the
//! current session/weekly percentages, so trends need readings kept over
//! time: an in-memory list mirrored to a small append-only JSONL file.
//! It holds percentages and reset times only - no tokens, credentials or
//! account data - and anything older than `retention()` is dropped.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub struct HistoryStore {
    readings: Mutex<Vec<Reading>>,
    /// `None` for `in_memory()` - never touches disk.
    path: Option<PathBuf>,
    /// Set after the first I/O failure, so a read-only disk warns once
    /// instead of on every poll.
    warned: AtomicBool,
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
            && self.session_resets_at == other.session_resets_at
            && self.weekly == other.weekly
            && self.weekly_resets_at == other.weekly_resets_at
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
            warned: AtomicBool::new(false),
        })
    }

    /// Loads, prunes and sorts the recorded readings, rewriting the file
    /// pruned so it never grows beyond a few days of readings across
    /// restarts. A missing file just starts empty; unreadable lines (e.g.
    /// a crash mid-append) are skipped.
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Arc<Self> {
        let store = Self {
            readings: Mutex::new(Vec::new()),
            path: Some(path.clone()),
            warned: AtomicBool::new(false),
        };
        let mut readings: Vec<Reading> = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                store.warn_once(&format!(
                    "could not read usage history {}: {e}",
                    path.display()
                ));
                Vec::new()
            }
        };
        let cutoff = now - retention();
        readings.retain(|r| r.at >= cutoff);
        readings.sort_by_key(|r| r.at);
        if path.exists() {
            store.rewrite(&readings);
        }
        *store.readings.lock().unwrap() = readings;
        Arc::new(store)
    }

    pub fn record(&self, snapshot: &UsageSnapshot, now: DateTime<Utc>) {
        let reading = Reading::from_snapshot(snapshot, now);
        {
            let mut readings = self.readings.lock().unwrap();
            if readings
                .last()
                .is_some_and(|last| last.same_values(&reading))
            {
                return;
            }
            readings.push(reading.clone());
            let cutoff = now - retention();
            readings.retain(|r| r.at >= cutoff);
        }
        self.append(&reading);
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
                std::fs::create_dir_all(dir)?;
            }
            let line = serde_json::to_string(reading).map_err(std::io::Error::other)?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(file, "{line}")
        })();
        if let Err(e) = result {
            self.warn_once(&format!(
                "could not write usage history {}: {e}",
                path.display()
            ));
        }
    }

    /// Write-then-rename, so a crash mid-rewrite leaves the old file.
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
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
            self.warn_once(&format!(
                "could not rewrite usage history {}: {e}",
                path.display()
            ));
        }
    }

    fn warn_once(&self, message: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            log::warn!("{message}; keeping usage history in memory only");
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
}
