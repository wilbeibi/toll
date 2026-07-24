//! `turnpike check` — answer one question with an exit code: is spend in a
//! window at or over a budget? It reads the call database exactly like `stats`
//! and touches no daemon state. Delivery is not turnpike's job: the exit code
//! composes with whatever you already run (`... || ntfy send ...`, a coding-
//! agent hook, a shell prompt), so turnpike stays a meter, not a notifier.

use crate::cost::{call_cost, usage_from_counts};
use crate::paths::{calls_db, prices_json};
use crate::pricing::PriceTable;
use crate::record::open_db;
use anyhow::{bail, Context, Result};
use jiff::{Span, Zoned};
use rusqlite::Connection;

pub struct CheckOpts {
    /// Budget ceiling in USD (always finite and > 0; see [`parse_budget`]).
    pub budget: f64,
    /// Window label: `day` / `week` / `month` (calendar) or any `--since` form.
    pub period: String,
    pub json: bool,
    pub quiet: bool,
}

/// Split `AMOUNT` or `AMOUNT/PERIOD` (`50`, `50/day`, `300/7d`, `500/month`)
/// into a positive budget and a window label (defaulting to `day`).
pub fn parse_budget(spec: &str) -> Result<(f64, String)> {
    let (amount, period) = match spec.split_once('/') {
        Some((a, p)) => (a.trim(), p.trim()),
        None => (spec.trim(), "day"),
    };
    if period.is_empty() {
        bail!("budget period is empty in {spec:?}; try 50/day");
    }
    let budget: f64 = amount
        .parse()
        .with_context(|| format!("bad budget amount {amount:?}; expected a number like 50"))?;
    if !budget.is_finite() || budget <= 0.0 {
        bail!("budget must be a positive number, got {amount:?}");
    }
    Ok((budget, period.to_string()))
}

/// Returns `true` when spend in the window is at or over budget — the caller
/// maps that to process exit code 1.
pub fn run(opts: CheckOpts) -> Result<bool> {
    let lower = window_bound(&opts.period)?;

    let path = calls_db();
    let (spent, unpriced) = if path.exists() {
        let conn = open_db(&path)?;
        let prices = PriceTable::load(&prices_json());
        sum_spend(&conn, &prices, &lower)?
    } else {
        (0.0, 0)
    };

    let over = spent >= opts.budget;
    let remaining = opts.budget - spent;
    let pct = spent / opts.budget * 100.0;

    if opts.json {
        let v = serde_json::json!({
            "window": opts.period,
            "since": lower,
            "spent": spent,
            "budget": opts.budget,
            "pct": pct,
            "over": over,
            "remaining": remaining,
            "unpriced_calls": unpriced,
        });
        println!(
            "{}",
            serde_json::to_string(&v).expect("serializing known-valid JSON")
        );
    } else if !opts.quiet {
        let verdict = if over {
            format!("OVER by ${:.2}", -remaining)
        } else {
            "ok".to_string()
        };
        println!(
            "{}: ${:.2} / ${:.2} ({:.0}%) — {}",
            opts.period, spent, opts.budget, pct, verdict
        );
        if unpriced > 0 {
            eprintln!(
                "warning: {unpriced} calls with tokens had no price and count as $0 — \
                 real spend may be higher; run `turnpike prices pull`"
            );
        }
    }

    Ok(over)
}

/// Total USD spent since `lower`, and the count of token-bearing calls that had
/// no price (summed as $0, so a caller can warn that spend may be higher).
fn sum_spend(conn: &Connection, prices: &PriceTable, lower: &str) -> Result<(f64, i64)> {
    let mut stmt = conn.prepare(
        "SELECT model,
                COALESCE(input_tokens, 0),
                COALESCE(output_tokens, 0),
                COALESCE(cache_read_input_tokens, 0),
                COALESCE(cache_creation_input_tokens, 0),
                cost
         FROM calls
         WHERE ts >= ?1",
    )?;
    let rows = stmt.query_map([lower], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Option<f64>>(5)?,
        ))
    })?;

    let mut spent = 0.0;
    let mut unpriced = 0i64;
    for row in rows.filter_map(|r| r.ok()) {
        let (model, input, output, cache_read, cache_write, stored) = row;
        let usage = usage_from_counts(input, output, cache_read, cache_write);
        match call_cost(prices, model.as_deref(), stored, &usage) {
            Some(c) => spent += c,
            None => unpriced += 1,
        }
    }
    Ok((spent, unpriced))
}

/// Resolve a budget period to an RFC-3339 UTC lower bound. `day` / `week` /
/// `month` are calendar windows in local time (today's midnight, this ISO
/// week's Monday, this month's 1st — matching how a provider bills). Anything
/// else is handed to the `--since` grammar, so rolling windows like `7d` or
/// `24h` work too.
fn window_bound(period: &str) -> Result<String> {
    let now = Zoned::now();
    let today = now.datetime().date();
    let start = match period {
        "day" => today,
        "week" => {
            let back = i64::from(today.weekday().to_monday_zero_offset());
            today
                .checked_sub(Span::new().try_days(back)?)
                .context("resolving start of week")?
        }
        "month" => today.first_of_month(),
        other => return crate::since::lower_bound(other),
    };
    Ok(start
        .to_zoned(now.time_zone().clone())
        .context("resolving local midnight")?
        .timestamp()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    #[test]
    fn parse_budget_amount_and_period() {
        assert_eq!(parse_budget("50").unwrap(), (50.0, "day".into()));
        assert_eq!(parse_budget("50/day").unwrap(), (50.0, "day".into()));
        assert_eq!(parse_budget(" 300 / 7d ").unwrap(), (300.0, "7d".into()));
        assert_eq!(parse_budget("500/month").unwrap(), (500.0, "month".into()));
        assert_eq!(parse_budget("12.50/week").unwrap(), (12.5, "week".into()));
    }

    #[test]
    fn parse_budget_rejects_nonpositive_and_garbage() {
        for bad in ["0", "-5", "abc", "", "nan", "inf", "50/"] {
            assert!(parse_budget(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn calendar_windows_resolve_before_now_and_utc() {
        let now = Timestamp::now().to_string();
        for period in ["day", "week", "month"] {
            let bound = window_bound(period).unwrap();
            assert!(
                bound <= now,
                "{period}: {bound} should sort at/before {now}"
            );
            assert!(bound.ends_with('Z'), "{period}: {bound} must be UTC");
        }
    }

    #[test]
    fn nonkeyword_period_delegates_to_since() {
        // A rolling spec falls through to the --since grammar; garbage there
        // still errors rather than being treated as a window.
        assert!(window_bound("7d").is_ok());
        assert!(window_bound("today").is_ok());
        assert!(window_bound("yesterday").is_err());
    }

    /// Minimal fixture holding only the columns `sum_spend` reads.
    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE calls (
                ts TEXT NOT NULL, model TEXT,
                input_tokens INTEGER, output_tokens INTEGER,
                cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER,
                cost REAL
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn sum_prefers_stored_cost_flags_unpriced_and_honors_window() {
        let conn = fixture();
        // Empty price table: token-bearing rows without a stored cost are
        // unpriceable, so they must be flagged, not summed as $0.
        let prices = PriceTable::load(std::path::Path::new("/definitely/not/here.json"));

        conn.execute_batch(
            "INSERT INTO calls VALUES
                -- in window, provider-reported cost wins
                ('2026-07-20T10:00:00Z', 'gpt-x',  100, 50, 0, 0, 0.12),
                -- in window, tokens but no price -> unpriced, adds $0
                ('2026-07-20T11:00:00Z', 'mystery', 100, 50, 0, 0, NULL),
                -- in window, pure error, no tokens -> a definite $0, not unpriced
                ('2026-07-20T12:00:00Z', NULL,       0,  0, 0, 0, NULL),
                -- before the window -> excluded entirely
                ('2026-07-01T09:00:00Z', 'gpt-x',  999, 99, 0, 0, 5.00);",
        )
        .unwrap();

        let (spent, unpriced) = sum_spend(&conn, &prices, "2026-07-15T00:00:00Z").unwrap();
        assert!((spent - 0.12).abs() < 1e-9, "spent was {spent}");
        assert_eq!(unpriced, 1);
    }
}
