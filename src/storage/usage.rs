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
