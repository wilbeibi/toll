use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::{open_db, Usage};
use anyhow::Result;

struct Row {
    ts: String,
    provider: String,
    model: Option<String>,
    status: Option<u16>,
    latency_ms: u64,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    error_kind: Option<String>,
    stored_cost: Option<f64>,
}

pub fn run(n: usize) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let prices = PriceTable::load(&prices_json());

    let mut stmt = conn.prepare(
        "SELECT ts, provider, model, status, latency_ms,
                input_tokens, output_tokens,
                cache_read_input_tokens, cache_creation_input_tokens,
                error_kind, cost
         FROM calls
         ORDER BY rowid DESC
         LIMIT ?1",
    )?;

    let mut rows: Vec<Row> = stmt
        .query_map([n as i64], |r| {
            Ok(Row {
                ts: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                status: r.get(3)?,
                latency_ms: r.get::<_, i64>(4)? as u64,
                input: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
                output: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                cache_read: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                cache_creation: r.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                error_kind: r.get(9)?,
                stored_cost: r.get(10)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    rows.reverse(); // oldest first
    for row in &rows {
        print_row(row, &prices);
    }
    Ok(())
}

fn print_row(r: &Row, prices: &PriceTable) {
    let model = r.model.as_deref().unwrap_or("?");
    let status = r
        .status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "err".into());
    let tokens = match (r.input, r.output) {
        (Some(i), Some(o)) => format!("{i}→{o}"),
        (Some(i), None) => format!("{i}→?"),
        _ => "?".into(),
    };
    let cache_hit = r.cache_read.map(|n| n > 0).unwrap_or(false);
    let cache = if cache_hit {
        format!(" cache_read={}", r.cache_read.unwrap_or(0))
    } else {
        String::new()
    };
    let cost = r.stored_cost.or_else(|| {
        let usage = Usage {
            input_tokens: r.input,
            output_tokens: r.output,
            cache_read_input_tokens: r.cache_read,
            cache_creation_input_tokens: r.cache_creation,
            ..Default::default()
        };
        prices.compute(r.model.as_deref(), &usage)
    });
    let cost = cost.map(|c| format!(" ${c:.4}")).unwrap_or_default();
    let err = r
        .error_kind
        .as_deref()
        .map(|k| format!(" ERROR={k}"))
        .unwrap_or_default();
    println!(
        "[{}] {} {} {} {}ms tokens={}{}{}{}",
        r.ts, r.provider, model, status, r.latency_ms, tokens, cache, cost, err,
    );
}
