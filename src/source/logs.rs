//! Token usage from Claude Code's own transcripts
//! (`~/.claude/projects/**/*.jsonl`), for Metric Tile and Usage Heatmap.
//!
//! Three rules decide what counts, matching how Claude Code writes them:
//! - **One message, one entry.** Claude Code writes a line per content block
//!   (thinking, text, tool use), and every line repeats the message's
//!   `message.id` + `requestId` with a copy - or a zeroed or still-growing
//!   copy - of its usage. Lines are deduplicated on that pair, keeping the
//!   fullest usage (most output tokens, then most tokens overall).
//! - **Every transcript.** Subagents write their own files under
//!   `<project>/<session>/subagents/`, and a resumed session can copy
//!   earlier messages into a new file; the walk is recursive and the dedup
//!   is across files.
//! - **Billable turns only.** `<synthetic>` (locally generated content such
//!   as compaction summaries) and lines without usage are skipped.
//!
//! Transcripts are append-only, so each file is read incrementally: only
//! the bytes after the last complete line already read, a line at a time.
//! A file that shrank or disappeared means something rewrote history, and
//! the whole store is rebuilt. A projects folder that can't be listed at
//! all (anything but "not found") is not taken as "every file was
//! deleted": the store is kept until a listing works again. The same goes
//! for a sub-folder or transcript that exists but can't be read: its known
//! entries stay until it can be read again.
//!
//! Several tiles ask within moments of each other (start-up, a minute
//! boundary); the default source shares one directory walk between calls
//! less than `DEFAULT_MIN_WALK_INTERVAL` apart.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    /// Interned: tens of thousands of entries share a handful of names.
    pub model: Arc<str>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Every cache write, whatever its lifetime.
    pub cache_creation_input_tokens: u64,
    /// The part of `cache_creation_input_tokens` written with the 1-hour
    /// lifetime, which is billed at a higher rate than the 5-minute one.
    pub cache_creation_1h_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Which project the transcript belongs to: its folder under
    /// `projects_dir`, with worktree folders folded into their parent (see
    /// `project_id`). Interned like `model`.
    pub project: Arc<str>,
}

impl LogEntry {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    /// Which of two copies of one message's usage to keep: the one that
    /// got further (a streamed copy grows; the per-block repeats are often
    /// zeroed).
    fn fuller_than(&self, other: &LogEntry) -> bool {
        (self.output_tokens, self.total_tokens()) > (other.output_tokens, other.total_tokens())
    }
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    /// The session's working directory - only used to name its project.
    cwd: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
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
    #[serde(default)]
    cache_creation: Option<RawCacheCreation>,
}

#[derive(Deserialize, Default)]
struct RawCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

/// One billable line: its entry, and the dedup key of the message it
/// belongs to (`None` for a line without a message id, which then counts
/// on its own).
#[derive(Debug, Clone, PartialEq)]
struct Parsed {
    key: Option<u64>,
    entry: LogEntry,
    cwd: Option<String>,
}

fn message_key(message_id: &str, request_id: Option<&str>) -> u64 {
    // `DefaultHasher::new()` is SipHash with fixed keys, so a key is stable
    // across scans; a 64-bit collision among ~10^5 messages is ~10^-10.
    let mut hasher = DefaultHasher::new();
    message_id.hash(&mut hasher);
    request_id.hash(&mut hasher);
    hasher.finish()
}

/// Parses one JSONL line, or `None` if it isn't a billable assistant turn:
/// not `"type": "assistant"`, no `usage` object, an unparseable timestamp,
/// or `model == "<synthetic>"`. Malformed JSON is `None` too - one bad line
/// must not cost the rest of the file.
fn parse_line(line: &str, models: &mut HashSet<Arc<str>>) -> Option<Parsed> {
    // Most lines (user turns, tool results, summaries) carry no usage;
    // skip them without a full parse.
    if !line.contains("\"usage\"") {
        return None;
    }
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
    let model = match models.get(model.as_str()) {
        Some(interned) => Arc::clone(interned),
        None => {
            let interned: Arc<str> = model.into();
            models.insert(Arc::clone(&interned));
            interned
        }
    };
    Some(Parsed {
        key: message
            .id
            .as_deref()
            .map(|id| message_key(id, raw.request_id.as_deref())),
        entry: LogEntry {
            timestamp,
            model,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_creation_1h_input_tokens: usage
                .cache_creation
                .map_or(0, |c| c.ephemeral_1h_input_tokens)
                .min(usage.cache_creation_input_tokens),
            cache_read_input_tokens: usage.cache_read_input_tokens,
            project: Arc::default(),
        },
        cwd: raw.cwd,
    })
}

/// Where a worktree's path continues below its repository: Claude Code's
/// own worktrees and the `.worktrees/` convention. In an encoded folder
/// name (every non-alphanumeric character a `-`) `/.` becomes `--`, so the
/// markers can't occur inside an ordinary repository name.
const WORKTREE_PATH_MARKERS: [&str; 2] = ["/.claude/worktrees/", "/.worktrees/"];
const WORKTREE_FOLDER_MARKERS: [&str; 2] = ["--claude-worktrees-", "--worktrees-"];

/// Cuts `text` at the earliest of `markers`, so a (nested) worktree folds
/// into its outermost repository.
fn fold_at<'a>(text: &'a str, markers: &[&str]) -> &'a str {
    markers
        .iter()
        .filter_map(|marker| text.find(marker))
        .min()
        .map_or(text, |at| &text[..at])
}

/// A project's id from its folder name under `projects_dir` (Claude Code's
/// encoded working directory), with worktree folders folded into the
/// repository they belong to.
pub fn project_id(folder: &str) -> &str {
    fold_at(folder, &WORKTREE_FOLDER_MARKERS)
}

/// A project's name from a session's working directory: the repository's
/// folder name, worktrees folded. `None` for a path with no name.
fn project_name_from_cwd(cwd: &str) -> Option<&str> {
    let repo = fold_at(cwd, &WORKTREE_PATH_MARKERS).trim_end_matches(['/', '\\']);
    repo.rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
}

/// A name for a project whose transcripts carry no `cwd`: its id, without
/// the encoded home directory in front.
fn project_name_from_id(id: &str) -> String {
    let home = crate::paths::home();
    let encoded_home: String = home
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    id.strip_prefix(&encoded_home)
        .map(|rest| rest.trim_start_matches('-'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(id)
        .to_string()
}

/// A project as the Property Inspector lists it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
}

/// A transcript as last seen: its size and mtime, and how far into it
/// complete lines have been read.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FileState {
    len: u64,
    mtime: SystemTime,
    offset: u64,
}

/// Every entry exactly once. `entries` is the only copy: readers share it
/// through the `Arc`, and it is only cloned if a reader still holds the
/// previous version when new lines arrive.
#[derive(Default)]
struct Store {
    entries: Arc<Vec<LogEntry>>,
    /// Message key → index into `entries`.
    by_key: HashMap<u64, usize>,
    files: HashMap<PathBuf, FileState>,
    models: HashSet<Arc<str>>,
    /// Interned project ids, and each one's name from the first `cwd` seen.
    projects: HashSet<Arc<str>>,
    project_names: HashMap<Arc<str>, Arc<str>>,
    /// A name was added since `LogUsageSource::names` was last updated.
    names_changed: bool,
    /// When the last successful listing of the projects folder happened.
    last_walk: Option<Instant>,
    /// The last listing failed, so the next failure doesn't warn again.
    listing_failed: bool,
}

impl Store {
    /// The interned project id for a transcript at `path`.
    fn project_of(&mut self, projects_dir: &Path, path: &Path) -> Arc<str> {
        let folder = path
            .strip_prefix(projects_dir)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = project_id(&folder);
        match self.projects.get(id) {
            Some(interned) => Arc::clone(interned),
            None => {
                let interned: Arc<str> = id.into();
                self.projects.insert(Arc::clone(&interned));
                interned
            }
        }
    }

    /// Every project with usage, most recently used first.
    fn project_list(&self) -> Vec<ProjectInfo> {
        let mut latest: HashMap<&Arc<str>, DateTime<Utc>> = HashMap::new();
        for entry in self.entries.iter() {
            latest
                .entry(&entry.project)
                .and_modify(|at| *at = (*at).max(entry.timestamp))
                .or_insert(entry.timestamp);
        }
        let mut list: Vec<(DateTime<Utc>, ProjectInfo)> = latest
            .into_iter()
            .filter(|(id, _)| !id.is_empty())
            .map(|(id, at)| {
                let name = self
                    .project_names
                    .get(id)
                    .map_or_else(|| project_name_from_id(id), |name| name.to_string());
                (
                    at,
                    ProjectInfo {
                        id: id.to_string(),
                        name,
                    },
                )
            })
            .collect();
        list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        list.into_iter().map(|(_, info)| info).collect()
    }

    fn add(&mut self, parsed: Parsed) {
        let entries = Arc::make_mut(&mut self.entries);
        match parsed.key {
            Some(key) => match self.by_key.get(&key) {
                Some(&i) => {
                    if parsed.entry.fuller_than(&entries[i]) {
                        entries[i] = parsed.entry;
                    }
                }
                None => {
                    self.by_key.insert(key, entries.len());
                    entries.push(parsed.entry);
                }
            },
            None => entries.push(parsed.entry),
        }
    }

    /// Reads `path`'s complete lines from `from` on, returning where the
    /// next read should start, or `None` if the file couldn't be opened (so
    /// the caller keeps its old state and tries again next scan). A last
    /// line without its newline is taken only if it already parses -
    /// otherwise it is still being written.
    fn read_from(&mut self, path: &Path, project: &Arc<str>, from: u64, len: u64) -> Option<u64> {
        let mut file = File::open(path).ok()?;
        file.seek(SeekFrom::Start(from)).ok()?;
        let mut reader = BufReader::new(file);
        let mut offset = from;
        let mut line = Vec::new();
        loop {
            line.clear();
            let Ok(read) = reader.read_until(b'\n', &mut line) else {
                break;
            };
            if read == 0 {
                break;
            }
            let complete = line.last() == Some(&b'\n');
            let parsed = std::str::from_utf8(&line)
                .ok()
                .and_then(|text| parse_line(text.trim_end(), &mut self.models));
            if !complete && parsed.is_none() && offset + (read as u64) >= len {
                // A partial last line: leave it for the next scan.
                break;
            }
            offset += read as u64;
            if let Some(mut parsed) = parsed {
                if !self.project_names.contains_key(project)
                    && let Some(name) = parsed.cwd.as_deref().and_then(project_name_from_cwd)
                {
                    self.project_names
                        .insert(Arc::clone(project), Arc::from(name));
                    self.names_changed = true;
                }
                parsed.entry.project = Arc::clone(project);
                self.add(parsed);
            }
        }
        Some(offset)
    }
}

/// Scans Claude Code's transcripts for billable assistant turns (see the
/// module docs for what counts). Each file is read once and then only
/// tailed, so repeated `entries()` calls from several tiles cost a
/// directory walk and nothing else while no transcript grows.
pub struct LogUsageSource {
    projects_dir: PathBuf,
    /// Calls closer together than this share the previous walk (and its
    /// entries) instead of listing the tree again.
    min_walk_interval: Duration,
    store: Arc<Mutex<Store>>,
    /// A copy of the store's project names, so a render can name its
    /// project without waiting for a scan that holds the store.
    names: Arc<Mutex<HashMap<Arc<str>, Arc<str>>>>,
    /// Set once a scan has completed.
    scanned: Arc<AtomicBool>,
}

/// `projects/<project>/<session>/subagents/<agent>.jsonl` is the deepest
/// known layout; the limit only guards against a symlink loop.
const MAX_DEPTH: usize = 4;

/// The default source's `min_walk_interval`: long enough to fold a burst of
/// handler calls into one walk, short enough that a tap never shows data
/// more than a couple of seconds old.
pub const DEFAULT_MIN_WALK_INTERVAL: Duration = Duration::from_secs(2);

/// What listing the projects folder found: every transcript with its size
/// and mtime, and the paths (directories or files) that exist but couldn't
/// be read, whose already-known transcripts are kept as they are.
#[derive(Default)]
struct Listing {
    files: Vec<(PathBuf, u64, SystemTime)>,
    unreadable: Vec<PathBuf>,
}

impl Listing {
    fn is_unreadable(&self, path: &Path) -> bool {
        self.unreadable.iter().any(|dir| path.starts_with(dir))
    }
}

/// An I/O error that means "exists but can't be read now" rather than "gone".
fn is_transient(e: &std::io::Error) -> bool {
    e.kind() != std::io::ErrorKind::NotFound
}

impl LogUsageSource {
    pub fn new(projects_dir: PathBuf) -> Self {
        Self {
            projects_dir,
            min_walk_interval: Duration::ZERO,
            store: Arc::new(Mutex::new(Store::default())),
            names: Arc::default(),
            scanned: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Calls less than `interval` after the last walk return its entries
    /// without listing the tree again. `new` uses zero (every call walks).
    pub fn with_min_walk_interval(mut self, interval: Duration) -> Self {
        self.min_walk_interval = interval;
        self
    }

    /// Whether a scan has completed, so `entries()` normally only tails
    /// the files (milliseconds) instead of reading every transcript from
    /// the start. (A shrunk or vanished transcript still makes a later scan
    /// start over.)
    pub fn scanned(&self) -> bool {
        self.scanned.load(Ordering::Acquire)
    }

    /// The entries already scanned, without scanning or waiting: empty
    /// before the first scan, and while a scan holds the store.
    pub fn peek(&self) -> Arc<Vec<LogEntry>> {
        match self.store.try_lock() {
            Ok(store) => Arc::clone(&store.entries),
            Err(_) => Arc::default(),
        }
    }

    /// Copies the store's project names to `names` when one was added.
    fn publish_names(store: &Mutex<Store>, names: &Mutex<HashMap<Arc<str>, Arc<str>>>) {
        // The store stays locked while `names` is written, so two scans
        // can't publish out of order (lock order: store, then names;
        // `project_name` only takes names).
        let mut store = Self::lock(store);
        if store.names_changed {
            store.names_changed = false;
            *names
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = store.project_names.clone();
        }
    }

    /// A project's name for a key's caption: the repository folder from
    /// its transcripts' `cwd`, or (none seen yet) its id without the
    /// encoded home directory.
    pub fn project_name(&self, id: &str) -> String {
        let names = self
            .names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        names
            .get(id)
            .map_or_else(|| project_name_from_id(id), |name| name.to_string())
    }

    /// Every project with usage, most recently used first, for the
    /// Property Inspector's scope list. Waits for a running scan (on the
    /// blocking pool), so call it off the event loop.
    pub async fn projects(&self) -> Vec<ProjectInfo> {
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || Self::lock(&store).project_list())
            .await
            .unwrap_or_default()
    }

    /// `~/.claude/projects` - where Claude Code writes its transcripts, one
    /// directory per project.
    pub fn default_path() -> PathBuf {
        crate::paths::home().join(".claude/projects")
    }

    /// Every parsed, deduplicated `LogEntry`. Never fails: a missing or
    /// unreadable directory, file or line is skipped, so callers always get
    /// *some* answer (possibly empty). The scan runs on the blocking pool,
    /// since it may read files.
    pub async fn entries(&self) -> Arc<Vec<LogEntry>> {
        let projects_dir = self.projects_dir.clone();
        let interval = self.min_walk_interval;
        let store = Arc::clone(&self.store);
        let names = Arc::clone(&self.names);
        let scanned = Arc::clone(&self.scanned);
        let scan = move || {
            let entries = Self::scan(&projects_dir, &store, interval);
            Self::publish_names(&store, &names);
            scanned.store(true, Ordering::Release);
            entries
        };
        match tokio::task::spawn_blocking(scan).await {
            Ok(entries) => entries,
            Err(e) => {
                log::error!("scanning Claude Code transcripts failed: {e}");
                Arc::default()
            }
        }
    }

    /// Every transcript under `projects_dir`, or an error if `projects_dir`
    /// itself exists but can't be read (a missing one is an empty listing:
    /// no projects yet, or really removed). A sub-directory or file that
    /// exists but can't be read is recorded in `unreadable`, not dropped.
    fn list_files(projects_dir: &Path) -> std::io::Result<Listing> {
        fn walk_entries(entries: std::fs::ReadDir, depth: usize, out: &mut Listing) {
            for entry in entries.flatten() {
                let path = entry.path();
                // Follows symlinks, like the directories it came from.
                let meta = match std::fs::metadata(&path) {
                    Ok(meta) => meta,
                    Err(e) => {
                        if is_transient(&e) {
                            out.unreadable.push(path);
                        }
                        continue;
                    }
                };
                if meta.is_dir() {
                    if depth < MAX_DEPTH {
                        match std::fs::read_dir(&path) {
                            Ok(entries) => walk_entries(entries, depth + 1, out),
                            Err(e) if is_transient(&e) => out.unreadable.push(path),
                            Err(_) => {}
                        }
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && let Ok(mtime) = meta.modified()
                {
                    out.files.push((path, meta.len(), mtime));
                }
            }
        }
        let mut listing = Listing::default();
        match std::fs::read_dir(projects_dir) {
            Ok(entries) => walk_entries(entries, 1, &mut listing),
            Err(e) if !is_transient(&e) => {}
            Err(e) => return Err(e),
        }
        Ok(listing)
    }

    /// The store, recovering from a panic in an earlier scan: everything in
    /// it is derived from the files, so a possibly half-updated store is
    /// simply dropped and rebuilt.
    fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
        match store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                log::warn!("transcript cache was poisoned by an earlier panic; rebuilding it");
                store.clear_poison();
                let mut guard = poisoned.into_inner();
                *guard = Store::default();
                guard
            }
        }
    }

    fn scan(
        projects_dir: &Path,
        store: &Mutex<Store>,
        min_walk_interval: Duration,
    ) -> Arc<Vec<LogEntry>> {
        {
            let store = Self::lock(store);
            if store
                .last_walk
                .is_some_and(|at| at.elapsed() < min_walk_interval)
            {
                return Arc::clone(&store.entries);
            }
        }
        let walk_started = Instant::now();
        let listing = Self::list_files(projects_dir);
        let mut store = Self::lock(store);
        if store.last_walk.is_some_and(|at| at > walk_started) {
            // A concurrent call started walking after this one and has
            // already applied its (newer) listing: this one is stale.
            return Arc::clone(&store.entries);
        }
        let listing = match listing {
            Ok(listing) => listing,
            Err(e) => {
                // Not "every transcript was deleted": keep what we have
                // and try again next call.
                if !store.listing_failed {
                    log::warn!(
                        "can't list Claude Code transcripts in {}: {e}; keeping the last results",
                        projects_dir.display()
                    );
                }
                store.listing_failed = true;
                return Arc::clone(&store.entries);
            }
        };
        let present: HashSet<&Path> = listing.files.iter().map(|(p, _, _)| p.as_path()).collect();
        // A known transcript under a path that couldn't be read is kept as
        // it is, not taken for deleted.
        let rewritten = store
            .files
            .keys()
            .any(|p| !present.contains(p.as_path()) && !listing.is_unreadable(p))
            || listing
                .files
                .iter()
                .any(|(path, len, _)| store.files.get(path).is_some_and(|seen| *len < seen.offset));
        if rewritten {
            // Dedup can't tell which entries only the vanished or shortened
            // file held, so start over (rare: Claude Code only deletes old
            // transcripts).
            let models = std::mem::take(&mut store.models);
            let projects = std::mem::take(&mut store.projects);
            let project_names = std::mem::take(&mut store.project_names);
            *store = Store {
                models,
                projects,
                project_names,
                ..Store::default()
            };
        }
        for (path, len, mtime) in listing.files {
            let seen = store.files.get(&path).copied();
            if seen.is_some_and(|s| s.len == len && s.mtime == mtime) {
                continue;
            }
            let from = seen.map_or(0, |s| s.offset);
            let offset = if len > from {
                let project = store.project_of(projects_dir, &path);
                match store.read_from(&path, &project, from, len) {
                    Some(offset) => offset,
                    // Couldn't open it: keep the old state, retry next scan.
                    None => continue,
                }
            } else {
                from
            };
            store.files.insert(path, FileState { len, mtime, offset });
        }
        store.listing_failed = false;
        store.last_walk = Some(walk_started);
        Arc::clone(&store.entries)
    }
}

impl Default for LogUsageSource {
    fn default() -> Self {
        Self::new(Self::default_path()).with_min_walk_interval(DEFAULT_MIN_WALK_INTERVAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    // Trimmed real samples from a ~/.claude/projects/*/*.jsonl transcript on
    // this machine, keeping only the fields this module reads.
    const ASSISTANT_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"claude-opus-5","usage":{"input_tokens":2,"output_tokens":346,"cache_creation_input_tokens":19929,"cache_read_input_tokens":29011}}}"#;
    const USER_LINE: &str = r#"{"type":"user","timestamp":"2026-09-13T12:59:00.000Z","message":{"role":"user","content":"hi"}}"#;
    const SYNTHETIC_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"<synthetic>","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
    const MISSING_CACHE_FIELDS_LINE: &str = r#"{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","message":{"model":"claude-sonnet-5","usage":{"input_tokens":100,"output_tokens":50}}}"#;

    /// One real message, written as Claude Code writes it: a line per
    /// content block, every line repeating the message id and request id
    /// (here the first line carries the usage, the others zeroed copies).
    const MULTIBLOCK: &str = include_str!("testdata/multiblock_message.jsonl");

    /// An assistant line for message `id`, with `output` output tokens.
    fn message_line(id: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-09-13T12:59:01.971Z","requestId":"req_{id}","message":{{"id":"msg_{id}","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":{output},"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#
        )
    }

    fn parse(line: &str) -> Option<Parsed> {
        parse_line(line, &mut HashSet::new())
    }

    fn project(root: &Path) -> PathBuf {
        let project = root.join("project-a");
        fs::create_dir_all(&project).unwrap();
        project
    }

    async fn entries_in(root: &Path) -> Arc<Vec<LogEntry>> {
        LogUsageSource::new(root.to_path_buf()).entries().await
    }

    #[tokio::test]
    async fn peek_and_scanned_reflect_only_a_completed_scan() {
        let root = tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("session.jsonl"), format!("{ASSISTANT_LINE}\n")).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert!(!source.scanned());
        assert!(source.peek().is_empty());

        let entries = source.entries().await;
        assert!(source.scanned());
        assert_eq!(entries.len(), 1);
        assert!(Arc::ptr_eq(&source.peek(), &entries));
    }

    #[test]
    fn total_tokens_sums_all_four_categories() {
        let entry = LogEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 9, 13, 12, 59, 1).unwrap(),
            model: "claude-opus-5".into(),
            input_tokens: 2,
            output_tokens: 346,
            cache_creation_input_tokens: 19929,
            cache_creation_1h_input_tokens: 0,
            cache_read_input_tokens: 29011,
            project: Default::default(),
        };
        assert_eq!(entry.total_tokens(), 49288);
    }

    #[test]
    fn parse_line_parses_a_well_formed_assistant_line() {
        let entry = parse(ASSISTANT_LINE).unwrap().entry;
        assert_eq!(&*entry.model, "claude-opus-5");
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
    fn parse_line_reads_the_one_hour_cache_writes() {
        let first = MULTIBLOCK.lines().next().unwrap();
        let entry = parse(first).unwrap().entry;
        assert_eq!(entry.cache_creation_input_tokens, 1050);
        assert_eq!(entry.cache_creation_1h_input_tokens, 1050);
    }

    #[test]
    fn parse_line_keys_a_line_by_message_and_request_id() {
        let mut lines = MULTIBLOCK.lines().map(|l| parse(l).unwrap().key);
        let first = lines.next().unwrap();
        assert!(first.is_some());
        assert!(lines.all(|k| k == first));
        assert_ne!(parse(&message_line("a", 1)).unwrap().key, first);
        assert_eq!(parse(ASSISTANT_LINE).unwrap().key, None);
    }

    #[test]
    fn parse_line_skips_non_assistant_lines() {
        assert!(parse(USER_LINE).is_none());
    }

    #[test]
    fn parse_line_skips_synthetic_model() {
        assert!(parse(SYNTHETIC_LINE).is_none());
    }

    #[test]
    fn parse_line_skips_malformed_json() {
        assert!(parse("not json at all").is_none());
        assert!(parse(r#"{"usage": "#).is_none());
    }

    #[test]
    fn parse_line_defaults_missing_cache_fields_to_zero() {
        let entry = parse(MISSING_CACHE_FIELDS_LINE).unwrap().entry;
        assert_eq!(entry.input_tokens, 100);
        assert_eq!(entry.output_tokens, 50);
        assert_eq!(entry.cache_creation_input_tokens, 0);
        assert_eq!(entry.cache_creation_1h_input_tokens, 0);
        assert_eq!(entry.cache_read_input_tokens, 0);
    }

    #[test]
    fn model_names_are_interned() {
        let mut models = HashSet::new();
        let a = parse_line(ASSISTANT_LINE, &mut models).unwrap().entry;
        let b = parse_line(ASSISTANT_LINE, &mut models).unwrap().entry;
        assert!(Arc::ptr_eq(&a.model, &b.model));
    }

    #[tokio::test]
    async fn malformed_lines_are_skipped_and_valid_ones_kept() {
        let root = tempdir().unwrap();
        fs::write(
            project(root.path()).join("session.jsonl"),
            format!("{ASSISTANT_LINE}\nnot json\n{SYNTHETIC_LINE}\n{USER_LINE}\n"),
        )
        .unwrap();
        let entries = entries_in(root.path()).await;
        assert_eq!(entries.len(), 1);
        assert_eq!(&*entries[0].model, "claude-opus-5");
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
        assert_eq!(entries_in(root.path()).await.len(), 2);
    }

    #[tokio::test]
    async fn a_message_split_over_several_lines_counts_once() {
        let root = tempdir().unwrap();
        fs::write(project(root.path()).join("session1.jsonl"), MULTIBLOCK).unwrap();
        let entries = entries_in(root.path()).await;
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].total_tokens(), 2 + 1050 + 965_063 + 197);
    }

    /// A streamed message's later copy has grown; it wins whichever order
    /// the copies come in.
    #[tokio::test]
    async fn the_fullest_copy_of_a_message_is_kept() {
        for (first, second) in [(5, 300), (300, 5)] {
            let root = tempdir().unwrap();
            fs::write(
                project(root.path()).join("s.jsonl"),
                format!(
                    "{}\n{}\n",
                    message_line("a", first),
                    message_line("a", second)
                ),
            )
            .unwrap();
            let entries = entries_in(root.path()).await;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].output_tokens, 300);
        }
    }

    #[tokio::test]
    async fn subagent_transcripts_are_counted() {
        let root = tempdir().unwrap();
        let subagents = root.path().join("project-a/session1/subagents");
        fs::create_dir_all(&subagents).unwrap();
        fs::write(subagents.join("agent-1.jsonl"), ASSISTANT_LINE).unwrap();
        assert_eq!(entries_in(root.path()).await.len(), 1);
    }

    #[tokio::test]
    async fn a_message_copied_into_another_transcript_counts_once() {
        let root = tempdir().unwrap();
        let project = project(root.path());
        fs::write(project.join("session1.jsonl"), MULTIBLOCK).unwrap();
        fs::write(project.join("session2.jsonl"), MULTIBLOCK).unwrap();
        assert_eq!(entries_in(root.path()).await.len(), 1);
    }

    #[tokio::test]
    async fn entries_returns_empty_when_projects_dir_is_missing() {
        let source = LogUsageSource::new(PathBuf::from("/nonexistent/claude/projects"));
        assert!(source.entries().await.is_empty());
    }

    #[tokio::test]
    async fn entries_picks_up_lines_appended_to_a_file() {
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        fs::write(&file_path, format!("{}\n", message_line("a", 1))).unwrap();

        let source = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(source.entries().await.len(), 1);

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .unwrap();
        writeln!(file, "{}", message_line("b", 1)).unwrap();
        assert_eq!(source.entries().await.len(), 2);
    }

    /// Appended lines are read from where the last read stopped, not from
    /// the start: rewriting the already-read part in place (same length)
    /// changes nothing.
    #[tokio::test]
    async fn only_the_appended_bytes_are_read() {
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        let first = format!("{}\n", message_line("a", 111));
        fs::write(&file_path, &first).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        source.entries().await;

        let same_length = format!("{}\n", message_line("z", 999));
        assert_eq!(same_length.len(), first.len());
        fs::write(
            &file_path,
            format!("{same_length}{}\n", message_line("b", 222)),
        )
        .unwrap();
        let outputs: Vec<u64> = source
            .entries()
            .await
            .iter()
            .map(|e| e.output_tokens)
            .collect();
        assert_eq!(outputs, vec![111, 222]);
    }

    /// Claude Code may be mid-write: a last line without its newline that
    /// doesn't parse yet is read once it's complete.
    #[tokio::test]
    async fn a_partly_written_last_line_is_read_once_complete() {
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        let line = message_line("a", 7);
        let (head, tail) = line.split_at(line.len() / 2);
        fs::write(&file_path, head).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert!(source.entries().await.is_empty());

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .unwrap();
        writeln!(file, "{tail}").unwrap();
        let entries = source.entries().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].output_tokens, 7);
    }

    #[tokio::test]
    async fn a_file_that_shrank_is_read_again_from_the_start() {
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        fs::write(
            &file_path,
            format!("{}\n{}\n", message_line("a", 1), message_line("b", 1)),
        )
        .unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(source.entries().await.len(), 2);

        fs::write(&file_path, format!("{}\n", message_line("c", 1))).unwrap();
        let entries = source.entries().await;
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn a_deleted_file_no_longer_counts() {
        let root = tempdir().unwrap();
        let project = project(root.path());
        fs::write(
            project.join("a.jsonl"),
            format!("{}\n", message_line("a", 1)),
        )
        .unwrap();
        fs::write(
            project.join("b.jsonl"),
            format!("{}\n", message_line("b", 1)),
        )
        .unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(source.entries().await.len(), 2);

        fs::remove_file(project.join("a.jsonl")).unwrap();
        assert_eq!(source.entries().await.len(), 1);
    }

    #[tokio::test]
    async fn unchanged_logs_share_one_list_of_entries() {
        let root = tempdir().unwrap();
        fs::write(project(root.path()).join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        let first = source.entries().await;
        let second = source.entries().await;
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn a_new_log_file_is_a_new_list() {
        let root = tempdir().unwrap();
        let project = project(root.path());
        fs::write(project.join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        let first = source.entries().await;
        fs::write(project.join("session2.jsonl"), ASSISTANT_LINE).unwrap();
        let second = source.entries().await;
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(first.len(), 1, "a reader's list never changes under it");
        assert_eq!(second.len(), 2);
    }

    /// A projects folder that can't be listed (here: mode 000) is not
    /// "every transcript was deleted": the entries stay, and once it can be
    /// listed again only the appended bytes are read.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_projects_folder_keeps_the_index() {
        if running_as_root() {
            eprintln!("skipped: running as root");
            return;
        }
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        let first = format!("{}\n", message_line("a", 111));
        fs::write(&file_path, &first).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(source.entries().await.len(), 1);

        set_mode(root.path(), 0o000);
        let during = source.entries().await;
        set_mode(root.path(), 0o755);
        assert_eq!(during.len(), 1, "a failed listing keeps the index");

        // Rewrite the read part in place (same length) and append: a full
        // re-read would see "z" instead of "a".
        let same_length = format!("{}\n", message_line("z", 999));
        assert_eq!(same_length.len(), first.len());
        fs::write(
            &file_path,
            format!("{same_length}{}\n", message_line("b", 222)),
        )
        .unwrap();
        let outputs: Vec<u64> = source
            .entries()
            .await
            .iter()
            .map(|e| e.output_tokens)
            .collect();
        assert_eq!(outputs, vec![111, 222], "only the tail is read");
    }

    /// Whether the test runs as root, where mode 000 stops nothing.
    #[cfg(unix)]
    fn running_as_root() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// One project folder that can't be read keeps its entries; the others
    /// are unaffected.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_project_folder_keeps_its_entries() {
        if running_as_root() {
            eprintln!("skipped: running as root");
            return;
        }
        let root = tempdir().unwrap();
        let left = root.path().join("left");
        let right = root.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        fs::write(
            left.join("s.jsonl"),
            format!("{}\n", message_line("l", 100)),
        )
        .unwrap();
        fs::write(
            right.join("s.jsonl"),
            format!("{}\n", message_line("r", 200)),
        )
        .unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(source.entries().await.len(), 2);

        set_mode(&right, 0o000);
        let during = source.entries().await;
        set_mode(&right, 0o755);
        assert_eq!(during.len(), 2, "the unreadable project still counts");
    }

    /// A transcript that can be listed but not opened is retried on the
    /// next scan rather than marked as read.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unopenable_transcript_is_read_once_it_opens() {
        if running_as_root() {
            eprintln!("skipped: running as root");
            return;
        }
        let root = tempdir().unwrap();
        let file_path = project(root.path()).join("session1.jsonl");
        fs::write(&file_path, format!("{}\n", message_line("a", 1))).unwrap();
        set_mode(&file_path, 0o000);
        let source = LogUsageSource::new(root.path().to_path_buf());
        let during = source.entries().await;
        set_mode(&file_path, 0o644);
        assert!(during.is_empty());
        assert_eq!(source.entries().await.len(), 1);
    }

    /// A listing taken before another call's newer walk was applied is
    /// dropped, not applied over it.
    #[tokio::test]
    async fn a_listing_older_than_the_applied_walk_is_dropped() {
        let root = tempdir().unwrap();
        let project = project(root.path());
        fs::write(project.join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        let first = source.entries().await;
        // As if a concurrent call started walking after this one will.
        LogUsageSource::lock(&source.store).last_walk =
            Some(Instant::now() + Duration::from_secs(3600));
        fs::write(project.join("session2.jsonl"), ASSISTANT_LINE).unwrap();
        assert!(Arc::ptr_eq(&source.entries().await, &first));
    }

    #[tokio::test]
    async fn a_missing_projects_folder_still_empties_the_index() {
        let root = tempdir().unwrap();
        let projects = root.path().join("projects");
        fs::create_dir(&projects).unwrap();
        fs::write(
            project(&projects).join("session1.jsonl"),
            format!("{}\n", message_line("a", 1)),
        )
        .unwrap();
        let source = LogUsageSource::new(projects.clone());
        assert_eq!(source.entries().await.len(), 1);

        fs::remove_dir_all(&projects).unwrap();
        assert!(source.entries().await.is_empty());
    }

    #[tokio::test]
    async fn calls_within_the_walk_interval_share_one_walk() {
        let root = tempdir().unwrap();
        let project = project(root.path());
        fs::write(project.join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        let shared = LogUsageSource::new(root.path().to_path_buf())
            .with_min_walk_interval(Duration::from_secs(3600));
        let first = shared.entries().await;
        assert_eq!(first.len(), 1);

        fs::write(project.join("session2.jsonl"), ASSISTANT_LINE).unwrap();
        let second = shared.entries().await;
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.len(), 1);

        let every_call = LogUsageSource::new(root.path().to_path_buf());
        assert_eq!(every_call.entries().await.len(), 2);
    }

    #[test]
    fn the_default_source_shares_walks_and_new_does_not() {
        assert_eq!(
            LogUsageSource::default().min_walk_interval,
            DEFAULT_MIN_WALK_INTERVAL
        );
        assert_eq!(
            LogUsageSource::new(PathBuf::new()).min_walk_interval,
            Duration::ZERO
        );
    }

    #[test]
    fn worktree_folders_fold_into_their_project() {
        for (folder, id) in [
            ("-home-u-git-repo", "-home-u-git-repo"),
            (
                "-home-u-git-repo--claude-worktrees-fix-x",
                "-home-u-git-repo",
            ),
            ("-home-u-git-repo--worktrees-feat-a-b", "-home-u-git-repo"),
            // Nested: the outermost repository wins.
            (
                "-home-u-git-repo--worktrees-a--claude-worktrees-b",
                "-home-u-git-repo",
            ),
            (
                "-home-u-git-my-worktrees-tool",
                "-home-u-git-my-worktrees-tool",
            ),
        ] {
            assert_eq!(project_id(folder), id, "{folder}");
        }
    }

    #[test]
    fn a_project_is_named_after_its_repository_folder() {
        for (cwd, name) in [
            ("/home/u/git/repo", Some("repo")),
            ("/home/u/git/repo/", Some("repo")),
            ("/home/u/git/repo/.claude/worktrees/fix-x", Some("repo")),
            (
                "/home/u/git/repo/.worktrees/a/.claude/worktrees/b",
                Some("repo"),
            ),
            ("/", None),
            ("", None),
        ] {
            assert_eq!(project_name_from_cwd(cwd), name, "{cwd:?}");
        }
    }

    /// A line for message `id` with a `cwd`.
    fn line_in(id: &str, cwd: &str, timestamp: &str) -> String {
        message_line(id, 1)
            .replacen(
                "\"requestId\"",
                &format!("\"cwd\":\"{cwd}\",\"requestId\""),
                1,
            )
            .replace("2026-09-13T12:59:01.971Z", timestamp)
    }

    #[tokio::test]
    async fn entries_carry_their_folded_project_and_projects_are_listed() {
        let root = tempdir().unwrap();
        let repo = root.path().join("-home-u-git-repo");
        let worktree = root.path().join("-home-u-git-repo--claude-worktrees-fix");
        let other = root.path().join("-home-u-git-other");
        let subagents = repo.join("session").join("subagents");
        for dir in [&subagents, &worktree, &other] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(
            repo.join("s.jsonl"),
            format!(
                "{}\n",
                line_in("a", "/home/u/git/repo", "2026-09-13T10:00:00Z")
            ),
        )
        .unwrap();
        fs::write(
            subagents.join("agent.jsonl"),
            format!("{}\n", message_line("b", 1)),
        )
        .unwrap();
        fs::write(
            worktree.join("s.jsonl"),
            format!(
                "{}\n",
                line_in(
                    "c",
                    "/home/u/git/repo/.claude/worktrees/fix",
                    "2026-09-13T11:00:00Z"
                )
            ),
        )
        .unwrap();
        fs::write(
            other.join("s.jsonl"),
            format!(
                "{}\n",
                line_in("d", "/home/u/git/other", "2026-09-14T09:00:00Z")
            ),
        )
        .unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        let entries = source.entries().await;
        let mut projects: Vec<&str> = entries.iter().map(|e| &*e.project).collect();
        projects.sort_unstable();
        assert_eq!(
            projects,
            [
                "-home-u-git-other",
                "-home-u-git-repo",
                "-home-u-git-repo",
                "-home-u-git-repo"
            ]
        );
        assert_eq!(
            source.projects().await,
            [
                ProjectInfo {
                    id: "-home-u-git-other".into(),
                    name: "other".into()
                },
                ProjectInfo {
                    id: "-home-u-git-repo".into(),
                    name: "repo".into()
                },
            ],
            "most recently used first, named from cwd"
        );
        assert_eq!(source.project_name("-home-u-git-repo"), "repo");
        assert_eq!(source.project_name("-nowhere"), "-nowhere");
    }

    /// Scans this machine's real transcripts and prints what it found - run
    /// by hand (`cargo test -- --ignored live_ --nocapture`) to sanity-check
    /// the dedup and pricing against real data, never in CI.
    #[tokio::test]
    #[ignore]
    async fn live_scan_of_this_machines_transcripts() {
        let started = std::time::Instant::now();
        let source = LogUsageSource::default();
        let entries = source.entries().await;
        let first = started.elapsed();
        let again = std::time::Instant::now();
        source.entries().await;
        let tokens: u64 = entries.iter().map(LogEntry::total_tokens).sum();
        let cost = crate::pricing::CostTotal::of(entries.iter());
        println!(
            "{} messages, {tokens} tokens, ${:.2} (partial: {}); first scan {first:?}, rescan {:?}",
            entries.len(),
            cost.dollars,
            cost.partial,
            again.elapsed()
        );
        assert!(!entries.is_empty());
    }

    /// A panic while the store was locked must not wedge every later scan.
    #[tokio::test]
    async fn a_poisoned_cache_is_rebuilt_rather_than_panicking_forever() {
        let root = tempdir().unwrap();
        fs::write(project(root.path()).join("session1.jsonl"), ASSISTANT_LINE).unwrap();
        let source = LogUsageSource::new(root.path().to_path_buf());
        let store = Arc::clone(&source.store);
        let _ = std::thread::spawn(move || {
            let _guard = store.lock().unwrap();
            panic!("simulated panic mid-scan");
        })
        .join();
        assert!(source.store.is_poisoned());
        assert_eq!(source.entries().await.len(), 1);
        assert!(!source.store.is_poisoned());
    }
}
