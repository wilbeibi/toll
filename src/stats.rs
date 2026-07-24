use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::open_db;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

pub struct StatsOpts {
    pub by_model: bool,
    pub by_client: bool,
    pub by_day: bool,
    pub by_exe: bool,
    pub since: Option<String>,
    pub json: bool,
}

struct Agg {
    key: String,
    calls: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    /// Denominator for the cache-hit rate, respecting each model's cache
    /// accounting shape (subset vs additive; see pricing::cache_in_input).
    cache_denom: i64,
    errors: i64,
    cost: f64,
    lats: Vec<i64>,
}

pub fn run(opts: StatsOpts) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let prices = PriceTable::load(&prices_json());

    // col/key_label are always one of five known literal pairs, never user input.
    let (col, key_label) = if opts.by_model {
        ("COALESCE(model, 'unknown')", "model")
    } else if opts.by_client {
        ("COALESCE(client, 'unknown')", "client")
    } else if opts.by_day {
        ("substr(ts, 1, 10)", "day")
    } else if opts.by_exe {
        ("COALESCE(peer_exe, 'unknown')", "exe")
    } else {
        ("provider", "provider")
    };

    // Every stored ts sorts >= "", so the no-filter case binds an empty bound.
    let lower = match &opts.since {
        Some(spec) => crate::since::lower_bound(spec)?,
        None => String::new(),
    };

    let sql = format!(
        "SELECT {col} as grp, model,
                COALESCE(input_tokens, 0),
                COALESCE(output_tokens, 0),
                COALESCE(cache_read_input_tokens, 0),
                COALESCE(cache_creation_input_tokens, 0),
                cost,
                (error_kind IS NOT NULL OR COALESCE(status, 0) >= 400),
                latency_ms
         FROM calls
         WHERE ts >= ?1"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Accumulate per group. BTreeMap keeps keys sorted.
    let mut groups: BTreeMap<String, Agg> = BTreeMap::new();
    let mut unpriced_calls: i64 = 0;
    let mut unpriced_models: BTreeMap<String, i64> = BTreeMap::new();

    let rows = stmt.query_map([&lower], |r| {
        Ok((
            r.get::<_, String>(0)?,         // group key
            r.get::<_, Option<String>>(1)?, // model (for price lookup)
            r.get::<_, i64>(2)?,            // input_tokens
            r.get::<_, i64>(3)?,            // output_tokens
            r.get::<_, i64>(4)?,            // cache_read
            r.get::<_, i64>(5)?,            // cache_creation
            r.get::<_, Option<f64>>(6)?,    // stored cost (provider-reported)
            r.get::<_, bool>(7)?,           // is_error (transport or HTTP >= 400)
            r.get::<_, i64>(8)?,            // latency_ms
        ))
    })?;

    for row in rows.filter_map(|r| r.ok()) {
        let (grp, model, input, output, cache_read, cache_write, stored_cost, is_error, lat) = row;

        let usage = crate::cost::usage_from_counts(input, output, cache_read, cache_write);
        let call_cost = match crate::cost::call_cost(&prices, model.as_deref(), stored_cost, &usage)
        {
            Some(c) => c,
            None => {
                // Token-bearing rows with no price would silently under-report;
                // count them and warn below. Rows with no tokens (pure errors)
                // price to a definite $0 and never reach this arm.
                unpriced_calls += 1;
                *unpriced_models
                    .entry(model.clone().unwrap_or_else(|| "unknown".into()))
                    .or_insert(0) += 1;
                0.0
            }
        };

        // Subset-style caches (OpenAI/DeepSeek) count hits against input;
        // additive caches (Anthropic) against input + cache fields.
        let cache_denom = if prices.cache_in_input(model.as_deref()).unwrap_or(true) {
            input
        } else {
            input + cache_read + cache_write
        };

        let e = groups.entry(grp.clone()).or_insert(Agg {
            key: grp,
            calls: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_denom: 0,
            errors: 0,
            cost: 0.0,
            lats: Vec::new(),
        });
        e.calls += 1;
        e.input += input;
        e.output += output;
        e.cache_read += cache_read;
        e.cache_write += cache_write;
        e.cache_denom += cache_denom;
        if is_error {
            e.errors += 1;
        }
        e.cost += call_cost;
        e.lats.push(lat);
    }

    if groups.is_empty() {
        println!("No records in {}", path.display());
        return Ok(());
    }

    if opts.json {
        print_json(key_label, &mut groups);
    } else {
        print_table(key_label, &mut groups);
    }

    if unpriced_calls > 0 {
        let mut tops: Vec<(String, i64)> = unpriced_models.into_iter().collect();
        tops.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let names: Vec<String> = tops
            .iter()
            .take(3)
            .map(|(m, c)| format!("{m} ({c})"))
            .collect();
        let age = match price_age_days(&prices_json()) {
            Some(0) => "price table pulled today".to_string(),
            Some(d) => format!("price table is {d} days old"),
            None => "no price table found".to_string(),
        };
        eprintln!(
            "\nwarning: {unpriced_calls} calls with tokens had no price and sum as $0 \
             (top: {}); {age} — run `turnpike prices pull`",
            names.join(", ")
        );
    }

    Ok(())
}

fn print_json(key_label: &str, groups: &mut BTreeMap<String, Agg>) {
    let rows: Vec<serde_json::Value> = groups
        .values_mut()
        .map(|a| {
            let (p50, p95) = percentiles(&mut a.lats);
            serde_json::json!({
                key_label: a.key,
                "calls": a.calls,
                "input_tokens": a.input,
                "output_tokens": a.output,
                "cache_read_tokens": a.cache_read,
                "cache_creation_tokens": a.cache_write,
                "cache_hit_pct": cache_pct(a.cache_read, a.cache_denom),
                "errors": a.errors,
                "p50_ms": p50,
                "p95_ms": p95,
                "cost_usd": a.cost,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&rows).expect("serializing known-valid JSON")
    );
}

fn print_table(key_label: &str, groups: &mut BTreeMap<String, Agg>) {
    let has_cache_write = groups.values().any(|r| r.cache_write > 0);

    // Client keys are raw User-Agent strings and can be long; group on the
    // full value, truncate only for display.
    struct Shown {
        key: String,
        cols: Vec<String>,
    }
    let mut shown: Vec<Shown> = Vec::new();
    for a in groups.values_mut() {
        let (p50, p95) = percentiles(&mut a.lats);
        let mut cols = vec![
            a.calls.to_string(),
            a.input.to_string(),
            a.output.to_string(),
            a.cache_read.to_string(),
        ];
        if has_cache_write {
            cols.push(a.cache_write.to_string());
        }
        cols.push(
            cache_pct(a.cache_read, a.cache_denom)
                .map(|p| format!("{p:.0}%"))
                .unwrap_or_else(|| "-".into()),
        );
        cols.push(a.errors.to_string());
        cols.push(p50.to_string());
        cols.push(p95.to_string());
        cols.push(format!("{:.4}", a.cost));
        shown.push(Shown {
            key: display_key(&a.key),
            cols,
        });
    }

    let w_key = shown
        .iter()
        .map(|s| s.key.chars().count())
        .max()
        .unwrap_or(8)
        .max(key_label.len());

    let mut headers = vec![key_label, "calls", "input", "output", "cache_read"];
    let mut widths = vec![w_key, 5, 7, 7, 10];
    if has_cache_write {
        headers.push("cache_write");
        widths.push(11);
    }
    headers.push("cache%");
    widths.push(6);
    headers.push("errors");
    widths.push(6);
    headers.push("p50_ms");
    widths.push(6);
    headers.push("p95_ms");
    widths.push(6);
    headers.push("cost_usd");
    widths.push(10);

    print_row(
        &headers.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &widths,
    );
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    print_row(&sep, &widths);

    for s in &shown {
        let mut cols = vec![s.key.clone()];
        cols.extend(s.cols.iter().cloned());
        print_row(&cols, &widths);
    }
}

fn cache_pct(cache_read: i64, denom: i64) -> Option<f64> {
    if denom > 0 && cache_read > 0 {
        Some(cache_read as f64 * 100.0 / denom as f64)
    } else {
        None
    }
}

/// (p50, p95) over the group's latencies, nearest-rank. Sorts in place.
fn percentiles(lats: &mut [i64]) -> (i64, i64) {
    if lats.is_empty() {
        return (0, 0);
    }
    lats.sort_unstable();
    let at = |p: usize| lats[(lats.len() * p).div_ceil(100).max(1) - 1];
    (at(50), at(95))
}

fn display_key(k: &str) -> String {
    const MAX_CHARS: usize = 48;
    let mut chars = k.chars();
    let head: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn price_age_days(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.elapsed().ok()?.as_secs() / 86_400)
}

fn print_row(cols: &[String], widths: &[usize]) {
    let parts: Vec<String> = cols
        .iter()
        .zip(widths.iter())
        .map(|(c, w)| format!("{c:<w$}"))
        .collect();
    println!("{}", parts.join("  "));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_pick_median_and_tail() {
        let mut lats = vec![100, 200, 300, 400, 1000];
        assert_eq!(percentiles(&mut lats), (300, 1000));
        let mut one = vec![42];
        assert_eq!(percentiles(&mut one), (42, 42));
    }

    #[test]
    fn cache_pct_needs_positive_denominator() {
        assert_eq!(cache_pct(50, 100), Some(50.0));
        assert_eq!(cache_pct(0, 100), None);
        assert_eq!(cache_pct(50, 0), None);
    }
}
