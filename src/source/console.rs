//! Billed spend for a Claude Console organization, from the Usage & Cost
//! Admin API (`/v1/organizations/cost_report` and
//! `/v1/organizations/usage_report/messages`, daily UTC buckets). Needs an
//! Admin key (`sk-ant-admin01-…`) read from a private file: regular API
//! keys can't read usage, and individual accounts can't create Admin keys.

use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
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
}
