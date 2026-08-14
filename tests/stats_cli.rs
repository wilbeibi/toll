//! CLI-level tests for `stats --by-tool`: the unified attribution chain
//! (declared header > observed process > User-Agent) must group rows into the
//! same buckets the read-side kernel defines, on both current-schema and
//! pre-`client_source` databases (readers never migrate).

use rusqlite::Connection;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDataDir(PathBuf);

impl TestDataDir {
    fn new() -> Self {
        let id = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("turnpike-stats-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(path.join("turnpike")).unwrap();
        Self(path)
    }

    fn db_path(&self) -> PathBuf {
        self.0.join("turnpike/calls.db")
    }
}

impl Drop for TestDataDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every column `stats` references, matching the real schema minus indexes.
fn create_full_db(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE calls (
            id TEXT PRIMARY KEY, ts TEXT NOT NULL, provider TEXT NOT NULL,
            model TEXT, status INTEGER, latency_ms INTEGER NOT NULL,
            ttft_ms INTEGER, stream INTEGER, input_tokens INTEGER,
            output_tokens INTEGER, cache_read_input_tokens INTEGER,
            cache_creation_input_tokens INTEGER, reasoning_output_tokens INTEGER,
            error_kind TEXT, error_message TEXT, cost REAL, client TEXT,
            endpoint TEXT, anomaly TEXT, raw_usage TEXT, peer_exe TEXT,
            client_source TEXT
        );",
    )
    .unwrap();
    conn
}

/// A database from before `client_source` (and before `peer_exe`): both
/// guards must substitute NULL and grouping must keep the legacy order.
fn create_legacy_db(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE calls (
            ts TEXT NOT NULL, provider TEXT NOT NULL, model TEXT,
            status INTEGER, latency_ms INTEGER NOT NULL, error_kind TEXT,
            cost REAL, client TEXT, input_tokens INTEGER, output_tokens INTEGER,
            cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER
        );",
    )
    .unwrap();
    conn
}

fn run_stats(data: &TestDataDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_turnpike"))
        .arg("stats")
        .args(args)
        .env("XDG_DATA_HOME", &data.0)
        .output()
        .unwrap()
}

/// Run `stats --by-tool --json` and return tool => calls as a map.
fn tool_buckets(output: &Output) -> std::collections::BTreeMap<String, i64> {
    assert!(
        output.status.success(),
        "stats failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    rows.into_iter()
        .map(|r| {
            (
                r["tool"].as_str().unwrap().to_string(),
                r["calls"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn insert_full(
    conn: &Connection,
    ts: &str,
    client: Option<&str>,
    source: Option<&str>,
    exe: Option<&str>,
) {
    conn.execute(
        "INSERT INTO calls (ts, provider, model, status, latency_ms, error_kind, cost,
            client, client_source, peer_exe, input_tokens, output_tokens,
            cache_read_input_tokens, cache_creation_input_tokens)
         VALUES (?1, 'openai', 'gpt-x', 200, 100, NULL, NULL, ?2, ?3, ?4, 10, 5, 0, 0)",
        rusqlite::params![ts, client, source, exe],
    )
    .unwrap();
}

#[test]
fn by_tool_groups_by_unified_attribution_chain() {
    let data = TestDataDir::new();
    let conn = create_full_db(&data.db_path());

    // Declared header wins over everything.
    insert_full(
        &conn,
        "2026-08-01T10:00:00Z",
        Some("opencode"),
        Some("header"),
        Some("/usr/bin/node"),
    );
    // Observed process beats a bare runtime UA like "node".
    insert_full(
        &conn,
        "2026-08-01T10:01:00Z",
        Some("node"),
        Some("ua"),
        Some("/usr/bin/python"),
    );
    // UA fallback with no resolvable process stays the UA.
    insert_full(
        &conn,
        "2026-08-01T10:02:00Z",
        Some("curl/8.5.0"),
        Some("ua"),
        None,
    );
    // Legacy row (NULL provenance): historical client-then-exe order.
    insert_full(
        &conn,
        "2026-08-01T10:03:00Z",
        None,
        None,
        Some("/usr/bin/cat"),
    );
    // Nothing recorded at all is one "unknown" bucket, never dropped.
    insert_full(&conn, "2026-08-01T10:04:00Z", None, None, None);

    let output = run_stats(&data, &["--by-tool", "--json"]);
    let buckets = tool_buckets(&output);

    assert_eq!(buckets.get("opencode"), Some(&1));
    assert_eq!(buckets.get("/usr/bin/python"), Some(&1));
    assert_eq!(buckets.get("curl/8.5.0"), Some(&1));
    assert_eq!(buckets.get("/usr/bin/cat"), Some(&1));
    assert_eq!(buckets.get("unknown"), Some(&1));
}

#[test]
fn by_tool_reads_pre_client_source_database() {
    let data = TestDataDir::new();
    let conn = create_legacy_db(&data.db_path());
    conn.execute(
        "INSERT INTO calls (ts, provider, model, status, latency_ms, error_kind, cost,
            client, input_tokens, output_tokens, cache_read_input_tokens,
            cache_creation_input_tokens)
         VALUES ('2026-08-01T10:00:00Z', 'openai', 'gpt-x', 200, 100, NULL, NULL,
            'agent-x', 10, 5, 0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO calls (ts, provider, model, status, latency_ms, error_kind, cost,
            client, input_tokens, output_tokens, cache_read_input_tokens,
            cache_creation_input_tokens)
         VALUES ('2026-08-01T10:01:00Z', 'openai', 'gpt-x', 200, 100, NULL, NULL,
            NULL, 10, 5, 0, 0)",
        [],
    )
    .unwrap();

    let output = run_stats(&data, &["--by-tool", "--json"]);
    let buckets = tool_buckets(&output);

    // Legacy rows keep the historical COALESCE order: client wins, and
    // absent attribution is "unknown" rather than a dropped row.
    assert_eq!(buckets.get("agent-x"), Some(&1));
    assert_eq!(buckets.get("unknown"), Some(&1));
}

#[test]
fn other_group_modes_still_run_on_full_schema() {
    // The widened SELECT must not disturb the pre-existing modes.
    let data = TestDataDir::new();
    let conn = create_full_db(&data.db_path());
    insert_full(
        &conn,
        "2026-08-01T10:00:00Z",
        Some("opencode"),
        Some("header"),
        Some("/usr/bin/node"),
    );

    // The widened SELECT must not disturb the pre-existing modes: each
    // mode keeps its key, groups the single row, and counts it once.
    for (args, key, want) in [
        (vec!["--json"], "provider", "openai"),
        (vec!["--by-model", "--json"], "model", "gpt-x"),
        (vec!["--by-client", "--json"], "client", "opencode"),
        (vec!["--by-exe", "--json"], "exe", "/usr/bin/node"),
        (vec!["--by-day", "--json"], "day", "2026-08-01"),
    ] {
        let output = run_stats(&data, &args);
        assert!(
            output.status.success(),
            "args {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rows.len(), 1, "{args:?}");
        assert_eq!(rows[0][key], want, "{args:?}");
        assert_eq!(rows[0]["calls"], 1, "{args:?}");
    }
}
