//! The usage page: report views, filters, and a background thread that reads
//! session logs and builds reports without waiting behind route commands.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use chrono::Local;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use switchx::{
    app::{self, USAGE_PAGE, USAGE_ROW_LIMIT, UsageQuery, UsageView},
    storage::UsageFilter,
    ui::{AppWindow, UsageSyncState},
    usage::report::Period,
};

use super::{
    icon_picker,
    rows::sync_rows,
    usage_view::{detail_row, heat, points, scale, share_rows, summary},
};

// Session logs are append-only, so an unchanged scan costs a few milliseconds.
const SYNC_INTERVAL: Duration = Duration::from_secs(60);

enum Job {
    Load(UsageQuery),
    Sync { home: PathBuf, force: bool },
}

/// Runs usage work on its own thread; queued loads collapse to the latest query.
#[derive(Clone)]
pub(super) struct UsageService {
    sender: mpsc::Sender<Job>,
    cancel: Arc<AtomicBool>,
}

impl UsageService {
    pub(super) fn start(weak: Weak<AppWindow>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = cancel.clone();
        std::thread::Builder::new()
            .name("switchx-usage".into())
            .spawn(move || run(receiver, weak, thread_cancel))
            .expect("start usage thread");
        Self { sender, cancel }
    }

    pub(super) fn load(&self, query: UsageQuery) {
        let _ = self.sender.send(Job::Load(query));
    }

    pub(super) fn sync(&self, home: &str, force: bool) {
        let home = PathBuf::from(home);
        if home.is_absolute() {
            let _ = self.sender.send(Job::Sync { home, force });
        }
    }

    /// Stops a running scan at the next file; the thread ends with its channel.
    pub(super) fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn run(receiver: mpsc::Receiver<Job>, weak: Weak<AppWindow>, cancel: Arc<AtomicBool>) {
    let data_dir = app::data_directory();
    let mut home: Option<PathBuf> = None;
    let mut last_query: Option<UsageQuery> = None;
    let mut next_sync = Instant::now();
    loop {
        let wait = next_sync.saturating_duration_since(Instant::now());
        let first = match receiver.recv_timeout(wait) {
            Ok(job) => Some(job),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut query = None;
        let mut sync = None::<bool>;
        for job in first.into_iter().chain(receiver.try_iter()) {
            match job {
                Job::Load(next) => query = Some(next),
                Job::Sync { home: next, force } => {
                    home = Some(next);
                    sync = Some(sync.unwrap_or(false) || force);
                }
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(data_dir) = data_dir.as_deref() else {
            next_sync = Instant::now() + SYNC_INTERVAL;
            if query.is_some() {
                post_error(&weak, app::AppError::DataDirectory.message());
            }
            continue;
        };
        // Periodic scans run while the app is open; a page refresh may force one sooner.
        if Instant::now() >= next_sync && sync.is_none() && home.is_some() {
            sync = Some(false);
        }
        if let (Some(force), Some(home)) = (sync, home.as_deref()) {
            next_sync = Instant::now() + SYNC_INTERVAL;
            let added = sync_logs(&weak, data_dir, home, force, &cancel);
            if added && query.is_none() {
                query = last_query.clone();
            }
        } else if Instant::now() >= next_sync {
            next_sync = Instant::now() + SYNC_INTERVAL;
        }
        if let Some(query) = query {
            let result = app::load_usage(data_dir, &query);
            last_query = Some(query.clone());
            let _ = weak.upgrade_in_event_loop(move |app| {
                app.set_usage_loading(false);
                match result {
                    Ok(view) => {
                        show_usage(&app, &query, view);
                        app.set_usage_error("".into());
                    }
                    Err(error) => app.set_usage_error(
                        format!("用量读取失败：{}；保留上次结果", error.message()).into(),
                    ),
                }
            });
        }
    }
}

/// Imports new log usage and reports progress; true when rows were added.
fn sync_logs(
    weak: &Weak<AppWindow>,
    data_dir: &Path,
    home: &Path,
    force: bool,
    cancel: &AtomicBool,
) -> bool {
    let mut last_post = Instant::now();
    let mut announced = false;
    let result = app::sync_usage_logs(data_dir, home, force, cancel, |progress| {
        // Large first imports show progress; quick incremental scans stay quiet.
        if progress.total == 0 || last_post.elapsed() < Duration::from_millis(120) {
            return;
        }
        last_post = Instant::now();
        announced = true;
        let fraction = progress.done as f32 / progress.total as f32;
        let _ = weak.upgrade_in_event_loop(move |app| {
            app.set_usage_sync(UsageSyncState {
                running: true,
                progress: fraction,
                text: format!("正在读取会话日志 {}%", (fraction * 100.0).round()).into(),
                ..app.get_usage_sync()
            });
        });
    });
    let now = Local::now().format("%H:%M").to_string();
    let added = matches!(result, Ok(Some(summary)) if summary.added > 0);
    let _ = weak.upgrade_in_event_loop(move |app| {
        let mut state = app.get_usage_sync();
        state.running = false;
        state.progress = if announced { 1.0 } else { 0.0 };
        match result {
            Ok(Some(summary)) => {
                state.enabled = true;
                state.error = "".into();
                state.text = format!("已同步 · {now}").into();
                state.detail = format!(
                    "上次扫描 {now}，读取 {} 个文件，新增 {} 条",
                    summary.read,
                    app::format_count(summary.added as u64)
                )
                .into();
            }
            Ok(None) => {
                state.enabled = false;
                state.error = "".into();
                state.text = "仅路由记录".into();
            }
            Err(error) => {
                state.error = error.into();
                state.text = "同步未完成".into();
            }
        }
        app.set_usage_sync(state);
    });
    added
}

fn post_error(weak: &Weak<AppWindow>, message: &'static str) {
    let _ = weak.upgrade_in_event_loop(move |app| {
        app.set_usage_loading(false);
        app.set_usage_error(format!("用量读取失败：{message}").into());
    });
}

/// The page's current query, rebuilt from the window so every trigger agrees.
fn current_query(app: &AppWindow, rows: usize) -> UsageQuery {
    let text = |value: SharedString| Some(value.to_string()).filter(|value| !value.is_empty());
    UsageQuery {
        period: Period::from_index(app.get_usage_period()),
        filter: UsageFilter {
            group: text(app.get_usage_group_filter()),
            model: text(app.get_usage_model_filter()),
        },
        rows,
    }
}

pub(super) fn connect(app: &AppWindow, service: &UsageService) {
    // Detail rows already requested; reset whenever the period or a filter changes.
    let rows = Rc::new(Cell::new(USAGE_PAGE));
    let request = {
        let weak = app.as_weak();
        let service = service.clone();
        let rows = rows.clone();
        Rc::new(move |reset: bool| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            if reset {
                rows.set(USAGE_PAGE);
                app.set_usage_rows_arrival(0);
            }
            app.set_usage_loading(true);
            service.load(current_query(&app, rows.get()));
        })
    };
    let load = request.clone();
    app.on_usage_select_period(move |_| load(true));
    let load = request.clone();
    let weak = app.as_weak();
    app.on_usage_filter_group(move |key| {
        if let Some(app) = weak.upgrade() {
            let label = app
                .get_usage_connections()
                .iter()
                .find(|row| row.key == key)
                .map(|row| row.name)
                .unwrap_or_default();
            app.set_usage_group_filter(key);
            app.set_usage_group_filter_label(label);
            load(true);
        }
    });
    let load = request.clone();
    let weak = app.as_weak();
    app.on_usage_filter_model(move |key| {
        if let Some(app) = weak.upgrade() {
            app.set_usage_model_filter(key);
            load(true);
        }
    });
    let load = request.clone();
    let more = rows.clone();
    let weak = app.as_weak();
    app.on_usage_load_more(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_usage_loading()
            && app.get_usage_rows_more()
        {
            more.set((more.get() + USAGE_PAGE).min(USAGE_ROW_LIMIT));
            load(false);
        }
    });
    let weak = app.as_weak();
    let refresh_service = service.clone();
    app.on_usage_refresh(move || {
        if let Some(app) = weak.upgrade() {
            refresh_service.sync(&app.get_config_home(), false);
            if !app.get_usage_loading() {
                request(false);
            }
        }
    });
    let weak = app.as_weak();
    let sources_service = service.clone();
    app.on_usage_open_sources(move || {
        if let Some(app) = weak.upgrade() {
            sources_service.sync(&app.get_config_home(), true);
        }
    });
}

// Remembered between reports to tell arrivals from rows already on screen.
thread_local! {
    static SHOWN: RefCell<(String, HashSet<String>, i32)> = RefCell::new((String::new(), HashSet::new(), 0));
}

fn query_key(query: &UsageQuery) -> String {
    format!(
        "{:?}|{}|{}",
        query.period,
        query.filter.group.as_deref().unwrap_or(""),
        query.filter.model.as_deref().unwrap_or("")
    )
}

/// The icon rows the window already loaded, by catalog ID.
fn loaded_icon(app: &AppWindow, id: &str) -> Option<(slint::Image, bool)> {
    let row = icon_picker::provider_icon_row(app, id);
    (!row.id.is_empty()).then_some((row.icon, row.monochrome))
}

pub(super) fn show_usage(app: &AppWindow, query: &UsageQuery, view: UsageView) {
    let report = &view.report;
    let icons = |id: &str| loaded_icon(app, id);
    app.set_usage_summary(summary(report));
    app.set_usage_scale(scale(report));
    sync_rows(
        app.get_usage_points(),
        points(report),
        |point| point.key.clone(),
        |model| app.set_usage_points(model),
    );
    replace_in_place(
        app.get_usage_connections(),
        share_rows(&icons, &report.connections, &view.providers, true),
        |model| app.set_usage_connections(model),
    );
    replace_in_place(
        app.get_usage_models(),
        share_rows(&icons, &report.models, &view.providers, false),
        |model| app.set_usage_models(model),
    );
    replace_in_place(app.get_usage_heat(), heat(report), |model| {
        app.set_usage_heat(model)
    });
    app.set_usage_heat_caption("按本地时间 · 颜色越深，Token 越多".into());
    // Rows unseen under the same query are arrivals; a new query starts quietly.
    let key = query_key(query);
    let arrival = SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let fresh_query = shown.0 != key;
        let stamp = if fresh_query { 0 } else { shown.2 + 1 };
        let arrived = if fresh_query {
            0
        } else {
            view.rows
                .iter()
                .filter(|row| !shown.1.contains(&row.id))
                .count()
        };
        shown.0 = key;
        shown.1 = view.rows.iter().map(|row| row.id.clone()).collect();
        if arrived > 0 {
            shown.2 = stamp;
            stamp
        } else {
            0
        }
    });
    let previous = app
        .get_usage_rows()
        .iter()
        .map(|row| (row.id.clone(), row.stamp))
        .collect::<std::collections::HashMap<_, _>>();
    let rows = view
        .rows
        .iter()
        .map(|row| {
            let stamp = previous.get(row.id.as_str()).copied().unwrap_or(arrival);
            detail_row(&icons, row, &view.providers, stamp)
        })
        .collect::<Vec<_>>();
    let shown = rows.len();
    sync_rows(
        app.get_usage_rows(),
        rows,
        |row| row.id.clone(),
        |model| app.set_usage_rows(model),
    );
    if arrival > 0 {
        app.set_usage_rows_arrival(arrival);
    }
    let available = view.total_rows.min(USAGE_ROW_LIMIT);
    app.set_usage_rows_more(shown < available);
    app.set_usage_rows_summary(
        if view.total_rows == 0 {
            String::new()
        } else if shown < view.total_rows {
            format!(
                "共 {} 条 · 显示最近 {} 条",
                app::format_count(view.total_rows as u64),
                app::format_count(shown as u64)
            )
        } else {
            format!("共 {} 条", app::format_count(view.total_rows as u64))
        }
        .into(),
    );
    app.set_usage_today(if view.today.usage.is_empty() {
        "".into()
    } else {
        format!("今日 {}", app::format_tokens(view.today.usage.total())).into()
    });
    let mut sync = app.get_usage_sync();
    if sync.enabled != view.log_import && !sync.running {
        sync.enabled = view.log_import;
        if !view.log_import {
            sync.text = "仅路由记录".into();
        }
        app.set_usage_sync(sync);
    }
    app.set_usage_loaded(true);
}

/// Updates list rows by position so bars glide to their new values.
fn replace_in_place<T: Clone + PartialEq + 'static>(
    model: ModelRc<T>,
    rows: Vec<T>,
    replace: impl FnOnce(ModelRc<T>),
) {
    let Some(current) = model.as_any().downcast_ref::<VecModel<T>>() else {
        replace(ModelRc::new(VecModel::from(rows)));
        return;
    };
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        if index < current.row_count() {
            if current.row_data(index).as_ref() != Some(&row) {
                current.set_row_data(index, row);
            }
        } else {
            current.push(row);
        }
    }
    while current.row_count() > count {
        current.remove(current.row_count() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchx::{
        headless,
        storage::{Store, UsageLogFile, UsageRecord, UsageSource},
        usage::TokenUsage,
    };

    fn record(id: &str, at: i64, model: &str) -> UsageRecord {
        UsageRecord {
            id: id.into(),
            source: UsageSource::CodexLog,
            started_at_ms: at,
            provider_id: None,
            codex_provider: Some("openai".into()),
            public_model: None,
            model: model.into(),
            usage: TokenUsage {
                input: 10_000,
                cached_input: 9_000,
                cache_write: 0,
                output: 500,
                reasoning: 100,
            },
            duration_ms: None,
            first_event_ms: None,
            response_id: None,
            session_id: None,
        }
    }

    #[test]
    fn reports_fill_the_page_and_mark_only_new_rows_as_arrivals() {
        headless::install();
        let app = AppWindow::new().unwrap();
        let data = std::env::temp_dir().join(format!(
            "switchx-usage-desktop-{}",
            switchx::id::new_id().unwrap()
        ));
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        let file = UsageLogFile {
            path: "synthetic".into(),
            size: 0,
            modified_ms: 0,
            read_offset: 0,
            state: "{}".into(),
        };
        let now = Local::now().timestamp_millis();
        store
            .put_imported_usage(
                &[
                    record("a", now - 2_000, "m1"),
                    record("b", now - 1_000, "m2"),
                ],
                &file,
            )
            .unwrap();
        let query = UsageQuery {
            rows: USAGE_PAGE,
            ..Default::default()
        };
        show_usage(&app, &query, app::load_usage(&data, &query).unwrap());
        let summary = app.get_usage_summary();
        assert_eq!((summary.tokens, summary.requests), (21_000.0, 2.0));
        assert!((summary.cache_rate - 0.9).abs() < 1e-6);
        assert_eq!(app.get_usage_points().row_count(), 24);
        assert_eq!(
            app.get_usage_connections().row_data(0).unwrap().name,
            "Codex 原生登录"
        );
        assert_eq!(app.get_usage_models().row_count(), 2);
        assert_eq!(app.get_usage_heat().row_count(), 168);
        assert_eq!(app.get_usage_rows().row_count(), 2);
        assert_eq!(
            app.get_usage_rows_arrival(),
            0,
            "a first report has no arrivals"
        );
        assert_eq!(app.get_usage_today(), "今日 21.0K");
        assert!(app.get_usage_loaded() && !app.get_usage_rows_more());

        store
            .put_imported_usage(&[record("c", now, "m1")], &file)
            .unwrap();
        show_usage(&app, &query, app::load_usage(&data, &query).unwrap());
        let stamps = app
            .get_usage_rows()
            .iter()
            .map(|row| (row.id.to_string(), row.stamp))
            .collect::<Vec<_>>();
        let arrival = app.get_usage_rows_arrival();
        assert!(arrival > 0);
        assert_eq!(
            stamps,
            [("c".into(), arrival), ("b".into(), 0), ("a".into(), 0)]
        );

        // A different query starts quietly, without replaying arrivals.
        let filtered = UsageQuery {
            filter: UsageFilter {
                group: None,
                model: Some("m1".into()),
            },
            ..query
        };
        app.set_usage_rows_arrival(0);
        show_usage(&app, &filtered, app::load_usage(&data, &filtered).unwrap());
        assert_eq!(app.get_usage_rows().row_count(), 2);
        assert_eq!(app.get_usage_rows_arrival(), 0);
        assert_eq!(app.get_usage_connections().row_count(), 1);
        drop(store);
        std::fs::remove_dir_all(data).unwrap();
    }
}
