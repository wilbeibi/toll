use crate::paths::calls_db;
use crate::record::open_db;
use anyhow::Result;

pub fn run(n: usize) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let mut stmt = conn.prepare(
        "SELECT ts, provider, model, status, latency_ms,
                input_tokens, output_tokens,
                cache_read_input_tokens, cache_hit, error_kind
         FROM calls
         ORDER BY rowid DESC
         LIMIT ?1",
    )?;

    let mut rows: Vec<_> = stmt
        .query_map([n as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,          // ts
                r.get::<_, String>(1)?,          // provider
                r.get::<_, Option<String>>(2)?,  // model
                r.get::<_, Option<u16>>(3)?,     // status
                r.get::<_, i64>(4)? as u64,      // latency_ms
                r.get::<_, Option<i64>>(5)?.map(|v| v as u64), // input_tokens
                r.get::<_, Option<i64>>(6)?.map(|v| v as u64), // output_tokens
                r.get::<_, Option<i64>>(7)?.map(|v| v as u64), // cache_read
                r.get::<_, Option<i64>>(8)?.map(|v| v != 0),   // cache_hit
                r.get::<_, Option<String>>(9)?,  // error_kind
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    rows.reverse(); // oldest first
    for (ts, provider, model, status, latency_ms, input, output, cache_read, cache_hit, error_kind) in rows {
        print_row(&ts, &provider, model.as_deref(), status, latency_ms, input, output, cache_read, cache_hit, error_kind.as_deref());
    }

    Ok(())
}

fn print_row(
    ts: &str,
    provider: &str,
    model: Option<&str>,
    status: Option<u16>,
    latency_ms: u64,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_hit: Option<bool>,
    error_kind: Option<&str>,
) {
    let model = model.unwrap_or("?");
    let status = status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "err".into());
    let tokens = match (input, output) {
        (Some(i), Some(o)) => format!("{i}→{o}"),
        (Some(i), None) => format!("{i}→?"),
        _ => "?".into(),
    };
    let cache = if cache_hit.unwrap_or(false) {
        format!(" cache_read={}", cache_read.unwrap_or(0))
    } else {
        String::new()
    };
    let err = error_kind
        .map(|k| format!(" ERROR={k}"))
        .unwrap_or_default();
    println!(
        "[{}] {} {} {} {}ms tokens={}{}{}",
        ts, provider, model, status, latency_ms, tokens, cache, err,
    );
}
