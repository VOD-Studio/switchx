//! Usage report view models. Pure conversions, shared with the synthetic
//! UI preview, so the page and its previews render the same figures.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use slint::{Image, SharedString};
use switchx::{
    app::{self, UsageRow},
    provider_icons,
    storage::{Currency, ProviderKind, ProviderRecord},
    ui::{
        UsageDetailRow, UsageHeatCell, UsageMoment, UsagePoint, UsagePriceRow, UsageRange,
        UsageScale, UsageShareRow, UsageSummary,
    },
    usage::{
        TokenUsage,
        report::{Granularity, GroupKind, Money, Period, Report, Share, Totals, Window},
    },
};

const SHARE_ROWS: usize = 7;

/// Looks up a loaded provider icon by its catalog ID: the image and whether it is monochrome.
pub type IconLookup<'a> = &'a dyn Fn(&str) -> Option<(Image, bool)>;

pub fn share(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 / whole as f64) as f32
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 || part == 0 {
        return "—".into();
    }
    let value = part as f64 * 100.0 / whole as f64;
    if value < 1.0 {
        "<1%".into()
    } else {
        format!("{value:.0}%")
    }
}

fn money(money: Money) -> String {
    app::format_amounts(money).join(" + ")
}

/// Billed first; subscription usage shows its equivalent API price.
fn cost_text(totals: &Totals) -> String {
    let api = money(totals.cost.api);
    let subscription = money(totals.cost.subscription);
    match (api.is_empty(), subscription.is_empty()) {
        (false, true) => api,
        (true, false) => format!("等效 {subscription}"),
        (false, false) => format!("{api} · 等效 {subscription}"),
        (true, true) if totals.requests > totals.cost.unpriced_requests => "0.00".into(),
        (true, true) => String::new(),
    }
}

/// A local instant as the range picker shows it, to the minute.
pub fn moment(time: DateTime<Local>) -> UsageMoment {
    UsageMoment {
        year: time.year(),
        month: time.month() as i32,
        day: time.day() as i32,
        hour: time.hour() as i32,
        minute: time.minute() as i32,
    }
}

/// The instant a picked local minute starts; a minute skipped by a clock change moves an hour on.
pub fn moment_ms(moment: &UsageMoment) -> Option<i64> {
    let part = |value: i32| u32::try_from(value).ok();
    let time = NaiveDate::from_ymd_opt(moment.year, part(moment.month)?, part(moment.day)?)?
        .and_hms_opt(part(moment.hour)?, part(moment.minute)?, 0)?;
    Local
        .from_local_datetime(&time)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(time + Duration::hours(1)))
                .earliest()
        })
        .map(|time| time.timestamp_millis())
}

fn local(ms: i64, fallback: DateTime<Local>) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or(fallback)
}

/// Where a preset starts, and now, to seed the picker. "All" reaches back to the first
/// record, which the calendar cannot show usefully, so it starts thirty days back.
pub fn preset_range(index: i32, now: DateTime<Local>) -> UsageRange {
    let period = match Period::from_index(index) {
        Period::All => Period::Days30,
        period => period,
    };
    UsageRange {
        start: moment(local(Window::new(period, now, None).from_ms, now)),
        end: moment(now),
    }
}

/// A picked range as a report period. The end minute is included; a live end follows the clock.
pub fn custom_period(start: &UsageMoment, end: &UsageMoment, live: bool) -> Option<Period> {
    let from_ms = moment_ms(start)?;
    let to_ms = if live {
        None
    } else {
        Some((moment_ms(end)? + 60_000).max(from_ms + 60_000))
    };
    Some(Period::Custom { from_ms, to_ms })
}

/// `M/D HH:MM`, with the year when it is not the current one.
fn stamp(time: DateTime<Local>, now: DateTime<Local>) -> String {
    format!("{} {}", day_text(time, now), time.format("%H:%M"))
}

fn day_text(time: DateTime<Local>, now: DateTime<Local>) -> String {
    if time.year() == now.year() {
        format!("{}/{}", time.month(), time.day())
    } else {
        format!("{}/{}/{}", time.year(), time.month(), time.day())
    }
}

/// How the period button and trend title name a custom range.
pub fn custom_label(period: Period, now: DateTime<Local>) -> String {
    let Period::Custom { from_ms, to_ms } = period else {
        return String::new();
    };
    format!(
        "{} – {}",
        stamp(local(from_ms, now), now),
        to_ms.map_or_else(
            || "现在".to_owned(),
            |to| stamp(local(to - 60_000, now), now)
        )
    )
}

/// The span a report covers and how its trend is sliced.
fn range_text(window: &Window) -> String {
    let now = local(window.now_ms, Local::now());
    let span = match window.period {
        Period::Today => "今天".to_owned(),
        Period::Hours24 => format!("{} – 现在", stamp(local(window.from_ms, now), now)),
        Period::Custom { .. } => custom_label(window.period, now),
        _ => format!("{} – 今天", day_text(local(window.from_ms, now), now)),
    };
    let slices = match window.granularity {
        Granularity::Hour => "按小时",
        Granularity::Day => "按天",
        Granularity::Week => "按周",
        Granularity::Month => "按月",
    };
    format!("{span} · {slices}")
}

pub fn summary(report: &Report) -> UsageSummary {
    let totals = &report.totals;
    let usage = totals.usage;
    let total = usage.total();
    let delta = report.previous.and_then(|previous| {
        let before = previous.usage.total();
        (before > 0 && total > 0).then(|| (total as f64 - before as f64) * 100.0 / before as f64)
    });
    let api = app::format_amounts(totals.cost.api);
    let subscription = money(totals.cost.subscription);
    let mut notes = Vec::new();
    if !subscription.is_empty() {
        notes.push(if api.is_empty() {
            "订阅连接 · 等效 API 价格".to_owned()
        } else {
            format!("另有订阅等效 {subscription}")
        });
    }
    if !report.unpriced.is_empty() {
        notes.push(format!("{} 个模型未定价", report.unpriced.len()));
    }
    let (cost_text, cost_secondary) = match (api.as_slice(), subscription.is_empty()) {
        ([first, rest @ ..], _) => (
            first.clone(),
            rest.first()
                .map(|other| format!("+ {other}"))
                .unwrap_or_default(),
        ),
        ([], false) => (
            format!("等效 {}", app::format_amounts(totals.cost.subscription)[0]),
            String::new(),
        ),
        ([], true) if totals.requests == 0 => ("—".to_owned(), String::new()),
        ([], true) if totals.requests > totals.cost.unpriced_requests => {
            ("0.00".to_owned(), String::new())
        }
        ([], true) => ("未定价".to_owned(), String::new()),
    };
    let range_text = range_text(&report.window);
    UsageSummary {
        tokens: total as f32,
        requests: totals.requests as f32,
        cache_rate: if usage.input == 0 {
            -1.0
        } else {
            share(usage.cached_input, usage.input)
        },
        fresh_share: share(usage.input - usage.cached_input, total),
        output_share: share(usage.output, total),
        cache_share: share(usage.cached_input, total),
        fresh_text: app::format_tokens(usage.input - usage.cached_input).into(),
        output_text: app::format_tokens(usage.output).into(),
        reasoning_text: if usage.reasoning == 0 {
            "".into()
        } else {
            app::format_tokens(usage.reasoning).into()
        },
        cache_text: app::format_tokens(usage.cached_input).into(),
        delta_text: match delta {
            Some(delta) if delta.abs() < 0.5 => "与上一周期持平".into(),
            Some(delta) => format!(
                "{} {:.0}% · 较上一周期",
                if delta > 0.0 { "↑" } else { "↓" },
                delta.abs()
            )
            .into(),
            None if report.previous.is_some() && total > 0 => "上一周期没有用量".into(),
            None => "".into(),
        },
        delta_tone: match delta {
            Some(delta) if delta >= 0.5 => 1,
            Some(delta) if delta <= -0.5 => 2,
            _ => 0,
        },
        cost_text: cost_text.into(),
        cost_secondary: cost_secondary.into(),
        cost_note: notes.join(" · ").into(),
        speed_text: report
            .speed_median
            .map(|speed| format!("路由中位速度 {speed:.0} tok/s"))
            .unwrap_or_default()
            .into(),
        range_text: range_text.into(),
        empty: totals.requests == 0,
    }
}

/// Axis labels drop a trailing `.0`: 2M rather than 2.0M.
fn axis_tokens(value: u64) -> String {
    app::format_tokens(value).replacen(".0", "", 1)
}

/// The smallest 1, 2, 2.5, or 5 times a power of ten at or above `value`.
fn nice_max(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let power = 10f64.powf(value.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|step| step * power)
        .find(|candidate| *candidate >= value * 0.999_999)
        .unwrap_or(10.0 * power)
}

/// The currency the cost chart is drawn in: the one leading the summary,
/// so billed API cost is charted before subscription equivalents.
fn chart_currency(report: &Report) -> Currency {
    let cost = report.totals.cost;
    if cost.api.is_zero() {
        cost.subscription.primary()
    } else {
        cost.api.primary()
    }
}

/// Axis money drops trailing zeros: $0.5 rather than $0.5000.
fn axis_money(currency: Currency, micros: u128) -> String {
    let text = app::format_money(currency, micros);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

fn bucket_cost(totals: &Totals, currency: Currency) -> (u128, u128) {
    (
        totals.cost.api.get(currency),
        totals.cost.subscription.get(currency),
    )
}

pub fn scale(report: &Report) -> UsageScale {
    let currency = chart_currency(report);
    let tokens = nice_max(
        report
            .points
            .iter()
            .map(|point| point.totals.usage.total())
            .max()
            .unwrap_or(0) as f64,
    );
    let cost = nice_max(
        report
            .points
            .iter()
            .map(|point| {
                let (api, subscription) = bucket_cost(&point.totals, currency);
                api + subscription
            })
            .max()
            .unwrap_or(0) as f64,
    );
    let requests = nice_max(
        report
            .points
            .iter()
            .map(|point| point.totals.requests)
            .max()
            .unwrap_or(0) as f64,
    )
    .max(2.0);
    let other = match currency {
        Currency::Usd => Currency::Cny,
        Currency::Cny => Currency::Usd,
    };
    let left_out = {
        let all = report.totals.cost.api.get(other) + report.totals.cost.subscription.get(other);
        (all > 0).then(|| app::format_money(other, all))
    };
    let priced = !report.totals.cost.api.is_zero() || !report.totals.cost.subscription.is_zero();
    // An empty range keeps only the baseline label.
    let used = report.totals.requests > 0;
    let label = |text: String| {
        if used {
            text.into()
        } else {
            SharedString::new()
        }
    };
    UsageScale {
        tokens_top: label(axis_tokens(tokens as u64)),
        tokens_mid: label(axis_tokens((tokens / 2.0) as u64)),
        cost_top: if priced {
            axis_money(currency, cost as u128).into()
        } else {
            "".into()
        },
        cost_mid: if priced {
            axis_money(currency, (cost / 2.0) as u128).into()
        } else {
            "".into()
        },
        requests_top: label(app::format_count(requests as u64)),
        requests_mid: label(app::format_count((requests / 2.0) as u64)),
        cost_caption: if !priced {
            "尚无已定价的用量".into()
        } else if let Some(left_out) = left_out {
            format!("单位 {} · 另有 {left_out} 未计入图表", currency.as_str()).into()
        } else {
            format!("单位 {}", currency.as_str()).into()
        },
    }
}

const WEEKDAYS: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

pub fn points(report: &Report) -> Vec<UsagePoint> {
    let currency = chart_currency(report);
    let token_max = nice_max(
        report
            .points
            .iter()
            .map(|point| point.totals.usage.total())
            .max()
            .unwrap_or(0) as f64,
    );
    let cost_max = nice_max(
        report
            .points
            .iter()
            .map(|point| {
                let (api, subscription) = bucket_cost(&point.totals, currency);
                api + subscription
            })
            .max()
            .unwrap_or(0) as f64,
    );
    let request_max = nice_max(
        report
            .points
            .iter()
            .map(|point| point.totals.requests)
            .max()
            .unwrap_or(0) as f64,
    )
    .max(2.0);
    let now = report.window.now_ms;
    report
        .points
        .iter()
        .map(|point| {
            let usage = point.totals.usage;
            let total = usage.total();
            let (api, subscription) = bucket_cost(&point.totals, currency);
            let start = Local
                .timestamp_millis_opt(point.bucket.start_ms)
                .single()
                .unwrap_or_else(Local::now);
            let end = Local
                .timestamp_millis_opt(point.bucket.end_ms - 1)
                .single()
                .unwrap_or_else(Local::now);
            let tip_title = match report.window.granularity {
                Granularity::Hour => format!(
                    "{} {}–{:02}:00",
                    start.format("%-m/%-d"),
                    start.format("%H:%M"),
                    (start.format("%H").to_string().parse::<u32>().unwrap_or(0) + 1) % 24
                ),
                Granularity::Day => format!(
                    "{} {}",
                    start.format("%-m/%-d"),
                    WEEKDAYS[start.format("%w").to_string().parse::<usize>().unwrap_or(0) % 7]
                ),
                Granularity::Week => {
                    format!("{} – {}", start.format("%-m/%-d"), end.format("%-m/%-d"))
                }
                Granularity::Month => point.bucket.label.clone(),
            };
            let cost = app::format_amounts({
                let mut all = point.totals.cost.api;
                all.usd += point.totals.cost.subscription.usd;
                all.cny += point.totals.cost.subscription.cny;
                all
            })
            .join(" + ");
            let tip_detail = if point.totals.requests == 0 {
                if point.bucket.start_ms > now {
                    "尚未到来".to_owned()
                } else {
                    "没有用量".to_owned()
                }
            } else {
                format!(
                    "{} Token · {} 次请求{}\n输入（含缓存写入） {} · 输出 {} · 缓存 {}",
                    app::format_tokens(total),
                    app::format_count(point.totals.requests),
                    if cost.is_empty() {
                        String::new()
                    } else {
                        format!(" · {cost}")
                    },
                    app::format_tokens(usage.input - usage.cached_input),
                    app::format_tokens(usage.output),
                    app::format_tokens(usage.cached_input),
                )
            };
            UsagePoint {
                key: point.bucket.key.clone().into(),
                label: point.bucket.label.clone().into(),
                tip_title: tip_title.into(),
                tip_detail: tip_detail.into(),
                future: point.bucket.start_ms > now,
                current: point.bucket.start_ms <= now && now < point.bucket.end_ms,
                tokens_level: (total as f64 / token_max) as f32,
                fresh_share: share(usage.input - usage.cached_input, total),
                output_share: share(usage.output, total),
                cost_level: ((api + subscription) as f64 / cost_max) as f32,
                api_share: share(api as u64, (api + subscription) as u64),
                requests_level: (point.totals.requests as f64 / request_max) as f32,
            }
        })
        .collect()
}

fn letter(name: &str) -> SharedString {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "·".into())
        .into()
}

/// The provider icon catalog ID for a connection group, when it has one.
pub fn group_icon_id(key: &str, providers: &[ProviderRecord]) -> Option<&'static str> {
    if let Some(id) = key.strip_prefix("p:") {
        let provider = providers.iter().find(|provider| provider.id == id)?;
        let chosen = provider.icon_id.as_deref().and_then(provider_icons::icon);
        let id = chosen.map_or_else(
            || provider_icons::default_icon_id(provider.kind, &provider.base_url),
            |icon| icon.id,
        );
        (!id.is_empty()).then_some(id)
    } else if key == "client:claude_code" {
        Some("claude")
    } else if key == "c:openai" {
        Some(provider_icons::default_icon_id(ProviderKind::Chatgpt, ""))
    } else {
        None
    }
}

fn kind_detail(kind: GroupKind, requests: u64) -> String {
    format!("{} · {} 次请求", kind.label(), app::format_count(requests))
}

pub fn share_rows(
    icons: IconLookup<'_>,
    shares: &[Share],
    providers: &[ProviderRecord],
    connections: bool,
) -> Vec<UsageShareRow> {
    let whole = shares
        .iter()
        .map(|share| share.totals.usage.total())
        .sum::<u64>();
    let top = shares.first().map_or(0, |share| share.totals.usage.total());
    let mut rows = shares
        .iter()
        .take(if shares.len() > SHARE_ROWS + 1 {
            SHARE_ROWS
        } else {
            SHARE_ROWS + 1
        })
        .map(|item| {
            let tokens = item.totals.usage.total();
            let icon = connections
                .then(|| group_icon_id(&item.key, providers).and_then(icons))
                .flatten();
            UsageShareRow {
                key: item.key.clone().into(),
                name: item.name.clone().into(),
                detail: if connections {
                    kind_detail(item.kind, item.totals.requests)
                } else {
                    format!("{} 次请求", app::format_count(item.totals.requests))
                }
                .into(),
                has_icon: icon.is_some(),
                monochrome: icon.as_ref().is_some_and(|icon| icon.1),
                icon: icon.map(|icon| icon.0).unwrap_or_default(),
                letter: letter(&item.name),
                level: share(tokens, top),
                percent: percent(tokens, whole).into(),
                value: app::format_tokens(tokens).into(),
                cost: cost_text(&item.totals).into(),
                selectable: true,
            }
        })
        .collect::<Vec<_>>();
    if shares.len() > SHARE_ROWS + 1 {
        let rest = &shares[SHARE_ROWS..];
        let tokens = rest
            .iter()
            .map(|share| share.totals.usage.total())
            .sum::<u64>();
        rows.push(UsageShareRow {
            key: "".into(),
            name: format!("其他 {} 项", rest.len()).into(),
            detail: "".into(),
            letter: "…".into(),
            level: share(tokens, top),
            percent: percent(tokens, whole).into(),
            value: app::format_tokens(tokens).into(),
            selectable: false,
            ..Default::default()
        });
    }
    rows
}

pub fn heat(report: &Report) -> Vec<UsageHeatCell> {
    let max = report
        .heat
        .iter()
        .flat_map(|day| day.iter())
        .copied()
        .max()
        .unwrap_or(0);
    // Monday first; a square-root scale keeps quiet hours visible beside busy ones.
    [1usize, 2, 3, 4, 5, 6, 0]
        .into_iter()
        .flat_map(|weekday| {
            (0..24).map(move |hour| {
                let tokens = report.heat[weekday][hour];
                UsageHeatCell {
                    level: if max == 0 {
                        0.0
                    } else {
                        (tokens as f64 / max as f64).sqrt() as f32
                    },
                    tip: format!(
                        "{} {hour:02}:00 · {}",
                        WEEKDAYS[weekday],
                        if tokens == 0 {
                            "没有用量".to_owned()
                        } else {
                            format!("{} Token", app::format_tokens(tokens))
                        }
                    )
                    .into(),
                }
            })
        })
        .collect()
}

pub fn detail_row(
    icons: IconLookup<'_>,
    row: &UsageRow,
    providers: &[ProviderRecord],
    stamp: i32,
) -> UsageDetailRow {
    let usage: TokenUsage = row.usage;
    let total = usage.total();
    let icon = group_icon_id(&row.group, providers).and_then(icons);
    let time = Local
        .timestamp_millis_opt(row.started_at_ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    UsageDetailRow {
        id: row.id.clone().into(),
        clock: row.clock.clone().into(),
        time: time.into(),
        connection: row.connection.clone().into(),
        kind: if row.request_count > 1 {
            format!("{} · {} 次模型调用", row.kind.label(), row.request_count)
        } else if row.kind.subscription() {
            format!("{}（等效 API 价格）", row.kind.label())
        } else {
            row.kind.label().to_owned()
        }
        .into(),
        has_icon: icon.is_some(),
        monochrome: icon.as_ref().is_some_and(|icon| icon.1),
        icon: icon.map(|icon| icon.0).unwrap_or_default(),
        letter: letter(&row.connection),
        model: if row.model.is_empty() {
            "未知模型".into()
        } else {
            row.model.clone().into()
        },
        public_model: row
            .public_model
            .clone()
            .filter(|name| *name != row.model)
            .unwrap_or_else(|| "与模型相同".into())
            .into(),
        fresh: app::format_tokens(usage.input - usage.cached_input).into(),
        cached: app::format_tokens(usage.cached_input).into(),
        output: app::format_tokens(usage.output).into(),
        reasoning: app::format_tokens(usage.reasoning).into(),
        cost: match row.cost {
            Some((currency, micros)) if row.kind.subscription() => {
                format!("≈{}", app::format_money(currency, micros))
            }
            Some((currency, micros)) => app::format_money(currency, micros),
            None => "未定价".into(),
        }
        .into(),
        speed: row
            .speed
            .map(|speed| {
                if row.estimated_timing {
                    format!("≈{speed:.0} tok/s")
                } else {
                    format!("{speed:.0} tok/s")
                }
            })
            .unwrap_or_else(|| "—".into())
            .into(),
        duration: row
            .duration_ms
            .map(|duration| {
                let text = app::format_duration(duration);
                if row.estimated_timing {
                    format!("≈{text}")
                } else {
                    text
                }
            })
            .unwrap_or_else(|| "—".into())
            .into(),
        source: format!(
            "{}{}",
            match row.source {
                switchx::storage::UsageSource::Router => "SwitchX 路由",
                switchx::storage::UsageSource::CodexLog => "Codex 会话日志",
                switchx::storage::UsageSource::ClaudeLog => "Claude Code 会话日志",
                switchx::storage::UsageSource::SessionLog => "本地会话用量",
            },
            if row.reported_cost {
                " · 客户端上报费用"
            } else {
                ""
            }
        )
        .into(),
        client: row.client.label().into(),
        cache_write: if usage.cache_write > 0 {
            format!(
                "缓存写入：5 分钟 {} · 1 小时 {}",
                app::format_tokens(usage.cache_write - usage.cache_write_1h),
                app::format_tokens(usage.cache_write_1h)
            )
        } else {
            String::new()
        }
        .into(),
        routed: row.routed,
        fresh_share: share(usage.input - usage.cached_input, total),
        cache_share: share(usage.cached_input, total),
        output_share: share(usage.output, total),
        stamp,
    }
}

pub fn price_rows(prices: Vec<app::PriceView>) -> Vec<UsagePriceRow> {
    prices
        .into_iter()
        .map(|price| UsagePriceRow {
            provider_id: "".into(),
            scope: "全部连接".into(),
            currency: price
                .currency
                .map_or("", |currency| currency.as_str())
                .into(),
            priced: price.currency.is_some(),
            input: price.input.into(),
            cached: price.cached_input.into(),
            write: price.cache_write.into(),
            write_hour: price.cache_write_1h.into(),
            output: price.output.into(),
            usage: if price.requests == 0 {
                "暂无用量".to_owned()
            } else {
                format!(
                    "{} Token · {} 次请求",
                    app::format_tokens(price.tokens),
                    app::format_count(price.requests)
                )
            }
            .into(),
            reference: "".into(),
            model: price.model.into(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_zero_cost_is_distinct_from_missing_pricing() {
        use switchx::usage::{
            TokenUsage,
            report::{self, Cost, Inputs},
        };
        let mut report = report::build(
            Window::new(Period::Today, Local::now(), None),
            &switchx::storage::UsageFilter::default(),
            Inputs {
                facts: &[],
                previous: None,
                heat: &[],
                speeds: &[],
                prices: &[],
                providers: &[],
            },
        );
        report.totals = Totals {
            requests: 1,
            usage: TokenUsage {
                input: 100,
                ..Default::default()
            },
            cost: Cost::reported(0, GroupKind::Session),
        };
        assert_eq!(summary(&report).cost_text, "0.00");
        assert_eq!(cost_text(&report.totals), "0.00");
        report.totals.cost.unpriced_requests = 1;
        assert_eq!(summary(&report).cost_text, "未定价");
        assert_eq!(cost_text(&report.totals), "");
    }

    #[test]
    fn scales_and_labels_stay_round_and_readable() {
        assert_eq!(nice_max(0.0), 1.0);
        assert_eq!(nice_max(7.0), 10.0);
        assert_eq!(nice_max(1_500_000.0), 2_000_000.0);
        assert_eq!(nice_max(2_100.0), 2_500.0);
        assert_eq!(nice_max(5_000.0), 5_000.0);
        assert_eq!(axis_tokens(2_000_000), "2M");
        assert_eq!(axis_tokens(2_500_000), "2.5M");
        assert_eq!(axis_money(Currency::Usd, 500_000), "$0.5");
        assert_eq!(axis_money(Currency::Cny, 20_000_000), "¥20");
        assert_eq!(percent(0, 10), "—");
        assert_eq!(percent(1, 1_000), "<1%");
        assert_eq!(percent(1, 3), "33%");
        assert_eq!(letter("custom_provider"), "C");
        assert_eq!(letter("——"), "·");
    }

    #[test]
    fn picked_minutes_resolve_to_periods_and_labels() {
        let now = Local.with_ymd_and_hms(2026, 10, 14, 17, 7, 30).unwrap();
        let picked = |year, month, day, hour, minute| UsageMoment {
            year,
            month,
            day,
            hour,
            minute,
        };
        let start = picked(2026, 10, 1, 9, 30);
        let end = picked(2026, 10, 7, 17, 7);
        assert_eq!(moment(now), picked(2026, 10, 14, 17, 7));
        let from = moment_ms(&start).unwrap();
        assert_eq!(moment(local(from, now)), start);
        assert_eq!(moment_ms(&picked(2026, 2, 30, 0, 0)), None);
        assert_eq!(moment_ms(&picked(2026, 10, 1, 24, 0)), None);

        // The end minute is included, so the range closes a minute after it starts.
        let fixed = custom_period(&start, &end, false).unwrap();
        assert_eq!(
            fixed,
            Period::Custom {
                from_ms: from,
                to_ms: Some(moment_ms(&end).unwrap() + 60_000)
            }
        );
        assert_eq!(custom_label(fixed, now), "10/1 09:30 – 10/7 17:07");
        let live = custom_period(&picked(2025, 12, 30, 8, 0), &end, true).unwrap();
        assert_eq!(custom_label(live, now), "2025/12/30 08:00 – 现在");
        assert_eq!(
            range_text(&Window::new(live, now, None)),
            "2025/12/30 08:00 – 现在 · 按月"
        );
        assert_eq!(
            range_text(&Window::new(Period::Days7, now, None)),
            "10/8 – 今天 · 按天"
        );
        assert_eq!(
            range_text(&Window::new(Period::Hours24, now, None)),
            "10/13 17:07 – 现在 · 按小时"
        );

        // Presets seed the picker from where they start until now.
        assert_eq!(
            preset_range(2, now),
            UsageRange {
                start: picked(2026, 10, 8, 0, 0),
                end: picked(2026, 10, 14, 17, 7),
            }
        );
        assert_eq!(preset_range(6, now).start, picked(2026, 9, 15, 0, 0));
    }
}
