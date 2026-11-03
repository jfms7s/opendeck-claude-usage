//! Live Claude Code sessions, read from the registry Claude Code keeps in
//! `~/.claude/sessions/<pid>.json` (one file per running process). The
//! format is undocumented and grows fields between point releases, so
//! every field is optional, unknown fields are ignored, and a file that
//! doesn't parse is still counted (as "unknown") rather than blanking the
//! key. Only `*.json` files are ever opened: each sits next to a
//! `<pid>.<hex>.key` file holding secret material.
//!
//! A file outlives a process that crashed, so a session only counts while
//! its process is alive (see `SystemLiveness`).

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::Value;

use crate::level::Level;

/// Largest session file read; anything bigger is not a registry entry.
const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Waiting this long turns the key Watch colour...
pub const WATCH_AFTER: Duration = Duration::minutes(2);
/// ...and this long Critical.
pub const CRITICAL_AFTER: Duration = Duration::minutes(10);

/// `~/.claude/sessions`.
pub fn default_dir() -> PathBuf {
    crate::paths::home().join(".claude").join("sessions")
}

/// A session's `status`, as Claude Code writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Busy,
    Idle,
    /// A dialog (a permission prompt, a question) is blocking it.
    Waiting,
    /// Missing, unrecognised, or the file didn't parse.
    Unknown,
}

impl State {
    fn from_status(status: Option<&str>) -> State {
        match status {
            Some("busy") => State::Busy,
            Some("idle") => State::Idle,
            Some("waiting") => State::Waiting,
            _ => State::Unknown,
        }
    }

    /// Whether the session is waiting on the user (idle or blocked).
    pub fn waits_for_you(self) -> bool {
        matches!(self, State::Idle | State::Waiting)
    }

    /// The word the key shows.
    pub fn word(self) -> &'static str {
        match self {
            State::Busy => "busy",
            State::Idle => "idle",
            State::Waiting => "waiting",
            State::Unknown => "?",
        }
    }
}

/// One registry entry, as far as it could be read.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `sessionId`, or `file:<stem>` when the file has none.
    pub id: String,
    pub pid: Option<i64>,
    pub name: String,
    pub state: State,
    /// When the state was last set (`statusUpdatedAt`, else `updatedAt`,
    /// else `startedAt`).
    pub since: Option<DateTime<Utc>>,
    pub started: Option<DateTime<Utc>>,
    pub proc_start: Option<String>,
    pub pid_domain: Option<String>,
    /// A pre-started process nobody has opened yet.
    pub spare: bool,
}

impl Session {
    /// How long it has been in its state at `now` (0 for a future stamp).
    pub fn in_state(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.since.map(|t| (now - t).max(Duration::zero()))
    }

    /// The colour level of how long it has waited for you; `Normal` when
    /// it isn't waiting or the time is unknown.
    pub fn level(&self, now: DateTime<Utc>) -> Level {
        match self.in_state(now) {
            Some(d) if self.state.waits_for_you() => wait_level(d),
            _ => Level::Normal,
        }
    }
}

/// Watch from 2 minutes of waiting, Critical from 10.
pub fn wait_level(waited: Duration) -> Level {
    if waited >= CRITICAL_AFTER {
        Level::Critical
    } else if waited >= WATCH_AFTER {
        Level::Watch
    } else {
        Level::Normal
    }
}

fn epoch_ms(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    if let Some(ms) = value.as_i64() {
        return Utc.timestamp_millis_opt(ms).single();
    }
    if let Some(ms) = value.as_f64().filter(|f| f.is_finite()) {
        return Utc.timestamp_millis_opt(ms as i64).single();
    }
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

fn string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The basename of a path, ignoring a trailing slash.
fn basename(path: &str) -> Option<String> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parses one file's bytes. `stem` is the file name without `.json` (the
/// pid, by Claude Code's naming), used when the JSON lacks a field.
pub fn parse(stem: &str, bytes: &[u8]) -> Session {
    let stem_pid = stem.parse::<i64>().ok();
    let fallback_name = |pid: Option<i64>| match pid {
        Some(pid) => format!("pid {pid}"),
        None => "?".to_string(),
    };
    let unknown = || Session {
        id: format!("file:{stem}"),
        pid: stem_pid,
        name: fallback_name(stem_pid),
        state: State::Unknown,
        since: None,
        started: None,
        proc_start: None,
        pid_domain: None,
        spare: false,
    };
    let Ok(Value::Object(o)) = serde_json::from_slice::<Value>(bytes) else {
        return unknown();
    };
    let pid = o.get("pid").and_then(Value::as_i64).or(stem_pid);
    let name = string(&o, "name")
        .or_else(|| string(&o, "cwd").as_deref().and_then(basename))
        .unwrap_or_else(|| fallback_name(pid));
    Session {
        id: string(&o, "sessionId").unwrap_or_else(|| format!("file:{stem}")),
        pid,
        name,
        state: State::from_status(o.get("status").and_then(Value::as_str)),
        since: epoch_ms(o.get("statusUpdatedAt"))
            .or_else(|| epoch_ms(o.get("updatedAt")))
            .or_else(|| epoch_ms(o.get("startedAt"))),
        started: epoch_ms(o.get("startedAt")),
        proc_start: o.get("procStart").and_then(|v| match v {
            Value::String(s) => Some(s.trim().to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }),
        pid_domain: string(&o, "pidDomain"),
        spare: o.get("spare").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// Whether a session's process is still running.
pub trait Liveness {
    fn is_alive(&self, session: &Session) -> bool;
}

/// The real check: `kill(pid, 0)` finds the pid and, where
/// `/proc/<pid>/stat` exists (Linux) and the file has `procStart`, the
/// process started then (a reused pid is a different process). A file from another pid namespace
/// (e.g. OpenDeck in a Flatpak) can't be checked here and is trusted.
pub struct SystemLiveness;

/// The `pid:[N]` part of a `pidDomain` (`linux:<machine-id>:pid:[N]`).
fn namespace_of(domain: &str) -> Option<&str> {
    domain.find("pid:[").map(|i| &domain[i..])
}

/// This process's pid namespace, as `pid:[N]`.
fn own_namespace() -> Option<&'static str> {
    static OWN: OnceLock<Option<String>> = OnceLock::new();
    OWN.get_or_init(|| {
        std::fs::read_link("/proc/self/ns/pid")
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
    })
    .as_deref()
}

/// Field 22 (start time) of a `/proc/<pid>/stat` line. The command name
/// (field 2) is in parentheses and may hold spaces or `)`, so fields are
/// counted from the last `)`.
pub fn stat_start_time(stat: &str) -> Option<&str> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // Fields 3.. follow; field 22 is the 20th of them.
    rest.split_whitespace().nth(19)
}

/// A pid that can't be a session (`kill` treats 0 and negatives as process
/// groups, 1 is init).
fn plausible_pid(pid: i64) -> Option<i32> {
    i32::try_from(pid).ok().filter(|p| *p > 1)
}

impl Liveness for SystemLiveness {
    fn is_alive(&self, session: &Session) -> bool {
        let Some(pid) = session.pid.and_then(plausible_pid) else {
            return false;
        };
        if let (Some(theirs), Some(ours)) = (
            session.pid_domain.as_deref().and_then(namespace_of),
            own_namespace(),
        ) && theirs != ours
        {
            return true;
        }
        // The same POSIX check on every platform. SAFETY: signal 0 only
        // asks whether the pid exists, and `plausible_pid` rules out the
        // group/broadcast pids (0, -1, ...).
        let rc = unsafe { libc::kill(pid, 0) };
        if rc != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
            return false;
        }
        // Where /proc exists (Linux), a reused pid is told apart by its
        // start time. Elsewhere (macOS) it can't be, so an existing pid
        // counts as the session's.
        match (
            &session.proc_start,
            std::fs::read_to_string(format!("/proc/{pid}/stat")),
        ) {
            (Some(expected), Ok(stat)) => stat_start_time(&stat).is_none_or(|a| a == expected),
            _ => true,
        }
    }
}

/// Reads one session file: a regular file only (a symlink fails to open,
/// a FIFO doesn't block), at most `MAX_FILE_BYTES`.
pub fn read_session_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "too large",
        ));
    }
    Ok(bytes)
}

/// Whether a read error means "not a session file" (gone since the
/// listing, a symlink, not a regular file) rather than an unreadable one.
fn skip_error(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
        || e.kind() == std::io::ErrorKind::InvalidInput
        || matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENXIO))
}

/// The live sessions in `dir`, oldest first (then by pid). Opens only
/// `*.json` entries, through `read`; an unreadable or garbled file is an
/// unknown session; dead and spare ones are dropped. A missing `dir` is
/// `NotFound`, which callers show as no sessions.
pub fn scan(
    dir: &Path,
    read: impl Fn(&Path) -> std::io::Result<Vec<u8>>,
    alive: &impl Liveness,
) -> std::io::Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let session = match read(&path) {
            Ok(bytes) => parse(stem, &bytes),
            Err(e) if skip_error(&e) => continue,
            Err(_) => parse(stem, b""),
        };
        if !session.spare && alive.is_alive(&session) {
            sessions.push(session);
        }
    }
    sessions.sort_by(|a, b| {
        (a.started.is_none(), a.started, a.pid, &a.id).cmp(&(
            b.started.is_none(),
            b.started,
            b.pid,
            &b.id,
        ))
    });
    Ok(sessions)
}

/// What the summary key shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub waiting: usize,
    pub idle: usize,
    pub busy: usize,
    pub unknown: usize,
    /// The longest wait for you, if any session is waiting with a time.
    pub longest_wait: Option<Duration>,
}

pub fn summarize(sessions: &[Session], now: DateTime<Utc>) -> Summary {
    let of = |state: State| sessions.iter().filter(|s| s.state == state).count();
    Summary {
        count: sessions.len(),
        waiting: of(State::Waiting),
        idle: of(State::Idle),
        busy: of(State::Busy),
        unknown: of(State::Unknown),
        longest_wait: sessions
            .iter()
            .filter(|s| s.state.waits_for_you())
            .filter_map(|s| s.in_state(now))
            .max(),
    }
}

impl Summary {
    pub fn level(&self) -> Level {
        self.longest_wait.map_or(Level::Normal, wait_level)
    }

    /// `1 wait · 1 idle · 1 busy`, only the non-zero parts.
    pub fn breakdown(&self) -> String {
        [
            (self.waiting, "wait"),
            (self.idle, "idle"),
            (self.busy, "busy"),
            (self.unknown, "?"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

/// What a Sessions key details: all sessions, or one by id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Summary,
    Session(String),
}

/// What a short press moves to: summary → each session in order →
/// summary. A focused session that is gone goes back to the summary.
pub fn next_focus(focus: &Focus, sessions: &[Session]) -> Focus {
    let next = match focus {
        Focus::Summary => sessions.first(),
        Focus::Session(id) => sessions
            .iter()
            .position(|s| &s.id == id)
            .and_then(|i| sessions.get(i + 1)),
    };
    next.map_or(Focus::Summary, |s| Focus::Session(s.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/testdata/sessions");

    /// The fixtures' "now": 1001 idle 12m, 1002 idle 3m, 1003 busy 1m.
    fn now() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_793_700_000_000).unwrap()
    }

    /// Every pid alive except `dead`.
    struct AliveExcept(Vec<i64>);

    impl Liveness for AliveExcept {
        fn is_alive(&self, s: &Session) -> bool {
            s.pid.is_some_and(|p| !self.0.contains(&p))
        }
    }

    fn all_alive() -> AliveExcept {
        AliveExcept(vec![])
    }

    /// A reader that fails the test if it is asked for anything but a
    /// `.json` file (the `.key` fixture sits in the same folder).
    fn json_only(path: &Path) -> std::io::Result<Vec<u8>> {
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("json"),
            "opened {path:?}"
        );
        read_session_file(path)
    }

    #[test]
    fn three_sessions_count_three_and_show_the_longest_wait() {
        let sessions = scan(Path::new(FIXTURES), json_only, &all_alive()).unwrap();
        let summary = summarize(&sessions, now());
        assert_eq!(summary.count, 3);
        assert_eq!((summary.idle, summary.busy), (2, 1));
        assert_eq!(summary.longest_wait, Some(Duration::minutes(12)));
        assert_eq!(summary.level(), Level::Critical);
        assert_eq!(summary.breakdown(), "2 idle · 1 busy");
        let names: Vec<_> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "gamma"], "oldest first");
    }

    /// The reader never opens the `.key` file next to the session files.
    #[test]
    fn only_json_files_are_opened() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["1001.json", "1002.json", "1003.json"] {
            std::fs::copy(Path::new(FIXTURES).join(f), dir.path().join(f)).unwrap();
        }
        // A decoy next to them, as Claude Code keeps a `<pid>.<hex>.key`.
        std::fs::write(dir.path().join("1001.0123abcd.key"), b"decoy").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"decoy").unwrap();
        let opened = RefCell::new(BTreeSet::new());
        scan(
            dir.path(),
            |p| {
                opened
                    .borrow_mut()
                    .insert(p.file_name().unwrap().to_owned());
                json_only(p)
            },
            &all_alive(),
        )
        .unwrap();
        let opened: Vec<_> = opened.into_inner().into_iter().collect();
        assert_eq!(opened, ["1001.json", "1002.json", "1003.json"]);
    }

    #[test]
    fn a_dead_pid_is_dropped() {
        let sessions = scan(Path::new(FIXTURES), json_only, &AliveExcept(vec![1001])).unwrap();
        let summary = summarize(&sessions, now());
        assert_eq!(summary.count, 2);
        assert_eq!(summary.longest_wait, Some(Duration::minutes(3)));
        assert_eq!(summary.level(), Level::Watch);
    }

    #[test]
    fn extra_fields_parse() {
        let bytes = std::fs::read(Path::new(FIXTURES).join("1002.json")).unwrap();
        let s = parse("1002", &bytes);
        assert_eq!(s.state, State::Idle);
        assert_eq!(s.pid, Some(1002));
        assert_eq!(s.pid_domain.as_deref(), Some("linux:0000:pid:[1]"));
        assert_eq!(s.name, "beta", "cwd basename when there is no name");
    }

    #[test]
    fn missing_fields_fall_back() {
        let s = parse("4242", b"{}");
        assert_eq!(s.pid, Some(4242), "pid from the file name");
        assert_eq!(s.id, "file:4242");
        assert_eq!(s.name, "pid 4242");
        assert_eq!(s.state, State::Unknown);
        assert_eq!(s.since, None);
        let s = parse("1", br#"{"status":"waiting","updatedAt":1000,"pid":"7"}"#);
        assert_eq!(s.state, State::Waiting);
        assert_eq!(s.pid, Some(1), "a non-number pid falls back to the stem");
        assert_eq!(s.since, Utc.timestamp_millis_opt(1000).single());
    }

    #[test]
    fn wrong_types_are_ignored() {
        let s = parse(
            "9",
            br#"{"status":3,"name":[],"cwd":{},"statusUpdatedAt":"soon","procStart":12345,"spare":"yes"}"#,
        );
        assert_eq!(s.state, State::Unknown);
        assert_eq!(s.name, "pid 9");
        assert_eq!(s.since, None);
        assert_eq!(s.proc_start.as_deref(), Some("12345"));
        assert!(!s.spare);
    }

    /// A garbage file, an unreadable one and an unknown status each count
    /// as one unknown session, next to the readable ones.
    #[test]
    fn a_garbage_file_counts_as_unknown() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["1001.json", "1003.json"] {
            std::fs::copy(Path::new(FIXTURES).join(f), dir.path().join(f)).unwrap();
        }
        std::fs::write(dir.path().join("2001.json"), b"\x00{not json").unwrap();
        std::fs::write(dir.path().join("2002.json"), br#"{"status":"pondering"}"#).unwrap();
        std::fs::write(dir.path().join("2003.json"), vec![b' '; 70 * 1024]).unwrap();
        let sessions = scan(dir.path(), json_only, &all_alive()).unwrap();
        let summary = summarize(&sessions, now());
        assert_eq!(summary.count, 5);
        assert_eq!(summary.unknown, 3);
        assert_eq!(summary.breakdown(), "1 idle · 1 busy · 3 ?");
        assert_eq!(summary.longest_wait, Some(Duration::minutes(12)));
    }

    #[test]
    fn spare_sessions_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("3001.json"),
            br#"{"pid":3001,"status":"idle","spare":true}"#,
        )
        .unwrap();
        assert!(
            scan(dir.path(), json_only, &all_alive())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_missing_folder_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan(&dir.path().join("sessions"), json_only, &all_alive()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// A `.json` symlink (say, to a `.key` file) is skipped, not followed.
    #[test]
    fn a_symlinked_json_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("5001.abc.key");
        std::fs::write(&target, br#"{"pid":5001,"status":"idle"}"#).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("5001.json")).unwrap();
        std::fs::create_dir(dir.path().join("5002.json")).unwrap();
        assert!(
            scan(dir.path(), read_session_file, &all_alive())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn waiting_and_idle_wait_for_you_and_busy_does_not() {
        let mut s = parse("1", br#"{"status":"busy","statusUpdatedAt":0}"#);
        assert_eq!(s.level(now()), Level::Normal);
        s.state = State::Waiting;
        assert_eq!(s.level(now()), Level::Critical);
        s.since = Some(now() - Duration::seconds(119));
        assert_eq!(s.level(now()), Level::Normal);
        s.since = Some(now() - Duration::minutes(2));
        assert_eq!(s.level(now()), Level::Watch);
        s.since = Some(now() + Duration::minutes(5));
        assert_eq!(s.in_state(now()), Some(Duration::zero()), "clock skew");
    }

    #[test]
    fn no_sessions_summarize_to_nothing() {
        let summary = summarize(&[], now());
        assert_eq!(summary.count, 0);
        assert_eq!(summary.longest_wait, None);
        assert_eq!(summary.level(), Level::Normal);
        assert_eq!(summary.breakdown(), "");
    }

    #[test]
    fn focus_cycles_through_each_session_and_back() {
        let sessions = scan(Path::new(FIXTURES), json_only, &all_alive()).unwrap();
        let mut focus = Focus::Summary;
        let mut seen = vec![];
        for _ in 0..4 {
            focus = next_focus(&focus, &sessions);
            seen.push(focus.clone());
        }
        let id = |n: u8| Focus::Session(format!("aaaaaaaa-0000-0000-0000-00000000000{n}"));
        assert_eq!(seen, [id(1), id(2), id(3), Focus::Summary]);
        assert_eq!(
            next_focus(&Focus::Session("gone".into()), &sessions),
            Focus::Summary
        );
        assert_eq!(next_focus(&Focus::Summary, &[]), Focus::Summary);
    }

    #[test]
    fn stat_start_time_skips_a_command_name_with_spaces() {
        let stat = "4242 (a b) c) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 777 19 20";
        assert_eq!(stat_start_time(stat), Some("777"));
        assert_eq!(stat_start_time("garbage"), None);
    }

    #[test]
    fn implausible_pids_are_dead() {
        for pid in [-1, 0, 1, i64::from(i32::MAX) + 1] {
            let mut s = parse("x", b"{}");
            s.pid = Some(pid);
            assert!(!SystemLiveness.is_alive(&s), "pid {pid}");
        }
        assert!(!SystemLiveness.is_alive(&parse("x", b"{}")), "no pid");
    }

    /// This test's own process is alive; with the wrong start time it is
    /// a reused pid, i.e. dead.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_check_compares_the_start_time() {
        let pid = i64::from(std::process::id());
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat_start_time(&stat).unwrap().to_string();
        let own = own_namespace().map(|ns| format!("linux:x:{ns}"));
        let mut s = parse("x", b"{}");
        s.pid = Some(pid);
        s.pid_domain = own.clone();
        assert!(SystemLiveness.is_alive(&s), "no procStart");
        s.proc_start = Some(start);
        assert!(SystemLiveness.is_alive(&s));
        s.proc_start = Some("1".into());
        assert!(!SystemLiveness.is_alive(&s), "reused pid");
        let mut gone = parse("x", b"{}");
        gone.pid = Some(2_000_000_000);
        assert!(!SystemLiveness.is_alive(&gone), "no such pid");
        // From another pid namespace: can't be checked, trusted.
        s.pid_domain = Some("linux:x:pid:[1]".into());
        assert!(own.is_none() || SystemLiveness.is_alive(&s));
    }
}
