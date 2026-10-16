//! Incremental Claude Code transcript import. Only counters, model/message/session
//! IDs, parent links, and timestamps are decoded. A content block's type is read
//! when estimating time to first token; conversation text and credentials are ignored.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use serde_json::Value;

use super::{
    TokenUsage,
    codex_logs::{ImportError, ImportProgress, ImportSummary},
};
use crate::storage::{Store, UsageClient, UsageLogFile, UsageRecord, UsageSource};

fn identifier(value: Option<String>) -> Option<String> {
    value.filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
}

/// Shorter than this is not a real request; longer than this includes sleep or a retry.
const MIN_PLAUSIBLE_LATENCY_MS: i64 = 100;
const MAX_PLAUSIBLE_LATENCY_MS: i64 = 60 * 60 * 1000;
/// The user or tool line that started a reply is usually already behind the read cursor.
const CHAIN_SEED_BYTES: i64 = 256 * 1024;
const MAX_CHAIN_HOPS: usize = 32;

struct ChainNode {
    parent: Option<String>,
    timestamp_ms: Option<i64>,
    attachment: bool,
}

/// Timing clues for one reply. Blocks that share a message ID belong together.
struct BlockTiming {
    first_parent: Option<String>,
    /// False when the first block is outside the lookback window. Estimating from a
    /// later block starts too late and inflates speed.
    starts_at_first_block: bool,
    /// When the reply started producing output. Only a leading thinking block records
    /// this, as its write time minus `thinkingDurationMs`.
    first_output_ms: Option<i64>,
    end_ms: Option<i64>,
    finished: bool,
}

fn line_id(value: &Value, key: &str) -> Option<String> {
    identifier(value.get(key).and_then(Value::as_str).map(str::to_owned))
}

fn timestamp_ms(value: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value.get("timestamp").and_then(Value::as_str)?)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn record_chain(chain: &mut HashMap<String, ChainNode>, value: &Value) {
    let Some(id) = line_id(value, "uuid") else {
        return;
    };
    chain.insert(
        id,
        ChainNode {
            parent: line_id(value, "parentUuid"),
            timestamp_ms: timestamp_ms(value),
            attachment: value.get("type").and_then(Value::as_str) == Some("attachment"),
        },
    );
}

fn thinking_start_ms(value: &Value, block_end_ms: i64) -> Option<i64> {
    let duration_ms = value.get("thinkingDurationMs")?.as_i64()?;
    let kind = value.pointer("/message/content/0/type")?.as_str()?;
    (kind == "thinking" && duration_ms > 0).then_some(block_end_ms - duration_ms)
}

fn record_block(timings: &mut HashMap<String, BlockTiming>, value: &Value, msg_id: &str) {
    let block_end_ms = timestamp_ms(value);
    let timing = timings.entry(msg_id.to_owned()).or_insert_with(|| {
        let starts_at_first_block = value
            .get("apiBlockIndex")
            .is_none_or(|index| index.as_u64() == Some(0));
        BlockTiming {
            first_parent: line_id(value, "parentUuid"),
            starts_at_first_block,
            first_output_ms: starts_at_first_block
                .then(|| block_end_ms.and_then(|end| thinking_start_ms(value, end)))
                .flatten(),
            end_ms: None,
            finished: false,
        }
    });
    if let Some(end) = block_end_ms {
        timing.end_ms = Some(timing.end_ms.map_or(end, |current| current.max(end)));
    }
    timing.finished |= value
        .pointer("/message/stop_reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| !reason.is_empty());
}

fn observe(
    value: &Value,
    chain: &mut HashMap<String, ChainNode>,
    timings: &mut HashMap<String, BlockTiming>,
) {
    record_chain(chain, value);
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(id) = value
        .get("message")
        .and_then(|message| line_id(message, "id"))
    else {
        return;
    };
    record_block(timings, value, &id);
}

/// Request start: walk up from the first block's parent, skip attachments, and take
/// the first user message, tool result, or retry line.
fn resolve_start(chain: &HashMap<String, ChainNode>, first_parent: Option<&str>) -> Option<i64> {
    let mut cursor = first_parent?.to_owned();
    for _ in 0..MAX_CHAIN_HOPS {
        let node = chain.get(&cursor)?;
        if !node.attachment {
            return node.timestamp_ms;
        }
        cursor = node.parent.clone()?;
    }
    None
}

fn apply_timing(
    record: &mut UsageRecord,
    timing: &BlockTiming,
    chain: &HashMap<String, ChainNode>,
) {
    if !timing.finished || !timing.starts_at_first_block {
        return;
    }
    let Some(start) = resolve_start(chain, timing.first_parent.as_deref()) else {
        return;
    };
    let Some(end) = timing.end_ms else {
        return;
    };
    let Some(latency) = end.checked_sub(start) else {
        return;
    };
    if !(MIN_PLAUSIBLE_LATENCY_MS..=MAX_PLAUSIBLE_LATENCY_MS).contains(&latency) {
        return;
    }
    record.duration_ms = Some(latency);
    record.first_event_ms = timing.first_output_ms.and_then(|first| {
        let offset = first.checked_sub(start)?;
        (offset > 0 && offset <= latency).then_some(offset)
    });
}

fn usage_record(value: &Value) -> Option<UsageRecord> {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = value.get("message")?;
    let id = identifier(message.get("id").and_then(Value::as_str).map(str::to_owned))?;
    let usage = TokenUsage::from_claude(message.get("usage")?)?;
    if usage.is_empty() {
        return None;
    }
    Some(UsageRecord {
        reported_cost_micros: None,
        request_count: 1,
        // Message IDs survive copied/forked transcripts and are shared by all blocks.
        id: format!("claude:{id}"),
        source: UsageSource::ClaudeLog,
        client: UsageClient::ClaudeCode,
        started_at_ms: timestamp_ms(value)?,
        provider_id: None,
        codex_provider: None,
        public_model: None,
        model: identifier(
            message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
        .unwrap_or_default(),
        usage,
        duration_ms: None,
        first_event_ms: None,
        response_id: Some(format!("claude:{id}")),
        session_id: identifier(
            value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
    })
}

/// Reads the lines just before `offset` for parent links and earlier blocks.
/// A failure only skips the estimate; the cursor itself stays put.
fn seed_chain(
    file: &mut File,
    offset: i64,
    chain: &mut HashMap<String, ChainNode>,
    timings: &mut HashMap<String, BlockTiming>,
) -> std::io::Result<()> {
    let len = offset.clamp(0, CHAIN_SEED_BYTES);
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::Start((offset - len) as u64))?;
    let mut window = vec![0u8; len as usize];
    let read_ok = file.read_exact(&mut window).is_ok();
    file.seek(SeekFrom::Start(offset as u64))?;
    if !read_ok {
        return Ok(());
    }
    let mut lines = window.split(|byte| *byte == b'\n');
    if offset > len {
        lines.next();
    }
    for line in lines {
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            observe(&value, chain, timings);
        }
    }
    Ok(())
}

/// Fixed-depth traversal includes regular and workflow subagents, without following
/// directory symlinks or visiting unrelated configuration files.
pub fn log_files(home: &Path) -> Vec<PathBuf> {
    fn children(path: &Path, directories: bool) -> Vec<PathBuf> {
        fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let kind = entry.file_type().ok()?;
                let path = entry.path();
                (if directories {
                    kind.is_dir()
                } else {
                    kind.is_file() && path.extension().is_some_and(|e| e == "jsonl")
                })
                .then_some(path)
            })
            .collect()
    }
    let mut files = Vec::new();
    for project in children(&home.join("projects"), true) {
        files.extend(children(&project, false));
        for session in children(&project, true) {
            let agents = session.join("subagents");
            // A named intermediate directory can itself be a symlink.
            if !fs::symlink_metadata(&agents).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            files.extend(children(&agents, false));
            let workflows = agents.join("workflows");
            if !fs::symlink_metadata(&workflows).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            for workflow in children(&workflows, true) {
                files.extend(children(&workflow, false));
            }
        }
    }
    files.sort();
    files
}

/// A stable fingerprint detects replacement even when the new file is no shorter.
/// Only the digest is persisted, never transcript text.
fn fingerprint(file: &mut File, offset: u64) -> std::io::Result<String> {
    let length = offset.min(4096);
    file.seek(SeekFrom::Start(offset - length))?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)?;
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    // v2 cursors force a reread of transcripts imported before duration estimates.
    // Message IDs keep that reread from counting the same reply twice.
    Ok(format!("claude-v2:{hash:016x}"))
}

enum ReadError {
    Io,
    Cancelled,
}
impl From<std::io::Error> for ReadError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

fn read_file(
    path: &Path,
    key: String,
    known: Option<&UsageLogFile>,
    cancel: &AtomicBool,
) -> Result<Option<(Vec<UsageRecord>, UsageLogFile)>, ReadError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let size = metadata.len() as i64;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis() as i64);
    let resume = match known {
        Some(cursor)
            if cursor.read_offset >= 0
                && cursor.read_offset <= size
                && fingerprint(&mut file, cursor.read_offset as u64)? == cursor.state =>
        {
            Some(cursor)
        }
        _ => None,
    };
    if resume.is_some_and(|cursor| cursor.size == size && cursor.modified_ms == modified_ms) {
        return Ok(None);
    }
    let mut offset = resume.map_or(0, |cursor| cursor.read_offset);
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut chain = HashMap::new();
    let mut timings = HashMap::new();
    seed_chain(&mut file, offset, &mut chain, &mut timings)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut records = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        // Do not consume an incomplete final line, even if it is valid JSON already.
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        offset += read as i64;
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        observe(&value, &mut chain, &mut timings);
        let Some(mut record) = usage_record(&value) else {
            continue;
        };
        let msg_id = record
            .id
            .strip_prefix("claude:")
            .unwrap_or(&record.id)
            .to_owned();
        if let Some(timing) = timings.get(&msg_id) {
            apply_timing(&mut record, timing, &chain);
        }
        records.push(record);
    }
    let state = fingerprint(reader.get_mut(), offset as u64)?;
    Ok(Some((
        records,
        UsageLogFile {
            path: key,
            size,
            modified_ms,
            read_offset: offset,
            state,
        },
    )))
}

pub fn import(
    store: &Store,
    home: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let known = store
        .usage_log_files()
        .map_err(|_| ImportError::Database)?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<std::collections::HashMap<_, _>>();
    let files = log_files(home);
    let mut summary = ImportSummary {
        files: files.len(),
        ..Default::default()
    };
    for (index, path) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }
        progress(ImportProgress {
            done: index,
            total: files.len(),
        });
        let key = format!("claude:{}", path.to_string_lossy());
        match read_file(path, key.clone(), known.get(&key), cancel) {
            Ok(Some((records, cursor))) => {
                summary.read += 1;
                summary.added += store
                    .put_imported_usage(&records, &cursor)
                    .map_err(|_| ImportError::Database)?;
            }
            Ok(None) => {}
            Err(ReadError::Io) => summary.unreadable += 1,
            Err(ReadError::Cancelled) => return Err(ImportError::Cancelled),
        }
    }
    progress(ImportProgress {
        done: files.len(),
        total: files.len(),
    });
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    struct Home(PathBuf);
    impl Home {
        fn new() -> Self {
            let home = Self(
                std::env::temp_dir()
                    .join(format!("switchx-claude-{}", crate::id::new_id().unwrap())),
            );
            fs::create_dir_all(&home.0).unwrap();
            home
        }
        fn append(&self, path: &str, bytes: &[u8]) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }
        fn line(&self, path: &str, value: Value) {
            self.append(path, format!("{value}\n").as_bytes());
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const MAIN: &str = "projects/project/session.jsonl";
    const AGENT: &str = "projects/project/session/subagents/agent.jsonl";
    const WORKFLOW: &str = "projects/project/session/subagents/workflows/wf_demo/agent.jsonl";

    fn message(id: &str, output: u64) -> Value {
        json!({"type":"assistant", "sessionId":"session", "timestamp":"2026-10-15T08:00:00Z",
            "cwd":"private-directory", "message":{"id":id, "model":"claude-test", "content":[{"type":"text", "text":"private-prompt-and-output"}],
                "usage":{"input_tokens":100, "output_tokens":output, "cache_read_input_tokens":1000,
                    "cache_creation_input_tokens":500, "cache_creation":{"ephemeral_5m_input_tokens":300, "ephemeral_1h_input_tokens":200}}}})
    }
    fn scan(store: &Store, home: &Home) -> ImportSummary {
        import(store, &home.0, &AtomicBool::new(false), |_| {}).unwrap()
    }
    fn rows(store: &Store) -> Vec<UsageRecord> {
        store.usage_records(0, i64::MAX).unwrap()
    }

    #[test]
    fn snapshots_merge_across_scans_and_copied_subagents_without_extra_requests() {
        let home = Home::new();
        let store = Store::open(&home.0.join("scratch.sqlite")).unwrap();
        home.line(MAIN, message("msg-one", 5));
        assert_eq!(scan(&store, &home).added, 1);
        assert_eq!(rows(&store)[0].usage.output, 5);
        home.line(AGENT, message("msg-one", 4660));
        home.line(WORKFLOW, message("msg-two", 0)); // Input-only requests count too.
        let summary = scan(&store, &home);
        assert_eq!((summary.files, summary.added), (3, 2));
        assert_eq!(rows(&store).len(), 2);
        let row = rows(&store)
            .into_iter()
            .find(|row| row.id == "claude:msg-one")
            .unwrap();
        assert_eq!(
            (
                row.usage.input,
                row.usage.fresh_input(),
                row.usage.cached_input,
                row.usage.cache_write,
                row.usage.cache_write_1h,
                row.usage.output
            ),
            (1600, 100, 1000, 500, 200, 4660)
        );
        assert_eq!(row.client, UsageClient::ClaudeCode);
        assert!(row.provider_id.is_none());
        home.line(MAIN, message("msg-one", 1)); // A stale block cannot roll back usage.
        assert_eq!(scan(&store, &home).added, 0);
        assert_eq!(scan(&store, &home).read, 0);
        assert_eq!(
            rows(&store).iter().map(|row| row.usage.output).sum::<u64>(),
            4660
        );
        let cursors = store.usage_log_files().unwrap();
        assert!(
            cursors
                .iter()
                .all(|cursor| cursor.state.starts_with("claude-v2:")
                    && !cursor.state.contains("private"))
        );
    }

    #[test]
    fn partial_lines_and_rewrites_resume_safely_and_rebuild_stays_idempotent() {
        let home = Home::new();
        let store = Store::open(&home.0.join("scratch.sqlite")).unwrap();
        let line = message("msg-one", 5).to_string();
        home.append(MAIN, &line.as_bytes()[..line.len() / 2]);
        assert_eq!(scan(&store, &home).added, 0);
        assert_eq!(store.usage_log_files().unwrap()[0].read_offset, 0);
        home.append(MAIN, &line.as_bytes()[line.len() / 2..]);
        assert_eq!(scan(&store, &home).added, 0); // Valid JSON without newline is still pending.
        home.append(MAIN, b"\n");
        assert_eq!(scan(&store, &home).added, 1);
        // Same-sized replacement: offset alone cannot detect it.
        let replacement = format!("{}\n", message("msg-two", 9));
        assert_eq!(replacement.len(), line.len() + 1);
        fs::write(home.0.join(MAIN), replacement).unwrap();
        assert_eq!(scan(&store, &home).added, 1);
        home.line(MAIN, message("msg-one", 10));
        assert_eq!(scan(&store, &home).added, 1);
        assert_eq!(rows(&store).len(), 2);
        fs::write(home.0.join(MAIN), format!("{}\n", message("msg-one", 10))).unwrap();
        assert_eq!(scan(&store, &home).added, 0); // Truncation replays existing IDs, not totals.
        store.clear_imported_usage().unwrap();
        assert_eq!(scan(&store, &home).added, 1);
        assert_eq!(scan(&store, &home).added, 0);
    }

    #[test]
    fn invalid_non_usage_records_and_cancelled_scans_never_create_usage() {
        let home = Home::new();
        let store = Store::open(&home.0.join("scratch.sqlite")).unwrap();
        home.append(MAIN, b"invalid-json\n");
        let mut user = message("user", 5);
        user["type"] = "user".into();
        home.line(MAIN, user);
        let mut invalid = message("invalid", 5);
        invalid["timestamp"] = "invalid".into();
        home.line(MAIN, invalid);
        let mut missing = message("missing", 5);
        missing["message"]["id"] = Value::Null;
        home.line(MAIN, missing);
        assert_eq!(scan(&store, &home).added, 0);
        let before = store.usage_log_files().unwrap();
        home.line(MAIN, message("valid", 5));
        assert!(matches!(
            import(&store, &home.0, &AtomicBool::new(true), |_| {}),
            Err(ImportError::Cancelled)
        ));
        assert_eq!(store.usage_log_files().unwrap(), before);
        assert_eq!(scan(&store, &home).added, 1);
        assert_eq!(
            import(
                &store,
                &home.0.join("missing"),
                &AtomicBool::new(false),
                |_| {}
            )
            .unwrap()
            .files,
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn traversal_does_not_follow_symlinks() {
        let home = Home::new();
        home.line(MAIN, message("main", 1));
        home.line(WORKFLOW, message("workflow", 2));
        std::os::unix::fs::symlink(
            home.0.join("projects/project"),
            home.0.join("projects/alias"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            home.0.join(MAIN),
            home.0.join("projects/project/alias.jsonl"),
        )
        .unwrap();
        assert_eq!(log_files(&home.0).len(), 2);
    }

    fn chain_uuid(n: u8) -> String {
        format!("00000000-0000-4000-8000-0000000000{n:02x}")
    }

    fn chain_line(kind: &str, id: u8, parent: Option<u8>, timestamp: &str) -> Value {
        json!({
            "parentUuid": parent.map(chain_uuid),
            "type": kind,
            "uuid": chain_uuid(id),
            "timestamp": timestamp,
            "sessionId": "session-t",
        })
    }

    fn assistant_block(
        msg_id: &str,
        id: u8,
        parent: u8,
        block_index: u32,
        timestamp: &str,
        output: u64,
        stop: Option<&str>,
    ) -> Value {
        json!({
            "parentUuid": chain_uuid(parent),
            "type": "assistant",
            "uuid": chain_uuid(id),
            "apiBlockIndex": block_index,
            "timestamp": timestamp,
            "sessionId": "session-t",
            "message": {
                "id": msg_id,
                "model": "claude-opus-4-8",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": output,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0
                },
                "stop_reason": stop,
            }
        })
    }

    fn thinking_block(
        msg_id: &str,
        id: u8,
        parent: u8,
        timestamp: &str,
        thinking_ms: i64,
        output: u64,
        stop: Option<&str>,
    ) -> Value {
        let mut value = assistant_block(msg_id, id, parent, 0, timestamp, output, stop);
        value["thinkingDurationMs"] = json!(thinking_ms);
        value["message"]["content"] = json!([{ "type": "thinking", "thinking": "" }]);
        value
    }

    fn message_row(store: &Store, id: &str) -> UsageRecord {
        rows(store)
            .into_iter()
            .find(|row| row.id == format!("claude:{id}"))
            .unwrap()
    }

    #[test]
    fn estimated_latency_follows_the_parent_chain_across_scans() {
        let home = Home::new();
        let store = Store::open(&home.0.join("scratch.sqlite")).unwrap();
        home.line(
            MAIN,
            chain_line("user", 1, None, "2026-06-07T13:00:00.000Z"),
        );
        home.line(
            MAIN,
            chain_line("attachment", 2, Some(1), "2026-06-07T13:00:00.003Z"),
        );
        home.line(
            MAIN,
            chain_line("attachment", 3, Some(2), "2026-06-07T13:00:05.001Z"),
        );
        home.line(
            MAIN,
            assistant_block("msg_t", 4, 3, 0, "2026-06-07T13:00:05.000Z", 100, None),
        );
        assert_eq!(scan(&store, &home).added, 1);
        assert_eq!(message_row(&store, "msg_t").duration_ms, None);

        home.line(
            MAIN,
            assistant_block("msg_t", 5, 4, 1, "2026-06-07T13:00:07.000Z", 400, None),
        );
        home.line(
            MAIN,
            chain_line("user", 6, Some(5), "2026-06-07T13:00:08.000Z"),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_t",
                7,
                6,
                2,
                "2026-06-07T13:00:12.500Z",
                900,
                Some("tool_use"),
            ),
        );
        assert!(scan(&store, &home).added >= 1);
        let row = message_row(&store, "msg_t");
        assert_eq!(
            (row.duration_ms, row.first_event_ms, row.usage.output),
            (Some(12_500), None, 900)
        );

        // A copied block without its parent chain must not erase the estimate.
        home.line(
            AGENT,
            assistant_block(
                "msg_t",
                8,
                6,
                0,
                "2026-06-07T13:00:12.500Z",
                1,
                Some("end_turn"),
            ),
        );
        scan(&store, &home);
        let row = message_row(&store, "msg_t");
        assert_eq!((row.duration_ms, row.usage.output), (Some(12_500), 900));
    }

    #[test]
    fn leading_thinking_block_estimates_time_to_first_token() {
        let home = Home::new();
        let store = Store::open(&home.0.join("scratch.sqlite")).unwrap();
        home.line(
            MAIN,
            chain_line("user", 1, None, "2026-06-07T13:00:00.000Z"),
        );
        home.line(
            MAIN,
            thinking_block(
                "msg_think",
                2,
                1,
                "2026-06-07T13:00:05.000Z",
                3_000,
                900,
                Some("tool_use"),
            ),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_think",
                3,
                2,
                1,
                "2026-06-07T13:00:09.000Z",
                900,
                Some("tool_use"),
            ),
        );
        home.line(
            MAIN,
            chain_line("user", 4, Some(3), "2026-06-07T13:00:10.000Z"),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_tool",
                5,
                4,
                0,
                "2026-06-07T13:00:14.000Z",
                50,
                Some("end_turn"),
            ),
        );
        home.line(
            MAIN,
            chain_line("user", 6, Some(5), "2026-06-07T13:00:20.000Z"),
        );
        home.line(
            MAIN,
            thinking_block(
                "msg_bad",
                7,
                6,
                "2026-06-07T13:00:22.000Z",
                5_000,
                900,
                Some("end_turn"),
            ),
        );
        home.line(
            MAIN,
            chain_line("user", 8, Some(7), "2026-06-07T13:00:30.000Z"),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_late",
                9,
                8,
                1,
                "2026-06-07T13:00:31.000Z",
                900,
                Some("end_turn"),
            ),
        );
        home.line(
            MAIN,
            chain_line("user", 10, Some(9), "2026-06-07T13:00:40.000Z"),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_fast",
                11,
                10,
                0,
                "2026-06-07T13:00:40.050Z",
                900,
                Some("end_turn"),
            ),
        );
        home.line(
            MAIN,
            chain_line("user", 12, Some(11), "2026-06-07T15:00:00.000Z"),
        );
        home.line(
            MAIN,
            assistant_block(
                "msg_slow",
                13,
                12,
                0,
                "2026-06-07T16:00:01.000Z",
                900,
                Some("end_turn"),
            ),
        );
        assert!(scan(&store, &home).added >= 5);
        let think = message_row(&store, "msg_think");
        assert_eq!(
            (think.duration_ms, think.first_event_ms),
            (Some(9_000), Some(2_000))
        );
        let tool = message_row(&store, "msg_tool");
        assert_eq!(
            (tool.duration_ms, tool.first_event_ms, tool.usage.output),
            (Some(4_000), None, 50)
        );
        let bad = message_row(&store, "msg_bad");
        assert_eq!((bad.duration_ms, bad.first_event_ms), (Some(2_000), None));
        assert_eq!(message_row(&store, "msg_late").duration_ms, None);
        assert_eq!(message_row(&store, "msg_fast").duration_ms, None);
        assert_eq!(message_row(&store, "msg_slow").duration_ms, None);
    }
}
