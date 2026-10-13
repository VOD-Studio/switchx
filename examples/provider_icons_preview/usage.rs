use std::{
    cell::Cell,
    error::Error,
    path::{Path, PathBuf},
    rc::Rc,
};

use chrono::{Duration, Local, Timelike};
use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};
use switchx::{
    app::{self, UsageQuery, UsageView},
    headless, provider_icons,
    storage::{
        AccountBinding, Currency, PriceRecord, ProviderKind, ProviderRecord, Store, UsageFilter,
        UsageLogFile, UsageRecord, UsageSource,
    },
    ui::{AppWindow, Theme, UsageSyncState},
    usage::{TokenUsage, report::Period},
};

use crate::{
    support::{frame, play, set_theme, snapshot_now},
    usage_view,
};

struct Data(PathBuf);

impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// A small deterministic generator keeps previews identical between runs.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 1_000_000) as f64 / 1_000_000.0
    }
}

fn provider(id: &str, name: &str, kind: ProviderKind, base_url: &str) -> ProviderRecord {
    ProviderRecord {
        id: id.into(),
        name: name.into(),
        base_url: base_url.into(),
        model_id: String::new(),
        credential_ref: None,
        kind,
        account_binding: (kind != ProviderKind::ApiKey).then_some(AccountBinding::Native),
        icon_id: None,
    }
}

fn price(model: &str, currency: Currency, prices: [u64; 3]) -> PriceRecord {
    PriceRecord {
        provider_id: String::new(),
        model: model.into(),
        currency,
        input_micros: prices[0],
        cached_input_micros: prices[1],
        output_micros: prices[2],
    }
}

/// Ninety days of synthetic usage across routed, subscription, and logged sessions.
fn seed(empty: bool) -> Result<Data, Box<dyn Error>> {
    let root =
        std::env::temp_dir().join(format!("switchx-usage-preview-{}", switchx::id::new_id()?));
    let data = Data(root);
    let store = Store::open(&{
        std::fs::create_dir_all(&data.0)?;
        data.0.join("switchx.sqlite")
    })?;
    if empty {
        return Ok(data);
    }
    for record in [
        provider("chatgpt", "ChatGPT 订阅", ProviderKind::Chatgpt, ""),
        provider(
            "deepseek",
            "DeepSeek",
            ProviderKind::ApiKey,
            "https://api.deepseek.com",
        ),
        provider(
            "kimi",
            "Kimi",
            ProviderKind::ApiKey,
            "https://api.moonshot.cn/v1",
        ),
    ] {
        store.put_provider(&record)?;
    }
    for record in [
        price(
            "gpt-6.1-sol",
            Currency::Usd,
            [1_250_000, 125_000, 10_000_000],
        ),
        price("gpt-6-luna", Currency::Usd, [600_000, 60_000, 2_400_000]),
        price(
            "deepseek-flash",
            Currency::Cny,
            [1_000_000, 100_000, 4_000_000],
        ),
        price("kimi-k3", Currency::Cny, [4_000_000, 1_000_000, 16_000_000]),
    ] {
        store.put_model_price(&record)?;
    }
    // (provider, Codex provider key, model, weight, routed)
    let sources = [
        (Some("chatgpt"), None, "gpt-6.1-sol", 0.38, true),
        (Some("deepseek"), None, "deepseek-flash", 0.24, true),
        (Some("kimi"), None, "kimi-k3", 0.1, true),
        (None, Some("openai"), "gpt-6-luna", 0.2, false),
        (None, Some("custom_provider"), "glm-5.3", 0.05, false),
        (None, Some("openai"), "gpt-6-astra", 0.03, false),
    ];
    let mut noise = Noise(0x5eed_1234_abcd);
    let now = Local::now();
    let mut records = Vec::new();
    for day in 0..92i64 {
        let date = now - Duration::days(day);
        let weekend = matches!(date.format("%u").to_string().as_str(), "6" | "7");
        for hour in 8..24u32 {
            let Some(start) = date.with_hour(hour).and_then(|time| time.with_minute(0)) else {
                continue;
            };
            let busy = (std::f64::consts::PI * (hour as f64 - 8.0) / 15.0)
                .sin()
                .max(0.1);
            let count = (busy * if weekend { 5.0 } else { 14.0 } * (0.4 + noise.next())) as i64;
            for request in 0..count {
                let at = start + Duration::seconds((noise.next() * 3_599.0) as i64);
                if at > now {
                    continue;
                }
                let pick = noise.next();
                let mut edge = 0.0;
                let source = sources
                    .iter()
                    .find(|source| {
                        edge += source.3;
                        pick <= edge
                    })
                    .unwrap_or(&sources[0]);
                let input = (18_000.0 + noise.next() * 160_000.0) as u64;
                let output = (180.0 + noise.next().powi(2) * 6_000.0) as u64;
                let duration =
                    (output as f64 / (40.0 + noise.next() * 90.0) * 1_000.0) as i64 + 600;
                records.push(UsageRecord {
                    id: format!("preview-{day}-{hour}-{request}"),
                    source: if source.4 {
                        UsageSource::Router
                    } else {
                        UsageSource::CodexLog
                    },
                    started_at_ms: at.timestamp_millis(),
                    provider_id: source.0.map(str::to_owned),
                    codex_provider: source.1.map(str::to_owned),
                    public_model: source.4.then(|| format!("{}/显示名称", source.2)),
                    model: source.2.into(),
                    usage: TokenUsage {
                        input,
                        cached_input: (input as f64 * (0.82 + noise.next() * 0.16)) as u64,
                        cache_write: 0,
                        output,
                        reasoning: (output as f64 * noise.next() * 0.4) as u64,
                    },
                    duration_ms: source.4.then_some(duration),
                    first_event_ms: source.4.then_some(500),
                    response_id: None,
                    session_id: None,
                });
            }
        }
    }
    store.put_imported_usage(
        &records,
        &UsageLogFile {
            path: "synthetic".into(),
            size: 0,
            modified_ms: 0,
            read_offset: 0,
            state: "{}".into(),
        },
    )?;
    Ok(data)
}

fn icon(id: &str) -> Option<(slint::Image, bool)> {
    let icon = provider_icons::icon(id)?;
    provider_icons::load_image(icon)
        .ok()
        .map(|image| (image, icon.monochrome))
}

/// Applies a report the way the desktop does, replacing models to replay entry motion.
fn show(app: &AppWindow, view: &UsageView) {
    let report = &view.report;
    app.set_usage_summary(usage_view::summary(report));
    app.set_usage_scale(usage_view::scale(report));
    app.set_usage_points(ModelRc::new(VecModel::from(usage_view::points(report))));
    app.set_usage_connections(ModelRc::new(VecModel::from(usage_view::share_rows(
        &icon,
        &report.connections,
        &view.providers,
        true,
    ))));
    app.set_usage_models(ModelRc::new(VecModel::from(usage_view::share_rows(
        &icon,
        &report.models,
        &view.providers,
        false,
    ))));
    app.set_usage_heat(ModelRc::new(VecModel::from(usage_view::heat(report))));
    app.set_usage_heat_caption("按本地时间 · 颜色越深，Token 越多".into());
    let rows = view
        .rows
        .iter()
        .map(|row| usage_view::detail_row(&icon, row, &view.providers, 0))
        .collect::<Vec<_>>();
    app.set_usage_rows_more(rows.len() < view.total_rows);
    app.set_usage_rows_summary(
        format!(
            "共 {} 条 · 显示最近 {} 条",
            app::format_count(view.total_rows as u64),
            rows.len()
        )
        .into(),
    );
    app.set_usage_rows(ModelRc::new(VecModel::from(rows)));
    app.set_usage_today(format!("今日 {}", app::format_tokens(view.today.usage.total())).into());
    app.set_usage_loaded(true);
}

fn load(data: &Path, period: Period, filter: UsageFilter) -> Result<UsageView, Box<dyn Error>> {
    app::load_usage(
        data,
        &UsageQuery {
            period,
            filter,
            rows: app::USAGE_PAGE,
        },
    )
    .map_err(|error| error.message().into())
}

fn pointer(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

pub(super) fn render_usage_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let data = seed(false)?;
    let refreshes = Rc::new(Cell::new(0));
    let counter = refreshes.clone();
    app.on_usage_refresh(move || counter.set(counter.get() + 1));
    app.set_usage_sync(UsageSyncState {
        enabled: true,
        text: "已同步 · 16:04".into(),
        ..Default::default()
    });
    let today = load(&data.0, Period::Today, UsageFilter::default())?;
    assert!(
        today.report.totals.requests > 0,
        "synthetic usage covers today"
    );
    let month = load(&data.0, Period::Days30, UsageFilter::default())?;
    let filtered = load(
        &data.0,
        Period::Days30,
        UsageFilter {
            group: Some("p:deepseek".into()),
            model: None,
        },
    )?;
    assert!(filtered.report.totals.requests < month.report.totals.requests);
    assert_eq!(
        filtered.report.connections.len(),
        month.report.connections.len()
    );
    for (width, height, dark) in [(1200, 820, true), (1200, 820, false), (1000, 680, true)] {
        let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
        app.window().set_size(PhysicalSize::new(width, height));
        app.global::<Theme>().set_animations_enabled(false);
        app.set_active_page(0);
        set_theme(app, window, dark);
        app.set_usage_period(0);
        app.set_usage_metric(0);
        app.set_usage_group_filter("".into());
        app.set_usage_model_filter("".into());
        show(app, &today);
        app.global::<Theme>().set_animations_enabled(true);
        frame(600);
        headless::render(window);
        let before = refreshes.get();
        app.set_active_page(2);
        headless::render(window);
        assert_eq!(
            refreshes.get(),
            before + 1,
            "entering the page refreshes at once"
        );
        play(
            window,
            output,
            &format!("usage-enter-{suffix}"),
            &[64, 160, 320, 560, 900, 1400],
        )?;
        frame(1_200);
        snapshot_now(window, output, &format!("usage-settled-{suffix}"))?;

        // Switching the metric morphs bar heights in a left-to-right wave.
        app.set_usage_metric(1);
        play(
            window,
            output,
            &format!("usage-cost-{suffix}"),
            &[0, 128, 304, 704],
        )?;
        app.set_usage_metric(0);
        frame(900);

        // Hovering a bar raises its tooltip and dims the rest.
        let plot_x = 180.0 + 28.0 + 54.0 + 8.0;
        let slot = (width as f32 - 180.0 - 56.0 - 54.0 - 36.0) / 24.0;
        pointer(window, plot_x + slot * 14.5, 580.0);
        frame(200);
        snapshot_now(window, output, &format!("usage-hover-{suffix}"))?;
        pointer(window, plot_x + slot * 16.5, 580.0);
        play(
            window,
            output,
            &format!("usage-hover-glide-{suffix}"),
            &[0, 96, 224],
        )?;
        pointer(window, 5.0, 5.0);
        frame(300);

        // A new period replaces every bucket, so the chart grows in again.
        app.set_usage_period(2);
        show(app, &month);
        play(
            window,
            output,
            &format!("usage-period-{suffix}"),
            &[0, 112, 256, 512, 960],
        )?;

        // Choosing a connection filters the page while the ranking keeps every row.
        app.set_usage_group_filter("p:deepseek".into());
        app.set_usage_group_filter_label("DeepSeek".into());
        show(app, &filtered);
        play(
            window,
            output,
            &format!("usage-filter-{suffix}"),
            &[0, 160, 480, 960],
        )?;
        app.set_usage_group_filter("".into());
        show(app, &month);
        frame(1_000);

        // Lower sections: rankings, the active-hours grid, and detail rows.
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(width as f32 / 2.0, 500.0),
            delta_x: 0.0,
            delta_y: -520.0,
        });
        frame(400);
        snapshot_now(window, output, &format!("usage-lower-{suffix}"))?;
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(width as f32 / 2.0, 500.0),
            delta_x: 0.0,
            delta_y: -700.0,
        });
        frame(400);
        snapshot_now(window, output, &format!("usage-details-{suffix}"))?;
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(width as f32 / 2.0, 500.0),
            delta_x: 0.0,
            delta_y: 4_000.0,
        });
        frame(400);
    }

    let empty = seed(true)?;
    app.set_usage_period(0);
    show(app, &load(&empty.0, Period::Today, UsageFilter::default())?);
    app.set_active_page(0);
    frame(400);
    app.set_active_page(2);
    play(window, output, "usage-empty-dark-1000x680", &[896])?;
    println!("Synthetic usage page frames: {}", output.display());
    Ok(())
}
