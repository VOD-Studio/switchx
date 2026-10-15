//! Token usage for the usage page: reports, detail rows, session-log sync, and prices.

use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

use chrono::Local;

use super::{AppError, open_store};
use crate::{
    storage::{
        Currency, PriceRecord, ProviderRecord, Store, UsageBucket, UsageFilter, UsageSource,
    },
    usage::{
        TokenUsage,
        codex_logs::{self, ImportError, ImportProgress, ImportSummary},
        report::{self, GroupKind, Inputs, Money, Period, Prices, Report, Totals, Window},
    },
};

/// Detail rows load in pages as the list scrolls, up to the storage limit.
pub const USAGE_PAGE: usize = 60;
pub const USAGE_ROW_LIMIT: usize = 1_000;

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
    pub duration_ms: Option<i64>,
    pub routed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageView {
    pub report: Report,
    pub today: Totals,
    pub rows: Vec<UsageRow>,
    pub total_rows: usize,
    pub prices: Vec<PriceRecord>,
    pub providers: Vec<ProviderRecord>,
    pub log_import: bool,
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
                (Some(id), _) => format!("p:{id}"),
                (None, Some(key)) => format!("c:{key}"),
                (None, None) => "u".into(),
            };
            let (connection, kind) = report::group_identity(&group, &providers);
            let cost = pricing.cost(
                record.provider_id.as_deref(),
                &record.model,
                kind,
                1,
                record.usage,
            );
            let money = if kind.subscription() {
                cost.subscription
            } else {
                cost.api
            };
            let speed = match (record.duration_ms, record.first_event_ms) {
                (Some(duration), Some(first))
                    if duration - first >= 50 && record.usage.output > 0 =>
                {
                    Some(record.usage.output as f64 * 1_000.0 / (duration - first) as f64)
                }
                _ => None,
            };
            UsageRow {
                clock: report::clock(record.started_at_ms, now),
                started_at_ms: record.started_at_ms,
                cost: (cost.unpriced_requests == 0 && !record.usage.is_empty())
                    .then(|| (money.primary(), money.get(money.primary()))),
                connection,
                kind,
                group,
                provider_id: record.provider_id,
                model: record.model,
                public_model: record.public_model,
                usage: record.usage,
                speed,
                duration_ms: record.duration_ms,
                routed: record.source == UsageSource::Router,
                id: record.id,
            }
        })
        .collect();
    Ok(UsageView {
        report,
        today,
        rows,
        total_rows,
        prices,
        providers,
        log_import: store.usage_log_import().map_err(database)?,
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
    if !force
        && !store
            .usage_log_import()
            .map_err(|_| AppError::Database.message().to_owned())?
    {
        return Ok(None);
    }
    codex_logs::import(&store, codex_home, cancel, progress)
        .map(Some)
        .map_err(import_message)
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
    let summary =
        codex_logs::import(&store, codex_home, cancel, progress).map_err(import_message)?;
    Ok((summary, backup))
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
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .put_model_price(&PriceRecord {
            provider_id: input.provider_id.clone(),
            model: model.to_owned(),
            currency,
            input_micros,
            cached_input_micros,
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
            .put_request(&RequestRecord {
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
