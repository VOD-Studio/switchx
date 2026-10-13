//! Token usage rows from routed responses and imported Codex session logs.

use rusqlite::{Result, Row, Transaction, params};

use super::RequestRecord;
use crate::usage::TokenUsage;

/// Reads five nullable usage columns starting at `start`; a missing row is `None`.
pub(super) fn usage_columns(row: &Row<'_>, start: usize) -> Result<Option<TokenUsage>> {
    let input: Option<i64> = row.get(start)?;
    let Some(input) = input else {
        return Ok(None);
    };
    let count =
        |index: usize| -> Result<u64> { Ok(row.get::<_, i64>(start + index)?.max(0) as u64) };
    Ok(Some(TokenUsage {
        input: input.max(0) as u64,
        cached_input: count(1)?,
        cache_write: count(2)?,
        output: count(3)?,
        reasoning: count(4)?,
    }))
}

impl super::Store {
    /// A routed response is authoritative over the same response imported from a log.
    pub(super) fn put_routed_usage(
        transaction: &Transaction<'_>,
        record: &RequestRecord,
        usage: TokenUsage,
    ) -> Result<()> {
        if let Some(response_id) = &record.response_id {
            transaction.execute(
                "DELETE FROM usage_records WHERE source = 'codex_log' AND response_id = ?1",
                [response_id],
            )?;
        }
        transaction.execute(
            "INSERT OR REPLACE INTO usage_records
             (id, source, started_at_ms, provider_id, codex_provider, public_model, model,
              input_tokens, cached_input_tokens, cache_write_tokens, output_tokens, reasoning_tokens,
              duration_ms, first_event_ms, response_id, session_id)
             VALUES (?1, 'router', ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL)",
            params![
                record.id,
                record.started_at_ms,
                record.provider_id,
                record.public_model,
                record
                    .upstream_model
                    .as_ref()
                    .or(record.public_model.as_ref())
                    .map_or("", String::as_str),
                usage.input as i64,
                usage.cached_input as i64,
                usage.cache_write as i64,
                usage.output as i64,
                usage.reasoning as i64,
                record.duration_ms,
                record.first_event_ms,
                record.response_id,
            ],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSource {
    Router,
    CodexLog,
}

impl UsageSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Router => "router",
            Self::CodexLog => "codex_log",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "router" => Ok(Self::Router),
            "codex_log" => Ok(Self::CodexLog),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// One model response's usage, attributed to a SwitchX connection when known
/// and otherwise to the Codex provider key that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub id: String,
    pub source: UsageSource,
    pub started_at_ms: i64,
    pub provider_id: Option<String>,
    pub codex_provider: Option<String>,
    pub public_model: Option<String>,
    pub model: String,
    pub usage: TokenUsage,
    pub duration_ms: Option<i64>,
    pub first_event_ms: Option<i64>,
    pub response_id: Option<String>,
    pub session_id: Option<String>,
}

/// How far a session log has been read, so later scans only read appended lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLogFile {
    pub path: String,
    pub size: i64,
    pub modified_ms: i64,
    pub read_offset: i64,
    pub state: String,
}

impl super::Store {
    /// When routed usage recording began; routed log entries after it are owned by the router.
    pub fn usage_router_since_ms(&self) -> Result<i64> {
        let value: String = self.connection.query_row(
            "SELECT value FROM app_settings WHERE key = 'usage_router_since_ms'",
            [],
            |row| row.get(0),
        )?;
        value.parse().map_err(|_| rusqlite::Error::InvalidQuery)
    }

    pub fn usage_log_files(&self) -> Result<Vec<UsageLogFile>> {
        let mut statement = self
            .connection
            .prepare("SELECT path, size, modified_ms, read_offset, state FROM usage_log_files")?;
        statement
            .query_map([], |row| {
                Ok(UsageLogFile {
                    path: row.get(0)?,
                    size: row.get(1)?,
                    modified_ms: row.get(2)?,
                    read_offset: row.get(3)?,
                    state: row.get(4)?,
                })
            })?
            .collect()
    }

    /// Adds imported rows and the file's new read position atomically. Rows whose
    /// response is already recorded, by the router or another log, are skipped.
    pub fn put_imported_usage(
        &self,
        records: &[UsageRecord],
        file: &UsageLogFile,
    ) -> Result<usize> {
        let transaction = self.connection.unchecked_transaction()?;
        let mut added = 0;
        {
            let mut insert = transaction.prepare(
                "INSERT OR IGNORE INTO usage_records
                 (id, source, started_at_ms, provider_id, codex_provider, public_model, model,
                  input_tokens, cached_input_tokens, cache_write_tokens, output_tokens, reasoning_tokens,
                  duration_ms, first_event_ms, response_id, session_id)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16
                 WHERE ?15 IS NULL
                    OR NOT EXISTS (SELECT 1 FROM usage_records WHERE response_id = ?15)",
            )?;
            for record in records {
                added += insert.execute(params![
                    record.id,
                    record.source.as_str(),
                    record.started_at_ms,
                    record.provider_id,
                    record.codex_provider,
                    record.public_model,
                    record.model,
                    record.usage.input as i64,
                    record.usage.cached_input as i64,
                    record.usage.cache_write as i64,
                    record.usage.output as i64,
                    record.usage.reasoning as i64,
                    record.duration_ms,
                    record.first_event_ms,
                    record.response_id,
                    record.session_id,
                ])?;
            }
        }
        transaction.execute(
            "INSERT INTO usage_log_files (path, size, modified_ms, read_offset, state)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET size = excluded.size,
                modified_ms = excluded.modified_ms, read_offset = excluded.read_offset,
                state = excluded.state",
            params![
                file.path,
                file.size,
                file.modified_ms,
                file.read_offset,
                file.state
            ],
        )?;
        transaction.commit()?;
        Ok(added)
    }

    /// Forgets every imported row and read position so logs can be imported again.
    pub fn clear_imported_usage(&self) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute("DELETE FROM usage_records WHERE source = 'codex_log'", [])?;
        transaction.execute("DELETE FROM usage_log_files", [])?;
        transaction.commit()
    }

    /// Usage rows started in `[from_ms, to_ms)`, newest first.
    pub fn usage_records(&self, from_ms: i64, to_ms: i64) -> Result<Vec<UsageRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, source, started_at_ms, provider_id, codex_provider, public_model, model,
                    input_tokens, cached_input_tokens, cache_write_tokens, output_tokens,
                    reasoning_tokens, duration_ms, first_event_ms, response_id, session_id
             FROM usage_records WHERE started_at_ms >= ?1 AND started_at_ms < ?2
             ORDER BY started_at_ms DESC, id DESC",
        )?;
        statement
            .query_map([from_ms, to_ms], |row| {
                let source: String = row.get(1)?;
                Ok(UsageRecord {
                    id: row.get(0)?,
                    source: UsageSource::parse(&source)?,
                    started_at_ms: row.get(2)?,
                    provider_id: row.get(3)?,
                    codex_provider: row.get(4)?,
                    public_model: row.get(5)?,
                    model: row.get(6)?,
                    usage: usage_columns(row, 7)?.unwrap_or_default(),
                    duration_ms: row.get(12)?,
                    first_event_ms: row.get(13)?,
                    response_id: row.get(14)?,
                    session_id: row.get(15)?,
                })
            })?
            .collect()
    }
}
