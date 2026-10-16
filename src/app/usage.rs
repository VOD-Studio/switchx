//! Token usage for the usage page: reports, detail rows, session-log sync, and prices.

use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use chrono::Local;

use super::{AppError, UsageSourceView, open_store, usage_sources::usage_sources_from};
use crate::{
    storage::{
        Currency, PriceRecord, ProviderRecord, Store, UsageBucket, UsageClient, UsageFilter,
        UsageSource,
    },
    usage::{
        TokenUsage, claude_logs,
        codex_logs::{self, ImportError, ImportProgress, ImportSummary},
        report::{self, GroupKind, Inputs, Money, Period, Prices, Report, Totals, Window},
        session_logs,
    },
};

/// Detail rows load in pages as the list scrolls, up to the storage limit.
pub const USAGE_PAGE: usize = 60;
pub const USAGE_ROW_LIMIT: usize = 1_000;

/// Estimated speed includes time to first token, so short replies understate it.
const ESTIMATED_SPEED_MIN_OUTPUT: u64 = 200;
const ESTIMATED_SPEED_MIN_DURATION_MS: i64 = 1_000;

/// Router rows use measured generation speed. Every other source divides output
/// tokens by the whole estimated duration, and only past the thresholds above.
fn output_speed(record: &crate::storage::UsageRecord) -> (Option<f64>, bool) {
    if record.source == UsageSource::Router {
        let speed = match (record.duration_ms, record.first_event_ms) {
            (Some(duration), Some(first))
                if duration >= first && duration - first >= 50 && record.usage.output > 0 =>
            {
                Some(record.usage.output as f64 * 1_000.0 / (duration - first) as f64)
            }
            _ => None,
        };
        return (speed, false);
    }
    let speed = match record.duration_ms {
        Some(duration)
            if duration >= ESTIMATED_SPEED_MIN_DURATION_MS
                && record.usage.output >= ESTIMATED_SPEED_MIN_OUTPUT =>
        {
            Some(record.usage.output as f64 * 1_000.0 / duration as f64)
        }
        _ => None,
    };
    (speed, record.duration_ms.is_some())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageQuery {
    pub period: Period,
    pub filter: UsageFilter,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub id: String,
    pub started_at_ms: i64,
    pub clock: String,
    pub group: String,
    pub connection: String,
    pub kind: GroupKind,
    pub provider_id: Option<String>,
    pub model: String,
    pub public_model: Option<String>,
    pub usage: TokenUsage,
    /// `None` when the model has no price.
    pub cost: Option<(Currency, u128)>,
    pub speed: Option<f64>,
    /// The duration and speed were estimated from session-log timestamps.
    pub estimated_timing: bool,
    pub duration_ms: Option<i64>,
    pub routed: bool,
    pub source: UsageSource,
    pub client: UsageClient,
    pub reported_cost: bool,
    pub request_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageView {
    pub sources: Vec<UsageSourceView>,
    pub report: Report,
    pub today: Totals,
    pub rows: Vec<UsageRow>,
    pub total_rows: usize,
    pub prices: Vec<PriceRecord>,
    pub providers: Vec<ProviderRecord>,
    pub log_import: bool,
    pub claude_import: bool,
    pub claude_home: PathBuf,
}

pub fn load_usage(data_dir: &Path, query: &UsageQuery) -> Result<UsageView, AppError> {
    usage_from(&open_store(data_dir)?, query)
}

pub(crate) fn usage_from(store: &Store, query: &UsageQuery) -> Result<UsageView, AppError> {
    let database = |_| AppError::Database;
    let now = Local::now();
    let first = if query.period == Period::All {
        store.first_usage_ms().map_err(database)?
    } else {
        None
    };
    let window = Window::new(query.period, now, first);
    let (from, to) = (window.from_ms, window.to_ms);
    let facts = store
        .usage_facts(from, to, window.fact_bucket())
        .map_err(database)?;
    let previous = window
        .previous
        .map(|(from, to)| store.usage_facts(from, to, UsageBucket::None))
        .transpose()
        .map_err(database)?;
    let heat = store
        .usage_heat(from, to, &query.filter)
        .map_err(database)?;
    let speeds = store
        .usage_speeds(from, to, &query.filter)
        .map_err(database)?;
    let prices = store.model_prices().map_err(database)?;
    let providers = store.providers().map_err(database)?;
    let today_window = Window::new(Period::Today, now, None);
    let today_facts = store
        .usage_facts(today_window.from_ms, today_window.to_ms, UsageBucket::None)
        .map_err(database)?;
    let today = report::build(
        today_window,
        &UsageFilter::default(),
        Inputs {
            facts: &today_facts,
            previous: None,
            heat: &[],
            speeds: &[],
            prices: &prices,
            providers: &providers,
        },
    )
    .totals;
    let report = report::build(
        window,
        &query.filter,
        Inputs {
            facts: &facts,
            previous: previous.as_deref(),
            heat: &heat,
            speeds: &speeds,
            prices: &prices,
            providers: &providers,
        },
    );
    let (records, total_rows) = store
        .usage_page(
            from,
            to,
            &query.filter,
            0,
            query.rows.clamp(USAGE_PAGE, USAGE_ROW_LIMIT),
        )
        .map_err(database)?;
    let pricing = Prices::new(&prices);
    let rows = records
        .into_iter()
        .map(|record| {
            let group = match (&record.provider_id, &record.codex_provider) {
                (None, _)
                    if matches!(
                        record.source,
                        UsageSource::ClaudeLog | UsageSource::SessionLog
                    ) =>
                {
                    format!("client:{}", record.client.as_str())
                }
                (Some(id), _) => format!("p:{id}"),
                (None, Some(key)) => format!("c:{key}"),
                (None, None) => "u".into(),
            };
            let (connection, kind) = report::group_identity(&group, &providers);
            let cost = record
                .reported_cost_micros
                .map(|n| report::Cost::reported(n as u128, kind))
                .unwrap_or_else(|| {
                    pricing.cost(
                        record.provider_id.as_deref(),
                        &record.model,
                        kind,
                        record.request_count,
                        record.usage,
                    )
                });
            let money = if kind.subscription() {
                cost.subscription
            } else {
                cost.api
            };
            let (speed, estimated_timing) = output_speed(&record);
            UsageRow {
                clock: report::clock(record.started_at_ms, now),
                started_at_ms: record.started_at_ms,
                cost: (cost.unpriced_requests == 0
                    && (!record.usage.is_empty() || record.reported_cost_micros.is_some()))
                .then(|| (money.primary(), money.get(money.primary()))),
                connection,
                kind,
                group,
                provider_id: record.provider_id,
                model: record.model,
                public_model: record.public_model,
                usage: record.usage,
                speed,
                estimated_timing,
                duration_ms: record.duration_ms,
                routed: record.source == UsageSource::Router,
                source: record.source,
                client: record.client,
                reported_cost: record.reported_cost_micros.is_some(),
                request_count: record.request_count,
                id: record.id,
            }
        })
        .collect();
    Ok(UsageView {
        sources: usage_sources_from(store)?,
        report,
        today,
        rows,
        total_rows,
        prices,
        providers,
        log_import: store.usage_log_import().map_err(database)?,
        claude_import: store.usage_claude_import().map_err(database)?,
        claude_home: claude_home(store).map_err(database)?,
    })
}

fn import_message(error: ImportError) -> String {
    match error {
        ImportError::Database => "无法写入用量记录；检查本地数据库后重试".into(),
        ImportError::Cancelled => "同步已取消".into(),
    }
}

/// Reads new session-log usage; `None` when automatic scanning is off and not forced.
pub fn sync_usage_logs(
    data_dir: &Path,
    codex_home: &Path,
    force: bool,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<Option<ImportSummary>, String> {
    let store = open_store(data_dir).map_err(|error| error.message().to_owned())?;
    let codex = force
        || store
            .usage_log_import()
            .map_err(|_| AppError::Database.message())?;
    let claude = force
        || store
            .usage_claude_import()
            .map_err(|_| AppError::Database.message())?;
    let codex = codex && codex_home.is_absolute();
    let sources = usage_sources_from(&store).map_err(|e| e.message())?;
    if !force && !codex && !claude && !sources.iter().any(|source| source.enabled) {
        return Ok(None);
    }
    import_logs(&store, codex_home, codex, claude, force, cancel, progress).map(Some)
}

/// Backs up the database, forgets imported rows, and imports every log again.
/// Rows whose source logs were deleted cannot come back.
pub fn rebuild_usage_logs(
    data_dir: &Path,
    codex_home: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(ImportProgress),
) -> Result<(ImportSummary, PathBuf), String> {
    let store = open_store(data_dir).map_err(|error| error.message().to_owned())?;
    let backup = data_dir.join(format!(
        "switchx-usage-backup-{}.sqlite",
        Local::now().format("%Y%m%d-%H%M%S")
    ));
    store
        .backup_to(&backup)
        .map_err(|_| "备份数据库失败，未改动用量记录".to_owned())?;
    store
        .clear_imported_usage()
        .map_err(|_| AppError::Database.message().to_owned())?;
    let summary = import_logs(&store, codex_home, true, true, true, cancel, progress)?;
    Ok((summary, backup))
}

fn claude_home(store: &Store) -> rusqlite::Result<PathBuf> {
    if let Some(path) = store.usage_claude_home()?.filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    Ok(
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".claude"),
    )
}

fn import_logs(
    store: &Store,
    codex_home: &Path,
    codex: bool,
    claude: bool,
    force: bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(ImportProgress),
) -> Result<ImportSummary, String> {
    let home = claude_home(store).map_err(|_| AppError::Database.message())?;
    let mut summary = ImportSummary::default();
    if codex && codex_home.is_absolute() {
        summary =
            codex_logs::import(store, codex_home, cancel, &mut progress).map_err(import_message)?;
    }
    if claude {
        let next = claude_logs::import(store, &home, cancel, |step| {
            progress(ImportProgress {
                done: summary.files + step.done,
                total: summary.files + step.total,
            })
        })
        .map_err(import_message)?;
        summary.files += next.files;
        summary.read += next.read;
        summary.added += next.added;
        summary.unreadable += next.unreadable;
    }
    for source in usage_sources_from(store).map_err(|e| e.message())? {
        if !force && !source.enabled {
            continue;
        }
        let next = session_logs::import(store, source.client, &source.path, cancel, |step| {
            progress(ImportProgress {
                done: summary.files + step.done,
                total: summary.files + step.total,
            })
        })
        .map_err(import_message)?;
        summary.files += next.files;
        summary.read += next.read;
        summary.added += next.added;
        summary.unreadable += next.unreadable;
    }
    Ok(summary)
}

pub fn save_usage_claude_source(data_dir: &Path, home: &str, enabled: bool) -> Result<(), String> {
    let home = home.trim();
    if !home.is_empty() && !Path::new(home).is_absolute() {
        return Err("Claude 配置目录须为绝对路径".into());
    }
    let store = open_store(data_dir).map_err(|error| error.message().to_owned())?;
    store
        .set_usage_claude_home(home)
        .and_then(|()| store.set_usage_claude_import(enabled))
        .map_err(|_| AppError::Database.message().to_owned())
}

pub fn set_usage_log_import(data_dir: &Path, enabled: bool) -> Result<(), String> {
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .set_usage_log_import(enabled)
        .map_err(|_| AppError::Database.message().to_owned())
}

/// A price as typed in the price editor, per million tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PriceInput {
    pub provider_id: String,
    pub model: String,
    pub currency: String,
    pub input: String,
    pub cached_input: String,
    pub cache_write: String,
    pub cache_write_1h: String,
    pub output: String,
}

/// Parses a non-negative decimal amount with at most six fractional digits.
pub fn parse_price(text: &str) -> Option<u64> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty()
        || fraction.len() > 6
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().ok()?
    };
    let fraction = format!("{fraction:0<6}").parse::<u64>().ok()?;
    whole.checked_mul(1_000_000)?.checked_add(fraction)
}

pub fn save_price(data_dir: &Path, input: &PriceInput) -> Result<(), String> {
    let model = input.model.trim();
    if model.is_empty() {
        return Err("缺少模型".into());
    }
    let currency = Currency::parse(&input.currency).ok_or("币种无效")?;
    let input_micros = parse_price(&input.input).ok_or("输入价格须为非负数，最多 6 位小数")?;
    let output_micros = parse_price(&input.output).ok_or("输出价格须为非负数，最多 6 位小数")?;
    // An empty cache price means cached input has no discount.
    let cached_input_micros = if input.cached_input.trim().is_empty() {
        input_micros
    } else {
        parse_price(&input.cached_input).ok_or("缓存输入价格须为非负数，最多 6 位小数")?
    };
    let write_price = |text: &str, numerator: u64, denominator: u64| -> Result<u64, String> {
        if text.trim().is_empty() {
            input_micros
                .checked_mul(numerator)
                .and_then(|n| n.checked_add(denominator / 2))
                .map(|n| n / denominator)
                .ok_or_else(|| "缓存写入价格过大".into())
        } else {
            parse_price(text).ok_or_else(|| "缓存写入价格须为非负数，最多 6 位小数".into())
        }
    };
    let cache_write_micros = write_price(&input.cache_write, 5, 4)?;
    let cache_write_1h_micros = write_price(&input.cache_write_1h, 2, 1)?;
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .put_model_price(&PriceRecord {
            provider_id: input.provider_id.clone(),
            model: model.to_owned(),
            currency,
            input_micros,
            cached_input_micros,
            cache_write_micros,
            cache_write_1h_micros,
            output_micros,
        })
        .map_err(|_| AppError::Database.message().to_owned())
}

pub fn delete_price(data_dir: &Path, provider_id: &str, model: &str) -> Result<(), String> {
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .delete_model_price(provider_id, model)
        .map_err(|_| AppError::Database.message().to_owned())
}

/// A model's saved price, if any, beside its all-time usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceView {
    pub model: String,
    pub currency: Option<Currency>,
    pub input: String,
    pub cached_input: String,
    pub cache_write: String,
    pub cache_write_1h: String,
    pub output: String,
    pub requests: u64,
    pub tokens: u64,
}

/// Unpriced models with usage come first, busiest first, then priced ones.
pub fn load_prices(data_dir: &Path) -> Result<Vec<PriceView>, AppError> {
    prices_from(&open_store(data_dir)?)
}

pub(crate) fn prices_from(store: &Store) -> Result<Vec<PriceView>, AppError> {
    let prices = store.model_prices().map_err(|_| AppError::Database)?;
    let mut views = store
        .usage_models()
        .map_err(|_| AppError::Database)?
        .into_iter()
        .map(|(model, requests, tokens)| PriceView {
            model,
            currency: None,
            input: String::new(),
            cached_input: String::new(),
            cache_write: String::new(),
            cache_write_1h: String::new(),
            output: String::new(),
            requests,
            tokens,
        })
        .collect::<Vec<_>>();
    for price in prices.iter().filter(|price| price.provider_id.is_empty()) {
        let index = match views.iter().position(|view| view.model == price.model) {
            Some(index) => index,
            None => {
                views.push(PriceView {
                    model: price.model.clone(),
                    currency: None,
                    input: String::new(),
                    cached_input: String::new(),
                    cache_write: String::new(),
                    cache_write_1h: String::new(),
                    output: String::new(),
                    requests: 0,
                    tokens: 0,
                });
                views.len() - 1
            }
        };
        let view = &mut views[index];
        view.currency = Some(price.currency);
        view.input = price_text(price.input_micros);
        view.cached_input = price_text(price.cached_input_micros);
        view.cache_write = price_text(price.cache_write_micros);
        view.cache_write_1h = price_text(price.cache_write_1h_micros);
        view.output = price_text(price.output_micros);
    }
    views.sort_by(|a, b| {
        (a.currency.is_some(), b.tokens, &a.model).cmp(&(b.currency.is_some(), a.tokens, &b.model))
    });
    Ok(views)
}

/// Price text for an editor field: no trailing zeros, at least one digit.
pub fn price_text(micros: u64) -> String {
    let text = format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000);
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

pub fn format_tokens(value: u64) -> String {
    let scaled = |divisor: f64, unit: &str| {
        let value = value as f64 / divisor;
        if value >= 100.0 {
            format!("{value:.0}{unit}")
        } else {
            format!("{value:.1}{unit}")
        }
    };
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => scaled(1e3, "K"),
        1_000_000..1_000_000_000 => scaled(1e6, "M"),
        _ => scaled(1e9, "B"),
    }
}

/// Thousands-grouped integer, as in request counts.
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Two decimals from one unit up, four below it so single requests stay visible.
pub fn format_money(currency: Currency, micros: u128) -> String {
    let symbol = currency.symbol();
    if micros >= 1_000_000 || micros == 0 {
        let cents = (micros + 5_000) / 10_000;
        format!(
            "{symbol}{}.{:02}",
            format_count((cents / 100) as u64),
            cents % 100
        )
    } else {
        let units = (micros + 50) / 100;
        format!("{symbol}0.{units:04}")
    }
}

/// Every non-zero currency, primary first; `None` when nothing is priced.
pub fn format_amounts(money: Money) -> Vec<String> {
    let primary = money.primary();
    let secondary = match primary {
        Currency::Usd => Currency::Cny,
        Currency::Cny => Currency::Usd,
    };
    [primary, secondary]
        .into_iter()
        .filter(|currency| money.get(*currency) > 0)
        .map(|currency| format_money(currency, money.get(currency)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_and_codex_share_reports_but_client_filters_apply_to_every_section() {
        use serde_json::json;
        use std::io::Write;
        let root = std::env::temp_dir().join(format!(
            "switchx-usage-clients-{}",
            crate::id::new_id().unwrap()
        ));
        let data = root.join("data");
        let codex = root.join("codex");
        let claude = root.join("claude");
        std::fs::create_dir_all(codex.join("sessions/2026/10/15")).unwrap();
        std::fs::create_dir_all(claude.join("projects/project")).unwrap();
        save_usage_claude_source(&data, claude.to_str().unwrap(), true).unwrap();
        assert!(save_usage_claude_source(&data, "relative", false).is_err());
        let now = Local::now();
        let current = (now - chrono::Duration::minutes(1)).to_rfc3339();
        let previous = (now - chrono::Duration::hours(30)).to_rfc3339();
        let claude_line = |id: &str, stamp: &str, output: u64| {
            json!({
                "type":"assistant", "timestamp":stamp, "sessionId":"session", "message":{
                    "id":id,"model":"shared-model","usage":{"input_tokens":100,"cache_read_input_tokens":1000,
                    "cache_creation_input_tokens":500,"cache_creation":{"ephemeral_5m_input_tokens":300,"ephemeral_1h_input_tokens":200},"output_tokens":output}
                }
            })
        };
        let claude_file = claude.join("projects/project/session.jsonl");
        std::fs::write(
            &claude_file,
            format!(
                "{}\n{}\n",
                claude_line("current", &current, 5),
                claude_line("previous", &previous, 2)
            ),
        )
        .unwrap();
        let codex_rows = [
            json!({"type":"session_meta","payload":{"id":"session","model_provider":"openai"}}),
            json!({"type":"turn_context","payload":{"model":"shared-model"}}),
            json!({"timestamp":current,"type":"token_usage_record","payload":{"response_id":"codex-current","usage":{"input_tokens":1000,"output_tokens":100}}}),
            json!({"timestamp":previous,"type":"token_usage_record","payload":{"response_id":"codex-previous","usage":{"input_tokens":2000,"output_tokens":200}}}),
        ];
        std::fs::write(
            codex.join("sessions/2026/10/15/rollout-test.jsonl"),
            codex_rows
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        for client in UsageClient::EXTERNAL {
            let store = open_store(&data).unwrap();
            store
                .save_usage_client_source(
                    client,
                    root.join(client.as_str()).to_str().unwrap(),
                    false,
                )
                .unwrap();
        }
        let cancel = AtomicBool::new(false);
        assert_eq!(
            sync_usage_logs(&data, &codex, false, &cancel, |_| {})
                .unwrap()
                .unwrap()
                .added,
            4
        );
        save_price(
            &data,
            &PriceInput {
                model: "shared-model".into(),
                currency: "USD".into(),
                input: "4".into(),
                cached_input: "0.5".into(),
                output: "20".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let price = &load_prices(&data).unwrap()[0];
        assert_eq!((&*price.cache_write, &*price.cache_write_1h), ("5", "8"));
        let query = UsageQuery {
            period: Period::Hours24,
            filter: UsageFilter {
                client: Some(UsageClient::ClaudeCode),
                ..Default::default()
            },
            ..Default::default()
        };
        let view = load_usage(&data, &query).unwrap();
        assert_eq!(view.claude_home, claude);
        assert_eq!(view.report.totals.requests, 1);
        assert_eq!(view.report.totals.usage.total(), 1605);
        assert_eq!(view.report.totals.cost.api.usd, 4100);
        assert_eq!(view.report.previous.unwrap().requests, 1);
        assert_eq!(view.report.previous.unwrap().usage.output, 2);
        assert_eq!(
            view.report
                .points
                .iter()
                .map(|point| point.totals.requests)
                .sum::<u64>(),
            1
        );
        assert_eq!(view.report.heat.iter().flatten().sum::<u64>(), 1605);
        assert_eq!(
            (
                view.total_rows,
                view.rows.len(),
                view.report.models.len(),
                view.report.connections.len()
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(view.rows[0].connection, "Claude Code 会话");
        assert_eq!(view.rows[0].source, UsageSource::ClaudeLog);
        let codex_view = load_usage(
            &data,
            &UsageQuery {
                filter: UsageFilter {
                    client: Some(UsageClient::Codex),
                    ..Default::default()
                },
                ..query.clone()
            },
        )
        .unwrap();
        assert_eq!(codex_view.report.totals.usage.total(), 1100);
        assert_eq!(codex_view.report.previous.unwrap().usage.total(), 2200);

        // An independent source switch stops automatic import without dropping history.
        save_usage_claude_source(&data, claude.to_str().unwrap(), false).unwrap();
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&claude_file)
                .unwrap(),
            "{}",
            claude_line("current", &current, 4660)
        )
        .unwrap();
        assert_eq!(
            sync_usage_logs(&data, &codex, false, &cancel, |_| {})
                .unwrap()
                .unwrap()
                .added,
            0
        );
        set_usage_log_import(&data, false).unwrap();
        assert!(
            sync_usage_logs(&data, &codex, false, &cancel, |_| {})
                .unwrap()
                .is_none()
        );
        save_usage_claude_source(&data, claude.to_str().unwrap(), true).unwrap();
        assert_eq!(
            sync_usage_logs(&data, &codex, false, &cancel, |_| {})
                .unwrap()
                .unwrap()
                .added,
            1
        );
        let updated = load_usage(&data, &query).unwrap();
        assert_eq!(
            (updated.total_rows, updated.report.totals.usage.output),
            (1, 4660)
        );
        let (_, backup) = rebuild_usage_logs(&data, &codex, &cancel, |_| {}).unwrap();
        assert!(backup.exists());
        assert_eq!(
            load_usage(&data, &query).unwrap().report.totals,
            updated.report.totals
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn numbers_and_money_are_compact_and_exact() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(12_345), "12.3K");
        assert_eq!(format_tokens(183_400_000), "183M");
        assert_eq!(format_tokens(3_840_000_000), "3.8B");
        assert_eq!(format_count(1_568), "1,568");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_money(Currency::Usd, 123_570_000), "$123.57");
        assert_eq!(
            format_money(Currency::Usd, 1_234_567_890_000),
            "$1,234,567.89"
        );
        assert_eq!(format_money(Currency::Cny, 30_500), "¥0.0305");
        assert_eq!(format_money(Currency::Usd, 0), "$0.00");
        assert_eq!(
            format_amounts(Money {
                usd: 1_000_000,
                cny: 7_000_000
            }),
            ["¥7.00", "$1.00"]
        );
        assert!(format_amounts(Money::default()).is_empty());
    }

    #[test]
    fn usage_view_sync_and_rebuild_use_an_isolated_data_directory() {
        use crate::storage::{RequestRecord, RequestStatus, Store};
        let root = std::env::temp_dir().join(format!(
            "switchx-usage-app-{}",
            crate::id::new_id().unwrap()
        ));
        let data = root.join("data");
        let home = root.join("codex");
        std::fs::create_dir_all(home.join("sessions/2026/10/13")).unwrap();
        let now = Local::now();
        let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        std::fs::write(
            home.join("sessions/2026/10/13/rollout-synthetic.jsonl"),
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({"timestamp": stamp, "type": "session_meta",
                    "payload": {"id": "session", "model_provider": "openai"}}),
                serde_json::json!({"timestamp": stamp, "type": "turn_context",
                    "payload": {"model": "logged-model"}}),
                serde_json::json!({"timestamp": stamp, "type": "token_usage_record",
                    "payload": {"thread_id": "thread", "response_id": "resp_logged",
                        "usage": {"input_tokens": 2_000, "cached_input_tokens": 1_000,
                            "output_tokens": 100, "reasoning_output_tokens": 0}}}),
            ),
        )
        .unwrap();
        let store = open_store(&data).unwrap();
        store
            .set_usage_claude_home(root.join("claude").to_str().unwrap())
            .unwrap();
        store.set_usage_claude_import(false).unwrap();
        store
            .put_request(&RequestRecord {
                client: crate::storage::UsageClient::Unknown,
                workspace_id: None,
                id: "routed".into(),
                started_at_ms: now.timestamp_millis(),
                public_model: Some("显示名称".into()),
                provider_id: Some("gone".into()),
                upstream_model: Some("routed-model".into()),
                generation: "generation".into(),
                http_status: Some(200),
                headers_ms: Some(10),
                first_event_ms: Some(100),
                duration_ms: 2_100,
                status: RequestStatus::Completed,
                error_code: None,
                fallback_from: None,
                usage: Some(TokenUsage {
                    input: 1_000,
                    output: 40,
                    ..Default::default()
                }),
                response_id: None,
            })
            .unwrap();
        drop::<Store>(store);
        for client in UsageClient::EXTERNAL {
            let store = open_store(&data).unwrap();
            store
                .save_usage_client_source(
                    client,
                    root.join(client.as_str()).to_str().unwrap(),
                    false,
                )
                .unwrap();
        }
        let cancel = AtomicBool::new(false);
        let summary = sync_usage_logs(&data, &home, false, &cancel, |_| {})
            .unwrap()
            .unwrap();
        assert_eq!(summary.added, 1);
        save_price(
            &data,
            &PriceInput {
                model: "routed-model".into(),
                currency: "USD".into(),
                input: "1".into(),
                output: "10".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let prices = load_prices(&data).unwrap();
        let order = prices
            .iter()
            .map(|price| (price.model.as_str(), price.currency, price.output.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                ("logged-model", None, ""),
                ("routed-model", Some(Currency::Usd), "10")
            ]
        );
        let view = load_usage(&data, &UsageQuery::default()).unwrap();
        assert_eq!(view.report.totals.requests, 2);
        assert_eq!(view.today.requests, 2);
        assert_eq!((view.total_rows, view.rows.len()), (2, 2));
        let routed = view.rows.iter().find(|row| row.routed).unwrap();
        assert_eq!(routed.speed, Some(20.0));
        // $0.001 input + $0.0004 output.
        assert_eq!(routed.cost, Some((Currency::Usd, 1_400)));
        let logged = view.rows.iter().find(|row| !row.routed).unwrap();
        assert_eq!(
            (logged.connection.as_str(), logged.cost),
            ("Codex 原生登录", None)
        );
        assert_eq!(view.report.unpriced, [("logged-model".to_owned(), 1)]);

        set_usage_log_import(&data, false).unwrap();
        assert!(
            !load_usage(&data, &UsageQuery::default())
                .unwrap()
                .log_import
        );
        assert!(
            sync_usage_logs(&data, &home, false, &cancel, |_| {})
                .unwrap()
                .is_none()
        );
        let (summary, backup) = rebuild_usage_logs(&data, &home, &cancel, |_| {}).unwrap();
        assert_eq!(summary.added, 1);
        assert!(backup.starts_with(&data) && backup.exists());
        assert_eq!(
            load_usage(&data, &UsageQuery::default())
                .unwrap()
                .total_rows,
            2
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claude_log_speed_uses_the_whole_estimated_duration() {
        use crate::storage::UsageRecord;
        let record = UsageRecord {
            reported_cost_micros: None,
            request_count: 1,
            id: "claude:msg".into(),
            source: UsageSource::ClaudeLog,
            client: UsageClient::ClaudeCode,
            started_at_ms: 0,
            provider_id: None,
            codex_provider: None,
            public_model: None,
            model: "claude-opus".into(),
            usage: TokenUsage {
                output: 900,
                ..Default::default()
            },
            duration_ms: Some(9_000),
            first_event_ms: Some(2_000),
            response_id: None,
            session_id: None,
        };
        assert_eq!(output_speed(&record), (Some(100.0), true));
        let short = UsageRecord {
            usage: TokenUsage {
                output: 199,
                ..Default::default()
            },
            ..record.clone()
        };
        assert_eq!(output_speed(&short), (None, true));
        let brief = UsageRecord {
            usage: TokenUsage {
                output: 200,
                ..Default::default()
            },
            duration_ms: Some(999),
            ..record.clone()
        };
        assert_eq!(output_speed(&brief), (None, true));
        let routed = UsageRecord {
            source: UsageSource::Router,
            duration_ms: Some(2_100),
            first_event_ms: Some(100),
            usage: TokenUsage {
                output: 40,
                ..Default::default()
            },
            ..record.clone()
        };
        assert_eq!(output_speed(&routed), (Some(20.0), false));
        let session = UsageRecord {
            source: UsageSource::SessionLog,
            client: UsageClient::GrokBuild,
            usage: TokenUsage {
                output: 400,
                ..Default::default()
            },
            duration_ms: Some(2_000),
            first_event_ms: Some(500),
            ..record
        };
        assert_eq!(output_speed(&session), (Some(200.0), true));
        let untimed = UsageRecord {
            source: UsageSource::CodexLog,
            client: UsageClient::Codex,
            duration_ms: None,
            first_event_ms: None,
            ..session
        };
        assert_eq!(output_speed(&untimed), (None, false));
    }

    #[test]
    fn prices_parse_as_exact_decimals() {
        assert_eq!(parse_price("1.25"), Some(1_250_000));
        assert_eq!(parse_price(" 10 "), Some(10_000_000));
        assert_eq!(parse_price(".5"), Some(500_000));
        assert_eq!(parse_price("0.000001"), Some(1));
        for invalid in ["", ".", "-1", "1.0000001", "1e3", "１"] {
            assert_eq!(parse_price(invalid), None, "{invalid}");
        }
        assert_eq!(price_text(1_250_000), "1.25");
        assert_eq!(price_text(10_000_000), "10");
        assert_eq!(price_text(0), "0");
    }
}
