use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl LogEntry {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// Parses one JSONL line into a `LogEntry`, or `None` if it isn't a
/// billable assistant turn: not `"type": "assistant"`, no `usage` object,
/// an unparseable timestamp, or `model == "<synthetic>"` (Claude Code's
/// placeholder for locally-generated content like compaction summaries —
/// no real API call, no real cost). Malformed JSON also yields `None`
/// rather than propagating a parse error — one bad line shouldn't
/// invalidate the rest of the file, same "malformed becomes a fallback"
/// convention `source/file.rs` uses for `resets_at`.
fn parse_line(line: &str) -> Option<LogEntry> {
    let raw: RawLine = serde_json::from_str(line).ok()?;
    if raw.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = raw.message?;
    let model = message.model?;
    if model == "<synthetic>" {
        return None;
    }
    let usage = message.usage?;
    let timestamp = DateTime::parse_from_rfc3339(raw.timestamp.as_deref()?)
        .ok()?
        .with_timezone(&Utc);

    Some(LogEntry {
        timestamp,
        model,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
    })
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

fn parse_file(path: &Path) -> Vec<LogEntry> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents.lines().filter_map(parse_line).collect()
}

type FileCache = HashMap<PathBuf, (SystemTime, Vec<LogEntry>)>;

/// Scans `~/.claude/projects/<project>/<session>.jsonl` transcript files
/// for billable assistant turns. Caches each file's parsed entries keyed
/// by its mtime, so an unchanged file across repeated `entries()` calls
/// (e.g. multiple tile instances polling at their own cadence) is never
/// re-read or re-parsed.
pub struct LogUsageSource {
    projects_dir: PathBuf,
    cache: Arc<Mutex<FileCache>>,
}

impl LogUsageSource {
    pub fn new(projects_dir: PathBuf) -> Self {
        Self {
            projects_dir,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// `~/.claude/projects` - the directory Claude Code writes one JSONL
    /// transcript file per session into, nested one level under a
    /// per-project directory.
    pub fn default_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
        PathBuf::from(home).join(".claude/projects")
    }

    /// Scans every transcript file and returns every parsed `LogEntry`.
    /// Never fails: a missing/unreadable top-level directory, an
    /// unreadable project subdirectory, an unreadable file, or a
    /// malformed line are all silently skipped rather than propagated -
    /// callers always get *some* answer (possibly empty), matching this
    /// plugin's "malformed becomes a fallback" convention. Runs inside
    /// `spawn_blocking` since this may synchronously read many files.
    pub async fn entries(&self) -> Vec<LogEntry> {
        let projects_dir = self.projects_dir.clone();
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || Self::scan(&projects_dir, &cache))
            .await
            .unwrap_or_default()
    }

    fn scan(projects_dir: &Path, cache: &Mutex<FileCache>) -> Vec<LogEntry> {
        let Ok(project_dirs) = std::fs::read_dir(projects_dir) else {
            return Vec::new();
        };
        let mut guard = cache.lock().unwrap();
        let mut all = Vec::new();
        for project_entry in project_dirs.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&project_path) else {
                continue;
            };
            for file_entry in files.flatten() {
                let file_path = file_entry.path();
                if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(metadata) = file_entry.metadata() else {
                    continue;
                };
                let Ok(mtime) = metadata.modified() else {
                    continue;
                };
                let cached = guard
                    .get(&file_path)
                    .filter(|(cached_mtime, _)| *cached_mtime == mtime)
                    .map(|(_, entries)| entries.clone());
                let entries = match cached {
                    Some(entries) => entries,
                    None => {
                        let parsed = parse_file(&file_path);
                        guard.insert(file_path.clone(), (mtime, parsed.clone()));
                        parsed
                    }
                };
                all.extend(entries);
            }
        }
        all
    }
}

impl Default for LogUsageSource {
    fn default() -> Self {
        Self::new(Self::default_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // Trimmed real samples from a ~/.claude/projects/*/*.jsonl transcript on
    // this machine, keeping only the fields this module reads.
    const ASSISTANT_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"claude-opus-5","usage":{"input_tokens":2,"output_tokens":346,"cache_creation_input_tokens":19929,"cache_read_input_tokens":29011}}}"#;
    const USER_LINE: &str = r#"{"type":"user","timestamp":"2026-09-13T12:59:00.000Z","message":{"role":"user","content":"hi"}}"#;
    const SYNTHETIC_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"<synthetic>","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
    const MISSING_CACHE_FIELDS_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"claude-sonnet-5","usage":{"input_tokens":100,"output_tokens":50}}}"#;

    #[test]
    fn total_tokens_sums_all_four_categories() {
        let entry = LogEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 9, 13, 12, 59, 1).unwrap(),
            model: "claude-opus-5".to_string(),
            input_tokens: 2,
            output_tokens: 346,
            cache_creation_input_tokens: 19929,
            cache_read_input_tokens: 29011,
        };
        assert_eq!(entry.total_tokens(), 49288);
    }

    #[test]
    fn parse_line_parses_a_well_formed_assistant_line() {
        let entry = parse_line(ASSISTANT_LINE).unwrap();
        assert_eq!(entry.model, "claude-opus-5");
        assert_eq!(entry.input_tokens, 2);
        assert_eq!(entry.output_tokens, 346);
        assert_eq!(entry.cache_creation_input_tokens, 19929);
        assert_eq!(entry.cache_read_input_tokens, 29011);
        let expected = DateTime::parse_from_rfc3339("2026-09-13T12:59:01.971Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(entry.timestamp, expected);
    }

    #[test]
    fn parse_line_skips_non_assistant_lines() {
        assert!(parse_line(USER_LINE).is_none());
    }

    #[test]
    fn parse_line_skips_synthetic_model() {
        assert!(parse_line(SYNTHETIC_LINE).is_none());
    }

    #[test]
    fn parse_line_skips_malformed_json() {
        assert!(parse_line("not json at all").is_none());
    }

    #[test]
    fn parse_line_defaults_missing_cache_fields_to_zero() {
        let entry = parse_line(MISSING_CACHE_FIELDS_LINE).unwrap();
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 50);
        assert_eq!(entry.cache_creation_input_tokens, 0);
        assert_eq!(entry.cache_read_input_tokens, 0);
    }

    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn parse_file_skips_malformed_lines_but_keeps_valid_ones() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(
            &path,
            format!("{ASSISTANT_LINE}\nnot json\n{SYNTHETIC_LINE}\n{USER_LINE}\n"),
        )
        .unwrap();

        let entries = parse_file(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model, "claude-opus-5");
    }

    #[tokio::test]
    async fn entries_discovers_files_across_multiple_project_directories() {
        let root = tempdir().unwrap();
        let project_a = root.path().join("project-a");
        let project_b = root.path().join("project-b");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();
        fs::write(project_a.join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        fs::write(project_b.join("session2.jsonl"), ASSISTANT_LINE).unwrap();
        fs::write(project_b.join("not-a-transcript.txt"), "ignore me").unwrap();

        let source = LogUsageSource::new(root.path().to_path_buf());
        let entries = source.entries().await;
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn entries_returns_empty_when_projects_dir_is_missing() {
        let source = LogUsageSource::new(PathBuf::from("/nonexistent/claude/projects"));
        assert_eq!(source.entries().await, Vec::new());
    }

    #[tokio::test]
    async fn entries_picks_up_changes_when_a_file_is_modified() {
        let root = tempdir().unwrap();
        let project = root.path().join("project-a");
        fs::create_dir_all(&project).unwrap();
        let file_path = project.join("session1.jsonl");
        fs::write(&file_path, ASSISTANT_LINE).unwrap();

        let source = LogUsageSource::new(root.path().to_path_buf());
        let first = source.entries().await;
        assert_eq!(first.len(), 1);

        // Sleep past common filesystem mtime granularity (up to 1s on some
        // filesystems/CI runners) before rewriting, so the cache's mtime
        // check reliably observes the change instead of reading a stale
        // cached parse.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        fs::write(&file_path, format!("{ASSISTANT_LINE}\n{ASSISTANT_LINE}\n")).unwrap();

        let second = source.entries().await;
        assert_eq!(second.len(), 2);
    }
}
