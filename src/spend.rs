//! API Spend: billed Console spend over a range of UTC days, from a
//! `ConsoleSnapshot`. Pure - `today` comes in as an argument.

use chrono::{Datelike, Days, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::format::DISABLED_COLOR;
use crate::level::{ColorSettings, Level};
use crate::metric::{COST_ACCENT, format_cost_compact};
use crate::serde_util::{or_default, positive_or_none};
use crate::source::console::{ConsoleError, ConsoleSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpendRange {
    Today,
    #[serde(rename = "sevenday")]
    SevenDay,
    #[default]
    Month,
}

impl SpendRange {
    /// The range a short press moves to.
    pub fn next(self) -> Self {
        match self {
            SpendRange::Today => SpendRange::SevenDay,
            SpendRange::SevenDay => SpendRange::Month,
            SpendRange::Month => SpendRange::Today,
        }
    }

    /// The first UTC day the range covers; every range ends at `today`.
    pub fn first_day(self, today: NaiveDate) -> NaiveDate {
        match self {
            SpendRange::Today => today,
            SpendRange::SevenDay => today - Days::new(6),
            SpendRange::Month => today.with_day(1).expect("day 1 exists"),
        }
    }

    fn caption(self) -> &'static str {
        match self {
            SpendRange::Today => "TODAY UTC",
            SpendRange::SevenDay => "7 DAYS",
            SpendRange::Month => "THIS MONTH",
        }
    }

    fn short(self) -> &'static str {
        match self {
            SpendRange::Today => "TODAY",
            SpendRange::SevenDay => "7D",
            SpendRange::Month => "MONTH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ApiSpendSettings {
    /// Changed only by pressing the key or dial.
    #[serde(default, deserialize_with = "or_default")]
    pub range: SpendRange,
    /// Monthly budget in dollars; `None` (blank, zero, junk) means none.
    #[serde(
        default,
        rename = "budgetDollars",
        deserialize_with = "positive_or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub budget_dollars: Option<f64>,
    #[serde(flatten)]
    pub colors: ColorSettings,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Totals {
    pub cost_dollars: f64,
    pub tokens: u64,
}

/// Sums the snapshot's days in `range`. A day the snapshot lacks counts as
/// zero.
pub fn totals(snapshot: &ConsoleSnapshot, range: SpendRange, today: NaiveDate) -> Totals {
    let first = range.first_day(today);
    snapshot
        .days
        .iter()
        .filter(|d| d.date >= first && d.date <= today)
        .fold(
            Totals {
                cost_dollars: 0.0,
                tokens: 0,
            },
            |t, d| Totals {
                cost_dollars: t.cost_dollars + d.cost_dollars,
                tokens: t.tokens + d.tokens,
            },
        )
}

/// Short text shown in place of the value when there's no data - never
/// a $0 that could pass for real spend.
pub fn error_label(error: &ConsoleError) -> &'static str {
    match error {
        ConsoleError::NoKey => "NO KEY",
        ConsoleError::InsecureKeyFile => "KEY PERMS",
        ConsoleError::Unauthorized(_) => "NOT ADMIN",
        ConsoleError::Request(_) | ConsoleError::Parse(_) => "NO DATA",
    }
}

/// What one API Spend instance shows, for either surface.
#[derive(Debug, Clone, PartialEq)]
pub struct SpendDisplay {
    /// "$12.34", or an error label like "NO KEY".
    pub value_text: String,
    /// False when `value_text` is an error label.
    pub available: bool,
    /// Under the value on a key: "TODAY UTC", "7 DAYS", "THIS MONTH".
    pub caption: &'static str,
    /// The dial's detail line, e.g. "API · MONTH · 42% of $50".
    pub detail: String,
    pub color: String,
    /// Share of the monthly budget, on This month with a budget set. Not
    /// clamped - renderers clamp the bar.
    pub budget_percent: Option<f64>,
}

pub fn build_spend_display(
    outcome: Result<&ConsoleSnapshot, &ConsoleError>,
    settings: &ApiSpendSettings,
    today: NaiveDate,
) -> SpendDisplay {
    let range = settings.range;
    let title = format!("API · {}", range.short());
    let snapshot = match outcome {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return SpendDisplay {
                value_text: error_label(error).to_string(),
                available: false,
                caption: range.caption(),
                detail: title,
                color: DISABLED_COLOR.to_string(),
                budget_percent: None,
            };
        }
    };
    let spent = totals(snapshot, range, today).cost_dollars;
    let budget = settings
        .budget_dollars
        .filter(|_| range == SpendRange::Month);
    let (color, detail, budget_percent) = match budget {
        Some(budget) => {
            let percent = spent / budget * 100.0;
            let level = Level::for_percent(percent, &settings.colors.marks);
            (
                settings.colors.palette.color(level).to_string(),
                format!("{title} · {percent:.0}% of ${budget:.0}"),
                Some(percent),
            )
        }
        None => (COST_ACCENT.to_string(), title, None),
    };
    SpendDisplay {
        value_text: format_cost_compact(spent),
        available: true,
        caption: range.caption(),
        detail,
        color,
        budget_percent,
    }
}

/// Dial payload for `layouts/usage.json` (`percent`, `bar`, `detail`).
pub fn spend_feedback(display: &SpendDisplay) -> Value {
    json!({
        "percent": display.value_text,
        "bar": {
            "value": display.budget_percent.unwrap_or(0.0).clamp(0.0, 100.0),
            "bar_fill_c": display.color,
        },
        "detail": display.detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::{DEFAULT_CRITICAL, DEFAULT_RISK};
    use crate::source::console::ConsoleDay;

    fn date(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    fn today() -> NaiveDate {
        date(10, 3)
    }

    fn day(m: u32, d: u32, cost_dollars: f64, tokens: u64) -> ConsoleDay {
        ConsoleDay {
            date: date(m, d),
            cost_dollars,
            tokens,
        }
    }

    /// Today is Oct 3: "7 days" is Sep 27..=Oct 3, "this month" Oct 1..=3.
    fn snapshot() -> ConsoleSnapshot {
        ConsoleSnapshot {
            days: vec![
                day(9, 26, 1000.0, 1),
                day(9, 27, 2.0, 10),
                day(9, 30, 3.0, 100),
                day(10, 1, 4.0, 1000),
                day(10, 3, 5.0, 10000),
            ],
        }
    }

    fn settings(range: SpendRange, budget_dollars: Option<f64>) -> ApiSpendSettings {
        ApiSpendSettings {
            range,
            budget_dollars,
            colors: ColorSettings::default(),
        }
    }

    #[test]
    fn a_short_press_cycles_today_seven_days_month() {
        assert_eq!(SpendRange::Today.next(), SpendRange::SevenDay);
        assert_eq!(SpendRange::SevenDay.next(), SpendRange::Month);
        assert_eq!(SpendRange::Month.next(), SpendRange::Today);
    }

    #[test]
    fn ranges_end_today_and_start_where_expected() {
        assert_eq!(SpendRange::Today.first_day(today()), date(10, 3));
        assert_eq!(SpendRange::SevenDay.first_day(today()), date(9, 27));
        assert_eq!(SpendRange::Month.first_day(today()), date(10, 1));
    }

    #[test]
    fn totals_sum_only_the_range() {
        let s = snapshot();
        assert_eq!(
            totals(&s, SpendRange::Today, today()),
            Totals {
                cost_dollars: 5.0,
                tokens: 10000
            }
        );
        assert_eq!(
            totals(&s, SpendRange::SevenDay, today()),
            Totals {
                cost_dollars: 14.0,
                tokens: 11110
            }
        );
        assert_eq!(
            totals(&s, SpendRange::Month, today()),
            Totals {
                cost_dollars: 9.0,
                tokens: 11000
            }
        );
    }

    /// Right after UTC midnight the cached snapshot may not have today's
    /// bucket yet - that's $0, not an error.
    #[test]
    fn a_day_missing_from_the_snapshot_counts_as_zero() {
        assert_eq!(
            totals(&snapshot(), SpendRange::Today, date(10, 4)),
            Totals {
                cost_dollars: 0.0,
                tokens: 0
            }
        );
    }

    #[test]
    fn without_a_budget_the_value_is_in_the_cost_accent() {
        let d = build_spend_display(Ok(&snapshot()), &settings(SpendRange::Month, None), today());
        assert_eq!(d.value_text, "$9.00");
        assert!(d.available);
        assert_eq!(d.caption, "THIS MONTH");
        assert_eq!(d.detail, "API · MONTH");
        assert_eq!(d.color, COST_ACCENT);
        assert_eq!(d.budget_percent, None);
    }

    #[test]
    fn a_budget_colors_this_month_by_the_marks() {
        // $9 of $12 is 75% - exactly the default Risk mark.
        let d = build_spend_display(
            Ok(&snapshot()),
            &settings(SpendRange::Month, Some(12.0)),
            today(),
        );
        assert_eq!(d.budget_percent, Some(75.0));
        assert_eq!(d.color, DEFAULT_RISK);
        assert_eq!(d.detail, "API · MONTH · 75% of $12");
    }

    #[test]
    fn the_budget_only_applies_to_this_month() {
        let d = build_spend_display(
            Ok(&snapshot()),
            &settings(SpendRange::Today, Some(12.0)),
            today(),
        );
        assert_eq!(d.value_text, "$5.00");
        assert_eq!(d.caption, "TODAY UTC");
        assert_eq!(d.detail, "API · TODAY");
        assert_eq!(d.color, COST_ACCENT);
        assert_eq!(d.budget_percent, None);
    }

    #[test]
    fn seven_days_reaches_into_last_month() {
        let d = build_spend_display(
            Ok(&snapshot()),
            &settings(SpendRange::SevenDay, None),
            today(),
        );
        assert_eq!(d.value_text, "$14.00");
        assert_eq!(d.caption, "7 DAYS");
        assert_eq!(d.detail, "API · 7D");
    }

    #[test]
    fn big_totals_drop_the_cents() {
        let s = ConsoleSnapshot {
            days: vec![day(10, 3, 1234.56, 0)],
        };
        let d = build_spend_display(Ok(&s), &settings(SpendRange::Today, None), today());
        assert_eq!(d.value_text, "$1235");
    }

    #[test]
    fn errors_show_a_label_never_a_zero() {
        let cases = [
            (ConsoleError::NoKey, "NO KEY"),
            (ConsoleError::InsecureKeyFile, "KEY PERMS"),
            (ConsoleError::Unauthorized(403), "NOT ADMIN"),
            (ConsoleError::Request("HTTP 500".into()), "NO DATA"),
            (ConsoleError::Parse("x".into()), "NO DATA"),
        ];
        for (error, label) in cases {
            let d = build_spend_display(
                Err(&error),
                &settings(SpendRange::Month, Some(10.0)),
                today(),
            );
            assert_eq!(d.value_text, label);
            assert!(!d.available);
            assert_eq!(d.color, DISABLED_COLOR);
            assert_eq!(d.budget_percent, None);
            assert_eq!(d.caption, "THIS MONTH");
        }
    }

    #[test]
    fn dial_feedback_fills_the_bar_with_the_budget_share_clamped() {
        // $9 of $6 is 150%: Critical, and the bar stops at full.
        let d = build_spend_display(
            Ok(&snapshot()),
            &settings(SpendRange::Month, Some(6.0)),
            today(),
        );
        let f = spend_feedback(&d);
        assert_eq!(f["percent"], "$9.00");
        assert_eq!(f["bar"]["value"], 100.0);
        assert_eq!(f["bar"]["bar_fill_c"], DEFAULT_CRITICAL);
        assert_eq!(f["detail"], "API · MONTH · 150% of $6");

        let no_budget =
            build_spend_display(Ok(&snapshot()), &settings(SpendRange::Month, None), today());
        assert_eq!(spend_feedback(&no_budget)["bar"]["value"], 0.0);
    }

    #[test]
    fn settings_default_to_this_month_without_a_budget() {
        let from_empty: ApiSpendSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(from_empty, ApiSpendSettings::default());
        assert_eq!(from_empty.range, SpendRange::Month);
        assert_eq!(from_empty.budget_dollars, None);
    }

    /// A bad field falls back alone - a hard error would make openaction
    /// reset every setting, colors included (KI-10).
    #[test]
    fn a_bad_range_or_budget_falls_back_alone() {
        let s: ApiSpendSettings =
            serde_json::from_str(r#"{"range": "weekly", "budgetDollars": "50", "watch": 60}"#)
                .unwrap();
        assert_eq!(s.range, SpendRange::Month);
        assert_eq!(s.budget_dollars, Some(50.0));
        assert_eq!(s.colors.marks.watch, 60.0);

        let s: ApiSpendSettings =
            serde_json::from_str(r#"{"range": "today", "budgetDollars": -5}"#).unwrap();
        assert_eq!(s.range, SpendRange::Today);
        assert_eq!(s.budget_dollars, None);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let s = settings(SpendRange::SevenDay, Some(40.0));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["range"], "sevenday");
        assert_eq!(json["budgetDollars"], 40.0);
        let back: ApiSpendSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back, s);
    }
}
