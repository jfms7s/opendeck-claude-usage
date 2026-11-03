use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::format::DISABLED_COLOR;
use crate::pricing::CostTotal;
use crate::source::logs::LogEntry;

pub const TOKENS_ACCENT: &str = "#38bdf8";
pub const COST_ACCENT: &str = "#fb923c";

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

/// Which models a Metric Tile or Heatmap counts: every model, or one
/// family, matched as a case-insensitive substring of the model id (so a
/// saved key keeps working when a new version ships). A model in no family
/// counts only under `All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelScope {
    #[default]
    All,
    Opus,
    Sonnet,
    Haiku,
}

impl ModelScope {
    /// The family's name as drawn on a key, `None` for `All`.
    pub fn family(self) -> Option<&'static str> {
        match self {
            ModelScope::All => None,
            ModelScope::Opus => Some("opus"),
            ModelScope::Sonnet => Some("sonnet"),
            ModelScope::Haiku => Some("haiku"),
        }
    }

    pub fn matches(self, entry: &LogEntry) -> bool {
        self.family()
            .is_none_or(|family| entry.model.to_ascii_lowercase().contains(family))
    }
}

/// What a Metric Tile or Heatmap counts: every model or one family, or
/// one project (its id from `logs::project_id`, every model). Stored as
/// one `scope` string: `all`, `opus`, `sonnet`, `haiku` or
/// `project:<id>`. Anything else fails to parse, which `lenient` turns
/// into `All`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Models(ModelScope),
    Project(Arc<str>),
}

const PROJECT_PREFIX: &str = "project:";

impl Default for Scope {
    fn default() -> Self {
        Scope::Models(ModelScope::All)
    }
}

impl From<ModelScope> for Scope {
    fn from(models: ModelScope) -> Self {
        Scope::Models(models)
    }
}

impl Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Scope::Models(models) => models.serialize(serializer),
            Scope::Project(id) => serializer.serialize_str(&format!("{PROJECT_PREFIX}{id}")),
        }
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if let Some(id) = text.strip_prefix(PROJECT_PREFIX) {
            if id.is_empty() {
                return Err(serde::de::Error::custom("empty project id"));
            }
            return Ok(Scope::Project(id.into()));
        }
        ModelScope::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&text))
            .map(Scope::Models)
    }
}

impl Scope {
    pub fn matches(&self, entry: &LogEntry) -> bool {
        match self {
            Scope::Models(models) => models.matches(entry),
            Scope::Project(id) => *entry.project == **id,
        }
    }

    pub fn is_all(&self) -> bool {
        *self == Scope::Models(ModelScope::All)
    }

    /// What a key names the scope with: the family, or the project's name
    /// (looked up by `name_of`, falling back to its id) cut to `max`
    /// characters. `None` when unscoped.
    pub fn caption(
        &self,
        max: usize,
        name_of: impl FnOnce(&str) -> Option<String>,
    ) -> Option<String> {
        match self {
            Scope::Models(models) => models.family().map(str::to_string),
            Scope::Project(id) => {
                Some(shorten(&name_of(id).unwrap_or_else(|| id.to_string()), max))
            }
        }
    }
}

/// `text` cut to `max` characters, the last one an ellipsis when cut.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

pub struct MetricDisplay {
    pub label: &'static str,
    pub value_text: String,
    pub subtitle: String,
    pub accent_color: &'static str,
}

/// `[start, end]` bounds for `range`, in UTC. `Today` runs from local
/// midnight (in `now`'s time zone, like the Heatmap's days and the
/// Sparkline's "Today"); `SevenDay` is a rolling 7x24h ending at `now`.
/// `Session` mirrors the Usage Gauge's 5-hour rate-limit window:
/// `[resets_at - 5h, resets_at]` when `session_resets_at` is known (from
/// the same usage fetch the gauge renders), falling back to a rolling
/// last-5h window when it isn't - a fallback, not an error, matching this
/// plugin's existing convention.
pub fn range_bounds<Tz: TimeZone>(
    range: RangeKind,
    now: DateTime<Tz>,
    session_resets_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    let local_midnight = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|m| now.timezone().from_local_datetime(&m).earliest())
        .map(|m| m.with_timezone(&Utc));
    let now = now.with_timezone(&Utc);
    match range {
        // A zone with no 00:00 that day (a DST jump at midnight) falls back
        // to the last 24 hours.
        RangeKind::Today => (local_midnight.unwrap_or(now - Duration::hours(24)), now),
        RangeKind::SevenDay => (now - Duration::hours(24 * 7), now),
        RangeKind::Session => {
            let end = session_resets_at.unwrap_or(now);
            let start = end - Duration::hours(5);
            (start, end)
        }
    }
}

/// `< 1000` as an exact integer; otherwise one decimal place with a
/// K/M/B suffix, trimming a trailing ".0" (`318000` -> `"318K"`, `318500`
/// -> `"318.5K"`, `1234567` -> `"1.2M"`) - matches the mockup this tile
/// is based on.
pub fn format_tokens(total: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    const B: u64 = 1_000_000_000;
    // The lower-tier cutoff for each boundary is 999_950 (or its scaled
    // equivalent), not the round power of ten - a raw `< 1_000_000` check
    // would let e.g. 999_950 format at the K tier, round to "1000.0" at
    // one decimal place, and display as the nonsensical "1000K" instead
    // of switching tiers to "1M".
    if total < 1000 {
        return total.to_string();
    }
    let (divisor, suffix) = if total < 999_950 {
        (K, "K")
    } else if total < 999_950 * K {
        (M, "M")
    } else {
        (B, "B")
    };
    let value = total as f64 / divisor as f64;
    let formatted = format!("{value:.1}");
    let trimmed = formatted.strip_suffix(".0").unwrap_or(&formatted);
    format!("{trimmed}{suffix}")
}

/// Always two decimal places, e.g. `"$8.40"`.
pub fn format_cost(total: f64) -> String {
    format!("${total:.2}")
}

/// `format_cost`, plus a trailing `+` when some usage couldn't be priced
/// (a model missing from `pricing.rs`): the real total is at least this.
pub fn format_cost_total(total: CostTotal) -> String {
    let marker = if total.partial { "+" } else { "" };
    format!("{}{marker}", format_cost(total.dollars))
}

/// Computes what to show for one instance's current metric/range
/// selection from the full set of parsed log entries. Always produces a
/// real number (possibly zero) - callers decide separately whether "no
/// entries at all were found anywhere" warrants `error_display()`
/// instead (see `metric_action.rs`), since a legitimate zero-usage range
/// is a different situation from no log data existing at all.
#[cfg(test)]
pub fn build_metric_display<Tz: TimeZone>(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    now: DateTime<Tz>,
    session_resets_at: Option<DateTime<Utc>>,
) -> MetricDisplay {
    build_scoped_metric_display(
        entries,
        metric,
        range,
        &Scope::default(),
        None,
        now,
        session_resets_at,
    )
}

/// `build_metric_display` counting only what `scope` matches; the subtitle
/// adds `scope_caption` (`today · opus`, `today · my-repo`). A scope with
/// no usage in range is a real zero, not "no data".
pub fn build_scoped_metric_display<Tz: TimeZone>(
    entries: &[LogEntry],
    metric: MetricKind,
    range: RangeKind,
    scope: &Scope,
    scope_caption: Option<&str>,
    now: DateTime<Tz>,
    session_resets_at: Option<DateTime<Utc>>,
) -> MetricDisplay {
    let (start, end) = range_bounds(range, now, session_resets_at);
    let in_range: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.timestamp >= start && e.timestamp <= end && scope.matches(e))
        .collect();

    let subtitle = match scope_caption {
        Some(caption) => format!("{} \u{b7} {caption}", range_caption(range)),
        None => range_caption(range).to_string(),
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
        MetricKind::Cost => MetricDisplay {
            label: "Cost",
            value_text: format_cost_total(CostTotal::of(in_range.iter().copied())),
            subtitle,
            accent_color: COST_ACCENT,
        },
    }
}

fn range_caption(range: RangeKind) -> &'static str {
    match range {
        RangeKind::Today => "today",
        RangeKind::SevenDay => "7 days",
        RangeKind::Session => "session",
    }
}

/// No log data available anywhere (a fresh install, or `~/.claude/projects`
/// missing/unreadable) - a clearly-labeled "no data" state, mirroring
/// `format::error_display()`.
pub fn error_display() -> MetricDisplay {
    MetricDisplay {
        label: "\u{2014}",
        value_text: "\u{2014}".to_string(),
        subtitle: "no data".to_string(),
        accent_color: DISABLED_COLOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 13, hour, minute, 0).unwrap()
    }

    /// "Today" is the local calendar day, as on the Heatmap and the
    /// Sparkline - not the last 24 hours.
    #[test]
    fn today_starts_at_local_midnight() {
        let plus2 = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        // 09:00 local on the 13th = 07:00 UTC.
        let now = dt(7, 0).with_timezone(&plus2);
        let (start, end) = range_bounds(RangeKind::Today, now, None);
        // Local midnight on the 13th = 22:00 UTC on the 12th.
        assert_eq!(start, dt(7, 0) - Duration::hours(9));
        assert_eq!(end, dt(7, 0));
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
    fn format_tokens_billions() {
        assert_eq!(format_tokens(2_586_419_159), "2.6B");
        assert_eq!(format_tokens(5_000_000_000), "5B");
    }

    #[test]
    fn format_tokens_switches_tiers_at_the_rounding_boundary_not_the_power_of_ten() {
        assert_eq!(format_tokens(999_949), "999.9K");
        assert_eq!(format_tokens(999_950), "1M");
        assert_eq!(format_tokens(999_999), "1M");
    }

    #[test]
    fn format_tokens_switches_from_millions_to_billions_at_the_same_rounding_boundary() {
        assert_eq!(format_tokens(999_999_999), "1B");
    }

    #[test]
    fn format_cost_always_shows_two_decimals() {
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(8.4), "$8.40");
        assert_eq!(format_cost(1234.567), "$1234.57");
    }

    fn entry(timestamp: DateTime<Utc>, model: &str, input: u64, output: u64) -> LogEntry {
        LogEntry {
            timestamp,
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            cache_read_input_tokens: 0,
            project: Default::default(),
        }
    }

    #[test]
    fn build_metric_display_sums_tokens_only_within_range() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 100, 50), // today
            entry(now - Duration::hours(21), "claude-sonnet-5", 999, 999), // yesterday, within 24h
        ];
        let display =
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::Today, now, None);
        assert_eq!(display.label, "Tokens");
        assert_eq!(display.value_text, "150");
        assert_eq!(display.subtitle, "today");
        assert_eq!(display.accent_color, TOKENS_ACCENT);
    }

    #[test]
    fn build_metric_display_cost_marks_entries_with_an_unrecognized_model() {
        let now = dt(20, 0);
        let entries = vec![
            entry(now - Duration::hours(1), "claude-sonnet-5", 1_000_000, 0), // $2.00 at Sonnet 5's input rate
            entry(now - Duration::hours(1), "some-future-model", 1_000_000, 0), // unpriced
        ];
        let display = build_metric_display(&entries, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.label, "Cost");
        // At least $2.00: one entry couldn't be priced.
        assert_eq!(display.value_text, "$2.00+");
        let priced = &entries[..1];
        let display = build_metric_display(priced, MetricKind::Cost, RangeKind::Today, now, None);
        assert_eq!(display.value_text, "$2.00");
    }

    #[test]
    fn build_metric_display_subtitle_matches_each_range() {
        let now = dt(20, 0);
        let entries: Vec<LogEntry> = vec![];
        assert_eq!(
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::SevenDay, now, None)
                .subtitle,
            "7 days"
        );
        assert_eq!(
            build_metric_display(&entries, MetricKind::Tokens, RangeKind::Session, now, None)
                .subtitle,
            "session"
        );
    }

    #[test]
    fn error_display_is_a_clear_no_data_state() {
        let display = error_display();
        assert_eq!(display.subtitle, "no data");
        assert_eq!(display.accent_color, DISABLED_COLOR);
    }

    fn model_entry(model: &str, tokens: u64) -> LogEntry {
        LogEntry {
            timestamp: dt(10, 0),
            model: model.into(),
            input_tokens: tokens,
            ..LogEntry::default()
        }
    }

    #[test]
    fn a_model_scope_matches_its_family_only() {
        let opus = model_entry("claude-opus-5", 1);
        let sonnet = model_entry("Claude-Sonnet-5-5", 1);
        let other = model_entry("gpt-free", 1);
        for e in [&opus, &sonnet, &other] {
            assert!(ModelScope::All.matches(e));
        }
        assert!(ModelScope::Opus.matches(&opus));
        assert!(!ModelScope::Opus.matches(&sonnet));
        assert!(ModelScope::Sonnet.matches(&sonnet), "case-insensitive");
        assert!(!ModelScope::Haiku.matches(&other));
    }

    #[test]
    fn a_scoped_display_counts_its_family_and_names_it() {
        let entries = [
            model_entry("claude-opus-5", 100),
            model_entry("claude-sonnet-5", 7),
        ];
        let now = dt(12, 0);
        let opus = build_scoped_metric_display(
            &entries,
            MetricKind::Tokens,
            RangeKind::Today,
            &ModelScope::Opus.into(),
            Some("opus"),
            now,
            None,
        );
        assert_eq!(opus.value_text, "100");
        assert_eq!(opus.subtitle, "today \u{b7} opus");
        // No Haiku usage: a real zero, not "no data".
        let haiku = build_scoped_metric_display(
            &entries,
            MetricKind::Tokens,
            RangeKind::SevenDay,
            &ModelScope::Haiku.into(),
            Some("haiku"),
            now,
            None,
        );
        assert_eq!(haiku.value_text, "0");
        assert_eq!(haiku.subtitle, "7 days \u{b7} haiku");
        assert_eq!(haiku.label, "Tokens");
    }

    #[test]
    fn a_garbage_or_missing_scope_is_all() {
        #[derive(serde::Deserialize)]
        struct W {
            #[serde(default, deserialize_with = "crate::settings::lenient")]
            scope: ModelScope,
        }
        for json in [r#"{}"#, r#"{"scope":"gpt"}"#, r#"{"scope":3}"#] {
            assert_eq!(
                serde_json::from_str::<W>(json).unwrap().scope,
                ModelScope::All
            );
        }
        let w: W = serde_json::from_str(r#"{"scope":"opus"}"#).unwrap();
        assert_eq!(w.scope, ModelScope::Opus);
    }
    #[test]
    fn a_scope_round_trips_through_its_string() {
        for (text, scope) in [
            ("all", Scope::default()),
            ("opus", ModelScope::Opus.into()),
            (
                "project:-home-u-git-repo",
                Scope::Project("-home-u-git-repo".into()),
            ),
        ] {
            let json = serde_json::Value::String(text.into());
            assert_eq!(
                serde_json::from_value::<Scope>(json.clone()).unwrap(),
                scope
            );
            assert_eq!(serde_json::to_value(&scope).unwrap(), json);
        }
        #[derive(serde::Deserialize)]
        struct W {
            #[serde(default, deserialize_with = "crate::settings::lenient")]
            scope: Scope,
        }
        for json in [
            r#"{"scope":"project:"}"#,
            r#"{"scope":"projects"}"#,
            r#"{"scope":[1]}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<W>(json).unwrap().scope,
                Scope::default(),
                "{json}"
            );
        }
    }

    #[test]
    fn a_project_scope_matches_its_project_only() {
        let mut mine = model_entry("claude-opus-5", 1);
        mine.project = "-home-u-git-repo".into();
        let theirs = model_entry("claude-opus-5", 1);
        let scope = Scope::Project("-home-u-git-repo".into());
        assert!(scope.matches(&mine));
        assert!(!scope.matches(&theirs));
        assert!(!scope.is_all());
    }

    #[test]
    fn a_project_caption_is_its_name_cut_to_fit() {
        let scope = Scope::Project("-home-u-git-repo".into());
        assert_eq!(
            scope.caption(12, |_| Some("repo".into())).as_deref(),
            Some("repo")
        );
        assert_eq!(
            scope
                .caption(12, |_| Some("opendeck-claude-usage".into()))
                .as_deref(),
            Some("opendeck-cl\u{2026}")
        );
        assert_eq!(
            scope.caption(30, |_| None).as_deref(),
            Some("-home-u-git-repo")
        );
        assert_eq!(Scope::default().caption(12, |_| None), None);
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("abcd", 3), "ab\u{2026}");
    }

    #[test]
    fn a_project_scoped_display_with_no_usage_is_zero_and_named() {
        let entries = [model_entry("claude-opus-5", 100)];
        let d = build_scoped_metric_display(
            &entries,
            MetricKind::Tokens,
            RangeKind::Today,
            &Scope::Project("-home-u-git-repo".into()),
            Some("repo"),
            dt(12, 0),
            None,
        );
        assert_eq!(d.value_text, "0");
        assert_eq!(d.subtitle, "today \u{b7} repo");
    }
}
