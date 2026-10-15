//! Incremental Claude Code transcript import. Only counters, model/message/session
//! IDs and timestamps are decoded; conversation contents and credentials are ignored.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use serde::Deserialize;
use serde_json::Value;

use super::{
    TokenUsage,
    codex_logs::{ImportError, ImportProgress, ImportSummary},
};
use crate::storage::{Store, UsageClient, UsageLogFile, UsageRecord, UsageSource};

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Value>,
}

fn identifier(value: Option<String>) -> Option<String> {
    value.filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
}

fn record(line: &[u8]) -> Option<UsageRecord> {
    let entry: Entry = serde_json::from_slice(line).ok()?;
    if entry.kind != "assistant" {
        return None;
    }
    let message = entry.message?;
    let id = identifier(message.id)?;
    let usage = TokenUsage::from_claude(message.usage.as_ref()?)?;
    if usage.is_empty() {
        return None;
    }
    let started_at_ms = chrono::DateTime::parse_from_rfc3339(entry.timestamp.as_deref()?)
        .ok()?
        .timestamp_millis();
    Some(UsageRecord {
        reported_cost_micros: None,
        request_count: 1,
        // Message IDs survive copied/forked transcripts and are shared by all blocks.
        id: format!("claude:{id}"),
        source: UsageSource::ClaudeLog,
        client: UsageClient::ClaudeCode,
        started_at_ms,
        provider_id: None,
        codex_provider: None,
        public_model: None,
        model: identifier(message.model).unwrap_or_default(),
        usage,
        duration_ms: None,
        first_event_ms: None,
        response_id: None,
        session_id: identifier(entry.session),
    })
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
    Ok(format!("claude-v1:{hash:016x}"))
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
        if let Some(record) = record(&line) {
            records.push(record);
        }
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
                .all(|cursor| cursor.state.starts_with("claude-v1:")
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
}
