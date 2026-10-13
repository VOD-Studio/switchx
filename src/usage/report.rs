//! Usage reports: local-time windows, cross-filtered breakdowns, and estimated cost.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, TimeZone, Timelike};

use super::TokenUsage;
use crate::storage::{Currency, PriceRecord, ProviderKind, ProviderRecord, UsageFact, UsageFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Period {
    #[default]
    Today,
    Days7,
    Days30,
    Days90,
    All,
}

impl Period {
    pub fn from_index(index: i32) -> Self {
        match index {
            1 => Self::Days7,
            2 => Self::Days30,
            3 => Self::Days90,
            4 => Self::All,
            _ => Self::Today,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Hour,
    Day,
    Week,
    Month,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub key: String,
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// The local-time range a report covers and the slots its trend is drawn in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub from_ms: i64,
    pub to_ms: i64,
    pub now_ms: i64,
    pub granularity: Granularity,
    pub buckets: Vec<Bucket>,
    /// The same elapsed span immediately before, for comparison.
    pub previous: Option<(i64, i64)>,
}

fn midnight(date: NaiveDate) -> DateTime<Local> {
    let time = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    // A skipped local midnight starts the day at the first valid instant.
    Local
        .from_local_datetime(&time)
        .earliest()
        .or_else(|| {
            Local
                .from_local_datetime(&(time + chrono::Duration::hours(1)))
                .earliest()
        })
        .unwrap_or_else(|| Local.from_utc_datetime(&time))
}

fn monday(date: NaiveDate) -> NaiveDate {
    date - Days::new(date.weekday().num_days_from_monday() as u64)
}

fn month_start(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

fn next_month(date: NaiveDate) -> NaiveDate {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(date)
}

impl Window {
    pub fn new(period: Period, now: DateTime<Local>, first_ms: Option<i64>) -> Self {
        let today = now.date_naive();
        let tomorrow = today + Days::new(1);
        let (start, granularity) = match period {
            Period::Today => (today, Granularity::Hour),
            Period::Days7 => (today - Days::new(6), Granularity::Day),
            Period::Days30 => (today - Days::new(29), Granularity::Day),
            Period::Days90 => (monday(today - Days::new(89)), Granularity::Week),
            Period::All => {
                let first = first_ms
                    .and_then(|ms| Local.timestamp_millis_opt(ms).single())
                    .map_or(today, |time| time.date_naive().min(today));
                let span = (today - first).num_days();
                if span < 31 {
                    (first, Granularity::Day)
                } else if span < 26 * 7 {
                    (monday(first), Granularity::Week)
                } else {
                    (month_start(first), Granularity::Month)
                }
            }
        };
        let mut buckets = Vec::new();
        match granularity {
            Granularity::Hour => {
                for hour in 0..24 {
                    let time = today.and_hms_opt(hour, 0, 0).unwrap_or_default();
                    let start = Local
                        .from_local_datetime(&time)
                        .earliest()
                        .map_or(midnight(today).timestamp_millis(), |t| t.timestamp_millis());
                    buckets.push(Bucket {
                        key: format!("{} {hour:02}", today.format("%Y-%m-%d")),
                        label: format!("{hour:02}:00"),
                        start_ms: start,
                        end_ms: start + 3_600_000,
                    });
                }
            }
            _ => {
                let mut day = start;
                while day < tomorrow {
                    let next = match granularity {
                        Granularity::Week => day + Days::new(7),
                        Granularity::Month => next_month(day),
                        _ => day + Days::new(1),
                    };
                    let (key, label) = match granularity {
                        Granularity::Month => (
                            day.format("%Y-%m").to_string(),
                            format!("{}年{}月", day.year(), day.month()),
                        ),
                        _ => (
                            day.format("%Y-%m-%d").to_string(),
                            format!("{}/{}", day.month(), day.day()),
                        ),
                    };
                    buckets.push(Bucket {
                        key,
                        label,
                        start_ms: midnight(day).timestamp_millis(),
                        end_ms: midnight(next).timestamp_millis(),
                    });
                    day = next;
                }
            }
        }
        let from_ms = midnight(start).timestamp_millis();
        let now_ms = now.timestamp_millis();
        let elapsed = now_ms - from_ms;
        Self {
            from_ms,
            to_ms: midnight(tomorrow).timestamp_millis(),
            now_ms,
            granularity,
            buckets,
            previous: (period != Period::All && elapsed > 0)
                .then_some((from_ms - elapsed, from_ms)),
        }
    }

    /// The SQL bucket format the facts must be grouped by.
    pub fn fact_bucket(&self) -> crate::storage::UsageBucket {
        if self.granularity == Granularity::Hour {
            crate::storage::UsageBucket::Hour
        } else {
            crate::storage::UsageBucket::Day
        }
    }

    /// The trend slot an hourly or daily fact key falls in.
    fn slot(&self, key: &str) -> Option<String> {
        match self.granularity {
            Granularity::Hour | Granularity::Day => Some(key.to_owned()),
            Granularity::Week => NaiveDate::parse_from_str(key, "%Y-%m-%d")
                .ok()
                .map(|day| monday(day).format("%Y-%m-%d").to_string()),
            Granularity::Month => key.get(..7).map(str::to_owned),
        }
    }
}

/// Amounts in millionths of a currency unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Money {
    pub usd: u128,
    pub cny: u128,
}

impl Money {
    fn add(&mut self, currency: Currency, micros: u128) {
        match currency {
            Currency::Usd => self.usd += micros,
            Currency::Cny => self.cny += micros,
        }
    }

    fn merge(&mut self, other: Self) {
        self.usd += other.usd;
        self.cny += other.cny;
    }

    pub fn is_zero(self) -> bool {
        self.usd == 0 && self.cny == 0
    }

    /// The currency holding more of the amount, preferring USD on ties.
    pub fn primary(self) -> Currency {
        if self.cny > self.usd {
            Currency::Cny
        } else {
            Currency::Usd
        }
    }

    pub fn get(self, currency: Currency) -> u128 {
        match currency {
            Currency::Usd => self.usd,
            Currency::Cny => self.cny,
        }
    }
}

/// Estimated cost: API connections bill it; subscriptions only show the
/// equivalent API price. Unpriced requests are counted, not guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cost {
    pub api: Money,
    pub subscription: Money,
    pub unpriced_requests: u64,
}

impl Cost {
    fn merge(&mut self, other: Self) {
        self.api.merge(other.api);
        self.subscription.merge(other.subscription);
        self.unpriced_requests += other.unpriced_requests;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub requests: u64,
    pub usage: TokenUsage,
    pub cost: Cost,
}

impl Totals {
    fn add(&mut self, requests: u64, usage: TokenUsage, cost: Cost) {
        self.requests += requests;
        self.usage = TokenUsage {
            input: self.usage.input + usage.input,
            cached_input: self.usage.cached_input + usage.cached_input,
            cache_write: self.usage.cache_write + usage.cache_write,
            output: self.usage.output + usage.output,
            reasoning: self.usage.reasoning + usage.reasoning,
        };
        self.cost.merge(cost);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Api,
    Subscription,
    Native,
    RoutedLegacy,
    Other,
    Unknown,
}

impl GroupKind {
    pub fn subscription(self) -> bool {
        matches!(self, Self::Subscription | Self::Native)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Api => "API",
            Self::Subscription => "订阅",
            Self::Native => "Codex 原生",
            Self::RoutedLegacy => "路由 · 历史",
            Self::Other => "其他提供方",
            Self::Unknown => "未知",
        }
    }
}

/// A connection group's display name and kind.
pub fn group_identity(group: &str, providers: &[ProviderRecord]) -> (String, GroupKind) {
    if let Some(id) = group.strip_prefix("p:") {
        return match providers.iter().find(|provider| provider.id == id) {
            Some(provider) => (
                provider.name.clone(),
                if provider.kind == ProviderKind::ApiKey {
                    GroupKind::Api
                } else {
                    GroupKind::Subscription
                },
            ),
            None => ("已删除的连接".into(), GroupKind::Api),
        };
    }
    match group.strip_prefix("c:") {
        Some("openai") => ("Codex 原生登录".into(), GroupKind::Native),
        Some("switchx_router") => ("SwitchX 路由（升级前）".into(), GroupKind::RoutedLegacy),
        Some(key) => (key.to_owned(), GroupKind::Other),
        None => ("未知来源".into(), GroupKind::Unknown),
    }
}

/// Usage for one connection group or model across the whole window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub key: String,
    pub name: String,
    pub kind: GroupKind,
    pub provider_id: Option<String>,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub bucket: Bucket,
    pub totals: Totals,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub window: Window,
    pub totals: Totals,
    pub previous: Option<Totals>,
    pub points: Vec<Point>,
    pub connections: Vec<Share>,
    pub models: Vec<Share>,
    /// Tokens by weekday (0 = Sunday) and local hour.
    pub heat: [[u64; 24]; 7],
    pub speed_median: Option<f64>,
    /// Models with usage but no price, with how many requests they made.
    pub unpriced: Vec<(String, u64)>,
}

/// Prices a model, preferring a connection-specific price over a global one.
pub struct Prices<'a>(HashMap<(&'a str, &'a str), &'a PriceRecord>);

impl<'a> Prices<'a> {
    pub fn new(prices: &'a [PriceRecord]) -> Self {
        Self(
            prices
                .iter()
                .map(|price| ((price.provider_id.as_str(), price.model.as_str()), price))
                .collect(),
        )
    }

    pub fn find(&self, provider_id: Option<&str>, model: &str) -> Option<&'a PriceRecord> {
        provider_id
            .and_then(|id| self.0.get(&(id, model)))
            .or_else(|| self.0.get(&("", model)))
            .copied()
    }

    pub fn cost(
        &self,
        provider_id: Option<&str>,
        model: &str,
        kind: GroupKind,
        requests: u64,
        usage: TokenUsage,
    ) -> Cost {
        let mut cost = Cost::default();
        if usage.is_empty() {
            return cost;
        }
        let Some(price) = self.find(provider_id, model) else {
            cost.unpriced_requests = requests;
            return cost;
        };
        let micros = price_micros(price, usage);
        if kind.subscription() {
            cost.subscription.add(price.currency, micros);
        } else {
            cost.api.add(price.currency, micros);
        }
        cost
    }
}

/// The cost of `usage` in millionths of the price's currency, rounded to nearest.
pub fn price_micros(price: &PriceRecord, usage: TokenUsage) -> u128 {
    let per_million = usage.fresh_input() as u128 * price.input_micros as u128
        + usage.cache_write as u128 * price.input_micros as u128
        + usage.cached_input as u128 * price.cached_input_micros as u128
        + usage.output as u128 * price.output_micros as u128;
    (per_million + 500_000) / 1_000_000
}

/// The SQL-side inputs a report is built from.
pub struct Inputs<'a> {
    pub facts: &'a [UsageFact],
    pub previous: Option<&'a [UsageFact]>,
    pub heat: &'a [(u8, u8, u64, u64)],
    pub speeds: &'a [f64],
    pub prices: &'a [PriceRecord],
    pub providers: &'a [ProviderRecord],
}

pub fn build(window: Window, filter: &UsageFilter, inputs: Inputs<'_>) -> Report {
    let prices = Prices::new(inputs.prices);
    let kinds = |group: &str| group_identity(group, inputs.providers);
    let mut identities = HashMap::<String, (String, GroupKind)>::new();
    let mut identity = |group: &str| {
        identities
            .entry(group.to_owned())
            .or_insert_with(|| kinds(group))
            .clone()
    };
    let group_ok = |fact: &UsageFact| {
        filter
            .group
            .as_ref()
            .is_none_or(|group| *group == fact.group)
    };
    let model_ok = |fact: &UsageFact| {
        filter
            .model
            .as_ref()
            .is_none_or(|model| *model == fact.model)
    };

    let slots = window
        .buckets
        .iter()
        .enumerate()
        .map(|(index, bucket)| (bucket.key.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut points = window
        .buckets
        .iter()
        .map(|bucket| Point {
            bucket: bucket.clone(),
            totals: Totals::default(),
        })
        .collect::<Vec<_>>();
    let mut totals = Totals::default();
    let mut connections = BTreeMap::<String, Share>::new();
    let mut models = BTreeMap::<String, Share>::new();
    let mut unpriced = BTreeMap::<String, u64>::new();
    for fact in inputs.facts {
        let (name, kind) = identity(&fact.group);
        let cost = prices.cost(
            fact.provider_id.as_deref(),
            &fact.model,
            kind,
            fact.requests,
            fact.usage,
        );
        let (in_group, in_model) = (group_ok(fact), model_ok(fact));
        if in_model {
            connections
                .entry(fact.group.clone())
                .or_insert_with(|| Share {
                    key: fact.group.clone(),
                    name,
                    kind,
                    provider_id: fact.provider_id.clone(),
                    totals: Totals::default(),
                })
                .totals
                .add(fact.requests, fact.usage, cost);
        }
        if in_group {
            models
                .entry(fact.model.clone())
                .or_insert_with(|| Share {
                    key: fact.model.clone(),
                    name: if fact.model.is_empty() {
                        "未知模型".into()
                    } else {
                        fact.model.clone()
                    },
                    kind,
                    provider_id: None,
                    totals: Totals::default(),
                })
                .totals
                .add(fact.requests, fact.usage, cost);
        }
        if !(in_group && in_model) {
            continue;
        }
        totals.add(fact.requests, fact.usage, cost);
        if cost.unpriced_requests > 0 {
            *unpriced.entry(fact.model.clone()).or_default() += cost.unpriced_requests;
        }
        if let Some(index) = window.slot(&fact.bucket).and_then(|key| slots.get(&key)) {
            points[*index].totals.add(fact.requests, fact.usage, cost);
        }
    }
    let previous = inputs.previous.map(|facts| {
        let mut previous = Totals::default();
        for fact in facts.iter().filter(|fact| group_ok(fact) && model_ok(fact)) {
            let (_, kind) = identity(&fact.group);
            let cost = prices.cost(
                fact.provider_id.as_deref(),
                &fact.model,
                kind,
                fact.requests,
                fact.usage,
            );
            previous.add(fact.requests, fact.usage, cost);
        }
        previous
    });
    let ranked = |shares: BTreeMap<String, Share>| {
        let mut shares = shares.into_values().collect::<Vec<_>>();
        shares.sort_by(|a, b| {
            b.totals
                .usage
                .total()
                .cmp(&a.totals.usage.total())
                .then_with(|| b.totals.requests.cmp(&a.totals.requests))
                .then_with(|| a.name.cmp(&b.name))
        });
        shares
    };
    let mut heat = [[0u64; 24]; 7];
    for &(weekday, hour, _, tokens) in inputs.heat {
        heat[weekday.min(6) as usize][hour.min(23) as usize] += tokens;
    }
    let mut unpriced = unpriced.into_iter().collect::<Vec<_>>();
    unpriced.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Report {
        totals,
        previous,
        points,
        connections: ranked(connections),
        models: ranked(models),
        heat,
        speed_median: median(inputs.speeds),
        unpriced,
        window,
    }
}

fn median(values: &[f64]) -> Option<f64> {
    let mut values = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[(values.len() - 1) / 2])
}

/// Local clock time of a usage row, `HH:MM:SS` when it is today and `M/D HH:MM` otherwise.
pub fn clock(ms: i64, now: DateTime<Local>) -> String {
    let Some(time) = Local.timestamp_millis_opt(ms).single() else {
        return String::new();
    };
    if time.date_naive() == now.date_naive() {
        time.format("%H:%M:%S").to_string()
    } else {
        format!(
            "{}/{} {:02}:{:02}",
            time.month(),
            time.day(),
            time.hour(),
            time.minute()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 13, 16, 4, 0).unwrap()
    }

    fn usage(input: u64, cached: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input,
            cached_input: cached,
            output,
            ..Default::default()
        }
    }

    fn fact(bucket: &str, group: &str, model: &str, requests: u64, usage: TokenUsage) -> UsageFact {
        UsageFact {
            bucket: bucket.into(),
            group: group.into(),
            provider_id: group.strip_prefix("p:").map(str::to_owned),
            model: model.into(),
            requests,
            usage,
        }
    }

    fn provider(id: &str, name: &str, kind: ProviderKind) -> ProviderRecord {
        ProviderRecord {
            id: id.into(),
            name: name.into(),
            base_url: String::new(),
            model_id: String::new(),
            credential_ref: None,
            kind,
            account_binding: None,
            icon_id: None,
        }
    }

    fn price(provider_id: &str, model: &str, currency: Currency, prices: [u64; 3]) -> PriceRecord {
        PriceRecord {
            provider_id: provider_id.into(),
            model: model.into(),
            currency,
            input_micros: prices[0],
            cached_input_micros: prices[1],
            output_micros: prices[2],
        }
    }

    #[test]
    fn windows_cover_local_days_with_matching_slots() {
        let today = Window::new(Period::Today, now(), None);
        assert_eq!(today.granularity, Granularity::Hour);
        assert_eq!(today.buckets.len(), 24);
        assert_eq!(today.buckets[0].key, "2026-10-13 00");
        assert_eq!(today.buckets[16].label, "16:00");
        assert_eq!(
            today.from_ms,
            midnight(now().date_naive()).timestamp_millis()
        );
        let elapsed = today.now_ms - today.from_ms;
        assert_eq!(
            today.previous,
            Some((today.from_ms - elapsed, today.from_ms))
        );

        let week = Window::new(Period::Days7, now(), None);
        let labels = week
            .buckets
            .iter()
            .map(|b| b.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            ["10/7", "10/8", "10/9", "10/10", "10/11", "10/12", "10/13"]
        );
        assert_eq!(Window::new(Period::Days30, now(), None).buckets.len(), 30);

        let quarter = Window::new(Period::Days90, now(), None);
        assert_eq!(quarter.granularity, Granularity::Week);
        assert_eq!(
            quarter.buckets[0].key, "2026-07-13",
            "the Monday on or before July 16"
        );
        assert_eq!(quarter.buckets.last().unwrap().key, "2026-10-12");
        assert_eq!(quarter.slot("2026-10-13").as_deref(), Some("2026-10-12"));

        let first = Local
            .with_ymd_and_hms(2025, 1, 20, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let all = Window::new(Period::All, now(), Some(first));
        assert_eq!((all.granularity, all.previous), (Granularity::Month, None));
        assert_eq!(all.buckets[0].key, "2025-01");
        assert_eq!(all.buckets.last().unwrap().label, "2026年10月");
        assert_eq!(all.slot("2026-10-13").as_deref(), Some("2026-10"));
        let recent = now().timestamp_millis() - 3 * 86_400_000;
        assert_eq!(
            Window::new(Period::All, now(), Some(recent)).buckets.len(),
            4
        );
        assert_eq!(
            Window::new(Period::All, now(), None).buckets.len(),
            1,
            "an empty history still shows today"
        );
    }

    #[test]
    fn reports_cross_filter_and_price_api_and_subscription_usage() {
        let window = Window::new(Period::Today, now(), None);
        let facts = [
            fact(
                "2026-10-13 09",
                "p:api",
                "model-a",
                3,
                usage(1_000_000, 800_000, 100_000),
            ),
            fact(
                "2026-10-13 09",
                "p:sub",
                "model-a",
                2,
                usage(1_000_000, 800_000, 100_000),
            ),
            fact("2026-10-13 15", "p:api", "model-b", 1, usage(500, 0, 50)),
            fact(
                "2026-10-13 15",
                "c:openai",
                "model-c",
                4,
                usage(10_000, 0, 1_000),
            ),
            fact("2026-10-13 15", "u", "", 1, usage(1, 0, 1)),
        ];
        let previous = [fact("", "p:api", "model-a", 1, usage(10, 0, 10))];
        let providers = [
            provider("api", "API 连接", ProviderKind::ApiKey),
            provider("sub", "订阅连接", ProviderKind::Chatgpt),
        ];
        let prices = [
            price(
                "",
                "model-a",
                Currency::Usd,
                [1_250_000, 125_000, 10_000_000],
            ),
            price("api", "model-b", Currency::Cny, [2_000_000, 0, 8_000_000]),
        ];
        let heat = [(2, 9, 5, 2_000), (2, 15, 6, 11_000)];
        let speeds = [30.0, f64::NAN, 10.0, 20.0];
        let report = build(
            window.clone(),
            &UsageFilter::default(),
            Inputs {
                facts: &facts,
                previous: Some(&previous),
                heat: &heat,
                speeds: &speeds,
                prices: &prices,
                providers: &providers,
            },
        );
        assert_eq!(report.totals.requests, 11);
        assert_eq!(report.totals.usage.input, 2_010_501);
        // $0.25 fresh + $0.10 cached + $1.00 output for each 1M-input fact.
        assert_eq!(report.totals.cost.api.usd, 1_350_000);
        assert_eq!(report.totals.cost.subscription.usd, 1_350_000);
        // ¥0.001 input + ¥0.0004 output.
        assert_eq!(report.totals.cost.api.cny, 1_400);
        assert_eq!(report.totals.cost.unpriced_requests, 5);
        assert_eq!(
            report.unpriced,
            [("model-c".to_owned(), 4), (String::new(), 1)]
        );
        assert_eq!(report.previous.unwrap().requests, 1);
        assert_eq!(report.points[9].totals.requests, 5);
        assert_eq!(report.points[15].totals.requests, 6);
        assert_eq!(report.heat[2][15], 11_000);
        assert_eq!(report.speed_median, Some(20.0));
        let names = report
            .connections
            .iter()
            .map(|share| (share.name.as_str(), share.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("API 连接", GroupKind::Api),
                ("订阅连接", GroupKind::Subscription),
                ("Codex 原生登录", GroupKind::Native),
                ("未知来源", GroupKind::Unknown),
            ]
        );
        assert_eq!(report.models[0].name, "model-a");
        assert_eq!(report.models.last().unwrap().name, "未知模型");

        // A connection filter narrows totals and models but keeps every connection.
        let filtered = build(
            window,
            &UsageFilter {
                group: Some("p:api".into()),
                model: None,
            },
            Inputs {
                facts: &facts,
                previous: Some(&previous),
                heat: &[],
                speeds: &[],
                prices: &prices,
                providers: &providers,
            },
        );
        assert_eq!(filtered.totals.requests, 4);
        assert_eq!(filtered.connections.len(), 4);
        let models = filtered
            .models
            .iter()
            .map(|share| share.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(models, ["model-a", "model-b"]);
        assert_eq!(filtered.speed_median, None);
    }

    #[test]
    fn connection_prices_override_global_prices_and_round_to_micros() {
        let prices = [
            price("", "m", Currency::Usd, [1_000_000, 0, 0]),
            price("special", "m", Currency::Cny, [3_000_000, 0, 0]),
        ];
        let prices = Prices::new(&prices);
        assert_eq!(
            prices.find(Some("special"), "m").unwrap().currency,
            Currency::Cny
        );
        assert_eq!(
            prices.find(Some("other"), "m").unwrap().currency,
            Currency::Usd
        );
        assert_eq!(prices.find(None, "m").unwrap().currency, Currency::Usd);
        assert!(prices.find(None, "missing").is_none());
        let one = price("", "m", Currency::Usd, [1, 0, 0]);
        assert_eq!(price_micros(&one, usage(499_999, 0, 0)), 0);
        assert_eq!(price_micros(&one, usage(500_000, 0, 0)), 1);
        assert_eq!(
            group_identity("p:gone", &[]),
            ("已删除的连接".to_owned(), GroupKind::Api)
        );
        assert_eq!(
            group_identity("c:switchx_router", &[]).1,
            GroupKind::RoutedLegacy
        );
        assert_eq!(group_identity("c:custom", &[]).0, "custom");
    }

    #[test]
    fn clock_shows_dates_only_for_earlier_days() {
        let today = now().timestamp_millis();
        assert_eq!(clock(today, now()), "16:04:00");
        assert_eq!(clock(today - 86_400_000, now()), "10/12 16:04");
    }
}
