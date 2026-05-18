use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::{open_db, Usage};
use anyhow::Result;
use std::collections::BTreeMap;

pub struct StatsOpts {
    pub by_model: bool,
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

    // col is always one of two known literal strings, never user input.
    let col = if opts.by_model {
        "COALESCE(model, 'unknown')"
    } else {
        "provider"
    };

    let sql = format!(
        "SELECT {col} as grp, model,
                COALESCE(input_tokens, 0),
                COALESCE(output_tokens, 0),
                COALESCE(cache_read_input_tokens, 0),
                COALESCE(cache_creation_input_tokens, 0),
                cost,
                error_kind IS NOT NULL
         FROM calls"
    );

    let mut stmt = conn.prepare(&sql)?;

    // Accumulate per group. BTreeMap keeps keys sorted.
    let mut groups: BTreeMap<String, Agg> = BTreeMap::new();

    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,           // group key
            r.get::<_, Option<String>>(1)?,   // model (for price lookup)
            r.get::<_, i64>(2)?,              // input_tokens
            r.get::<_, i64>(3)?,              // output_tokens
            r.get::<_, i64>(4)?,              // cache_read
            r.get::<_, i64>(5)?,              // cache_creation
            r.get::<_, Option<f64>>(6)?,      // stored cost (provider-reported)
            r.get::<_, bool>(7)?,             // is_error
        ))
    })?;

    for row in rows.filter_map(|r| r.ok()) {
        let (grp, model, input, output, cache_read, cache_write, stored_cost, is_error) = row;

        let call_cost = stored_cost.unwrap_or_else(|| {
            let usage = Usage {
                input_tokens: if input > 0 { Some(input as u64) } else { None },
                output_tokens: if output > 0 { Some(output as u64) } else { None },
                cache_read_input_tokens: if cache_read > 0 { Some(cache_read as u64) } else { None },
                cache_creation_input_tokens: if cache_write > 0 { Some(cache_write as u64) } else { None },
                ..Default::default()
            };
            prices.compute(model.as_deref(), &usage).unwrap_or(0.0)
        });

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
    let key_label = if opts.by_model { "model" } else { "provider" };

    let w_key = groups
        .keys()
        .map(|k| k.len())
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

    for agg in groups.values() {
        let mut cols = vec![
            agg.key.clone(),
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

    Ok(())
}

fn print_row(cols: &[String], widths: &[usize]) {
    let parts: Vec<String> = cols
        .iter()
        .zip(widths.iter())
        .map(|(c, w)| format!("{c:<w$}"))
        .collect();
    println!("{}", parts.join("  "));
}
