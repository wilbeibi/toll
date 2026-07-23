//! The per-call cost kernel shared by `stats` and `check`, so the two can
//! never drift on how a call is priced. Both prefer the provider-reported cost
//! and fall back to the local price table; both treat a token-bearing call with
//! no price as *unknown*, never a confident $0.

use crate::pricing::PriceTable;
use crate::record::Usage;

/// Build a `Usage` for cost accounting from the four token columns as they are
/// stored (`i64`, `0` meaning absent). `cost` is deliberately left `None` so
/// pricing computes from tokens; any provider-reported cost is supplied
/// separately to [`call_cost`] as `stored`.
pub fn usage_from_counts(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Usage {
    Usage {
        input_tokens: (input > 0).then_some(input as u64),
        output_tokens: (output > 0).then_some(output as u64),
        cache_read_input_tokens: (cache_read > 0).then_some(cache_read as u64),
        cache_creation_input_tokens: (cache_write > 0).then_some(cache_write as u64),
        ..Default::default()
    }
}

/// The billable cost of one call. Prefer the provider-reported `stored` cost;
/// otherwise price the tokens from the local table.
///
/// `None` means the call carried tokens but no price was found — the caller
/// decides how to surface that, and it must **not** be summed as a confident
/// $0 (which would silently under-report spend). A call with no tokens costs a
/// definite `Some(0.0)`.
pub fn call_cost(
    prices: &PriceTable,
    model: Option<&str>,
    stored: Option<f64>,
    usage: &Usage,
) -> Option<f64> {
    if let Some(c) = stored {
        return Some(c);
    }
    if let Some(c) = prices.compute(model, usage) {
        return Some(c);
    }
    if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
        None
    } else {
        Some(0.0)
    }
}
