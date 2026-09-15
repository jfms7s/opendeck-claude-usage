# Metric Tile (Tokens / Cost) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new keypad-only "Metric Tile" action showing total Tokens or Cost, parsed from Claude Code's local `~/.claude/projects/*/*.jsonl` transcript logs, over Today (rolling 24h) / 7 days (rolling 168h) / Session (current 5h rate-limit window) — with a per-instance configurable refresh interval and tap-to-refresh.

**Architecture:** A new `LogUsageSource` scans and parses transcript JSONL files with a per-file mtime cache (avoids re-parsing unchanged files across ticks and across multiple tile instances). A new `pricing` module estimates dollar cost from tokens via a hardcoded, explicitly-unverified per-model-family price table (log entries carry no cost field at all). A new `metric` module aggregates entries into a small `MetricDisplay` (label/value/subtitle/accent), reusing the existing "compute once, render from the same struct" pattern `format.rs`/`UsageDisplay` already establishes. A new `MetricTileAction` follows the existing `UsageGaugeAction`/`PeakClockAction` shape (registry + background tick loop + `Action` trait impl), except its registry stores each instance's own next-due time instead of one shared fixed interval, since refresh cadence is now per-instance configurable.

**Tech Stack:** Rust (edition 2024), tokio, serde/serde_json, chrono, dashmap, openaction 2.7. No new dependencies — directory walking uses `std::fs`, no `glob`/`walkdir` needed.

**Spec:** `docs/superpowers/specs/2026-09-15-metric-tile-design.md`

## Global Constraints

- No new crates added to `Cargo.toml` — everything needed (directory walking, blocking file I/O, caching) is achievable with the dependencies already present.
- Every failure mode (missing directory, unreadable file, malformed line, unrecognized model, missing `resets_at`) degrades to a graceful fallback value, never a panic or a propagated hard error — matches this plugin's existing convention (see `source/file.rs`'s `resets_at` handling and `peak.rs`'s per-field fallback).
- Cost figures are an explicitly unverified, best-effort estimate (no cost field exists in the source logs) — every place that computes or displays cost must carry a comment/doc note saying so, and the README must disclose it in the same style as the existing `extra_usage` dollar-amount caveat.
- New Rust modules follow the existing file-per-responsibility layout (`src/source/logs.rs`, `src/pricing.rs`, `src/metric.rs`, `src/metric_icon.rs`, `src/metric_action.rs`) and existing conventions: doc comments explain *why*, not *what*; SVG icons stay decorative-only (text stays as native tile title, per `icon.rs`'s existing rationale); tests live in a `#[cfg(test)] mod tests` block at the bottom of each file, mirroring every existing module.
- Action UUID: `com.jfms7s.claudeusage.metrictile`. Property inspector JS wiring copies the existing `index.html`/`peakclock.html` websocket boilerplate verbatim (don't redesign it).

---

### Task 1: Transcript log parsing and scanning (`LogUsageSource`)

**Files:**
- Create: `src/source/logs.rs`
- Modify: `src/source/mod.rs` (add `pub mod logs;`)

**Interfaces:**
- Consumes: nothing from other new modules (this is the foundation task).
- Produces (used by Tasks 2, 3, 5):
  ```rust
  pub struct LogEntry {
      pub timestamp: DateTime<Utc>,
      pub model: String,
      pub input_tokens: u64,
      pub output_tokens: u64,
      pub cache_creation_input_tokens: u64,
      pub cache_read_input_tokens: u64,
  }
  impl LogEntry {
      pub fn total_tokens(&self) -> u64;
  }
  pub struct LogUsageSource { /* private fields */ }
  impl LogUsageSource {
      pub fn new(projects_dir: PathBuf) -> Self;
      pub fn default_path() -> PathBuf; // ~/.claude/projects
      pub async fn entries(&self) -> Vec<LogEntry>; // never fails
  }
  impl Default for LogUsageSource { fn default() -> Self; }
  ```

- [ ] **Step 1: Write failing tests for line parsing**

Create `src/source/logs.rs` with just the type definitions and a `#[cfg(test)] mod tests` block (no `parse_line`/`parse_file` implementation yet):

```rust
use chrono::{DateTime, Utc};

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
        self.input_tokens + self.output_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
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
        assert_eq!(entry.timestamp, Utc.with_ymd_and_hms(2026, 9, 13, 12, 59, 1).unwrap());
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
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib source::logs`
Expected: FAIL to compile with `cannot find function 'parse_line' in this scope`

- [ ] **Step 3: Implement `parse_line`**

Add above the `#[cfg(test)]` block in `src/source/logs.rs`:

```rust
use serde::Deserialize;

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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib source::logs`
Expected: PASS (6 tests)

- [ ] **Step 5: Write failing tests for file and directory scanning**

Add to the `tests` module in `src/source/logs.rs`:

```rust
    use std::fs;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn parse_file_skips_malformed_lines_but_keeps_valid_ones() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(&path, format!("{ASSISTANT_LINE}\nnot json\n{SYNTHETIC_LINE}\n{USER_LINE}\n")).unwrap();

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
```

- [ ] **Step 6: Run tests to verify they fail to compile**

Run: `cargo test --lib source::logs`
Expected: FAIL to compile with `cannot find function 'parse_file'` / `cannot find type 'LogUsageSource'`

- [ ] **Step 7: Implement `parse_file` and `LogUsageSource`**

Add to `src/source/logs.rs`:

```rust
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
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --lib source::logs`
Expected: PASS (10 tests)

- [ ] **Step 9: Register the new module**

In `src/source/mod.rs`, change the top line:

```rust
pub mod file;
pub mod logs;
```

- [ ] **Step 10: Run the full test suite**

Run: `cargo test`
Expected: PASS (all existing tests still pass, plus the 10 new ones)

- [ ] **Step 11: Commit**

```bash
git add src/source/logs.rs src/source/mod.rs
git commit -m "$(cat <<'EOF'
feat: add LogUsageSource for parsing Claude Code transcript logs

Scans ~/.claude/projects/*/*.jsonl for billable assistant turns
(token usage per message), with a per-file mtime cache so unchanged
files aren't re-parsed across repeated scans. Foundation for the
upcoming Tokens/Cost metric tile.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Cost pricing table

**Files:**
- Create: `src/pricing.rs`
- Modify: `src/main.rs:1-7` (add `mod pricing;`)

**Interfaces:**
- Consumes: `crate::source::logs::LogEntry` (from Task 1).
- Produces (used by Task 3):
  ```rust
  pub struct PriceTable { pub input: f64, pub output: f64, pub cache_write: f64, pub cache_read: f64 } // $ per 1,000,000 tokens
  pub fn price_for_model(model: &str) -> Option<PriceTable>;
  pub fn cost_for_entry(entry: &LogEntry) -> Option<f64>;
  ```

- [ ] **Step 1: Write failing tests**

Create `src/pricing.rs`:

```rust
use crate::source::logs::LogEntry;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(model: &str, input: u64, output: u64, cache_write: u64, cache_read: u64) -> LogEntry {
        LogEntry {
            timestamp: Utc::now(),
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: cache_write,
            cache_read_input_tokens: cache_read,
        }
    }

    #[test]
    fn matches_versioned_opus_model_name() {
        assert!(price_for_model("claude-opus-5").is_some());
    }

    #[test]
    fn matches_bare_family_model_names() {
        assert!(price_for_model("opus").is_some());
        assert!(price_for_model("sonnet").is_some());
        assert!(price_for_model("haiku").is_some());
    }

    #[test]
    fn returns_none_for_an_unrecognized_model() {
        assert!(price_for_model("gpt-4").is_none());
        assert!(price_for_model("<synthetic>").is_none());
    }

    #[test]
    fn cost_for_entry_prices_each_token_category_at_the_model_familys_rate() {
        // 1M of each category at Opus rates: $15 input + $75 output +
        // $18.75 cache-write + $1.5 cache-read = $110.25.
        let e = entry("claude-opus-5", 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert_eq!(cost_for_entry(&e), Some(110.25));
    }

    #[test]
    fn cost_for_entry_is_none_for_an_unrecognized_model() {
        let e = entry("gpt-4", 1_000_000, 0, 0, 0);
        assert_eq!(cost_for_entry(&e), None);
    }

    #[test]
    fn cost_for_entry_scales_linearly_with_token_count() {
        let e = entry("claude-sonnet-5", 500_000, 0, 0, 0);
        // Sonnet input rate is $3/M -> 500K tokens = $1.50.
        assert_eq!(cost_for_entry(&e), Some(1.5));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib pricing`
Expected: FAIL to compile with `cannot find function 'price_for_model'` (and `mod pricing` not yet registered — see Step 5, do that first if the test runner can't find the module at all)

- [ ] **Step 3: Implement the price table and lookup functions**

Add above the `#[cfg(test)]` block in `src/pricing.rs`:

```rust
/// $ per 1,000,000 tokens, split by how the token was spent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceTable {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

// These rates are a **best-effort, unverified snapshot** - not sourced
// from Claude Code or any live pricing API, and not re-checked against
// the account used to build this plugin (same caveat as the existing
// extra_usage dollar-amount assumption documented in the README). If
// Anthropic changes pricing, or a model family's rate is simply wrong,
// this is the first place to fix.
const OPUS: PriceTable = PriceTable { input: 15.0, output: 75.0, cache_write: 18.75, cache_read: 1.5 };
const SONNET: PriceTable = PriceTable { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.3 };
const HAIKU: PriceTable = PriceTable { input: 0.8, output: 4.0, cache_write: 1.0, cache_read: 0.08 };

/// Matches by substring against the lowercased model name - handles both
/// versioned names like `claude-opus-5` and bare family names like
/// `opus` identically, since Claude Code has been observed writing both
/// forms into transcript logs. Returns `None` for anything unrecognized
/// (a future model family not yet in this table, or a typo'd name) -
/// callers exclude such entries from Cost rather than guessing a price.
pub fn price_for_model(model: &str) -> Option<PriceTable> {
    let lower = model.to_lowercase();
    if lower.contains("opus") {
        Some(OPUS)
    } else if lower.contains("sonnet") {
        Some(SONNET)
    } else if lower.contains("haiku") {
        Some(HAIKU)
    } else {
        None
    }
}

/// Estimated cost in dollars for one log entry, or `None` if its model
/// isn't recognized - the entry still counts toward Tokens, just not
/// Cost (see `metric::build_metric_display`).
pub fn cost_for_entry(entry: &LogEntry) -> Option<f64> {
    let price = price_for_model(&entry.model)?;
    let million = 1_000_000.0;
    Some(
        entry.input_tokens as f64 / million * price.input
            + entry.output_tokens as f64 / million * price.output
            + entry.cache_creation_input_tokens as f64 / million * price.cache_write
            + entry.cache_read_input_tokens as f64 / million * price.cache_read,
    )
}
```

- [ ] **Step 4: Register the module**

In `src/main.rs`, the top `mod` block becomes:

```rust
mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod peak;
mod pricing;
mod source;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib pricing`
Expected: PASS (6 tests)

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/pricing.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat: add a hardcoded, best-effort cost price table

Claude Code's transcript logs carry token counts but no cost field,
so Cost has to be estimated from a per-model-family price table.
Explicitly documented as an unverified snapshot, same caveat as the
existing extra_usage dollar-amount assumption.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Range resolution, aggregation, and formatting

**Files:**
- Create: `src/metric.rs`
- Modify: `src/main.rs` (add `mod metric;`)

**Interfaces:**
- Consumes: `crate::source::logs::LogEntry` (Task 1), `crate::pricing::cost_for_entry` (Task 2).
- Produces (used by Tasks 4, 5):
  ```rust
  pub const TOKENS_ACCENT: &str; // fixed color identifying the Tokens metric
  pub const COST_ACCENT: &str;   // fixed color identifying the Cost metric

  #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
  pub enum MetricKind { #[default] Tokens, Cost }
  #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
  pub enum RangeKind { #[default] Today, SevenDay, Session }

  pub struct MetricDisplay {
      pub label: &'static str,
      pub value_text: String,
      pub subtitle: &'static str,
      pub accent_color: &'static str,
  }

  pub fn range_bounds(range: RangeKind, now: DateTime<Utc>, session_resets_at: Option<DateTime<Utc>>) -> (DateTime<Utc>, DateTime<Utc>);
  pub fn format_tokens(total: u64) -> String;
  pub fn format_cost(total: f64) -> String;
  pub fn build_metric_display(entries: &[LogEntry], metric: MetricKind, range: RangeKind, now: DateTime<Utc>, session_resets_at: Option<DateTime<Utc>>) -> MetricDisplay;
  pub fn error_display() -> MetricDisplay;
  ```

- [ ] **Step 1: Write failing tests for range bounds and formatting**

Create `src/metric.rs`:

```rust
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::pricing::cost_for_entry;
use crate::source::logs::LogEntry;

pub const TOKENS_ACCENT: &str = "#38bdf8";
pub const COST_ACCENT: &str = "#fb923c";
const NO_DATA_ACCENT: &str = "#6b7280";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricKind {
    #[default]
    Tokens,
    Cost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RangeKind {
    #[default]
    Today,
    #[serde(rename = "sevenday")]
    SevenDay,
    Session,
}

pub struct MetricDisplay {
    pub label: &'static str,
    pub value_text: String,
    pub subtitle: &'static str,
    pub accent_color: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    #[test]
    fn today_is_a_rolling_24h_window_ending_now() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::Today, now, None);
        assert_eq!(start, now - Duration::hours(24));
        assert_eq!(end, now);
    }

    #[test]
    fn seven_day_is_a_rolling_168h_window_ending_now() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::SevenDay, now, None);
        assert_eq!(start, now - Duration::hours(168));
        assert_eq!(end, now);
    }

    #[test]
    fn session_uses_the_five_hour_window_ending_at_resets_at_when_known() {
        let now = dt(20, 0);
        let resets_at = dt(22, 40);
        let (start, end) = range_bounds(RangeKind::Session, now, Some(resets_at));
        assert_eq!(start, resets_at - Duration::hours(5));
        assert_eq!(end, resets_at);
    }

    #[test]
    fn session_falls_back_to_a_rolling_five_hour_window_when_resets_at_is_unknown() {
        let now = dt(20, 0);
        let (start, end) = range_bounds(RangeKind::Session, now, None);
        assert_eq!(start, now - Duration::hours(5));
        assert_eq!(end, now);
    }

    #[test]
    fn format_tokens_under_a_thousand_is_exact() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_thousands_trims_a_trailing_zero_decimal() {
        assert_eq!(format_tokens(318_000), "318K");
    }

    #[test]
    fn format_tokens_thousands_keeps_a_meaningful_decimal() {
        assert_eq!(format_tokens(318_500), "318.5K");
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(format_tokens(1_234_567), "1.2M");
        assert_eq!(format_tokens(2_000_000), "2M");
    }

    #[test]
    fn format_cost_always_shows_two_decimals() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(8.4), "$8.40");
        assert_eq!(format_cost(1234.567), "$1234.57");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib metric`
Expected: FAIL to compile with `cannot find function 'range_bounds'` (register `mod metric;` first per Step 6 if the module isn't found at all)

- [ ] **Step 3: Implement `range_bounds` and formatting functions**

Add to `src/metric.rs`, above the tests module:

```rust
/// `[start, end]` bounds for `range`, in UTC. `Today`/`SevenDay` are
/// rolling windows ending at `now` (not calendar-aligned). `Session`
/// mirrors the existing Usage Gauge's 5-hour rate-limit window:
/// `[resets_at - 5h, resets_at]` when `session_resets_at` is known
/// (read from the same statusline-usage.json the gauge reads), falling
/// back to a rolling last-5h window when it isn't - a fallback, not an
/// error, matching this plugin's existing convention.
pub fn range_bounds(
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    match range {
        RangeKind::Today => (now - Duration::hours(24), now),
        RangeKind::SevenDay => (now - Duration::hours(24 * 7), now),
        RangeKind::Session => {
            let end = session_resets_at.unwrap_or(now);
            let start = end - Duration::hours(5);
            (start, end)
        }
    }
}

/// `< 1000` as an exact integer; otherwise one decimal place with a
/// K/M suffix, trimming a trailing ".0" (`318000` -> `"318K"`, `318500`
/// -> `"318.5K"`, `1234567` -> `"1.2M"`) - matches the mockup this tile
/// is based on.
pub fn format_tokens(total: u64) -> String {
    if total < 1000 {
        return total.to_string();
    }
    let (value, suffix) = if total < 1_000_000 {
        (total as f64 / 1_000.0, "K")
    } else {
        (total as f64 / 1_000_000.0, "M")
    };
    let formatted = format!("{value:.1}");
    let trimmed = formatted.strip_suffix(".0").unwrap_or(&formatted);
    format!("{trimmed}{suffix}")
}

/// Always two decimal places, e.g. `"$8.40"`.
pub fn format_cost(total: f64) -> String {
    format!("${total:.2}")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib metric`
Expected: PASS (9 tests)

- [ ] **Step 5: Write failing tests for aggregation**

Add to the `tests` module in `src/metric.rs`:

```rust
    fn entry(timestamp: DateTime<Utc>, model: &str, input: u64, output: u64) -> LogEntry {
        LogEntry {
            timestamp,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    #[test]
    fn build_metric_display_sums_tokens_only_within_range() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 100, 50), // within last 24h
            entry(now - Duration::hours(30), "claude-sonnet-5", 999, 999), // outside the Today (24h) window
        ];
        let display = build_metric_display(&entries, MetricKind::Tokens, RangeKind::Today, now, None);
        assert_eq!(display.label, "Tokens");
        assert_eq!(display.value_text, "150");
        assert_eq!(display.subtitle, "today");
        assert_eq!(display.accent_color, TOKENS_ACCENT);
    }

    #[test]
    fn build_metric_display_cost_skips_entries_with_an_unrecognized_model() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 1_000_000, 0), // $3.00 at sonnet input rate
            entry(now - Duration::hours(1), "some-future-model", 1_000_000, 0), // unrecognized - excluded
        ];
        let display = build_metric_display(&entries, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.label, "Cost");
        assert_eq!(display.value_text, "$3.00");
    }

    #[test]
    fn build_metric_display_subtitle_matches_each_range() {
        let now = dt(20, 0);
        let entries: Vec<LogEntry> = vec![];
        assert_eq!(build_metric_display(&entries, MetricKind::Tokens, RangeKind::SevenDay, now, None).subtitle, "7 days");
        assert_eq!(build_metric_display(&entries, MetricKind::Tokens, RangeKind::Session, now, None).subtitle, "session");
    }

    #[test]
    fn error_display_is_a_clear_no_data_state() {
        let display = error_display();
        assert_eq!(display.subtitle, "no data");
        assert_eq!(display.accent_color, NO_DATA_ACCENT);
    }
```

Add the missing `use` for `cost_for_entry` is already present at the top of the file (from Step 1's skeleton).

- [ ] **Step 6: Register the module (needed for the crate to compile at all)**

In `src/main.rs`, the top `mod` block becomes:

```rust
mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod metric;
mod peak;
mod pricing;
mod source;
```

- [ ] **Step 7: Run tests to verify they fail to compile**

Run: `cargo test --lib metric`
Expected: FAIL to compile with `cannot find function 'build_metric_display'` / `cannot find function 'error_display'`

- [ ] **Step 8: Implement `build_metric_display` and `error_display`**

Add to `src/metric.rs`, above the tests module:

```rust
/// Computes what to show for one instance's current metric/range
/// selection from the full set of parsed log entries. Always produces a
/// real number (possibly zero) - callers decide separately whether "no
/// entries at all were found anywhere" warrants `error_display()`
/// instead (see `metric_action.rs`), since a legitimate zero-usage range
/// is a different situation from no log data existing at all.
pub fn build_metric_display(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    now: DateTime<Utc>,
    session_resets_at: Option<DateTime<Utc>>,
) -> MetricDisplay {
    let (start, end) = range_bounds(range, now, session_resets_at);
    let in_range: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.timestamp >= start && e.timestamp <= end)
        .collect();

    let subtitle = match range {
        RangeKind::Today => "today",
        RangeKind::SevenDay => "7 days",
        RangeKind::Session => "session",
    };

    match metric {
        MetricKind::Tokens => {
            let total: u64 = in_range.iter().map(|e| e.total_tokens()).sum();
            MetricDisplay {
                label: "Tokens",
                value_text: format_tokens(total),
                subtitle,
                accent_color: TOKENS_ACCENT,
            }
        }
        MetricKind::Cost => {
            let total: f64 = in_range.iter().filter_map(|e| cost_for_entry(e)).sum();
            MetricDisplay {
                label: "Cost",
                value_text: format_cost(total),
                subtitle,
                accent_color: COST_ACCENT,
            }
        }
    }
}

/// No log data available anywhere (a fresh install, or `~/.claude/projects`
/// missing/unreadable) - a clearly-labeled "no data" state, mirroring
/// `format::error_display()`.
pub fn error_display() -> MetricDisplay {
    MetricDisplay {
        label: "\u{2014}",
        value_text: "\u{2014}".to_string(),
        subtitle: "no data",
        accent_color: NO_DATA_ACCENT,
    }
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test --lib metric`
Expected: PASS (13 tests)

- [ ] **Step 10: Run the full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add src/metric.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat: add range resolution and metric aggregation for the metric tile

Today/7-day are rolling windows; Session mirrors the existing Usage
Gauge's 5-hour rate-limit window (anchored to statusline-usage.json's
resets_at, with a rolling-last-5h fallback). Tokens and Cost each
aggregate to a small MetricDisplay, following the same "compute once,
render from one struct" pattern format.rs already establishes.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Metric tile icon rendering

**Files:**
- Create: `src/metric_icon.rs`
- Modify: `src/main.rs` (add `mod metric_icon;`)

**Interfaces:**
- Consumes: nothing (takes a plain `&str` color, decoupled from `MetricDisplay` so it doesn't need to know about metrics/ranges at all).
- Produces (used by Task 5):
  ```rust
  pub fn build_metric_icon(accent_color: &str) -> String; // base64 SVG data URI
  ```

- [ ] **Step 1: Write failing tests**

Create `src/metric_icon.rs`:

```rust
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(uri: &str) -> String {
        let prefix = "data:image/svg+xml;base64,";
        assert!(uri.starts_with(prefix), "got: {uri}");
        let bytes = STANDARD.decode(&uri[prefix.len()..]).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn builds_a_valid_svg_data_uri() {
        let svg = decode(&build_metric_icon("#38bdf8"));
        assert!(svg.starts_with("<svg"), "got: {svg}");
        assert!(svg.contains(CARD_COLOR));
    }

    #[test]
    fn draws_the_passed_accent_color() {
        let svg = decode(&build_metric_icon("#fb923c"));
        assert!(svg.contains("#fb923c"));
    }

    #[test]
    fn different_accent_colors_produce_different_icons() {
        let a = build_metric_icon("#38bdf8");
        let b = build_metric_icon("#fb923c");
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib metric_icon`
Expected: FAIL to compile with `cannot find function 'build_metric_icon'` (register `mod metric_icon;` first per Step 4 if the module isn't found at all)

- [ ] **Step 3: Implement the icon renderer**

Add to `src/metric_icon.rs`, above the tests module:

```rust
const CARD_COLOR: &str = "#111827";
const UNDERLINE_WIDTH: f64 = 24.0;
const UNDERLINE_X: f64 = (100.0 - UNDERLINE_WIDTH) / 2.0;
const UNDERLINE_Y: f64 = 24.0;

fn render_svg(accent_color: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><rect x="0" y="0" width="100" height="100" rx="12" fill="{CARD_COLOR}" /><rect x="{UNDERLINE_X}" y="{UNDERLINE_Y}" width="{UNDERLINE_WIDTH}" height="3" rx="1.5" fill="{accent_color}" /></svg>"#
    )
}

/// Builds the `image` string OpenDeck's `setImage` event expects, same
/// base64 data-URI convention as `icon::build_icon` /
/// `clock_icon::build_clock_icon`. The label/value/subtitle text itself
/// renders as the tile's native title (crisper, consistent with the
/// other two tiles - see `icon.rs`'s rationale) - this SVG only draws
/// the card background and a colored underline accent beneath where the
/// label line sits.
pub fn build_metric_icon(accent_color: &str) -> String {
    let svg = render_svg(accent_color);
    let encoded = STANDARD.encode(svg.as_bytes());
    format!("data:image/svg+xml;base64,{encoded}")
}
```

- [ ] **Step 4: Register the module**

In `src/main.rs`, the top `mod` block becomes:

```rust
mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod metric;
mod metric_icon;
mod peak;
mod pricing;
mod source;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib metric_icon`
Expected: PASS (3 tests)

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/metric_icon.rs src/main.rs
git commit -m "$(cat <<'EOF'
feat: add the metric tile's card/underline icon renderer

Text (label/value/subtitle) stays as the tile's native title for
crispness, same as the existing gauge and clock tiles - this SVG only
draws the dark card background and a colored accent underline.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: `MetricTileAction`, manifest, property inspector, and wiring

**Files:**
- Create: `src/metric_action.rs`
- Create: `assets/propertyInspector/metrictile.html`
- Modify: `assets/manifest.json`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `LogUsageSource` (Task 1), `crate::source::FileUsageSource` (existing), `crate::metric::{MetricKind, RangeKind, MetricDisplay, build_metric_display, error_display}` (Task 3), `crate::metric_icon::build_metric_icon` (Task 4).
- Produces: `MetricTileAction` (registered in `main.rs`, implements `openaction::Action`), `MetricTileSettings`.

- [ ] **Step 1: Write failing tests for settings defaults and registry tracking**

Create `src/metric_action.rs`:

```rust
use crate::metric::{MetricKind, RangeKind};
use crate::source::file::FileUsageSource;
use crate::source::logs::LogUsageSource;
use async_trait::async_trait;
use dashmap::DashMap;
use openaction::{Action, Instance, OpenActionResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct MetricTileSettings {
    pub metric: MetricKind,
    pub range: RangeKind,
    pub refresh_seconds: u64,
}

impl Default for MetricTileSettings {
    fn default() -> Self {
        Self {
            metric: MetricKind::default(),
            range: RangeKind::default(),
            refresh_seconds: 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_are_tokens_today_at_sixty_seconds() {
        let settings = MetricTileSettings::default();
        assert_eq!(settings.metric, MetricKind::Tokens);
        assert_eq!(settings.range, RangeKind::Today);
        assert_eq!(settings.refresh_seconds, 60);
    }

    #[test]
    fn default_matches_missing_key_deserialization() {
        // Same footgun UsageGaugeSettings' own test guards against: openaction
        // falls back to Default::default() when settings JSON fails to
        // deserialize at all, not just on missing fields - confirm both
        // paths land on the same value.
        let from_missing_keys: MetricTileSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_missing_keys, MetricTileSettings::default());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib metric_action`
Expected: FAIL to compile - `mod metric_action;` isn't registered yet (add it now, in this step, in `src/main.rs`'s top `mod` block, alongside the others added in Tasks 2-4):

```rust
mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod metric;
mod metric_action;
mod metric_icon;
mod peak;
mod pricing;
mod source;
```

- [ ] **Step 3: Run tests again to verify they pass**

Run: `cargo test --lib metric_action`
Expected: PASS (2 tests) — the settings struct alone is enough to compile and pass these two.

- [ ] **Step 4: Write failing tests for the tracking registry and due-time scheduling**

Add to the `tests` module in `src/metric_action.rs`:

```rust
    #[test]
    fn track_then_untrack_round_trips_through_the_registry() {
        let action = MetricTileAction::new(LogUsageSource::default(), FileUsageSource::default());
        action.track("ctx1", MetricTileSettings::default());
        assert!(action.registry.contains_key("ctx1"));

        action.untrack("ctx1");
        assert!(!action.registry.contains_key("ctx1"));
    }

    #[test]
    fn tracking_the_same_instance_twice_overwrites_its_settings() {
        let action = MetricTileAction::new(LogUsageSource::default(), FileUsageSource::default());
        action.track("ctx1", MetricTileSettings { metric: MetricKind::Tokens, range: RangeKind::Today, refresh_seconds: 60 });
        action.track("ctx1", MetricTileSettings { metric: MetricKind::Cost, range: RangeKind::Session, refresh_seconds: 30 });
        let tracked = action.registry.get("ctx1").unwrap();
        assert_eq!(tracked.settings.metric, MetricKind::Cost);
        assert_eq!(tracked.settings.refresh_seconds, 30);
    }

    #[test]
    fn due_instance_ids_includes_only_elapsed_or_exactly_due_instances() {
        let now = Instant::now();
        let tracked = vec![
            ("already_due".to_string(), now - Duration::from_secs(1)),
            ("not_due_yet".to_string(), now + Duration::from_secs(30)),
            ("exactly_due".to_string(), now),
        ];
        let mut due = due_instance_ids(&tracked, now);
        due.sort();
        assert_eq!(due, vec!["already_due".to_string(), "exactly_due".to_string()]);
    }

    #[test]
    fn action_uuid_matches_the_shipped_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/manifest.json")).unwrap();
        let manifest_uuid = manifest["Actions"][2]["UUID"].as_str().unwrap();
        assert_eq!(manifest_uuid, <MetricTileAction as Action>::UUID);
    }
```

- [ ] **Step 5: Run tests to verify they fail to compile**

Run: `cargo test --lib metric_action`
Expected: FAIL to compile with `cannot find type 'MetricTileAction'` / `cannot find function 'due_instance_ids'`

- [ ] **Step 6: Implement `MetricTileAction` and `due_instance_ids`**

Add to `src/metric_action.rs`, above the tests module:

```rust
struct TrackedInstance {
    settings: MetricTileSettings,
    next_due: Instant,
}

#[derive(Clone)]
pub struct MetricTileAction {
    log_source: Arc<LogUsageSource>,
    session_source: Arc<FileUsageSource>,
    registry: Arc<DashMap<String, TrackedInstance>>,
}

impl MetricTileAction {
    pub fn new(log_source: LogUsageSource, session_source: FileUsageSource) -> Self {
        Self {
            log_source: Arc::new(log_source),
            session_source: Arc::new(session_source),
            registry: Arc::new(DashMap::new()),
        }
    }

    /// Tracks (or re-tracks, overwriting prior settings) an instance,
    /// due immediately so it renders on the very next tick rather than
    /// waiting a full `refresh_seconds` interval.
    fn track(&self, instance_id: &str, settings: MetricTileSettings) {
        self.registry.insert(
            instance_id.to_string(),
            TrackedInstance { settings, next_due: Instant::now() },
        );
    }

    fn untrack(&self, instance_id: &str) {
        self.registry.remove(instance_id);
    }

    /// Reads the log source and (for the Session range) the existing
    /// statusline-usage.json reset time, then renders one instance -
    /// used by `will_appear`/`did_receive_settings` (so a tile shows
    /// real data immediately), `key_up` (tap-to-refresh), and the tick
    /// loop.
    async fn render(
        instance: &Instance,
        log_source: &LogUsageSource,
        session_source: &FileUsageSource,
        settings: &MetricTileSettings,
    ) -> OpenActionResult<()> {
        let entries = log_source.entries().await;
        let display = if entries.is_empty() {
            crate::metric::error_display()
        } else {
            let session_resets_at = session_source
                .read()
                .await
                .ok()
                .and_then(|s| s.session.resets_at);
            crate::metric::build_metric_display(
                &entries,
                settings.metric,
                settings.range,
                chrono::Utc::now(),
                session_resets_at,
            )
        };
        let title = format!("{}\n{}\n{}", display.label, display.value_text, display.subtitle);
        instance.set_title(Some(title), None).await?;
        instance
            .set_image(Some(crate::metric_icon::build_metric_icon(display.accent_color)), None)
            .await
    }

    /// Runs forever: every 1s, checks every registered instance's
    /// `next_due` and re-renders (then reschedules) only the ones that
    /// have elapsed. A 1s tick is cheap - just an `Instant` comparison
    /// per instance - and is what lets each instance honor its own
    /// `refresh_seconds` independently, unlike the other two tiles'
    /// single shared fixed-interval loop.
    pub async fn tick_loop(&self) {
        loop {
            self.tick_once().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn tick_once(&self) {
        let now = Instant::now();
        let snapshot: Vec<(String, Instant)> = self
            .registry
            .iter()
            .map(|e| (e.key().clone(), e.next_due))
            .collect();

        for instance_id in due_instance_ids(&snapshot, now) {
            let Some(settings) = self.registry.get(&instance_id).map(|t| t.settings.clone()) else {
                continue; // removed between the snapshot and now
            };
            if let Some(mut tracked) = self.registry.get_mut(&instance_id) {
                tracked.next_due = Instant::now() + Duration::from_secs(settings.refresh_seconds.max(1));
            }
            let Some(instance) = openaction::get_instance(instance_id).await else {
                continue; // instance disappeared between the snapshot and now
            };
            if let Err(e) = Self::render(&instance, &self.log_source, &self.session_source, &settings).await {
                log::warn!("metric tile render failed: {e}");
            }
        }
    }
}

/// Pure due-time filter, extracted from `tick_once` so the scheduling
/// decision is unit-testable without touching the DashMap/openaction
/// instance machinery.
fn due_instance_ids(tracked: &[(String, Instant)], now: Instant) -> Vec<String> {
    tracked
        .iter()
        .filter(|(_, due)| *due <= now)
        .map(|(id, _)| id.clone())
        .collect()
}

#[async_trait]
impl Action for MetricTileAction {
    const UUID: &'static str = "com.jfms7s.claudeusage.metrictile";
    type Settings = MetricTileSettings;

    async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.clone());
        Self::render(instance, &self.log_source, &self.session_source, settings).await
    }

    async fn did_receive_settings(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        self.track(&instance.instance_id, settings.clone());
        Self::render(instance, &self.log_source, &self.session_source, settings).await
    }

    async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
        self.untrack(&instance.instance_id);
        Ok(())
    }

    /// A tap forces an immediate refresh of just that tile, same as the
    /// other two tiles' `key_up` - and does not reset its scheduled
    /// `next_due`, since a tap is a bonus refresh, not a reason to skip
    /// the next one.
    async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
        Self::render(instance, &self.log_source, &self.session_source, settings).await
    }
}
```

- [ ] **Step 7: Create the manifest entry and property inspector before running tests**

The new `action_uuid_matches_the_shipped_manifest` test reads `assets/manifest.json`, so update it now. In `assets/manifest.json`, add a third entry to the `"Actions"` array (after the `peakclock` entry):

```json
		{
			"UUID": "com.jfms7s.claudeusage.metrictile",
			"Name": "Metric Tile",
			"Icon": "icons/icon",
			"Tooltip": "Shows total Claude Code tokens or estimated cost, parsed from local transcript logs, for today/7 days/the current session",
			"Controllers": ["Keypad"],
			"PropertyInspectorPath": "propertyInspector/metrictile.html",
			"States": [{ "Image": "icons/actionDefaultImage" }]
		}
```

Create `assets/propertyInspector/metrictile.html`, copying the existing `index.html`'s structure and websocket wiring, adapted for three fields:

```html
<!doctype html>
<html lang="en">
<head>
	<meta charset="utf-8" />
	<meta name="viewport" content="width=device-width, initial-scale=1" />
	<style>
		body { font: 12px system-ui, sans-serif; margin: 0; padding: 8px 12px; }
		label { display: block; margin-top: 10px; font-size: 11px; opacity: 0.8; }
		select, input { width: 100%; box-sizing: border-box; margin-top: 2px; }
	</style>
</head>
<body>
	<label for="metric">Metric</label>
	<select id="metric">
		<option value="tokens">Tokens</option>
		<option value="cost">Cost</option>
	</select>

	<label for="range">Range</label>
	<select id="range">
		<option value="today">Today</option>
		<option value="sevenday">7 days</option>
		<option value="session">Session (5 hour)</option>
	</select>

	<label for="refresh_seconds">Refresh every (seconds)</label>
	<input type="number" id="refresh_seconds" min="5" max="3600" step="1" />

	<script>
		window.connectOpenActionSocketData = new Promise((resolve) => {
			window.connectOpenActionSocket = (...args) => resolve(args);
			window.connectElgatoStreamDeckSocket = window.connectOpenActionSocket;
		});

		let websocket;
		let uuid;

		window.connectOpenActionSocketData.then(([inPort, inUUID, inRegisterEvent, inInfo, inActionInfo]) => {
			uuid = inUUID;
			const actionInfo = JSON.parse(inActionInfo);
			websocket = new WebSocket(`ws://127.0.0.1:${inPort}`);

			websocket.onopen = () => {
				websocket.send(JSON.stringify({ event: inRegisterEvent, uuid: inUUID }));
				applySettings(actionInfo.payload.settings || {});
			};

			websocket.onmessage = (event) => {
				const message = JSON.parse(event.data);
				if (message.event === "didReceiveSettings") {
					applySettings(message.payload.settings || {});
				}
			};
		});

		function applySettings(settings) {
			document.getElementById("metric").value = settings.metric || "tokens";
			document.getElementById("range").value = settings.range || "today";
			document.getElementById("refresh_seconds").value = settings.refresh_seconds || 60;
		}

		function sendSettings() {
			websocket.send(JSON.stringify({
				event: "setSettings",
				context: uuid,
				payload: {
					metric: document.getElementById("metric").value,
					range: document.getElementById("range").value,
					refresh_seconds: parseInt(document.getElementById("refresh_seconds").value, 10) || 60,
				},
			}));
		}

		document.getElementById("metric").addEventListener("change", sendSettings);
		document.getElementById("range").addEventListener("change", sendSettings);
		document.getElementById("refresh_seconds").addEventListener("change", sendSettings);
	</script>
</body>
</html>
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --lib metric_action`
Expected: PASS (6 tests)

- [ ] **Step 9: Wire the action into `main.rs`**

Replace the full contents of `src/main.rs` with:

```rust
mod action;
mod clock_action;
mod clock_icon;
mod format;
mod icon;
mod metric;
mod metric_action;
mod metric_icon;
mod peak;
mod pricing;
mod source;

use action::UsageGaugeAction;
use clock_action::PeakClockAction;
use metric_action::MetricTileAction;
use openaction::{OpenActionResult, register_action, run};
use source::file::FileUsageSource;
use source::logs::LogUsageSource;

#[tokio::main]
async fn main() -> OpenActionResult<()> {
    simplelog::SimpleLogger::init(log::LevelFilter::Info, simplelog::Config::default())
        .expect("logger init");

    let action = UsageGaugeAction::new(FileUsageSource::default());
    let poller = action.clone();
    tokio::spawn(async move { poller.poll_loop().await });

    let clock = PeakClockAction::new();
    let ticker = clock.clone();
    tokio::spawn(async move { ticker.tick_loop().await });

    let metric_tile = MetricTileAction::new(LogUsageSource::default(), FileUsageSource::default());
    let metric_ticker = metric_tile.clone();
    tokio::spawn(async move { metric_ticker.tick_loop().await });

    register_action(action).await;
    register_action(clock).await;
    register_action(metric_tile).await;
    run(std::env::args().collect()).await
}
```

- [ ] **Step 10: Run the full test suite and build**

Run: `cargo test && cargo build`
Expected: PASS, clean build

- [ ] **Step 11: Commit**

```bash
git add src/metric_action.rs src/main.rs assets/manifest.json assets/propertyInspector/metrictile.html
git commit -m "$(cat <<'EOF'
feat: add the Metric Tile action (Tokens/Cost, configurable range and refresh)

New keypad-only action showing total tokens or estimated cost parsed
from local Claude Code transcript logs, for today/7 days/the current
5h session window. Each instance's refresh interval is independently
configurable and tap-to-refresh, unlike the other two tiles' shared
fixed 20s cadence.

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: README documentation

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: nothing (documentation only, no code dependency).
- Produces: nothing consumed by other tasks.

- [ ] **Step 1: Add a data-source and cost-caveat section**

In `README.md`, after the existing "Where the data comes from" section (which ends after the `extra_usage` paragraph, before "## Installing"), insert:

```markdown
## Where the Tokens/Cost tile's data comes from

The **Metric Tile** action reads a different source: Claude Code's own
per-session transcript logs at `~/.claude/projects/<project>/<session-id>.jsonl`,
one file per session, across every project. Each assistant turn in these
files carries token usage (input/output/cache-read/cache-write) but
**no cost figure at all** - Cost is estimated by multiplying tokens by a
hardcoded per-model-family price table in `src/pricing.rs`.

That price table is a **best-effort, unverified snapshot** - it isn't
sourced from Claude Code, isn't fetched from any live pricing API, and
hasn't been re-checked against a real invoice. If Anthropic changes
pricing, the Cost tile's numbers will drift until the table is updated
by hand. Treat Cost as an estimate; treat Tokens (a direct sum from the
logs) as exact.

Entries with `model == "<synthetic>"` (Claude Code's placeholder for
locally-generated content like compaction summaries) are excluded
entirely, since they represent no real API call.

The tile's **Session** range reuses the Usage Gauge's 5-hour rate-limit
window (`resets_at` from `~/.claude/statusline-usage.json`) rather than
a fixed rolling window, so it lines up with what "session" means
elsewhere in this plugin. If that file is missing or has no
`resets_at`, it falls back to a rolling last-5-hour window instead of
erroring.
```

- [ ] **Step 2: Mention the tile in the "Using" section and add smoke-test items**

In `README.md`, after the existing "## Using a dial or tile" section's three numbered steps, insert a new subsection:

```markdown
## Using a Metric Tile

1. Add a **Metric Tile** key on a keypad tile (no dial/Encoder variant).
2. Pick the metric (Tokens or Cost), the range (Today/7 days/Session),
   and how often it refreshes (in seconds).
3. It updates automatically on that schedule; tap the tile for an
   immediate refresh (this doesn't reset the schedule - the next
   automatic refresh still happens on time).
```

Then, in the existing "## Manual smoke-test checklist" list, add three new unchecked items at the end:

```markdown
- [ ] Metric Tile shows the right label/value/subtitle for each metric
      (Tokens/Cost) × range (Today/7 days/Session) combination.
      *(not yet verified)*
- [ ] Metric Tile's configured refresh interval actually changes how
      often it updates (e.g. set to 5s, confirm faster updates than the
      default 60s). *(not yet verified)*
- [ ] Tapping a Metric Tile refreshes it immediately without disrupting
      its next scheduled refresh. *(not yet verified)*
```

- [ ] **Step 3: Run the full test suite one more time (documentation shouldn't affect it, but confirms nothing else was left uncommitted)**

Run: `cargo test`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "$(cat <<'EOF'
docs: document the Metric Tile's log source, cost caveat, and usage

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
EOF
)"
```
