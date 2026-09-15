use crate::source::logs::LogEntry;

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
