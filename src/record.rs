use anyhow::Result;
use log::warn;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    /// Provider-reported billed cost in USD, when the response includes it
    /// (e.g. OpenRouter's `usage.cost`). Authoritative — no price lookup needed.
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TotalTokenSemantics {
    CacheIncludedInInput,
    CacheAdditiveToInput,
}

impl Usage {
    pub fn merge(&mut self, other: &Usage) {
        macro_rules! take {
            ($field:ident) => {
                if let Some(v) = other.$field {
                    self.$field = Some(v);
                }
            };
        }
        take!(input_tokens);
        take!(output_tokens);
        take!(total_tokens);
        take!(cache_read_input_tokens);
        take!(cache_creation_input_tokens);
        take!(reasoning_output_tokens);
        take!(cost);
    }

    pub fn cache_hit(&self) -> Option<bool> {
        self.cache_read_input_tokens.map(|n| n > 0)
    }

    pub fn total(&self, semantics: TotalTokenSemantics) -> Option<u64> {
        if let Some(t) = self.total_tokens {
            return Some(t);
        }
        let has_base = self.input_tokens.is_some() || self.output_tokens.is_some();
        let has_cache =
            self.cache_read_input_tokens.is_some() || self.cache_creation_input_tokens.is_some();
        let total = self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0);
        match semantics {
            TotalTokenSemantics::CacheIncludedInInput => has_base.then_some(total),
            TotalTokenSemantics::CacheAdditiveToInput => (has_base || has_cache).then_some(
                total
                    + self.cache_read_input_tokens.unwrap_or(0)
                    + self.cache_creation_input_tokens.unwrap_or(0),
            ),
        }
    }
}

/// Wire schema — locked. Additions must be optional and appended to end.
/// Renames/removals require a version bump + CHANGELOG entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub ts: String,
    pub provider: String,
    pub model: Option<String>,
    pub endpoint: String,
    pub method: String,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub ttft_ms: Option<u64>,
    pub stream: bool,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_hit: Option<bool>,
    pub reasoning_output_tokens: Option<u64>,
    pub request_id: Option<String>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub cost: Option<f64>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    /// Run on clean shutdown: fold the WAL back into the main DB so the file
    /// is compact at rest, and refresh the query planner's stats. The pragmas
    /// during normal operation are already write-optimal; this is the only
    /// piece that needs an explicit lifecycle hook.
    pub fn checkpoint(&self) {
        if let Err(e) = self
            .conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA optimize;")
        {
            warn!("toll: shutdown checkpoint failed: {e}");
        }
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA cache_size=-8000;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=67108864;",
        )?;
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS calls (
                id                          TEXT PRIMARY KEY,
                ts                          TEXT NOT NULL,
                provider                    TEXT NOT NULL,
                model                       TEXT,
                endpoint                    TEXT NOT NULL,
                method                      TEXT NOT NULL,
                status                      INTEGER,
                latency_ms                  INTEGER NOT NULL,
                ttft_ms                     INTEGER,
                stream                      INTEGER NOT NULL DEFAULT 0,
                input_tokens                INTEGER,
                output_tokens               INTEGER,
                total_tokens                INTEGER,
                cache_read_input_tokens     INTEGER,
                cache_creation_input_tokens INTEGER,
                cache_hit                   INTEGER,
                reasoning_output_tokens     INTEGER,
                request_id                  TEXT,
                error_kind                  TEXT,
                error_message               TEXT,
                cost                        REAL
            );
            CREATE INDEX IF NOT EXISTS idx_ts       ON calls(ts);
            CREATE INDEX IF NOT EXISTS idx_provider ON calls(provider);
            CREATE INDEX IF NOT EXISTS idx_model    ON calls(model);",
        )?;
        // Forward-compatible: ignore duplicate column errors on existing DBs.
        for sql in &[
            "ALTER TABLE calls ADD COLUMN reasoning_output_tokens INTEGER",
            "ALTER TABLE calls ADD COLUMN cost REAL",
        ] {
            if let Err(e) = self.conn.execute_batch(sql) {
                if !e.to_string().contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }
        Ok(())
    }

    pub fn insert(&self, r: &Record) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO calls (
                id, ts, provider, model, endpoint, method, status,
                latency_ms, ttft_ms, stream,
                input_tokens, output_tokens, total_tokens,
                cache_read_input_tokens, cache_creation_input_tokens,
                cache_hit, reasoning_output_tokens,
                request_id, error_kind, error_message, cost
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            params![
                r.id,
                r.ts,
                r.provider,
                r.model,
                r.endpoint,
                r.method,
                r.status,
                r.latency_ms as i64,
                r.ttft_ms.map(|v| v as i64),
                r.stream as i64,
                r.input_tokens.map(|v| v as i64),
                r.output_tokens.map(|v| v as i64),
                r.total_tokens.map(|v| v as i64),
                r.cache_read_input_tokens.map(|v| v as i64),
                r.cache_creation_input_tokens.map(|v| v as i64),
                r.cache_hit.map(|v| v as i64),
                r.reasoning_output_tokens.map(|v| v as i64),
                r.request_id,
                r.error_kind,
                r.error_message,
                r.cost,
            ],
        )?;
        Ok(())
    }
}

/// Open a WAL-mode connection for read-only consumers (stats, tail).
pub fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;",
    )?;
    Ok(conn)
}

const ERROR_PATTERNS: &[(&str, &[&str])] = &[
    ("upstream_tls", &["tls", "ssl", "certificate", "handshake"]),
    ("upstream_timeout", &["timeout", "timed out", "deadline"]),
    (
        "client_disconnect",
        &["client disconnect", "connection reset", "broken pipe"],
    ),
    (
        "upstream_connect",
        &["connect", "refused", "unreachable", "no route", "dns"],
    ),
];

pub fn classify_error(message: &str) -> &'static str {
    let low = message.to_lowercase();
    for (kind, needles) in ERROR_PATTERNS {
        if needles.iter().any(|n| low.contains(n)) {
            return kind;
        }
    }
    "other"
}

#[cfg(test)]
impl Store {
    fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init()?;
        Ok(store)
    }

    fn count(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0))
            .unwrap_or(0)
    }

    fn get_by_id(&self, id: &str) -> Option<Record> {
        self.conn
            .query_row(
                "SELECT id, ts, provider, model, endpoint, method, status,
                        latency_ms, ttft_ms, stream,
                        input_tokens, output_tokens, total_tokens,
                        cache_read_input_tokens, cache_creation_input_tokens,
                        cache_hit, reasoning_output_tokens,
                        request_id, error_kind, error_message, cost
                 FROM calls WHERE id = ?1",
                [id],
                |row| {
                    Ok(Record {
                        id: row.get(0)?,
                        ts: row.get(1)?,
                        provider: row.get(2)?,
                        model: row.get(3)?,
                        endpoint: row.get(4)?,
                        method: row.get(5)?,
                        status: row.get::<_, Option<u16>>(6)?,
                        latency_ms: row.get::<_, i64>(7)? as u64,
                        ttft_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                        stream: row.get::<_, i64>(9)? != 0,
                        input_tokens: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                        output_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                        total_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                        cache_read_input_tokens: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                        cache_creation_input_tokens: row
                            .get::<_, Option<i64>>(14)?
                            .map(|v| v as u64),
                        cache_hit: row.get::<_, Option<i64>>(15)?.map(|v| v != 0),
                        reasoning_output_tokens: row.get::<_, Option<i64>>(16)?.map(|v| v as u64),
                        request_id: row.get(17)?,
                        error_kind: row.get(18)?,
                        error_message: row.get(19)?,
                        cost: row.get::<_, Option<f64>>(20)?,
                    })
                },
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_tls() {
        assert_eq!(classify_error("TLS handshake failed"), "upstream_tls");
    }

    #[test]
    fn classify_timeout() {
        assert_eq!(classify_error("request timed out"), "upstream_timeout");
    }

    #[test]
    fn classify_connect() {
        assert_eq!(classify_error("connection refused"), "upstream_connect");
    }

    #[test]
    fn classify_client_disconnect() {
        assert_eq!(classify_error("broken pipe"), "client_disconnect");
    }

    #[test]
    fn classify_connection_reset_is_client_not_connect() {
        assert_eq!(
            classify_error("connection reset by peer"),
            "client_disconnect"
        );
    }

    #[test]
    fn classify_other() {
        assert_eq!(classify_error("something weird"), "other");
    }

    #[test]
    fn usage_merge_last_write_wins_per_field() {
        let mut base = Usage {
            input_tokens: Some(100),
            output_tokens: None,
            ..Default::default()
        };
        let delta = Usage {
            output_tokens: Some(50),
            ..Default::default()
        };
        base.merge(&delta);
        assert_eq!(base.input_tokens, Some(100));
        assert_eq!(base.output_tokens, Some(50));
    }

    #[test]
    fn usage_cache_hit() {
        let u = Usage {
            cache_read_input_tokens: Some(100),
            ..Default::default()
        };
        assert_eq!(u.cache_hit(), Some(true));
    }

    #[test]
    fn usage_total_cache_included_in_input() {
        let u = Usage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_input_tokens: Some(3),
            ..Default::default()
        };
        assert_eq!(u.total(TotalTokenSemantics::CacheIncludedInInput), Some(15));
    }

    #[test]
    fn usage_total_cache_additive_to_input() {
        let u = Usage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_input_tokens: Some(3),
            ..Default::default()
        };
        assert_eq!(u.total(TotalTokenSemantics::CacheAdditiveToInput), Some(18));
    }

    #[test]
    fn usage_total_cache_only_is_unknown_when_cache_is_included() {
        let u = Usage {
            cache_read_input_tokens: Some(3),
            ..Default::default()
        };
        assert_eq!(u.total(TotalTokenSemantics::CacheIncludedInInput), None);
        assert_eq!(u.total(TotalTokenSemantics::CacheAdditiveToInput), Some(3));
    }

    fn sample_record(id: &str) -> Record {
        Record {
            id: id.into(),
            ts: "2024-01-01T00:00:00Z".into(),
            provider: "openai".into(),
            model: Some("gpt-4o".into()),
            endpoint: "/v1/chat/completions".into(),
            method: "POST".into(),
            status: Some(200),
            latency_ms: 800,
            ttft_ms: Some(120),
            stream: false,
            input_tokens: Some(50),
            output_tokens: Some(25),
            total_tokens: Some(75),
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            cache_hit: None,
            reasoning_output_tokens: None,
            request_id: Some("req_xyz".into()),
            error_kind: None,
            error_message: None,
            cost: None,
        }
    }

    #[test]
    fn store_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let rec = sample_record("abc123");
        store.insert(&rec).unwrap();

        let back = store.get_by_id("abc123").unwrap();
        assert_eq!(back.id, "abc123");
        assert_eq!(back.provider, "openai");
        assert_eq!(back.model.as_deref(), Some("gpt-4o"));
        assert_eq!(back.input_tokens, Some(50));
        assert_eq!(back.ttft_ms, Some(120));
        assert!(back.error_kind.is_none());
    }

    #[test]
    fn store_multiple_records() {
        let store = Store::open_in_memory().unwrap();
        store.insert(&sample_record("r1")).unwrap();
        store.insert(&sample_record("r2")).unwrap();
        assert_eq!(store.count(), 2);
    }

    #[test]
    fn store_insert_or_ignore_duplicate() {
        let store = Store::open_in_memory().unwrap();
        store.insert(&sample_record("dup")).unwrap();
        store.insert(&sample_record("dup")).unwrap(); // must not error
        assert_eq!(store.count(), 1);
    }
}
