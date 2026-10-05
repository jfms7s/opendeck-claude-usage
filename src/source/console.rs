//! Billed spend for a Claude Console organization, from the Usage & Cost
//! Admin API (`/v1/organizations/cost_report` and
//! `/v1/organizations/usage_report/messages`, daily UTC buckets). Needs an
//! Admin key (`sk-ant-admin01-…`) read from a private file: regular API
//! keys can't read usage, and individual accounts can't create Admin keys.

use super::cached::{CachedSource, Fetch};
use async_trait::async_trait;
use chrono::SecondsFormat;
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// One UTC day of org-wide usage.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleDay {
    pub date: NaiveDate,
    pub cost_dollars: f64,
    pub tokens: u64,
}

/// Every UTC day in the fetch window (see `window`), oldest first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConsoleSnapshot {
    pub days: Vec<ConsoleDay>,
}

/// Errors carry only a message or status, never the key or a response
/// body, and are `Clone` so the cache can hand one to every caller.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ConsoleError {
    #[error("no Admin key file")]
    NoKey,
    #[error("Admin key file is readable by other users")]
    InsecureKeyFile,
    #[error("Admin API refused the key (HTTP {0})")]
    Unauthorized(u16),
    #[error("Admin API request failed: {0}")]
    Request(String),
    #[error("could not parse Admin API response: {0}")]
    Parse(String),
}

/// The Admin key. `Debug` never prints it, so it can't reach a log line.
#[derive(Clone, PartialEq)]
pub struct AdminKey(String);

impl fmt::Debug for AdminKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdminKey([redacted])")
    }
}

impl AdminKey {
    /// The key itself - only for the `x-api-key` header.
    fn expose(&self) -> &str {
        &self.0
    }
}

/// `~/.config/opendeck-claude-usage/admin-key`.
pub fn default_key_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    PathBuf::from(home).join(".config/opendeck-claude-usage/admin-key")
}

/// Reads the key, refusing a file that group or others could read - the
/// key can read the whole org's billing. Any failure to stat or read
/// (missing file, unreadable folder, a directory) is `NoKey`: nothing is
/// usable either way, and the cache re-checks it on the next read.
pub async fn read_admin_key(path: &Path) -> Result<AdminKey, ConsoleError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| ConsoleError::NoKey)?;
    if !meta.is_file() {
        return Err(ConsoleError::NoKey);
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(ConsoleError::InsecureKeyFile);
    }
    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| ConsoleError::NoKey)?;
    let key = contents.trim();
    if key.is_empty() {
        return Err(ConsoleError::NoKey);
    }
    Ok(AdminKey(key.to_string()))
}

/// One page of a report: a value per bucket day, plus the cursor for the
/// next page (`None` once `has_more` is false).
#[derive(Debug, PartialEq)]
pub struct Page<V> {
    pub days: Vec<(NaiveDate, V)>,
    pub next_page: Option<String>,
}

#[derive(Deserialize)]
struct RawPage<R> {
    data: Vec<RawBucket<R>>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_page: Option<String>,
}

#[derive(Deserialize)]
struct RawBucket<R> {
    starting_at: String,
    // `Vec::new` rather than plain `default`, which would make serde
    // require `R: Default`.
    #[serde(default = "Vec::new")]
    results: Vec<R>,
}

#[derive(Deserialize)]
struct RawCost {
    amount: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
    uncached_input_tokens: u64,
    cache_creation: RawCacheCreation,
    cache_read_input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

impl RawUsage {
    /// Every token kind, the same "total tokens" Metric Tile shows.
    fn total(&self) -> u64 {
        self.uncached_input_tokens
            + self.cache_creation.ephemeral_5m_input_tokens
            + self.cache_creation.ephemeral_1h_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }
}

fn parse_error(e: impl fmt::Display) -> ConsoleError {
    ConsoleError::Parse(e.to_string())
}

fn parse_page<R: DeserializeOwned, V>(
    json: &str,
    value: impl Fn(&[R]) -> Result<V, ConsoleError>,
) -> Result<Page<V>, ConsoleError> {
    let raw: RawPage<R> = serde_json::from_str(json).map_err(parse_error)?;
    let mut days = Vec::with_capacity(raw.data.len());
    for bucket in &raw.data {
        let date = DateTime::parse_from_rfc3339(&bucket.starting_at)
            .map_err(|e| parse_error(format!("bucket start {:?}: {e}", bucket.starting_at)))?
            .with_timezone(&Utc)
            .date_naive();
        days.push((date, value(&bucket.results)?));
    }
    Ok(Page {
        days,
        next_page: if raw.has_more { raw.next_page } else { None },
    })
}

/// `amount` is a decimal string in cents: "123.45" is $1.2345.
fn dollars(amount: &str) -> Result<f64, ConsoleError> {
    let cents: f64 = amount
        .trim()
        .parse()
        .map_err(|e| parse_error(format!("amount {amount:?}: {e}")))?;
    if !cents.is_finite() {
        return Err(parse_error(format!("amount {amount:?} is not finite")));
    }
    Ok(cents / 100.0)
}

/// A cost-report page as dollars per day.
pub fn parse_cost_page(json: &str) -> Result<Page<f64>, ConsoleError> {
    parse_page(json, |results: &[RawCost]| {
        results.iter().map(|r| dollars(&r.amount)).sum()
    })
}

/// A usage-report page as total tokens per day.
pub fn parse_usage_page(json: &str) -> Result<Page<u64>, ConsoleError> {
    parse_page(json, |results: &[RawUsage]| {
        Ok(results.iter().map(RawUsage::total).sum())
    })
}

/// Both reports' days in one snapshot. A day only one report has counts
/// zero for the other; repeated days (one per page boundary) are summed.
pub fn merge_days(cost: Vec<(NaiveDate, f64)>, tokens: Vec<(NaiveDate, u64)>) -> ConsoleSnapshot {
    let empty = |date| ConsoleDay {
        date,
        cost_dollars: 0.0,
        tokens: 0,
    };
    let mut by_day: BTreeMap<NaiveDate, ConsoleDay> = BTreeMap::new();
    for (date, dollars) in cost {
        by_day
            .entry(date)
            .or_insert_with(|| empty(date))
            .cost_dollars += dollars;
    }
    for (date, n) in tokens {
        by_day.entry(date).or_insert_with(|| empty(date)).tokens += n;
    }
    ConsoleSnapshot {
        days: by_day.into_values().collect(),
    }
}

fn midnight(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .expect("midnight exists")
        .and_utc()
}

/// The fetch window: from the earlier of the 1st of this UTC month and six
/// days ago (so "7 days" stays whole early in a month), to the start of
/// tomorrow - the API returns buckets that *end* before `ending_at`, so
/// ending at `now` would drop today's.
pub fn window(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.date_naive();
    let month_start = today.with_day(1).expect("day 1 exists");
    let start = month_start.min(today - Days::new(6));
    (midnight(start), midnight(today + Days::new(1)))
}

const BASE_URL: &str = "https://api.anthropic.com";
const COST_PATH: &str = "/v1/organizations/cost_report";
const USAGE_PATH: &str = "/v1/organizations/usage_report/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The window is at most 37 daily buckets - two pages of 31. More pages
/// than this means the API isn't paging the way we expect.
const MAX_PAGES: usize = 4;

/// Fetches month-to-date (plus the last 7 days) of org-wide cost and
/// tokens on every `fetch`. Unthrottled on its own - wrap it in
/// `CachedSource`.
pub struct ConsoleSource {
    key_path: PathBuf,
    client: reqwest::Client,
    /// The key file as it was at the last fetch, so a replaced key can be
    /// retried at once (see `Fetch::input_changed`).
    fetched_with: std::sync::Mutex<KeyStamp>,
}

/// What identifies one version of the key file without reading it:
/// modified time, size and mode. `None` when there's no file.
type KeyStamp = Option<(std::time::SystemTime, u64, u32)>;

async fn key_stamp(path: &Path) -> KeyStamp {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some((meta.modified().ok()?, meta.len(), meta.permissions().mode()))
}

impl ConsoleSource {
    pub fn new(key_path: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("opendeck-claude-usage/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with static config");
        Self {
            key_path,
            client,
            fetched_with: std::sync::Mutex::new(None),
        }
    }

    async fn get(&self, key: &AdminKey, url: reqwest::Url) -> Result<String, ConsoleError> {
        let response = self
            .client
            .get(url)
            .header("x-api-key", key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .send()
            .await
            .map_err(|e| ConsoleError::Request(e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(error_for_status(status.as_u16()));
        }
        response
            .text()
            .await
            .map_err(|e| ConsoleError::Request(e.without_url().to_string()))
    }

    /// Every page of one report, as per-day values.
    async fn report<V>(
        &self,
        key: &AdminKey,
        path: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        parse: fn(&str) -> Result<Page<V>, ConsoleError>,
    ) -> Result<Vec<(NaiveDate, V)>, ConsoleError> {
        let mut days = Vec::new();
        let mut page: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let body = self
                .get(key, report_url(path, start, end, page.as_deref()))
                .await?;
            let parsed = parse(&body)?;
            days.extend(parsed.days);
            match parsed.next_page {
                Some(next) => page = Some(next),
                None => return Ok(days),
            }
        }
        Err(ConsoleError::Parse(format!("more than {MAX_PAGES} pages")))
    }
}

impl Default for ConsoleSource {
    fn default() -> Self {
        Self::new(default_key_path())
    }
}

/// One report request: daily buckets over `[start, end)`, optionally from a
/// `next_page` cursor.
fn report_url(
    path: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    page: Option<&str>,
) -> reqwest::Url {
    let start = start.to_rfc3339_opts(SecondsFormat::Secs, true);
    let end = end.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut params = vec![
        ("starting_at", start.as_str()),
        ("ending_at", end.as_str()),
        ("bucket_width", "1d"),
        ("limit", "31"),
    ];
    if let Some(page) = page {
        params.push(("page", page));
    }
    reqwest::Url::parse_with_params(&format!("{BASE_URL}{path}"), &params)
        .expect("BASE_URL is a valid URL")
}

/// 401/403 mean the key isn't an Admin key (or the account has no
/// organization); anything else is a plain request failure to back off on.
fn error_for_status(status: u16) -> ConsoleError {
    match status {
        401 | 403 => ConsoleError::Unauthorized(status),
        _ => ConsoleError::Request(format!("HTTP {status}")),
    }
}

#[async_trait]
impl Fetch for ConsoleSource {
    type Snapshot = ConsoleSnapshot;
    type Error = ConsoleError;
    const LABEL: &'static str = "console spend";

    async fn fetch(&self) -> Result<ConsoleSnapshot, ConsoleError> {
        let stamp = key_stamp(&self.key_path).await;
        *self.fetched_with.lock().expect("key stamp lock") = stamp;
        let key = read_admin_key(&self.key_path).await?;
        let (start, end) = window(Utc::now());
        let cost = self
            .report(&key, COST_PATH, start, end, parse_cost_page)
            .await?;
        let tokens = self
            .report(&key, USAGE_PATH, start, end, parse_usage_page)
            .await?;
        Ok(merge_days(cost, tokens))
    }

    /// Key-file problems are a local stat, not a request - re-check them on
    /// every read so a newly added key shows up within a tick.
    fn is_local(error: &ConsoleError) -> bool {
        matches!(error, ConsoleError::NoKey | ConsoleError::InsecureKeyFile)
    }

    /// A replaced key file (after NOT ADMIN, say) is retried at once
    /// instead of after a backoff of up to 30 minutes. One `stat` per read.
    async fn input_changed(&self) -> bool {
        let now = key_stamp(&self.key_path).await;
        now != *self.fetched_with.lock().expect("key stamp lock")
    }
}

/// What API Spend and Console-sourced Metric Tiles read: the throttled
/// snapshot. A trait so their tests can hand in fixed data.
#[async_trait]
pub trait ConsoleData: Send + Sync {
    async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError>;
}

#[async_trait]
impl<S> ConsoleData for CachedSource<S>
where
    S: Fetch<Snapshot = ConsoleSnapshot, Error = ConsoleError>,
{
    async fn snapshot(&self) -> Result<ConsoleSnapshot, ConsoleError> {
        self.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::os::unix::fs::PermissionsExt;

    // Trimmed from the Admin API reference's example responses.
    const COST_PAGE: &str = r#"{
        "data": [{"starting_at": "2025-08-01T00:00:00Z", "ending_at": "2025-08-02T00:00:00Z",
                  "results": [{"amount": "123.78912", "currency": "USD", "cost_type": "tokens"}]}],
        "has_more": true, "next_page": "page_MjAyNS0wNS0xNFQwMDowMDowMFo="
    }"#;

    const USAGE_PAGE: &str = r#"{
        "data": [{"starting_at": "2025-08-01T00:00:00Z", "ending_at": "2025-08-02T00:00:00Z",
                  "results": [{"cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
                               "cache_read_input_tokens": 200, "output_tokens": 500,
                               "server_tool_use": {"web_search_requests": 10},
                               "uncached_input_tokens": 1500, "model": "claude-opus-5"}]}],
        "has_more": false, "next_page": null
    }"#;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn cost_page_amounts_are_cents() {
        let page = parse_cost_page(COST_PAGE).unwrap();
        assert_eq!(page.days.len(), 1);
        assert_eq!(page.days[0].0, date(2025, 8, 1));
        assert!((page.days[0].1 - 1.2378912).abs() < 1e-9);
        assert_eq!(
            page.next_page.as_deref(),
            Some("page_MjAyNS0wNS0xNFQwMDowMDowMFo=")
        );
    }

    #[test]
    fn cost_results_in_a_bucket_are_summed_and_an_empty_bucket_is_zero() {
        let json = r#"{"data": [
            {"starting_at": "2026-10-01T00:00:00Z", "results": [{"amount": "100"}, {"amount": "250"}]},
            {"starting_at": "2026-10-02T00:00:00Z", "results": []}
        ], "has_more": false}"#;
        let page = parse_cost_page(json).unwrap();
        assert_eq!(
            page.days,
            vec![(date(2026, 10, 1), 3.5), (date(2026, 10, 2), 0.0)]
        );
        assert_eq!(page.next_page, None);
    }

    #[test]
    fn a_next_page_without_has_more_is_ignored() {
        let json = r#"{"data": [], "has_more": false, "next_page": "page_x"}"#;
        assert_eq!(parse_cost_page(json).unwrap().next_page, None);
    }

    #[test]
    fn a_bad_amount_is_a_parse_error() {
        for amount in ["12abc", "NaN", "inf"] {
            let json = format!(
                r#"{{"data": [{{"starting_at": "2026-10-01T00:00:00Z", "results": [{{"amount": "{amount}"}}]}}]}}"#
            );
            assert!(
                matches!(parse_cost_page(&json), Err(ConsoleError::Parse(_))),
                "{amount}"
            );
        }
    }

    #[test]
    fn a_bad_bucket_start_is_a_parse_error() {
        let json = r#"{"data": [{"starting_at": "yesterday", "results": []}]}"#;
        assert!(matches!(
            parse_usage_page(json),
            Err(ConsoleError::Parse(_))
        ));
    }

    #[test]
    fn a_body_that_is_not_a_report_is_a_parse_error() {
        assert!(matches!(
            parse_cost_page(r#"{"error": "nope"}"#),
            Err(ConsoleError::Parse(_))
        ));
    }

    #[test]
    fn usage_page_sums_every_token_kind() {
        let page = parse_usage_page(USAGE_PAGE).unwrap();
        assert_eq!(page.days, vec![(date(2025, 8, 1), 2200)]);
        assert_eq!(page.next_page, None);

        let json = r#"{"data": [{"starting_at": "2026-10-01T00:00:00Z", "results": [
            {"uncached_input_tokens": 1, "cache_creation": {"ephemeral_5m_input_tokens": 10, "ephemeral_1h_input_tokens": 100},
             "cache_read_input_tokens": 1000, "output_tokens": 10000},
            {"output_tokens": 5}
        ]}]}"#;
        assert_eq!(
            parse_usage_page(json).unwrap().days,
            vec![(date(2026, 10, 1), 11116)]
        );
    }

    #[test]
    fn merge_days_unions_sums_and_sorts_dates() {
        let snapshot = merge_days(
            vec![
                (date(2026, 10, 2), 1.5),
                (date(2026, 10, 1), 2.0),
                (date(2026, 10, 2), 0.5),
            ],
            vec![(date(2026, 10, 3), 40), (date(2026, 10, 1), 10)],
        );
        assert_eq!(
            snapshot.days,
            vec![
                ConsoleDay {
                    date: date(2026, 10, 1),
                    cost_dollars: 2.0,
                    tokens: 10
                },
                ConsoleDay {
                    date: date(2026, 10, 2),
                    cost_dollars: 2.0,
                    tokens: 0
                },
                ConsoleDay {
                    date: date(2026, 10, 3),
                    cost_dollars: 0.0,
                    tokens: 40
                },
            ]
        );
    }

    #[test]
    fn window_mid_month_starts_on_the_first_and_ends_at_tomorrow() {
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 13, 0, 0).unwrap();
        let (start, end) = window(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
        // The API returns buckets ending *before* ending_at - ending at
        // `now` would drop today's.
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap());
    }

    #[test]
    fn window_early_in_a_month_reaches_back_six_days() {
        let now = Utc.with_ymd_and_hms(2026, 10, 3, 0, 30, 0).unwrap();
        let (start, end) = window(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 9, 27, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2026, 10, 4, 0, 0, 0).unwrap());
    }

    fn key_file(contents: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-key");
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn a_missing_key_file_is_no_key() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_admin_key(&dir.path().join("admin-key")).await,
            Err(ConsoleError::NoKey)
        );
    }

    #[tokio::test]
    async fn a_key_file_others_can_read_is_refused() {
        for mode in [0o644, 0o640, 0o604] {
            let (_dir, path) = key_file("sk-ant-admin01-abc\n", mode);
            assert_eq!(
                read_admin_key(&path).await,
                Err(ConsoleError::InsecureKeyFile),
                "{mode:o}"
            );
        }
    }

    #[tokio::test]
    async fn a_private_key_file_is_read_and_trimmed() {
        for mode in [0o600, 0o400] {
            let (_dir, path) = key_file("  sk-ant-admin01-abc\r\n", mode);
            assert_eq!(
                read_admin_key(&path).await,
                Ok(AdminKey("sk-ant-admin01-abc".to_string())),
                "{mode:o}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_key_file_is_no_key() {
        let (_dir, path) = key_file(" \n", 0o600);
        assert_eq!(read_admin_key(&path).await, Err(ConsoleError::NoKey));
    }

    #[tokio::test]
    async fn a_directory_at_the_key_path_is_no_key() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_admin_key(dir.path()).await, Err(ConsoleError::NoKey));
    }

    #[test]
    fn debug_never_prints_the_key() {
        let key = AdminKey("sk-ant-admin01-secret".to_string());
        assert!(!format!("{key:?}").contains("secret"));
    }

    #[test]
    fn the_key_lives_under_the_config_dir() {
        assert!(default_key_path().ends_with(".config/opendeck-claude-usage/admin-key"));
    }

    use crate::source::cached::CachePolicy;
    use std::time::Duration;

    #[test]
    fn report_urls_carry_the_window_daily_buckets_and_the_page() {
        let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 10, 16, 0, 0, 0).unwrap();
        assert_eq!(
            report_url(COST_PATH, start, end, None).as_str(),
            "https://api.anthropic.com/v1/organizations/cost_report?starting_at=2026-10-01T00%3A00%3A00Z&ending_at=2026-10-16T00%3A00%3A00Z&bucket_width=1d&limit=31"
        );
        let next = report_url(USAGE_PATH, start, end, Some("page_abc="));
        assert!(
            next.as_str()
                .starts_with("https://api.anthropic.com/v1/organizations/usage_report/messages?"),
            "{next}"
        );
        assert!(next.as_str().ends_with("&page=page_abc%3D"), "{next}");
    }

    #[test]
    fn rejected_keys_are_unauthorized_and_other_statuses_are_request_errors() {
        assert_eq!(error_for_status(401), ConsoleError::Unauthorized(401));
        assert_eq!(error_for_status(403), ConsoleError::Unauthorized(403));
        assert_eq!(
            error_for_status(429),
            ConsoleError::Request("HTTP 429".to_string())
        );
        assert_eq!(
            error_for_status(500),
            ConsoleError::Request("HTTP 500".to_string())
        );
    }

    #[test]
    fn only_key_file_problems_are_local() {
        assert!(ConsoleSource::is_local(&ConsoleError::NoKey));
        assert!(ConsoleSource::is_local(&ConsoleError::InsecureKeyFile));
        assert!(!ConsoleSource::is_local(&ConsoleError::Unauthorized(401)));
        assert!(!ConsoleSource::is_local(&ConsoleError::Request("x".into())));
        assert!(!ConsoleSource::is_local(&ConsoleError::Parse("x".into())));
    }

    #[tokio::test]
    async fn no_key_file_fails_without_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let source = ConsoleSource::new(dir.path().join("admin-key"));
        assert_eq!(source.fetch().await, Err(ConsoleError::NoKey));
    }

    #[tokio::test]
    async fn the_cached_console_reports_a_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let cached = CachedSource::new(
            ConsoleSource::new(dir.path().join("admin-key")),
            CachePolicy {
                min_interval: Duration::from_secs(300),
                max_backoff: Duration::from_secs(1800),
                stale_after: Duration::from_secs(3600),
            },
        );
        assert_eq!(cached.snapshot().await, Err(ConsoleError::NoKey));
    }

    /// Hits the real Admin API with the key at `default_key_path()` - run
    /// by hand (`cargo test -- --ignored live_`), never in CI. Skips when
    /// there's no key file.
    #[tokio::test]
    #[ignore]
    async fn live_console_reads_month_to_date() {
        let path = default_key_path();
        if !path.exists() {
            println!("skipped: no Admin key at {}", path.display());
            return;
        }
        let snapshot = ConsoleSource::default().fetch().await.unwrap();
        println!("{snapshot:?}");
        let today = Utc::now().date_naive();
        assert!(
            snapshot.days.iter().any(|d| d.date == today),
            "today's bucket is missing - check ending_at"
        );
    }

    /// After KEY PERMS or NOT ADMIN the user replaces the key: the cache
    /// must hear about it rather than wait out its backoff.
    #[tokio::test]
    async fn replacing_the_key_file_counts_as_changed_input() {
        let (_dir, path) = key_file("sk-ant-api03-regular", 0o644); // refused locally
        let source = ConsoleSource::new(path.clone());
        assert_eq!(source.fetch().await, Err(ConsoleError::InsecureKeyFile));
        assert!(!source.input_changed().await);

        std::fs::write(&path, "sk-ant-admin01-replacement").unwrap();
        assert!(source.input_changed().await);
    }

    #[tokio::test]
    async fn creating_a_missing_key_file_counts_as_changed_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-key");
        let source = ConsoleSource::new(path.clone());
        assert_eq!(source.fetch().await, Err(ConsoleError::NoKey));
        assert!(!source.input_changed().await);

        std::fs::write(&path, "sk-ant-admin01-new").unwrap();
        assert!(source.input_changed().await);
    }
}
