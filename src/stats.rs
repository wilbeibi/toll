use crate::paths::calls_db;
use crate::record::open_db;
use anyhow::Result;

pub struct StatsOpts {
    pub by_model: bool,
}

struct Row {
    key: String,
    calls: i64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    errors: i64,
}

pub fn run(opts: StatsOpts) -> Result<()> {
    let path = calls_db();
    if !path.exists() {
        println!("No records yet at {}", path.display());
        return Ok(());
    }

    let conn = open_db(&path)?;
    let col = if opts.by_model {
        "COALESCE(model, 'unknown')"
    } else {
        "provider"
    };

    let sql = format!(
        "SELECT {col} as grp,
                COUNT(*) as calls,
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cache_read_input_tokens), 0),
                COALESCE(SUM(cache_creation_input_tokens), 0),
                SUM(CASE WHEN error_kind IS NOT NULL THEN 1 ELSE 0 END)
         FROM calls
         GROUP BY grp
         ORDER BY grp"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok(Row {
                key: r.get(0)?,
                calls: r.get(1)?,
                input: r.get(2)?,
                output: r.get(3)?,
                cache_read: r.get(4)?,
                cache_write: r.get(5)?,
                errors: r.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    if rows.is_empty() {
        println!("No records in {}", path.display());
        return Ok(());
    }

    let has_cache_write = rows.iter().any(|r| r.cache_write > 0);
    let key_label = if opts.by_model { "model" } else { "provider" };

    let w_key = rows
        .iter()
        .map(|r| r.key.len())
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

    print_row(
        &headers.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &widths,
    );
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    print_row(&sep, &widths);

    for row in &rows {
        let mut cols = vec![
            row.key.clone(),
            row.calls.to_string(),
            row.input.to_string(),
            row.output.to_string(),
            row.cache_read.to_string(),
        ];
        if has_cache_write {
            cols.push(row.cache_write.to_string());
        }
        cols.push(row.errors.to_string());
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
