//! Incremental import of token usage from Codex session logs.
//!
//! Rollout files are append-only JSONL. Only four record kinds are decoded —
//! session metadata (session ID and provider key), turn context (model), and the
//! two usage records — and only counts, IDs, model IDs, and times are kept.
//! Paths, prompts, instructions, and tool output are never stored.

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
    if resume.is_some_and(|file| file.size == size && file.modified_ms == modified_ms) {
        return Ok(None);
    }
    let mut state = resume
        .and_then(|file| serde_json::from_str::<FileState>(&file.state).ok())
        .unwrap_or_default();
    let mut offset = resume.map_or(0, |file| file.read_offset);
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

impl FileState {
    /// Applies one log line, returning a usage row when it reports new counts.
    fn line(&mut self, line: &[u8], since: i64) -> Option<UsageRecord> {
        if !wanted(line) {
            return None;
        }
        let entry: Entry = serde_json::from_slice(line).ok()?;
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
                let usage = TokenUsage::from_codex(payload.usage.as_ref()?)?;
                let response_id = payload.response_id.as_ref().and_then(super::response_id);
                let thread = text(&payload.thread_id)
                    .or_else(|| text(&payload.session_id))
                    .or_else(|| self.session.clone())?;
                let key = response_id
                    .clone()
                    .or_else(|| entry.ordinal.map(|ordinal| format!("o{ordinal}")))?;
                self.record(
                    &entry,
                    usage,
                    format!("codex:{thread}:{key}"),
                    response_id,
                    thread,
                    since,
                )
            }
            "event_msg"
                if !self.records
                    && payload.kind.as_ref().and_then(Value::as_str) == Some("token_count") =>
            {
                let total =
                    TokenUsage::from_codex(payload.info.as_ref()?.get("total_token_usage")?)?;
                let earlier = self.total.map(from_array).unwrap_or_default();
                // Repeated events carry the same total; only growth is new usage.
                if total.total() <= earlier.total() {
                    return None;
                }
                self.total = Some(to_array(total));
                let session = self.session.clone()?;
                let id = format!("codex:{session}:t{}", total.total());
                self.record(&entry, total.since(earlier), id, None, session, since)
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
            let path = self.0.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            for line in lines {
                writeln!(file, "{line}").unwrap();
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
