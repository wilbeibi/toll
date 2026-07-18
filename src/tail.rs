use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::{open_db, Usage};
use anyhow::Result;
use rusqlite::params;

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
    client: Option<String>,
}

pub fn run(n: usize, since: Option<String>) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let prices = PriceTable::load(&prices_json());

    // Every stored ts sorts >= "", so the no-filter case binds an empty bound.
    let lower = match &since {
        Some(spec) => crate::since::lower_bound(spec)?,
        None => String::new(),
    };

    let mut stmt = conn.prepare(
        "SELECT ts, provider, model, status, latency_ms,
                input_tokens, output_tokens,
                cache_read_input_tokens, cache_creation_input_tokens,
                error_kind, cost, client
         FROM calls
         WHERE ts >= ?1
         ORDER BY rowid DESC
         LIMIT ?2",
    )?;

    let mut rows: Vec<Row> = stmt
        .query_map(params![lower, n as i64], |r| {
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
                client: r.get(11)?,
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
    // Transport errors carry error_kind; HTTP-level failures only a status.
    let err = match (r.error_kind.as_deref(), r.status) {
        (Some(k), _) => format!(" ERROR={k}"),
        (None, Some(429)) => " ERROR=rate_limit".to_string(),
        (None, Some(s)) if s >= 400 => format!(" ERROR=http_{s}"),
        _ => String::new(),
    };
    let client = r
        .client
        .as_deref()
        .map(|c| format!(" client={}", short_client(c)))
        .unwrap_or_default();
    println!(
        "[{}] {} {} {} {}ms tokens={}{}{}{}{}",
        r.ts, r.provider, model, status, r.latency_ms, tokens, cache, cost, err, client,
    );
}

/// First whitespace token of the raw client, bounded for one-line display.
fn short_client(c: &str) -> String {
    let tok = c.split_whitespace().next().unwrap_or(c);
    tok.chars().take(40).collect()
}
