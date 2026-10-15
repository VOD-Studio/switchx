use super::*;

#[derive(Default, Serialize, Deserialize)]
struct State {
    session: Option<String>,
}
#[derive(Default, Deserialize)]
struct Entry {
    #[serde(rename = "sessionId")]
    session: Option<String>,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<Value>,
    model: Option<String>,
    tokens: Option<Value>,
    #[serde(rename = "responseId")]
    response: Option<String>,
    #[serde(default)]
    messages: Vec<Entry>,
    #[serde(rename = "$set")]
    set: Option<Box<Entry>>,
}

fn records(mut entry: Entry, state: &mut State, fallback: &str) -> Vec<UsageRecord> {
    if let Some(session) = label(entry.session.take()) {
        state.session = Some(session);
    }
    let mut rows = Vec::new();
    for message in entry.messages {
        rows.extend(records(message, state, fallback));
    }
    if let Some(set) = entry.set {
        rows.extend(records(*set, state, fallback));
    }
    // Rewinds alter the conversation, not the already incurred usage. Repeated IDs
    // and checkpoint messages upsert the same record; no conversation text is kept.
    if entry.kind.as_deref() != Some("gemini") {
        return rows;
    }
    let (Some(id), Some(at), Some(tokens)) = (
        label(entry.id),
        entry.timestamp.as_ref().and_then(timestamp),
        entry.tokens,
    ) else {
        return rows;
    };
    let usage = usage(
        count(tokens.get("input")),
        count(tokens.get("cached")),
        0,
        count(tokens.get("output")),
        count(tokens.get("thoughts")),
        false,
        true,
    );
    if usage.is_empty() {
        return rows;
    }
    let mut row = record(
        UsageClient::Gemini,
        state.session.as_deref().unwrap_or(fallback),
        &id,
        label(entry.model).unwrap_or_default(),
        at,
        usage,
    );
    row.response_id = entry
        .response
        .as_ref()
        .and_then(|id| super::super::response_id(&Value::String(id.clone())));
    rows.push(row);
    rows
}

pub fn import(
    store: &Store,
    home: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let paths = files(&home.join("tmp"), 3, &|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "chats")
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-"))
            && (path.extension().is_some_and(|n| n == "jsonl")
                || (path.extension().is_some_and(|n| n == "json")
                    && !path.with_extension("jsonl").exists()))
    });
    scan(paths, cancel, progress, |path| {
        let fallback = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
        if path.extension().is_some_and(|n| n == "jsonl") {
            return jsonl::<State>(
                store,
                path,
                UsageClient::Gemini,
                cancel,
                |line, state, _| {
                    serde_json::from_slice::<Entry>(line)
                        .map(|entry| records(entry, state, fallback))
                        .unwrap_or_default()
                },
            );
        }
        let revision = stamp(path)?;
        let key = format!("gemini:json:{}", path.display());
        if store
            .usage_log_file(&key)
            .map_err(|_| ReadError::Database)?
            .is_some_and(|c| c.state == revision)
        {
            return Ok((false, 0));
        }
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let entry: Entry =
            serde_json::from_reader(BufReader::new(file)).map_err(|_| ReadError::Io)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ReadError::Cancelled);
        }
        let rows = records(entry, &mut State::default(), fallback);
        let cursor = UsageLogFile {
            path: key,
            size: size as i64,
            modified_ms: 0,
            read_offset: 0,
            state: revision,
        };
        Ok((
            true,
            store
                .put_imported_usage(&rows, &cursor)
                .map_err(|_| ReadError::Database)?,
        ))
    })
}
