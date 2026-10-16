//! Incremental import of token usage from Codex session logs.
//!
//! Rollout files are append-only JSONL. Only four record kinds are decoded —
//! session metadata (session ID and provider key), turn context (model), and the
//! two usage records — and only counts, IDs, model IDs, and times are kept.
//! Timing peeks event types and timestamps from the start of each line; paths,
//! prompts, instructions, and tool output are never stored.

use std::{
    fs::File,
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TokenUsage;
use crate::storage::{Store, UsageLogFile, UsageRecord, UsageSource};

const ROUTER_PROVIDER: &str = "switchx_router";
const DIRECT_PROVIDER_PREFIX: &str = "switchx_direct_";
// The record kind is among the first or last fields of every line, so other
// lines are skipped without decoding them. Escaped text cannot match.
const KIND_WINDOW: usize = 512;
/// Cursors written before estimated timing lack this version and are reread once.
const TIMING_VERSION: u8 = 1;
/// Two lines this close were flushed together, not separated by a real request.
const SAME_FLUSH_SLACK_MS: i64 = 100;
/// Event type, role, and timestamp sit in the line head. Tool bodies are not parsed.
const LINE_HEAD: usize = 512;
/// `item_completed` lines are short; skip a full parse past this size.
const ITEM_COMPLETED_LIMIT: usize = 8 * 1024;
const KINDS: [&[u8]; 4] = [
    b"\"type\":\"token_usage_record\"",
    b"\"type\":\"token_count\"",
    b"\"type\":\"turn_context\"",
    b"\"type\":\"session_meta\"",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub files: usize,
    pub read: usize,
    /// Inserted or updated rows, including exact router/client attribution.
    pub added: usize,
    pub unreadable: usize,
}

#[derive(Debug)]
pub enum ImportError {
    Database,
    Cancelled,
}

/// What the reader needs to continue a file where the last scan stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct FileState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    // Files with per-response records ignore the cumulative token_count events.
    #[serde(default)]
    records: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<[u64; 5]>,
    /// `TIMING_VERSION` once this cursor has observed event order. Missing forces a reread.
    #[serde(default)]
    timing: u8,
    #[serde(default)]
    timer: RequestTimer,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    ordinal: Option<u64>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    payload: Payload,
}

#[derive(Deserialize, Default)]
struct Payload {
    #[serde(default, rename = "type")]
    kind: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    session_id: Option<Value>,
    #[serde(default)]
    thread_id: Option<Value>,
    #[serde(default)]
    model_provider: Option<Value>,
    #[serde(default)]
    model: Option<Value>,
    #[serde(default)]
    response_id: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    info: Option<Value>,
}

fn text(value: &Option<Value>) -> Option<String> {
    value
        .as_ref()?
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 256)
        .map(str::to_owned)
}

/// Session logs under `codex_home`, oldest paths first.
pub fn log_files(codex_home: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth > 0 {
                walk(&path, depth - 1, files);
            } else if kind.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&codex_home.join("sessions"), 3, &mut files);
    walk(&codex_home.join("archived_sessions"), 0, &mut files);
    files.sort();
    files
}

/// Imports new usage from every session log under `codex_home`. Each file commits
/// separately, so a cancelled or failed scan keeps the files already read.
pub fn import(
    store: &Store,
    codex_home: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let since = store
        .usage_router_since_ms()
        .map_err(|_| ImportError::Database)?;
    let known = store
        .usage_log_files()
        .map_err(|_| ImportError::Database)?
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect::<std::collections::HashMap<_, _>>();
    let files = log_files(codex_home);
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
        let key = path.to_string_lossy().into_owned();
        match read_file(path, &key, known.get(&key), since, cancel) {
            Ok(None) => {}
            Ok(Some((records, file))) => {
                summary.read += 1;
                summary.added += store
                    .put_imported_usage(&records, &file)
                    .map_err(|_| ImportError::Database)?;
            }
            Err(ReadError::Cancelled) => return Err(ImportError::Cancelled),
            Err(ReadError::Io) => summary.unreadable += 1,
        }
    }
    progress(ImportProgress {
        done: files.len(),
        total: files.len(),
    });
    Ok(summary)
}

enum ReadError {
    Io,
    Cancelled,
}

/// Reads complete lines appended since the last scan; `None` means unchanged.
fn read_file(
    path: &Path,
    key: &str,
    known: Option<&UsageLogFile>,
    since: i64,
    cancel: &AtomicBool,
) -> Result<Option<(Vec<UsageRecord>, UsageLogFile)>, ReadError> {
    let metadata = std::fs::metadata(path).map_err(|_| ReadError::Io)?;
    let size = metadata.len() as i64;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_millis() as i64);
    // A shorter file was replaced; read it again and let record IDs deduplicate.
    let resume = known.filter(|file| file.read_offset <= size);
    let saved = resume.and_then(|file| serde_json::from_str::<FileState>(&file.state).ok());
    let current = saved
        .as_ref()
        .is_some_and(|state| state.timing == TIMING_VERSION);
    if current && resume.is_some_and(|file| file.size == size && file.modified_ms == modified_ms) {
        return Ok(None);
    }
    // An older cursor has no timer. Reread from the start so existing rows gain timing.
    let (mut state, mut offset) = if current {
        (
            saved.unwrap_or_default(),
            resume.map_or(0, |file| file.read_offset),
        )
    } else {
        (FileState::default(), 0)
    };
    let mut reader =
        BufReader::with_capacity(1 << 20, File::open(path).map_err(|_| ReadError::Io)?);
    reader
        .seek(SeekFrom::Start(offset as u64))
        .map_err(|_| ReadError::Io)?;
    let mut records = Vec::new();
    let mut line = Vec::new();
    for lines in 1usize.. {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| ReadError::Io)?;
        // A partial last line is still being written; read it next time.
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        offset += read as i64;
        if lines.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        if let Some(record) = state.line(&line, since) {
            records.push(record);
        }
    }
    state.timing = TIMING_VERSION;
    let file = UsageLogFile {
        path: key.into(),
        size,
        modified_ms,
        read_offset: offset,
        state: serde_json::to_string(&state).unwrap_or_default(),
    };
    Ok(Some((records, file)))
}

fn wanted(line: &[u8]) -> bool {
    let head = &line[..line.len().min(KIND_WINDOW)];
    let tail = &line[line.len().saturating_sub(KIND_WINDOW)..];
    KINDS.iter().any(|kind| {
        [head, tail]
            .iter()
            .any(|part| part.windows(kind.len()).any(|window| window == *kind))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimingKind {
    /// The next request can only start after this line: a user message or `turn_context`.
    Boundary,
    /// A tool result, which is also a start boundary for the next request.
    ToolOutput,
    /// One finished model item (reasoning, assistant text, or a tool call).
    ModelOutput,
    /// `token_usage_record`, written when the response ends.
    UsageRecord,
    /// `task_started` or `turn_aborted`. A request that never reached a usage line ends here.
    TurnReset,
    /// Earliest `started_at_ms` of a reasoning or assistant item in this response.
    OutputStarted(i64),
}

/// Estimates one request from event order. Codex logs have no per-request timer.
/// The end is `token_usage_record` when present, otherwise `token_count`; if a tool
/// result landed after the last model item and the count was flushed with it, the
/// end is that model item (slightly early). The start is the latest boundary before
/// the first model item. Model items flushed within 100 ms after a count belong to
/// the previous response and are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct RequestTimer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_boundary_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_token_count_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_start_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_model_output_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_output_after_model_output_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage_end_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_output_ms: Option<i64>,
}

impl RequestTimer {
    fn observe(&mut self, line: &[u8]) -> Option<TimingKind> {
        let (timestamp_ms, kind) = classify(line)?;
        match kind {
            TimingKind::Boundary => {
                self.last_boundary_ms = self.last_boundary_ms.max(Some(timestamp_ms));
            }
            TimingKind::ToolOutput => {
                self.last_boundary_ms = self.last_boundary_ms.max(Some(timestamp_ms));
                if self.last_model_output_ms.is_some() {
                    self.tool_output_after_model_output_ms = Some(timestamp_ms);
                }
            }
            TimingKind::ModelOutput => {
                let trails = self.request_start_ms.is_none()
                    && self
                        .last_token_count_ms
                        .is_some_and(|at| timestamp_ms.saturating_sub(at) <= SAME_FLUSH_SLACK_MS);
                if trails {
                    return Some(kind);
                }
                if self.request_start_ms.is_none() {
                    self.request_start_ms = self.last_boundary_ms;
                }
                self.last_model_output_ms = Some(timestamp_ms);
                self.tool_output_after_model_output_ms = None;
            }
            TimingKind::UsageRecord => self.usage_end_ms = Some(timestamp_ms),
            TimingKind::OutputStarted(started) => {
                self.first_output_ms =
                    Some(self.first_output_ms.map_or(started, |at| at.min(started)));
            }
            TimingKind::TurnReset => {
                let boundary = self.last_boundary_ms.max(Some(timestamp_ms));
                let last_count = self.last_token_count_ms;
                *self = Self {
                    last_boundary_ms: boundary,
                    last_token_count_ms: last_count,
                    ..Self::default()
                };
            }
        }
        Some(kind)
    }

    /// Closes the current request. `token_count_ms` is the usage line's own timestamp
    /// and becomes the next request's boundary. Returns `(duration_ms, first_event_ms)`.
    fn finish(&mut self, token_count_ms: Option<i64>) -> Option<(i64, Option<i64>)> {
        let start = self.request_start_ms.or(self.last_boundary_ms);
        let first = self.first_output_ms;
        let waited = match (self.tool_output_after_model_output_ms, token_count_ms) {
            (Some(tool), Some(count)) => count.saturating_sub(tool) <= SAME_FLUSH_SLACK_MS,
            (Some(_), None) => true,
            _ => false,
        };
        let end = self.usage_end_ms.or(if waited {
            self.last_model_output_ms
        } else {
            token_count_ms
        });
        let boundary = self.last_boundary_ms.max(token_count_ms);
        let last_count = token_count_ms.or(self.last_token_count_ms);
        *self = Self {
            last_boundary_ms: boundary,
            last_token_count_ms: last_count,
            ..Self::default()
        };
        let start = start?;
        let latency = crate::usage::plausible_latency_ms(start, end?)?;
        let first_token = first.and_then(|at| {
            let offset = at.checked_sub(start)?;
            (offset > 0 && offset <= latency).then_some(offset)
        });
        Some((latency, first_token))
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn head_str<'a>(head: &'a [u8], key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\":\"");
    let start = find_bytes(head, needle.as_bytes())? + needle.len();
    let rest = &head[start..];
    let len = rest.iter().position(|byte| *byte == b'"')?;
    std::str::from_utf8(&rest[..len]).ok()
}

fn classify(line: &[u8]) -> Option<(i64, TimingKind)> {
    let head = &line[..line.len().min(LINE_HEAD)];
    let (envelope, payload) = match find_bytes(head, b"\"payload\":{") {
        Some(at) => head.split_at(at),
        None => (head, &[][..]),
    };
    let timestamp_ms = head_str(envelope, "timestamp").and_then(parse_timestamp_ms)?;
    let kind = match head_str(envelope, "type")? {
        "turn_context" => TimingKind::Boundary,
        "token_usage_record" => TimingKind::UsageRecord,
        "response_item" => match head_str(payload, "type")? {
            "message" => match head_str(payload, "role")? {
                "assistant" => TimingKind::ModelOutput,
                _ => TimingKind::Boundary,
            },
            "reasoning" => TimingKind::ModelOutput,
            item if item.ends_with("_output") => TimingKind::ToolOutput,
            item if item.ends_with("_call") => TimingKind::ModelOutput,
            _ => return None,
        },
        "event_msg" => match head_str(payload, "type")? {
            "task_started" | "turn_aborted" => TimingKind::TurnReset,
            "item_completed" => output_started(line, payload)?,
            _ => return None,
        },
        _ => return None,
    };
    Some((timestamp_ms, kind))
}

fn output_started(line: &[u8], payload_head: &[u8]) -> Option<TimingKind> {
    let item_at = find_bytes(payload_head, b"\"item\":{")?;
    match head_str(&payload_head[item_at..], "type")? {
        "Reasoning" | "AgentMessage" => {}
        _ => return None,
    }
    if line.len() > ITEM_COMPLETED_LIMIT {
        return None;
    }
    let value: Value = serde_json::from_slice(line).ok()?;
    let started = value.pointer("/payload/started_at_ms")?.as_i64()?;
    Some(TimingKind::OutputStarted(started))
}

fn parse_timestamp_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn entry_millis(entry: &Entry) -> Option<i64> {
    parse_timestamp_ms(entry.timestamp.as_deref()?)
}

fn apply_timing(row: &mut UsageRecord, timing: Option<(i64, Option<i64>)>) {
    if let Some((duration, first)) = timing {
        row.duration_ms = Some(duration);
        row.first_event_ms = first;
    }
}

impl FileState {
    /// Applies one log line, returning a usage row when it reports new counts.
    fn line(&mut self, line: &[u8], since: i64) -> Option<UsageRecord> {
        let observed = self.timer.observe(line);
        if !wanted(line) {
            return None;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            if observed == Some(TimingKind::UsageRecord) {
                self.timer.finish(None);
            }
            return None;
        };
        let payload = &entry.payload;
        match entry.kind.as_str() {
            "session_meta" => {
                self.session = text(&payload.id).or_else(|| text(&payload.session_id));
                self.provider = text(&payload.model_provider);
                None
            }
            "turn_context" => {
                self.model = text(&payload.model).or(self.model.take());
                None
            }
            "token_usage_record" => {
                self.records = true;
                let timing = self.timer.finish(entry_millis(&entry));
                let usage = payload.usage.as_ref().and_then(TokenUsage::from_codex)?;
                let response_id = payload.response_id.as_ref().and_then(super::response_id);
                let thread = text(&payload.thread_id)
                    .or_else(|| text(&payload.session_id))
                    .or_else(|| self.session.clone())?;
                let key = response_id
                    .clone()
                    .or_else(|| entry.ordinal.map(|ordinal| format!("o{ordinal}")))?;
                let mut row = self.record(
                    &entry,
                    usage,
                    format!("codex:{thread}:{key}"),
                    response_id,
                    thread,
                    since,
                )?;
                apply_timing(&mut row, timing);
                Some(row)
            }
            "event_msg"
                if !self.records
                    && payload.kind.as_ref().and_then(Value::as_str) == Some("token_count") =>
            {
                let at = entry_millis(&entry);
                let Some(total) = payload
                    .info
                    .as_ref()
                    .and_then(|info| info.get("total_token_usage"))
                    .and_then(TokenUsage::from_codex)
                else {
                    self.timer.finish(at);
                    return None;
                };
                let earlier = self.total.map(from_array).unwrap_or_default();
                // Repeated events carry the same total; only growth is new usage.
                if total.total() <= earlier.total() {
                    return None;
                }
                let timing = self.timer.finish(at);
                self.total = Some(to_array(total));
                let session = self.session.clone()?;
                let id = format!("codex:{session}:t{}", total.total());
                let mut row =
                    self.record(&entry, total.since(earlier), id, None, session, since)?;
                apply_timing(&mut row, timing);
                Some(row)
            }
            _ => None,
        }
    }

    fn record(
        &self,
        entry: &Entry,
        usage: TokenUsage,
        id: String,
        response_id: Option<String>,
        session: String,
        since: i64,
    ) -> Option<UsageRecord> {
        let started_at_ms = chrono::DateTime::parse_from_rfc3339(entry.timestamp.as_deref()?)
            .ok()?
            .timestamp_millis();
        let provider = self.provider.as_deref();
        // The router owns routed usage after it began recording; a logged entry
        // with a response ID is kept only when the router has no such response.
        if provider == Some(ROUTER_PROVIDER) && started_at_ms >= since && response_id.is_none() {
            return None;
        }
        if usage.is_empty() {
            return None;
        }
        let model = self.model.clone().unwrap_or_default();
        let (provider_id, codex_provider, public_model) = match provider {
            Some(ROUTER_PROVIDER) => (None, Some(ROUTER_PROVIDER.to_owned()), Some(model.clone())),
            Some(key) if key.starts_with(DIRECT_PROVIDER_PREFIX) => (
                Some(key[DIRECT_PROVIDER_PREFIX.len()..].to_owned()).filter(|id| !id.is_empty()),
                None,
                None,
            ),
            other => (None, other.map(str::to_owned), None),
        };
        Some(UsageRecord {
            reported_cost_micros: None,
            request_count: 1,
            client: crate::storage::UsageClient::Codex,
            id,
            source: UsageSource::CodexLog,
            started_at_ms,
            provider_id,
            codex_provider,
            public_model,
            model,
            usage,
            duration_ms: None,
            first_event_ms: None,
            response_id,
            session_id: Some(session),
        })
    }
}

fn to_array(usage: TokenUsage) -> [u64; 5] {
    [
        usage.input,
        usage.cached_input,
        usage.cache_write,
        usage.output,
        usage.reasoning,
    ]
}

fn from_array(values: [u64; 5]) -> TokenUsage {
    TokenUsage {
        cache_write_1h: 0,
        input: values[0],
        cached_input: values[1],
        cache_write: values[2],
        output: values[3],
        reasoning: values[4],
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::storage::{RequestRecord, RequestStatus};
    use serde_json::json;

    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "switchx-codex-logs-{}",
                crate::id::new_id().unwrap()
            ));
            std::fs::create_dir_all(path.join("sessions/2026/10/13")).unwrap();
            Self(path)
        }

        fn append(&self, name: &str, lines: &[Value]) -> PathBuf {
            self.append_lines(name, lines.iter().map(|line| line.to_string()))
        }

        /// Writes lines as given. Codex puts `timestamp` and `type` before `payload`;
        /// `serde_json::Value` sorts keys and would hide those fields behind a long body.
        fn append_lines(
            &self,
            name: &str,
            lines: impl IntoIterator<Item = impl AsRef<str>>,
        ) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            for line in lines {
                writeln!(file, "{}", line.as_ref()).unwrap();
            }
            path
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ROLLOUT: &str = "sessions/2026/10/13/rollout-2026-10-13T08-00-00-synthetic.jsonl";

    fn meta(provider: &str) -> Value {
        json!({"timestamp":"2026-10-13T08:00:00.000Z","ordinal":0,"type":"session_meta","payload":{
            "id":"session-1","model_provider":provider,"cwd":"/private/project",
            "base_instructions":{"text":"private instructions"}}})
    }

    fn turn(model: &str) -> Value {
        json!({"timestamp":"2026-10-13T08:00:01.000Z","type":"turn_context","payload":{
            "model":model,"cwd":"/private/project","effort":"high"}})
    }

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        json!({"input_tokens":input,"cached_input_tokens":cached,"cache_write_input_tokens":0,
            "output_tokens":output,"reasoning_output_tokens":reasoning,"total_tokens":input + output})
    }

    fn record(time: &str, response: Option<&str>, value: Value) -> Value {
        json!({"timestamp":time,"ordinal":7,"type":"token_usage_record","payload":{
            "thread_id":"thread-1","session_id":"session-1","response_id":response,
            "usage":value,"turn_token_usage":value,"thread_token_usage":value}})
    }

    fn count(time: &str, total: Value) -> Value {
        json!({"timestamp":time,"type":"event_msg","payload":{"type":"token_count",
            "info":{"total_token_usage":total,"last_token_usage":total},
            "rate_limits":{"plan_type":"synthetic"}}})
    }

    fn run(store: &Store, home: &Home) -> ImportSummary {
        import(store, &home.0, &AtomicBool::new(false), |_| {}).unwrap()
    }

    fn rows(store: &Store) -> Vec<UsageRecord> {
        let mut rows = store.usage_records(0, i64::MAX).unwrap();
        rows.reverse();
        rows
    }

    #[test]
    fn per_response_records_import_incrementally_without_private_fields() {
        let home = Home::new();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let path = home.append(
            ROLLOUT,
            &[
                meta("openai"),
                turn("gpt-synthetic"),
                json!({"timestamp":"2026-10-13T08:00:02.000Z","type":"response_item","payload":{
                    "type":"message","content":"\"type\":\"token_count\" private text"}}),
                record(
                    "2026-10-13T08:00:03.000Z",
                    Some("resp_a"),
                    usage(1_000, 600, 50, 10),
                ),
                count("2026-10-13T08:00:03.500Z", usage(1_000, 600, 50, 10)),
                count("2026-10-13T08:00:04.000Z", usage(1_000, 600, 50, 10)),
            ],
        );
        // A line still being written is left for the next scan.
        let next = format!(
            "{}\n{}\n",
            turn("gpt-next"),
            record(
                "2026-10-13T08:00:06.000Z",
                Some("resp_b"),
                usage(400, 100, 20, 0)
            )
        );
        let (written, rest) = next.split_at(next.len() - 40);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(written.as_bytes()).unwrap();
        let summary = run(&store, &home);
        assert_eq!((summary.files, summary.read, summary.added), (1, 1, 1));
        let first = rows(&store);
        assert_eq!(first.len(), 1);
        let row = &first[0];
        assert_eq!(row.id, "codex:thread-1:resp_a");
        assert_eq!(row.source, UsageSource::CodexLog);
        assert_eq!(row.started_at_ms, 1_791_878_403_000);
        assert_eq!(row.codex_provider.as_deref(), Some("openai"));
        assert_eq!(
            (row.provider_id.as_deref(), row.model.as_str()),
            (None, "gpt-synthetic")
        );
        assert_eq!(row.session_id.as_deref(), Some("thread-1"));
        assert_eq!(
            (
                row.usage.input,
                row.usage.cached_input,
                row.usage.output,
                row.usage.reasoning
            ),
            (1_000, 600, 50, 10)
        );
        let state = &store.usage_log_files().unwrap()[0];
        assert!(!state.state.contains("private") && !format!("{first:?}").contains("private"));

        assert_eq!(run(&store, &home).read, 0, "unchanged files are skipped");
        file.write_all(rest.as_bytes()).unwrap();
        drop(file);
        assert_eq!(run(&store, &home).added, 1);
        let rows = rows(&store);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[1].model.as_str(), rows[1].usage.total()),
            ("gpt-next", 420)
        );
    }

    #[test]
    fn cumulative_counts_skip_repeats_and_survive_copies_and_rewrites() {
        let home = Home::new();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let lines = [
            meta("custom_provider"),
            turn("legacy-model"),
            count("2026-10-13T08:00:03.000Z", usage(100, 40, 10, 0)),
            count("2026-10-13T08:00:04.000Z", usage(100, 40, 10, 0)),
            count("2026-10-13T08:00:05.000Z", usage(250, 140, 30, 5)),
        ];
        home.append(ROLLOUT, &lines);
        assert_eq!(run(&store, &home).added, 2);
        let deltas = rows(&store)
            .iter()
            .map(|row| {
                (
                    row.usage.input,
                    row.usage.cached_input,
                    row.usage.output,
                    row.usage.reasoning,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(deltas, [(100, 40, 10, 0), (150, 100, 20, 5)]);
        assert_eq!(
            rows(&store)[0].codex_provider.as_deref(),
            Some("custom_provider")
        );

        // Archived copies and rewritten files are deduplicated by record identity.
        home.append(
            "archived_sessions/rollout-2026-10-13T08-00-00-synthetic.jsonl",
            &lines,
        );
        let path = home.0.join(ROLLOUT);
        std::fs::write(&path, "").unwrap();
        home.append(ROLLOUT, &lines[..3]);
        let summary = run(&store, &home);
        assert_eq!((summary.files, summary.added), (2, 0));
        assert_eq!(rows(&store).len(), 2);
    }

    #[test]
    fn routed_and_direct_sessions_are_attributed_without_double_counting() {
        let home = Home::new();
        let store = Store::open(Path::new(":memory:")).unwrap();
        let routed = RequestRecord {
            client: crate::storage::UsageClient::Unknown,
            workspace_id: None,
            id: "router-request".into(),
            started_at_ms: 4_000_000_000_000,
            public_model: Some("显示名称".into()),
            provider_id: Some("one".into()),
            upstream_model: Some("actual".into()),
            generation: "generation".into(),
            http_status: Some(200),
            headers_ms: None,
            first_event_ms: None,
            duration_ms: 10,
            status: RequestStatus::Completed,
            error_code: None,
            fallback_from: None,
            usage: Some(TokenUsage {
                input: 9,
                output: 1,
                ..Default::default()
            }),
            response_id: Some("resp_routed".into()),
        };
        store.put_request(&routed).unwrap();
        home.append(
            ROLLOUT,
            &[
                meta("switchx_router"),
                turn("显示名称"),
                // Before routed recording began: only the log knows this usage.
                record(
                    "2020-01-01T00:00:00.000Z",
                    Some("resp_old"),
                    usage(10, 0, 1, 0),
                ),
                record(
                    "2099-01-01T00:00:00.000Z",
                    Some("resp_routed"),
                    usage(9, 0, 1, 0),
                ),
                record("2099-01-01T00:00:01.000Z", None, usage(5, 0, 1, 0)),
                record(
                    "2099-01-01T00:00:02.000Z",
                    Some("resp_unrecorded"),
                    usage(7, 0, 1, 0),
                ),
            ],
        );
        home.append(
            "sessions/2026/10/13/rollout-direct.jsonl",
            &[
                meta("switchx_direct_deepseek-1"),
                turn("deepseek-chat"),
                record(
                    "2026-10-13T08:00:03.000Z",
                    Some("resp_direct"),
                    usage(20, 5, 2, 0),
                ),
            ],
        );
        run(&store, &home);
        let rows = rows(&store)
            .into_iter()
            .map(|row| {
                (
                    row.id,
                    row.provider_id,
                    row.codex_provider,
                    row.public_model,
                    row.model,
                )
            })
            .collect::<Vec<_>>();
        let routed_log = |id: &str| {
            (
                id.to_owned(),
                None,
                Some("switchx_router".to_owned()),
                Some("显示名称".to_owned()),
                "显示名称".to_owned(),
            )
        };
        assert_eq!(
            rows,
            [
                routed_log("codex:thread-1:resp_old"),
                (
                    "codex:thread-1:resp_direct".into(),
                    Some("deepseek-1".into()),
                    None,
                    None,
                    "deepseek-chat".into()
                ),
                (
                    "router-request".into(),
                    Some("one".into()),
                    None,
                    Some("显示名称".into()),
                    "actual".into()
                ),
                routed_log("codex:thread-1:resp_unrecorded"),
            ]
        );

        // A router write that lands after the import replaces the logged copy.
        store
            .put_request(&RequestRecord {
                client: crate::storage::UsageClient::Unknown,
                workspace_id: None,
                id: "late-router".into(),
                response_id: Some("resp_unrecorded".into()),
                ..routed
            })
            .unwrap();
        let ids = super::tests::rows(&store)
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        assert!(!ids.contains(&"codex:thread-1:resp_unrecorded".to_owned()));
        assert!(ids.contains(&"late-router".to_owned()));
    }

    fn millis(stamp: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(stamp)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn event_order_estimates_timing_backfills_it_and_leaves_router_rows_measured() {
        let home = Home::new();
        let db_path = std::env::temp_dir().join(format!(
            "switchx-codex-timing-{}.sqlite",
            crate::id::new_id().unwrap()
        ));
        let store = Store::open(&db_path).unwrap();
        let secret = "secret-prompt";
        let started = millis("2026-10-13T08:00:02.000Z");
        let later = millis("2026-10-13T08:00:03.500Z");
        home.append_lines(
            ROLLOUT,
            [
                r#"{"timestamp":"2026-10-13T08:00:00.000Z","type":"session_meta","payload":{"id":"session-1","model_provider":"openai"}}"#,
                r#"{"timestamp":"2026-10-13T08:00:00.500Z","type":"turn_context","payload":{"model":"gpt-synthetic"}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{secret}"}}}}"#),
                &format!(r#"{{"timestamp":"2026-10-13T08:00:03.000Z","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"Reasoning","id":"i1"}},"started_at_ms":{started}}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:03.000Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:00:04.000Z","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"AgentMessage","id":"i2"}},"started_at_ms":{later}}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:04.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":"secret-reply"}}"#,
                r#"{"timestamp":"2026-10-13T08:00:05.000Z","type":"token_usage_record","payload":{"thread_id":"thread-1","response_id":"resp_a","usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":400,"reasoning_output_tokens":0,"total_tokens":500}}}"#,
            ],
        );
        let long_output = "x".repeat(2_000);
        home.append_lines(
            "sessions/2026/10/13/rollout-legacy.jsonl",
            [
                r#"{"timestamp":"2026-10-13T08:00:00.000Z","type":"session_meta","payload":{"id":"legacy","model_provider":"openai"}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:00:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{secret}"}}}}"#),
                &format!(r#"{{"timestamp":"2026-10-13T08:00:03.000Z","type":"response_item","payload":{{"type":"custom_tool_call","name":"shell","input":"{secret}"}}}}"#),
                &format!(r#"{{"timestamp":"2026-10-13T08:00:10.000Z","type":"response_item","payload":{{"type":"custom_tool_call_output","output":"{long_output}"}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:10.050Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":110}}}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:00:10.080Z","type":"response_item","payload":{{"type":"reasoning","summary":["{secret}"]}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:11.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":110}}}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:01:40.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{secret}"}}}}"#),
                r#"{"timestamp":"2026-10-13T08:01:50.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"cached_input_tokens":0,"output_tokens":30,"reasoning_output_tokens":0,"total_tokens":190}}}}"#,
            ],
        );
        run(&store, &home);
        let timed = rows(&store);
        let row = |id: &str| timed.iter().find(|row| row.id == id).unwrap();
        assert_eq!(
            (
                row("codex:thread-1:resp_a").duration_ms,
                row("codex:thread-1:resp_a").first_event_ms
            ),
            (Some(4_000), Some(1_000))
        );
        assert_eq!(row("codex:thread-1:resp_a").usage.output, 400);
        // The count was flushed with the tool result, so the span ends at the tool call.
        assert_eq!(row("codex:legacy:t110").duration_ms, Some(2_000));
        // The reasoning line flushed just after the count is not the next request.
        assert_eq!(row("codex:legacy:t190").duration_ms, Some(10_000));
        assert_eq!(row("codex:legacy:t190").usage.output, 20);
        let state = store.usage_log_files().unwrap();
        assert!(state.iter().all(|file| {
            !file.state.contains(secret) && !format!("{timed:?}").contains(secret)
        }));

        store
            .put_request(&RequestRecord {
                client: crate::storage::UsageClient::Unknown,
                workspace_id: None,
                id: "router-request".into(),
                started_at_ms: millis("2026-10-13T08:00:30.000Z"),
                public_model: Some("显示名称".into()),
                provider_id: Some("one".into()),
                upstream_model: Some("actual".into()),
                generation: "generation".into(),
                http_status: Some(200),
                headers_ms: None,
                first_event_ms: Some(4),
                duration_ms: 10,
                status: RequestStatus::Completed,
                error_code: None,
                fallback_from: None,
                usage: Some(TokenUsage {
                    input: 9,
                    output: 1,
                    ..Default::default()
                }),
                response_id: Some("resp_keep".into()),
            })
            .unwrap();
        home.append_lines(
            ROLLOUT,
            [
                &format!(r#"{{"timestamp":"2026-10-13T08:00:30.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{secret}"}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:34.000Z","type":"token_usage_record","payload":{"thread_id":"thread-1","response_id":"resp_keep","usage":{"input_tokens":9,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":10}}}"#,
                &format!(r#"{{"timestamp":"2026-10-13T08:00:40.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":"{secret}"}}}}"#),
                r#"{"timestamp":"2026-10-13T08:00:42.000Z","type":"token_usage_record","payload":{"thread_id":"thread-1","response_id":"resp_b","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":20,"reasoning_output_tokens":0,"total_tokens":30}}}"#,
            ],
        );
        // One new logged row, plus the router row's client attribution.
        assert_eq!(run(&store, &home).added, 2);
        let timed = rows(&store);
        let row = |id: &str| timed.iter().find(|row| row.id == id).unwrap();
        assert_eq!(row("codex:thread-1:resp_b").duration_ms, Some(2_000));
        assert_eq!(row("codex:thread-1:resp_b").first_event_ms, None);
        let router = row("router-request");
        assert_eq!(
            (router.source, router.duration_ms, router.first_event_ms),
            (UsageSource::Router, Some(10), Some(4))
        );
        assert!(
            !timed
                .iter()
                .any(|row| row.response_id.as_deref() == Some("resp_keep")
                    && row.source == UsageSource::CodexLog)
        );
        assert_eq!(run(&store, &home).read, 0);

        let counts: Vec<_> = timed
            .iter()
            .filter(|row| row.source == UsageSource::CodexLog)
            .map(|row| (row.id.clone(), row.usage.output))
            .collect();
        let files = store.usage_log_files().unwrap();
        drop(store);
        let poke = rusqlite::Connection::open(&db_path).unwrap();
        poke.execute(
            "UPDATE usage_records SET duration_ms = NULL, first_event_ms = NULL WHERE source = 'codex_log'",
            [],
        )
        .unwrap();
        for file in &files {
            let mut value: Value = serde_json::from_str(&file.state).unwrap();
            let object = value.as_object_mut().unwrap();
            object.remove("timing");
            object.remove("timer");
            poke.execute(
                "UPDATE usage_log_files SET state = ?1 WHERE path = ?2",
                rusqlite::params![value.to_string(), file.path],
            )
            .unwrap();
        }
        drop(poke);
        let store = Store::open(&db_path).unwrap();
        assert_eq!(run(&store, &home).added, counts.len());
        let restored = rows(&store);
        assert_eq!(restored.len(), timed.len());
        for (id, output) in counts {
            let row = restored.iter().find(|row| row.id == id).unwrap();
            assert_eq!(row.usage.output, output);
            assert!(row.duration_ms.is_some(), "{id}");
        }
        let router = restored
            .iter()
            .find(|row| row.id == "router-request")
            .unwrap();
        assert_eq!(
            (router.duration_ms, router.first_event_ms),
            (Some(10), Some(4))
        );
        drop(store);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    }

    #[test]
    fn missing_directories_and_cancellation_are_reported() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let missing = std::env::temp_dir().join("switchx-no-such-codex-home");
        let summary = import(&store, &missing, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(summary, ImportSummary::default());
        let home = Home::new();
        home.append(ROLLOUT, &[meta("openai")]);
        let mut seen = Vec::new();
        let result = import(&store, &home.0, &AtomicBool::new(true), |p| seen.push(p));
        assert!(matches!(result, Err(ImportError::Cancelled)));
        assert!(store.usage_log_files().unwrap().is_empty());
        run(&store, &home);
        store.clear_imported_usage().unwrap();
        assert!(store.usage_log_files().unwrap().is_empty());
    }
}
