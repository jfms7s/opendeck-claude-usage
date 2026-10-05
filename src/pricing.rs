//! Estimated dollar cost of transcript entries, from Anthropic's published
//! per-model API rates. Claude Code's transcripts carry token counts but no
//! cost, so this is an estimate of what the same tokens would cost at API
//! list prices - not what a Pro/Max subscription is billed.

use crate::source::logs::LogEntry;

/// $ per 1,000,000 tokens, split by how the token was spent.
/// `cache_write` is the 5-minute-lifetime write rate; 1-hour writes cost
/// `ONE_HOUR_WRITE` times the input rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceTable {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
}

/// Cache writes are priced off the input rate: 1.25x for the default
/// 5-minute lifetime, 2x for the 1-hour one.
const FIVE_MINUTE_WRITE: f64 = 1.25;
const ONE_HOUR_WRITE: f64 = 2.0;

const fn rates(input: f64, output: f64, cache_read: f64) -> PriceTable {
    PriceTable {
        input,
        output,
        cache_write: input * FIVE_MINUTE_WRITE,
        cache_read,
    }
}

/// Exact model id -> rates. Checked 2026-10-05 against Anthropic's model
/// and pricing reference (the claude-api skill's model table); the legacy
/// rows are the list prices those models launched at. A model missing here
/// is reported as unpriced (Cost shows a trailing "+") rather than guessed
/// from its family: rates now differ between generations of one family.
const PRICES: &[(&str, PriceTable)] = &[
    ("claude-fable-5-1", rates(10.0, 50.0, 0.25)),
    ("claude-mythos-5-1", rates(10.0, 50.0, 0.25)),
    ("claude-fable-5", rates(10.0, 50.0, 1.0)),
    ("claude-mythos-5", rates(10.0, 50.0, 1.0)),
    ("claude-opus-5-5", rates(4.0, 20.0, 0.20)),
    ("claude-opus-5", rates(5.0, 25.0, 0.50)),
    ("claude-opus-4-8", rates(5.0, 25.0, 0.50)),
    ("claude-opus-4-7", rates(5.0, 25.0, 0.50)),
    ("claude-opus-4-6", rates(5.0, 25.0, 0.50)),
    ("claude-opus-4-5", rates(5.0, 25.0, 0.50)),
    ("claude-opus-4-1", rates(15.0, 75.0, 1.50)),
    ("claude-opus-4-0", rates(15.0, 75.0, 1.50)),
    ("claude-opus-4", rates(15.0, 75.0, 1.50)),
    ("claude-sonnet-5-5", rates(2.0, 10.0, 0.20)),
    ("claude-sonnet-5", rates(2.0, 10.0, 0.20)),
    ("claude-sonnet-4-6", rates(3.0, 15.0, 0.30)),
    ("claude-sonnet-4-5", rates(3.0, 15.0, 0.30)),
    ("claude-sonnet-4-0", rates(3.0, 15.0, 0.30)),
    ("claude-sonnet-4", rates(3.0, 15.0, 0.30)),
    ("claude-haiku-4-5", rates(1.0, 5.0, 0.10)),
];

/// `claude-haiku-4-5-20251001` -> `claude-haiku-4-5`: a dated snapshot id
/// costs the same as its model.
fn without_date_suffix(model: &str) -> &str {
    match model.rsplit_once(['-', '@']) {
        Some((base, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => model,
    }
}

/// The rates for `model`, or `None` for anything not in the table.
pub fn price_for_model(model: &str) -> Option<PriceTable> {
    let lower = model.to_ascii_lowercase();
    let id = without_date_suffix(&lower);
    PRICES
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, price)| *price)
}

/// Estimated cost in dollars for one log entry, or `None` if its model
/// isn't in the table - the entry still counts toward Tokens.
pub fn cost_for_entry(entry: &LogEntry) -> Option<f64> {
    let price = price_for_model(&entry.model)?;
    let million = 1_000_000.0;
    let one_hour_writes = entry
        .cache_creation_1h_input_tokens
        .min(entry.cache_creation_input_tokens);
    let five_minute_writes = entry.cache_creation_input_tokens - one_hour_writes;
    Some(
        entry.input_tokens as f64 / million * price.input
            + entry.output_tokens as f64 / million * price.output
            + five_minute_writes as f64 / million * price.cache_write
            + one_hour_writes as f64 / million * price.input * ONE_HOUR_WRITE
            + entry.cache_read_input_tokens as f64 / million * price.cache_read,
    )
}

/// A cost sum, and whether any entry in it couldn't be priced - in which
/// case the real total is higher, and the display says so.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CostTotal {
    pub dollars: f64,
    pub partial: bool,
}

impl CostTotal {
    pub fn of<'a>(entries: impl IntoIterator<Item = &'a LogEntry>) -> Self {
        entries
            .into_iter()
            .fold(CostTotal::default(), |total, entry| {
                match cost_for_entry(entry) {
                    Some(dollars) => CostTotal {
                        dollars: total.dollars + dollars,
                        ..total
                    },
                    None => CostTotal {
                        partial: true,
                        ..total
                    },
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    const M: u64 = 1_000_000;

    fn entry(model: &str, input: u64, output: u64, cache_write: u64, cache_read: u64) -> LogEntry {
        LogEntry {
            timestamp: Utc::now(),
            model: model.into(),
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: cache_write,
            cache_creation_1h_input_tokens: 0,
            cache_read_input_tokens: cache_read,
        }
    }

    fn rates(input: f64, output: f64, cache_read: f64) -> PriceTable {
        PriceTable {
            input,
            output,
            cache_write: input * 1.25,
            cache_read,
        }
    }

    /// Anthropic's published per-MTok rates (input / output / cache read;
    /// 5-minute cache writes are 1.25x input) - checked against the
    /// claude-api reference, 2026-10-05.
    #[test]
    fn current_models_are_priced_at_their_published_rates() {
        for (model, expected) in [
            ("claude-fable-5-1", rates(10.0, 50.0, 0.25)),
            ("claude-fable-5", rates(10.0, 50.0, 1.0)),
            ("claude-opus-5-5", rates(4.0, 20.0, 0.20)),
            ("claude-opus-5", rates(5.0, 25.0, 0.50)),
            ("claude-opus-4-8", rates(5.0, 25.0, 0.50)),
            ("claude-opus-4-7", rates(5.0, 25.0, 0.50)),
            ("claude-opus-4-6", rates(5.0, 25.0, 0.50)),
            ("claude-sonnet-5-5", rates(2.0, 10.0, 0.20)),
            ("claude-sonnet-5", rates(2.0, 10.0, 0.20)),
            ("claude-sonnet-4-6", rates(3.0, 15.0, 0.30)),
            ("claude-haiku-4-5", rates(1.0, 5.0, 0.10)),
        ] {
            assert_eq!(price_for_model(model), Some(expected), "{model}");
        }
    }

    /// Model ids are matched exactly: a newer generation of the same
    /// family can cost a different amount (Opus 5.5 is cheaper than Opus 5).
    #[test]
    fn a_family_name_alone_is_not_a_price() {
        for model in [
            "opus",
            "sonnet",
            "haiku",
            "claude-opus-9",
            "gpt-4",
            "<synthetic>",
        ] {
            assert_eq!(price_for_model(model), None, "{model}");
        }
    }

    #[test]
    fn a_dated_snapshot_id_is_priced_as_its_model() {
        assert_eq!(
            price_for_model("claude-haiku-4-5-20251001"),
            price_for_model("claude-haiku-4-5")
        );
        assert!(price_for_model("claude-haiku-4-5").is_some());
    }

    /// Every model id seen in this machine's real transcripts (2026-10-05)
    /// must get a price, or Cost silently drops its usage.
    #[test]
    fn every_model_seen_in_real_transcripts_is_priced() {
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-haiku-4-5-20251001",
            "claude-opus-5",
            "claude-fable-5-1",
        ] {
            assert!(price_for_model(model).is_some(), "{model} is unpriced");
        }
    }

    #[test]
    fn cost_for_entry_prices_each_token_category_at_its_own_rate() {
        // Opus 5.5, 1M of each: $4 input + $20 output + $5 cache write
        // (1.25x input) + $0.20 cache read.
        let e = entry("claude-opus-5-5", M, M, M, M);
        assert_eq!(cost_for_entry(&e), Some(29.2));
        // Each category on its own, so a swapped rate can't cancel out.
        assert_eq!(
            cost_for_entry(&entry("claude-opus-5-5", M, 0, 0, 0)),
            Some(4.0)
        );
        assert_eq!(
            cost_for_entry(&entry("claude-opus-5-5", 0, M, 0, 0)),
            Some(20.0)
        );
        assert_eq!(
            cost_for_entry(&entry("claude-opus-5-5", 0, 0, M, 0)),
            Some(5.0)
        );
        assert_eq!(
            cost_for_entry(&entry("claude-opus-5-5", 0, 0, 0, M)),
            Some(0.2)
        );
    }

    #[test]
    fn one_hour_cache_writes_cost_twice_the_input_rate() {
        let mut e = entry("claude-opus-5-5", 0, 0, 3 * M, 0);
        e.cache_creation_1h_input_tokens = M;
        // 2M 5-minute writes at $5 + 1M 1-hour writes at $8.
        assert_eq!(cost_for_entry(&e), Some(18.0));
    }

    #[test]
    fn cost_for_entry_is_none_for_an_unrecognized_model() {
        let e = entry("gpt-4", M, 0, 0, 0);
        assert_eq!(cost_for_entry(&e), None);
    }

    #[test]
    fn cost_for_entry_scales_linearly_with_token_count() {
        // Sonnet 5 input is $2/M -> 500K tokens = $1.00.
        let e = entry("claude-sonnet-5", 500_000, 0, 0, 0);
        assert_eq!(cost_for_entry(&e), Some(1.0));
    }

    #[test]
    fn a_total_over_entries_flags_unpriced_ones() {
        let priced = entry("claude-sonnet-5", 500_000, 0, 0, 0);
        let unpriced = entry("some-future-model", M, 0, 0, 0);
        let all = CostTotal::of([&priced, &unpriced]);
        assert_eq!(all.dollars, 1.0);
        assert!(all.partial);
        let known = CostTotal::of([&priced]);
        assert!(!known.partial);
    }
}
