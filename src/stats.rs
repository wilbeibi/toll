use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::{open_db, Usage};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

pub struct StatsOpts {
    pub by_model: bool,
    pub by_client: bool,
    pub by_day: bool,
    pub since: Option<String>,
}

struct Agg {
    key: String,
    calls: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    errors: i64,
    cost: f64,
}

pub fn run(opts: StatsOpts) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let prices = PriceTable::load(&prices_json());

    // col/key_label are always one of four known literal pairs, never user input.
    let (col, key_label) = if opts.by_model {
        ("COALESCE(model, 'unknown')", "model")
    } else if opts.by_client {
        ("COALESCE(client, 'unknown')", "client")
    } else if opts.by_day {
        ("substr(ts, 1, 10)", "day")
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
                (error_kind IS NOT NULL OR COALESCE(status, 0) >= 400)
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
        ))
    })?;

    for row in rows.filter_map(|r| r.ok()) {
        let (grp, model, input, output, cache_read, cache_write, stored_cost, is_error) = row;

        let call_cost = match stored_cost {
            Some(c) => c,
            None => {
                let usage = Usage {
                    input_tokens: if input > 0 { Some(input as u64) } else { None },
                    output_tokens: if output > 0 {
                        Some(output as u64)
                    } else {
                        None
                    },
                    cache_read_input_tokens: if cache_read > 0 {
                        Some(cache_read as u64)
                    } else {
                        None
                    },
                    cache_creation_input_tokens: if cache_write > 0 {
                        Some(cache_write as u64)
                    } else {
                        None
                    },
                    ..Default::default()
                };
                match prices.compute(model.as_deref(), &usage) {
                    Some(c) => c,
                    None => {
                        // Token-bearing rows summed at $0 would silently
                        // under-report; count them and say so below. Rows
                        // with no tokens (pure errors) genuinely cost nothing.
                        if input > 0 || output > 0 {
                            unpriced_calls += 1;
                            *unpriced_models
                                .entry(model.clone().unwrap_or_else(|| "unknown".into()))
                                .or_insert(0) += 1;
                        }
                        0.0
                    }
                }
            }
        };

        let e = groups.entry(grp.clone()).or_insert(Agg {
            key: grp,
            calls: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            errors: 0,
            cost: 0.0,
        });
        e.calls += 1;
        e.input += input;
        e.output += output;
        e.cache_read += cache_read;
        e.cache_write += cache_write;
        if is_error {
            e.errors += 1;
        }
        e.cost += call_cost;
    }

    if groups.is_empty() {
        println!("No records in {}", path.display());
        return Ok(());
    }

    let has_cache_write = groups.values().any(|r| r.cache_write > 0);

    // Client keys are raw User-Agent strings and can be long; group on the
    // full value, truncate only for display.
    let shown: Vec<(String, &Agg)> = groups.values().map(|a| (display_key(&a.key), a)).collect();

    let w_key = shown
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(8)
        .max(key_label.len());

    let mut headers = vec![key_label, "calls", "input", "output", "cache_read"];
    let mut widths = vec![w_key, 5, 7, 7, 10];
    if has_cache_write {
        headers.push("cache_write");
        widths.push(11);
    }
    headers.push("errors");
    widths.push(6);
    headers.push("cost_usd");
    widths.push(10);

    print_row(
        &headers.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &widths,
    );
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    print_row(&sep, &widths);

    for (key, agg) in &shown {
        let mut cols = vec![
            key.clone(),
            agg.calls.to_string(),
            agg.input.to_string(),
            agg.output.to_string(),
            agg.cache_read.to_string(),
        ];
        if has_cache_write {
            cols.push(agg.cache_write.to_string());
        }
        cols.push(agg.errors.to_string());
        cols.push(format!("{:.4}", agg.cost));
        print_row(&cols, &widths);
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
             (top: {}); {age} — run `toll prices pull`",
            names.join(", ")
        );
    }

    Ok(())
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
