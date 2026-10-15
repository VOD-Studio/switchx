use super::*;

#[derive(Default, Serialize, Deserialize)]
struct State {
    session: Option<String>,
}
#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    timestamp: Option<Value>,
    message: Option<Message>,
    usage: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: Option<String>,
    model: Option<String>,
    response_model: Option<String>,
    provider: Option<String>,
    timestamp: Option<Value>,
    usage: Option<Value>,
    response_id: Option<String>,
    tool_call_id: Option<String>,
}

fn parse(line: &[u8], state: &mut State) -> Vec<UsageRecord> {
    let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
        return vec![];
    };
    if entry.kind == "session" {
        state.session = label(entry.id);
        return vec![];
    }
    let message = entry.message.as_ref();
    let (kind, tokens) = match entry.kind.as_str() {
        "message" => match message.and_then(|m| m.role.as_deref()) {
            Some("assistant") => ("assistant", message.and_then(|m| m.usage.as_ref())),
            Some("toolResult") => ("tool_result", message.and_then(|m| m.usage.as_ref())),
            _ => return vec![],
        },
        "compaction" | "branch_summary" => (entry.kind.as_str(), entry.usage.as_ref()),
        _ => return vec![],
    };
    let Some(tokens) = tokens else { return vec![] };
    let Some(at) = entry.timestamp.as_ref().and_then(timestamp).or_else(|| {
        message
            .and_then(|m| m.timestamp.as_ref())
            .and_then(timestamp)
    }) else {
        return vec![];
    };
    let model = message
        .and_then(|m| label(m.response_model.clone()).or_else(|| label(m.model.clone())))
        .unwrap_or_default();
    let id = label(entry.id)
        .or_else(|| message.and_then(|m| label(m.response_id.clone())))
        .unwrap_or_else(|| {
            format!(
                "{kind}:{at}:{}:{}:{}",
                message.and_then(|m| m.provider.as_deref()).unwrap_or(""),
                model,
                message
                    .and_then(|m| m.tool_call_id.as_deref())
                    .unwrap_or("")
            )
        });
    let identity = format!("{kind}:{id}:{at}");
    let usage = usage(
        count(tokens.get("input")),
        count(tokens.get("cacheRead")),
        count(tokens.get("cacheWrite")),
        count(tokens.get("output")),
        0,
        true,
        false,
    );
    let costs = tokens.get("cost");
    let cost = costs
        .and_then(|c| c.get("total"))
        .and_then(usd_micros)
        .filter(|n| *n > 0)
        .or_else(|| {
            let cost = costs?;
            let mut sum = 0u64;
            for key in ["input", "output", "cacheRead", "cacheWrite"] {
                let amount = match cost.get(key).and_then(usd_micros) {
                    Some(value) => value,
                    None if count(tokens.get(key)) == 0 => 0,
                    None => return None,
                };
                sum = sum.checked_add(amount)?;
            }
            (sum > 0 && sum <= i64::MAX as u64).then_some(sum)
        });
    if usage.is_empty() && cost.is_none() {
        return vec![];
    }
    // Pi preserves ancestor entry IDs in forks. Do not count copies as new calls.
    let mut row = record(UsageClient::Pi, "", &identity, model, at, usage);
    row.session_id = state.session.clone();
    row.public_model = message.and_then(|m| label(m.model.clone()));
    row.reported_cost_micros = cost;
    row.response_id = message
        .and_then(|m| m.response_id.as_ref())
        .and_then(|id| super::super::response_id(&Value::String(id.clone())));
    vec![row]
}

pub fn import(
    store: &Store,
    sessions: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let paths = files(sessions, 8, &|p| {
        p.extension().is_some_and(|n| n == "jsonl")
    });
    scan(paths, cancel, progress, |path| {
        jsonl::<State>(store, path, UsageClient::Pi, cancel, |line, state, _| {
            parse(line, state)
        })
    })
}
