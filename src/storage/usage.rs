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
            .query_map([from_ms, to_ms], usage_record)?
            .collect()
    }
}

/// The connection a usage row belongs to: a SwitchX connection (`p:ID`), a
/// Codex provider key (`c:KEY`), or unknown (`u`).
const GROUP_KEY: &str = "CASE WHEN provider_id IS NOT NULL THEN 'p:' || provider_id
    WHEN codex_provider IS NOT NULL THEN 'c:' || codex_provider ELSE 'u' END";

/// Optional single-value filters on the connection group key and model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageFilter {
    pub group: Option<String>,
    pub model: Option<String>,
}

impl UsageFilter {
    fn clause(&self) -> String {
        format!(" AND (?3 IS NULL OR ({GROUP_KEY}) = ?3) AND (?4 IS NULL OR model = ?4)")
    }
}

/// Summed usage for one local-time bucket, connection group, and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageFact {
    pub bucket: String,
    pub group: String,
    pub provider_id: Option<String>,
    pub model: String,
    pub requests: u64,
    pub usage: TokenUsage,
}

/// How rows are bucketed by local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageBucket {
    Hour,
    Day,
    /// One bucket for the whole range.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Currency {
    Usd,
    Cny,
}

impl Currency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Cny => "CNY",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "USD" => Some(Self::Usd),
            "CNY" => Some(Self::Cny),
            _ => None,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Usd => "$",
            Self::Cny => "¥",
        }
    }
}

/// Prices per million tokens, in millionths of the currency unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceRecord {
    /// Empty for a price that applies to the model on every connection.
    pub provider_id: String,
    pub model: String,
    pub currency: Currency,
    pub input_micros: u64,
    pub cached_input_micros: u64,
    pub output_micros: u64,
}

impl super::Store {
    /// Usage grouped by local-time bucket, connection, and model in `[from_ms, to_ms)`.
    pub fn usage_facts(
        &self,
        from_ms: i64,
        to_ms: i64,
        bucket: UsageBucket,
    ) -> Result<Vec<UsageFact>> {
        let format = match bucket {
            UsageBucket::Hour => "%Y-%m-%d %H",
            UsageBucket::Day => "%Y-%m-%d",
            UsageBucket::None => "",
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT strftime(?3, started_at_ms / 1000, 'unixepoch', 'localtime') AS bucket,
                    {GROUP_KEY} AS grouping, MAX(provider_id), model, COUNT(*),
                    SUM(input_tokens), SUM(cached_input_tokens), SUM(cache_write_tokens),
                    SUM(output_tokens), SUM(reasoning_tokens)
             FROM usage_records WHERE started_at_ms >= ?1 AND started_at_ms < ?2
             GROUP BY bucket, grouping, model"
        ))?;
        statement
            .query_map(params![from_ms, to_ms, format], |row| {
                Ok(UsageFact {
                    bucket: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                    group: row.get(1)?,
                    provider_id: row.get(2)?,
                    model: row.get(3)?,
                    requests: row.get::<_, i64>(4)?.max(0) as u64,
                    usage: usage_columns(row, 5)?.unwrap_or_default(),
                })
            })?
            .collect()
    }

    /// Requests and tokens by local weekday (0 = Sunday) and hour.
    pub fn usage_heat(
        &self,
        from_ms: i64,
        to_ms: i64,
        filter: &UsageFilter,
    ) -> Result<Vec<(u8, u8, u64, u64)>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT CAST(strftime('%w', started_at_ms / 1000, 'unixepoch', 'localtime') AS INTEGER),
                    CAST(strftime('%H', started_at_ms / 1000, 'unixepoch', 'localtime') AS INTEGER),
                    COUNT(*), SUM(input_tokens + output_tokens)
             FROM usage_records WHERE started_at_ms >= ?1 AND started_at_ms < ?2{}
             GROUP BY 1, 2",
            filter.clause()
        ))?;
        statement
            .query_map(params![from_ms, to_ms, filter.group, filter.model], |row| {
                Ok((
                    row.get::<_, i64>(0)?.clamp(0, 6) as u8,
                    row.get::<_, i64>(1)?.clamp(0, 23) as u8,
                    row.get::<_, i64>(2)?.max(0) as u64,
                    row.get::<_, i64>(3)?.max(0) as u64,
                ))
            })?
            .collect()
    }

    /// Output tokens per second after the first event, for routed responses.
    pub fn usage_speeds(&self, from_ms: i64, to_ms: i64, filter: &UsageFilter) -> Result<Vec<f64>> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT output_tokens * 1000.0 / (duration_ms - first_event_ms)
             FROM usage_records WHERE started_at_ms >= ?1 AND started_at_ms < ?2
                AND source = 'router' AND output_tokens > 0
                AND first_event_ms IS NOT NULL AND duration_ms - first_event_ms >= 50{}",
            filter.clause()
        ))?;
        statement
            .query_map(params![from_ms, to_ms, filter.group, filter.model], |row| {
                row.get(0)
            })?
            .collect()
    }

    /// One page of usage rows, newest first, and the number of matching rows.
    pub fn usage_page(
        &self,
        from_ms: i64,
        to_ms: i64,
        filter: &UsageFilter,
        offset: usize,
        limit: usize,
    ) -> Result<(Vec<UsageRecord>, usize)> {
        let clause = filter.clause();
        let total: i64 = self.connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM usage_records
                 WHERE started_at_ms >= ?1 AND started_at_ms < ?2{clause}"
            ),
            params![from_ms, to_ms, filter.group, filter.model],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, source, started_at_ms, provider_id, codex_provider, public_model, model,
                    input_tokens, cached_input_tokens, cache_write_tokens, output_tokens,
                    reasoning_tokens, duration_ms, first_event_ms, response_id, session_id
             FROM usage_records WHERE started_at_ms >= ?1 AND started_at_ms < ?2{clause}
             ORDER BY started_at_ms DESC, id DESC LIMIT ?5 OFFSET ?6"
        ))?;
        let rows = statement
            .query_map(
                params![
                    from_ms,
                    to_ms,
                    filter.group,
                    filter.model,
                    limit.min(1_000) as i64,
                    offset as i64
                ],
                usage_record,
            )?
            .collect::<Result<_>>()?;
        Ok((rows, total.max(0) as usize))
    }

    /// Whether Codex session logs are scanned automatically; on unless turned off.
    pub fn usage_log_import(&self) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'usage_log_import'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() != Some("off"))
    }

    pub fn set_usage_log_import(&self, enabled: bool) -> Result<()> {
        self.connection.execute(
            "INSERT INTO app_settings (key, value) VALUES ('usage_log_import', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [if enabled { "on" } else { "off" }],
        )?;
        Ok(())
    }

    /// Copies the whole database, credentials included, to a new file beside it.
    pub fn backup_to(&self, path: &std::path::Path) -> Result<()> {
        let path = path.to_str().ok_or(rusqlite::Error::InvalidQuery)?;
        self.connection.execute("VACUUM INTO ?1", [path])?;
        Ok(())
    }

    /// Every model with recorded usage: requests and input plus output tokens.
    pub fn usage_models(&self) -> Result<Vec<(String, u64, u64)>> {
        let mut statement = self.connection.prepare(
            "SELECT model, COUNT(*), SUM(input_tokens + output_tokens)
             FROM usage_records WHERE model != '' GROUP BY model",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?
            .collect()
    }

    /// The earliest recorded usage, for an all-time range.
    pub fn first_usage_ms(&self) -> Result<Option<i64>> {
        self.connection
            .query_row("SELECT MIN(started_at_ms) FROM usage_records", [], |row| {
                row.get(0)
            })
    }

    pub fn model_prices(&self) -> Result<Vec<PriceRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT provider_id, model, currency, input_micros, cached_input_micros, output_micros
             FROM model_prices ORDER BY model, provider_id",
        )?;
        statement
            .query_map([], |row| {
                let currency: String = row.get(2)?;
                Ok(PriceRecord {
                    provider_id: row.get(0)?,
                    model: row.get(1)?,
                    currency: Currency::parse(&currency).ok_or(rusqlite::Error::InvalidQuery)?,
                    input_micros: row.get::<_, i64>(3)?.max(0) as u64,
                    cached_input_micros: row.get::<_, i64>(4)?.max(0) as u64,
                    output_micros: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })?
            .collect()
    }

    pub fn put_model_price(&self, price: &PriceRecord) -> Result<()> {
        let bound = |value: u64| value.min(i64::MAX as u64) as i64;
        if price.model.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute(
            "INSERT INTO model_prices (provider_id, model, currency, input_micros,
                cached_input_micros, output_micros, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, CAST(unixepoch('subsec') * 1000 AS INTEGER))
             ON CONFLICT(provider_id, model) DO UPDATE SET currency = excluded.currency,
                input_micros = excluded.input_micros,
                cached_input_micros = excluded.cached_input_micros,
                output_micros = excluded.output_micros, updated_at_ms = excluded.updated_at_ms",
            params![
                price.provider_id,
                price.model,
                price.currency.as_str(),
                bound(price.input_micros),
                bound(price.cached_input_micros),
                bound(price.output_micros)
            ],
        )?;
        Ok(())
    }

    pub fn delete_model_price(&self, provider_id: &str, model: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM model_prices WHERE provider_id = ?1 AND model = ?2",
            [provider_id, model],
        )?;
        Ok(())
    }
}

fn usage_record(row: &Row<'_>) -> Result<UsageRecord> {
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
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::storage::Store;

    fn row(
        id: &str,
        at: i64,
        provider: Option<&str>,
        codex: Option<&str>,
        model: &str,
    ) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            source: if provider.is_some() {
                UsageSource::Router
            } else {
                UsageSource::CodexLog
            },
            started_at_ms: at,
            provider_id: provider.map(str::to_owned),
            codex_provider: codex.map(str::to_owned),
            public_model: None,
            model: model.into(),
            usage: TokenUsage {
                input: 100,
                cached_input: 60,
                cache_write: 0,
                output: 10,
                reasoning: 2,
            },
            duration_ms: provider.map(|_| 1_100),
            first_event_ms: provider.map(|_| 100),
            response_id: None,
            session_id: None,
        }
    }

    #[test]
    fn facts_heat_speeds_and_pages_respect_range_and_filters() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let hour = 3_600_000;
        let base = 1_800_000_000_000;
        let file = UsageLogFile {
            path: "synthetic".into(),
            size: 0,
            modified_ms: 0,
            read_offset: 0,
            state: "{}".into(),
        };
        let rows = [
            row("a", base, Some("one"), None, "m1"),
            row("b", base + 1, Some("one"), None, "m2"),
            row("c", base + hour, None, Some("openai"), "m1"),
            row("d", base + 30 * hour, None, None, "m1"),
        ];
        assert_eq!(store.put_imported_usage(&rows, &file).unwrap(), 4);
        let facts = store
            .usage_facts(base, base + 48 * hour, UsageBucket::None)
            .unwrap();
        let mut groups = facts
            .iter()
            .map(|fact| (fact.group.as_str(), fact.model.as_str(), fact.requests))
            .collect::<Vec<_>>();
        groups.sort();
        assert_eq!(
            groups,
            [
                ("c:openai", "m1", 1),
                ("p:one", "m1", 1),
                ("p:one", "m2", 1),
                ("u", "m1", 1)
            ]
        );
        assert!(facts.iter().all(|fact| fact.bucket.is_empty()));
        let hourly = store
            .usage_facts(base, base + hour, UsageBucket::Hour)
            .unwrap();
        assert_eq!(hourly.iter().map(|fact| fact.requests).sum::<u64>(), 2);
        assert_eq!(hourly[0].bucket.len(), 13);
        assert_eq!(hourly[0].usage.output, 10);

        let one = UsageFilter {
            group: Some("p:one".into()),
            model: None,
        };
        assert_eq!(
            store.usage_speeds(base, base + hour, &one).unwrap(),
            [10.0, 10.0]
        );
        let m1 = UsageFilter {
            group: None,
            model: Some("m1".into()),
        };
        let heat = store.usage_heat(base, base + 48 * hour, &m1).unwrap();
        assert_eq!(heat.iter().map(|cell| cell.2).sum::<u64>(), 3);
        assert_eq!(heat[0].3 % 110, 0);

        let (page, total) = store.usage_page(base, base + 48 * hour, &m1, 1, 1).unwrap();
        assert_eq!((page[0].id.as_str(), total), ("c", 3));
        assert_eq!(store.first_usage_ms().unwrap(), Some(base));

        let price = PriceRecord {
            provider_id: String::new(),
            model: "m1".into(),
            currency: Currency::Cny,
            input_micros: 2_000_000,
            cached_input_micros: 500_000,
            output_micros: 8_000_000,
        };
        store.put_model_price(&price).unwrap();
        store
            .put_model_price(&PriceRecord {
                input_micros: 1,
                ..price.clone()
            })
            .unwrap();
        assert_eq!(store.model_prices().unwrap()[0].input_micros, 1);
        assert!(
            store
                .put_model_price(&PriceRecord {
                    model: String::new(),
                    ..price
                })
                .is_err()
        );
        store.delete_model_price("", "m1").unwrap();
        assert!(store.model_prices().unwrap().is_empty());
    }
}
