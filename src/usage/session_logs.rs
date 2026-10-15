//! Shared read-only import plumbing. Persist only usage metadata and file cursors.
use super::{
    TokenUsage,
    codex_logs::{ImportError, ImportProgress, ImportSummary},
};
use crate::storage::{Store, UsageClient, UsageLogFile, UsageRecord, UsageSource};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

pub mod gemini;
pub mod grok;
pub mod pi;
pub mod sqlite;

#[cfg(test)]
mod tests;

pub fn import(
    store: &Store,
    client: UsageClient,
    path: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    match client {
        UsageClient::Gemini => gemini::import(store, path, cancel, progress),
        UsageClient::GrokBuild => grok::import(store, path, cancel, progress),
        UsageClient::Pi => pi::import(store, path, cancel, progress),
        UsageClient::OpenCode | UsageClient::Mcode => {
            sqlite::import(store, client, path, cancel, progress)
        }
        _ => Ok(ImportSummary::default()),
    }
}

pub(super) fn label(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
}
pub(super) fn count(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(i64::MAX as u64 / 32)
}
pub(super) fn timestamp(value: &Value) -> Option<i64> {
    let ms = if let Some(n) = value.as_i64() {
        if (-100_000_000_000..100_000_000_000).contains(&n) {
            n.checked_mul(1000)?
        } else {
            n
        }
    } else {
        chrono::DateTime::parse_from_rfc3339(value.as_str()?)
            .ok()?
            .timestamp_millis()
    };
    (0..=253_402_300_799_999).contains(&ms).then_some(ms)
}

/// Decimal/scientific USD values rounded once to microdollars, without binary floats.
pub(super) fn usd_micros(value: &Value) -> Option<u64> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (number, exponent) = text
        .split_once(['e', 'E'])
        .map_or(Some((text.as_str(), 0)), |(m, e)| {
            Some((m, e.parse::<i32>().ok()?))
        })?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let coefficient = digits.parse::<u128>().ok()?;
    let power = 6i32
        .checked_add(exponent)?
        .checked_sub(fraction.len() as i32)?;
    let micros = if power >= 0 {
        coefficient.checked_mul(10u128.checked_pow(power as u32)?)?
    } else if power < -38 {
        0
    } else {
        let divisor = 10u128.pow((-power) as u32);
        coefficient.checked_add(divisor / 2)? / divisor
    };
    (micros <= i64::MAX as u128).then_some(micros as u64)
}

pub(super) fn usage(
    input: u64,
    cached: u64,
    write: u64,
    output: u64,
    reasoning: u64,
    fresh: bool,
    separate_reasoning: bool,
) -> TokenUsage {
    let limit = i64::MAX as u64 / 32;
    let (input, cached, write, output, reasoning) = (
        input.min(limit),
        cached.min(limit),
        write.min(limit),
        output.min(limit),
        reasoning.min(limit),
    );
    TokenUsage {
        input: if fresh {
            input + cached + write
        } else {
            input.max(cached + write)
        },
        cached_input: cached,
        cache_write: write,
        cache_write_1h: 0,
        output: if separate_reasoning {
            output + reasoning
        } else {
            output.max(reasoning)
        },
        reasoning,
    }
}

pub(super) fn record(
    client: UsageClient,
    session: &str,
    id: &str,
    model: String,
    at: i64,
    usage: TokenUsage,
) -> UsageRecord {
    UsageRecord {
        id: format!(
            "{}:{}",
            client.as_str(),
            serde_json::to_string(&(session, id)).unwrap()
        ),
        source: UsageSource::SessionLog,
        client,
        started_at_ms: at,
        provider_id: None,
        codex_provider: None,
        public_model: None,
        model,
        usage,
        duration_ms: None,
        first_event_ms: None,
        response_id: None,
        session_id: Some(session.into()),
        reported_cost_micros: None,
        request_count: 1,
    }
}

pub(super) fn files(root: &Path, depth: usize, accept: &impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return result;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() && depth > 0 {
            result.extend(files(&path, depth - 1, accept));
        } else if kind.is_file() && accept(&path) {
            result.push(path);
        }
    }
    result.sort();
    result
}

pub(super) fn stamp(path: &Path) -> std::io::Result<String> {
    let m = fs::metadata(path)?;
    let modified = m
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let identity = String::new();
    Ok(format!("{identity}:{}:{modified}", m.len()))
}

fn fingerprint(file: &mut File, offset: u64) -> std::io::Result<String> {
    let len = offset.min(4096);
    file.seek(SeekFrom::Start(offset - len))?;
    let mut bytes = vec![0; len as usize];
    file.read_exact(&mut bytes)?;
    Ok(format!(
        "{:016x}",
        bytes
            .iter()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b))
                .wrapping_mul(0x100000001b3))
    ))
}
#[derive(Serialize, Deserialize)]
struct Cursor<S> {
    tail: String,
    parser: S,
}

pub(super) fn jsonl<S: Default + Serialize + DeserializeOwned>(
    store: &Store,
    path: &Path,
    client: UsageClient,
    cancel: &AtomicBool,
    mut parse: impl FnMut(&[u8], &mut S, u64) -> Vec<UsageRecord>,
) -> Result<(bool, usize), ReadError> {
    let key = format!("{}:file:{}", client.as_str(), path.display());
    let known = store
        .usage_log_file(&key)
        .map_err(|_| ReadError::Database)?;
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let revision = stamp(path)?;
    let old = known
        .as_ref()
        .and_then(|k| serde_json::from_str::<Cursor<S>>(&k.state).ok());
    let resume = if let (Some(k), Some(old)) = (known.as_ref(), old.as_ref()) {
        k.read_offset >= 0
            && k.read_offset as u64 <= size
            && fingerprint(&mut file, k.read_offset as u64)? == old.tail
    } else {
        false
    };
    let mut state = if resume {
        old.unwrap().parser
    } else {
        S::default()
    };
    let mut offset = if resume {
        known.as_ref().unwrap().read_offset as u64
    } else {
        0
    };
    // This namespaced cursor uses modified_ms for a hash of the source revision.
    if resume
        && known
            .as_ref()
            .is_some_and(|k| k.size == size as i64 && k.modified_ms == revision_hash(&revision))
    {
        return Ok((false, 0));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut rows = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || !line.ends_with(b"\n") {
            break;
        }
        rows.extend(parse(&line, &mut state, offset));
        offset += read as u64;
    }
    let tail = fingerprint(reader.get_mut(), offset)?;
    let cursor = UsageLogFile {
        path: key,
        size: size as i64,
        modified_ms: revision_hash(&revision),
        read_offset: offset as i64,
        state: serde_json::to_string(&Cursor {
            tail,
            parser: state,
        })
        .map_err(|_| ReadError::Database)?,
    };
    let changed = store
        .put_imported_usage(&rows, &cursor)
        .map_err(|_| ReadError::Database)?;
    Ok((true, changed))
}
pub(super) fn revision_hash(text: &str) -> i64 {
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    }) as i64
}

pub(super) enum ReadError {
    Io,
    Database,
    Cancelled,
}
impl From<std::io::Error> for ReadError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
impl From<rusqlite::Error> for ReadError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Io
    }
}

pub(super) fn scan(
    paths: Vec<PathBuf>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ImportProgress),
    mut read: impl FnMut(&Path) -> Result<(bool, usize), ReadError>,
) -> Result<ImportSummary, ImportError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ImportError::Cancelled);
    }
    let mut summary = ImportSummary {
        files: paths.len(),
        ..Default::default()
    };
    for (index, path) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(ImportError::Cancelled);
        }
        progress(ImportProgress {
            done: index,
            total: paths.len(),
        });
        match read(path) {
            Ok((changed, rows)) => {
                summary.read += usize::from(changed);
                summary.added += rows;
            }
            Err(ReadError::Io) => summary.unreadable += 1,
            Err(ReadError::Database) => return Err(ImportError::Database),
            Err(ReadError::Cancelled) => return Err(ImportError::Cancelled),
        }
    }
    progress(ImportProgress {
        done: paths.len(),
        total: paths.len(),
    });
    Ok(summary)
}
