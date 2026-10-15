use super::*;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Entry {
    method: Option<String>,
    timestamp: Option<Value>,
    params: Option<Params>,
}
#[derive(Deserialize)]
struct Params {
    update: Option<Update>,
}
#[derive(Deserialize)]
struct Update {
    #[serde(rename = "sessionUpdate")]
    kind: Option<String>,
    prompt_id: Option<String>,
    usage: Option<Usage>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    #[serde(default)]
    model_usage: BTreeMap<String, Counters>,
    #[serde(default)]
    cost_is_partial: bool,
    #[serde(flatten)]
    counters: Counters,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counters {
    input_tokens: Option<Value>,
    output_tokens: Option<Value>,
    cached_read_tokens: Option<Value>,
    reasoning_tokens: Option<Value>,
    model_calls: Option<Value>,
    api_duration_ms: Option<Value>,
    cost_usd_ticks: Option<Value>,
    #[serde(default)]
    cost_is_partial: bool,
}

fn parse(line: &[u8], session: &str, offset: u64) -> Vec<UsageRecord> {
    let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
        return vec![];
    };
    if entry.method.as_deref() != Some("_x.ai/session/update") {
        return vec![];
    }
    let Some(at) = entry.timestamp.as_ref().and_then(timestamp) else {
        return vec![];
    };
    let Some(update) = entry.params.and_then(|p| p.update) else {
        return vec![];
    };
    if update
        .kind
        .as_deref()
        .is_some_and(|k| k != "turn_completed")
    {
        return vec![];
    }
    let Some(mut data) = update.usage else {
        return vec![];
    };
    if data.model_usage.is_empty() {
        data.model_usage.insert(String::new(), data.counters);
    }
    let prompt = label(update.prompt_id).unwrap_or_else(|| format!("at:{at}:offset:{offset}"));
    data.model_usage
        .into_iter()
        .filter_map(|(model, c)| {
            let usage = usage(
                count(c.input_tokens.as_ref()),
                count(c.cached_read_tokens.as_ref()),
                0,
                count(c.output_tokens.as_ref()),
                count(c.reasoning_tokens.as_ref()),
                false,
                false,
            );
            let native = if !data.cost_is_partial && !c.cost_is_partial {
                c.cost_usd_ticks
                    .as_ref()
                    .and_then(Value::as_u64)
                    .map(|n| ((n as u128 + 5000) / 10000) as u64)
            } else {
                None
            };
            if usage.is_empty() && native.is_none() {
                return None;
            }
            let identity = serde_json::to_string(&(&prompt, &model)).ok()?;
            let mut row = record(
                UsageClient::GrokBuild,
                session,
                &identity,
                label(Some(model)).unwrap_or_default(),
                at,
                usage,
            );
            row.reported_cost_micros = native;
            row.request_count = count(c.model_calls.as_ref()).clamp(1, 1_000_000);
            row.duration_ms = c
                .api_duration_ms
                .as_ref()
                .and_then(Value::as_u64)
                .map(|n| n.min(i64::MAX as u64) as i64);
            Some(row)
        })
        .collect()
}

pub fn import(
    store: &Store,
    home: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, ImportError> {
    let mut paths = files(&home.join("sessions"), 12, &|p| {
        p.file_name().is_some_and(|n| n == "updates.jsonl")
    });
    paths.extend(files(&home.join("archived_sessions"), 12, &|p| {
        p.file_name().is_some_and(|n| n == "updates.jsonl")
    }));
    scan(paths, cancel, progress, |path| {
        let session = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .unwrap_or("");
        jsonl::<()>(
            store,
            path,
            UsageClient::GrokBuild,
            cancel,
            |line, _, offset| parse(line, session, offset),
        )
    })
}
