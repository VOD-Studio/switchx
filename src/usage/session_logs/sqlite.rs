//! Read source databases in a snapshot with SQLITE_OPEN_READ_ONLY, including WAL.
use super::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::time::Duration;

#[derive(Deserialize)]
struct Message {
    role: Option<String>,
    tokens: Option<Value>,
    cost: Option<Value>,
    time: Option<Times>,
    status: Option<String>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    model: Option<Value>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
}
#[derive(Deserialize)]
struct Times {
    created: Option<i64>,
    completed: Option<i64>,
}

fn table(db: &Connection, name: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|v| v.is_some())
}
fn revision(path: &Path) -> std::io::Result<String> {
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    Ok(format!(
        "{}|{}",
        stamp(path)?,
        stamp(&wal).unwrap_or_default()
    ))
}
fn cursor(key: String, state: String, watermark: i64) -> UsageLogFile {
    UsageLogFile {
        path: key,
        size: 0,
        modified_ms: watermark,
        read_offset: watermark,
        state,
    }
}

pub fn import(
    store: &Store,
    client: UsageClient,
    path: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let paths = if path.exists() {
        vec![path.to_owned()]
    } else {
        vec![]
    };
    scan(paths, cancel, progress, |path| {
        let version = revision(path)?;
        let key = format!("{}:database:{}", client.as_str(), path.display());
        let previous = store
            .usage_log_file(&key)
            .map_err(|_| ReadError::Database)?;
        if previous.as_ref().is_some_and(|c| c.state == version) {
            return Ok((false, 0));
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(1))?;
        db.execute_batch("BEGIN;")?;
        if client == UsageClient::Mcode {
            mcode(store, &db, &key, &version, cancel)
        } else {
            opencode(store, &db, &key, &version, cancel)
        }
    })
}

fn opencode(
    store: &Store,
    db: &Connection,
    key: &str,
    version: &str,
    cancel: &AtomicBool,
) -> Result<(bool, usize), ReadError> {
    let v2 = table(db, "session_v2")? && table(db, "session_message")?;
    let (sessions, messages) = if v2 {
        ("session_v2", "session_message")
    } else {
        ("session", "message")
    };
    let mut query=db.prepare(&format!("SELECT s.id, MAX(s.time_updated,COALESCE(MAX(m.time_updated),s.time_updated)) FROM {sessions} s LEFT JOIN {messages} m ON m.session_id=s.id GROUP BY s.id ORDER BY s.id"))?;
    let sessions = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let layout = if v2 { "v2" } else { "v1" };
    let mut changed = 0;
    for (session, updated) in sessions {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        let session_key = format!("{key}:session:{session}");
        let sql = if v2 {
            "SELECT id,data,type,time_created FROM session_message WHERE session_id=?1 AND type IN ('assistant','compaction') ORDER BY time_created,id"
        } else {
            "SELECT id,data,NULL,time_created FROM message WHERE session_id=?1 ORDER BY time_created,id"
        };
        let mut query = db.prepare(sql)?;
        let mut source = query.query([&session])?;
        let mut records = Vec::new();
        let mut incomplete = false;
        while let Some(row) = source.next()? {
            if cancel.load(Ordering::Relaxed) {
                return Err(ReadError::Cancelled);
            }
            let id: String = row.get(0)?;
            let json: String = row.get(1)?;
            let kind: Option<String> = row.get(2)?;
            let Ok(message) = serde_json::from_str::<Message>(&json) else {
                continue;
            };
            if !v2 && message.role.as_deref() != Some("assistant") {
                continue;
            }
            let complete = if kind.as_deref() == Some("compaction") {
                matches!(message.status.as_deref(), Some("completed" | "failed"))
            } else {
                message.time.as_ref().and_then(|t| t.completed).is_some()
            };
            if !complete {
                incomplete = true;
                continue;
            }
            let Some(tokens) = message.tokens else {
                continue;
            };
            let usage = usage(
                count(tokens.get("input")),
                count(tokens.pointer("/cache/read")),
                count(tokens.pointer("/cache/write")),
                count(tokens.get("output")),
                count(tokens.get("reasoning")),
                true,
                true,
            );
            let cost = message
                .cost
                .as_ref()
                .and_then(usd_micros)
                .filter(|n| *n > 0);
            if usage.is_empty() && cost.is_none() {
                continue;
            }
            let model = label(message.model_id)
                .or_else(|| {
                    message
                        .model
                        .as_ref()
                        .and_then(|m| m.as_str().or_else(|| m.get("id").and_then(Value::as_str)))
                        .and_then(|s| label(Some(s.into())))
                })
                .unwrap_or_default();
            let at = message
                .time
                .as_ref()
                .and_then(|t| t.created)
                .unwrap_or(row.get(3)?);
            if !(0..=253_402_300_799_999).contains(&at) {
                continue;
            }
            let mut target = record(UsageClient::OpenCode, &session, &id, model, at, usage);
            target.reported_cost_micros = cost;
            target.response_id = message
                .response_id
                .as_ref()
                .and_then(|id| super::super::response_id(&Value::String(id.clone())));
            records.push(target);
        }
        let state = if incomplete { "pending" } else { layout };
        changed += store
            .put_imported_usage(&records, &cursor(session_key, state.into(), updated))
            .map_err(|_| ReadError::Database)?;
    }
    // The revision includes WAL size and nanosecond mtime. Completion writes are
    // observed even when message/session timestamps stay unchanged.
    store
        .put_imported_usage(&[], &cursor(key.into(), version.into(), 0))
        .map_err(|_| ReadError::Database)?;
    Ok((true, changed))
}

fn mcode(
    store: &Store,
    db: &Connection,
    key: &str,
    version: &str,
    cancel: &AtomicBool,
) -> Result<(bool, usize), ReadError> {
    // This is a compact committed usage projection, not conversation content. On a
    // changed DB, revisiting it also catches corrected costs and replaced databases.
    let mut query=db.prepare("SELECT id,session_id,model,ts,input_tokens,output_tokens,reasoning_tokens,cache_read_tokens,cache_write_tokens,cost_usd FROM local_runtime_token_usage ORDER BY id")?;
    let mut rows = query.query([])?;
    let mut records = Vec::new();
    let mut watermark = 0;
    while let Some(row) = rows.next()? {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        let id: i64 = row.get(0)?;
        watermark = watermark.max(id);
        let session: String = row.get(1)?;
        let model: Option<String> = row.get(2)?;
        let at: i64 = row.get(3)?;
        if id < 0 || !(0..=253_402_300_799_999).contains(&at) {
            continue;
        }
        let count_column = |index| row.get::<_, i64>(index).map(|n| n.max(0) as u64);
        let usage = usage(
            count_column(4)?,
            count_column(7)?,
            count_column(8)?,
            count_column(5)?,
            count_column(6)?,
            true,
            true,
        );
        // Read the decimal spelling through SQLite, so NULL and explicit zero differ.
        let native = match row.get_ref(9)? {
            rusqlite::types::ValueRef::Null => None,
            rusqlite::types::ValueRef::Real(n) => {
                serde_json::Number::from_f64(n).map(Value::Number)
            }
            rusqlite::types::ValueRef::Integer(n) => Some(Value::from(n)),
            rusqlite::types::ValueRef::Text(s) => {
                std::str::from_utf8(s).ok().map(|s| Value::String(s.into()))
            }
            _ => None,
        };
        let cost = native.as_ref().and_then(usd_micros);
        if usage.is_empty() && cost.is_none() {
            continue;
        }
        let model = label(model).unwrap_or_default();
        let actual = model
            .split_once('/')
            .map_or(model.as_str(), |(_, m)| m)
            .to_owned();
        let mut target = record(
            UsageClient::Mcode,
            &session,
            &id.to_string(),
            actual,
            at,
            usage,
        );
        target.reported_cost_micros = cost;
        records.push(target);
    }
    let changes = store
        .put_imported_usage(&records, &cursor(key.into(), version.into(), watermark))
        .map_err(|_| ReadError::Database)?;
    Ok((true, changes))
}
