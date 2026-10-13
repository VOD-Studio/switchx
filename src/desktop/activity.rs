//! The activity timeline of routed requests.

use std::collections::{HashMap, HashSet};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app,
    storage::RequestStatus,
    ui::{AppWindow, RequestBar, RequestRow, RequestStats},
};
use tokio::sync::mpsc;

use super::{command::Command, rows::sync_rows, worker::Worker};

pub(super) const PULSE_BARS: usize = 48;

// Rows aged out of the newest-100 window that a paused reader may still be looking at.
pub(super) const RETAINED_REQUEST_HISTORY: usize = 300;

pub(super) fn request_row(record: app::RequestView, stamp: i32) -> RequestRow {
    let millis = |value: i64| value.clamp(0, i32::MAX as i64) as i32;
    RequestRow {
        clock: record.time.get(11..).unwrap_or(&record.time).into(),
        duration: app::format_duration(record.duration_ms).into(),
        duration_ms: millis(record.duration_ms),
        // Log scale up to one minute keeps sub-second and multi-second calls comparable.
        level: ((record.duration_ms.max(0) as f64 + 1.0).log10() / 60_001_f64.log10())
            .clamp(0.0, 1.0) as f32,
        headers_ms: record.headers_ms.map_or(-1, millis),
        first_event_ms: record.first_event_ms.map_or(-1, millis),
        http: record
            .http_status
            .map(|status| status.to_string())
            .unwrap_or_default()
            .into(),
        status: record.status.label().into(),
        kind: match record.status {
            RequestStatus::Completed => 0,
            RequestStatus::Failed => 1,
            RequestStatus::Interrupted => 2,
            RequestStatus::Cancelled => 3,
        },
        id: record.id.into(),
        time: record.time.into(),
        model: record.model.into(),
        provider: record.provider.into(),
        upstream: record.upstream.into(),
        generation: record.generation.into(),
        error: record.error.into(),
        fallback: record.fallback.into(),
        tokens: record
            .usage
            .filter(|usage| !usage.is_empty())
            .map(|usage| format!("{} Token", app::format_tokens(usage.total())))
            .unwrap_or_default()
            .into(),
        token_detail: record
            .usage
            .filter(|usage| !usage.is_empty())
            .map(|usage| {
                let mut detail = format!(
                    "输入 {}（缓存 {}）· 输出 {}",
                    app::format_tokens(usage.input),
                    app::format_tokens(usage.cached_input),
                    app::format_tokens(usage.output)
                );
                if usage.reasoning > 0 {
                    detail += &format!("（推理 {}）", app::format_tokens(usage.reasoning));
                }
                detail
            })
            .unwrap_or_default()
            .into(),
        stamp,
    }
}

pub(super) fn request_matches(kind: i32, filter: i32) -> bool {
    match filter {
        1 => kind == 0,
        2 => kind == 1 || kind == 2,
        3 => kind == 3,
        _ => true,
    }
}

/// Orders the visible rows and counts arrivals. While the reader is paused,
/// leading rows that left the newest window stay above, so trimming never
/// shifts the entries under the pointer.
pub(super) fn merge_request_rows(
    current: &[RequestRow],
    rows: Vec<RequestRow>,
    keep_history: bool,
) -> (Vec<RequestRow>, usize) {
    let incoming = rows.iter().map(|row| &row.id).collect::<HashSet<_>>();
    let visible = current.iter().map(|row| &row.id).collect::<HashSet<_>>();
    let arrivals = rows.iter().filter(|row| !visible.contains(&row.id)).count();
    let aged = if keep_history {
        current
            .iter()
            .take_while(|row| !incoming.contains(&row.id))
            .count()
    } else {
        0
    };
    let mut merged = current[aged.saturating_sub(RETAINED_REQUEST_HISTORY)..aged].to_vec();
    merged.extend(rows);
    (merged, arrivals)
}

pub(super) fn show_request_rows(app: &AppWindow, keep_history: bool) {
    let filter = app.get_request_filter();
    let rows = app
        .get_request_log()
        .iter()
        .filter(|row| request_matches(row.kind, filter))
        .collect::<Vec<_>>();
    let model = app.get_requests();
    let current = model.iter().collect::<Vec<_>>();
    let (rows, arrivals) = merge_request_rows(&current, rows, keep_history);
    sync_rows(
        model,
        rows,
        |row| row.id.clone(),
        |model| app.set_requests(model),
    );
    if keep_history && arrivals > 0 {
        app.set_request_unseen(app.get_request_unseen().saturating_add(arrivals as i32));
    }
}

pub(super) fn show_requests(app: &AppWindow, records: Vec<app::RequestView>) {
    if !records.is_empty() {
        app.set_request_cleared(false);
    }
    let stats = app::request_stats(&records);
    let sync = app.get_request_sync().saturating_add(1);
    // The first refresh is history; later unknown ids are live arrivals.
    let arrival = if sync == 1 { 0 } else { sync };
    let stamps = app
        .get_request_log()
        .iter()
        .map(|row| (row.id, row.stamp))
        .collect::<HashMap<_, _>>();
    let log = records
        .into_iter()
        .rev()
        .map(|record| {
            let stamp = stamps.get(record.id.as_str()).copied().unwrap_or(arrival);
            request_row(record, stamp)
        })
        .collect::<Vec<_>>();
    let bars = log[log.len().saturating_sub(PULSE_BARS)..]
        .iter()
        .map(|row| RequestBar {
            id: row.id.clone(),
            level: row.level,
            kind: row.kind,
            label: format!("{} · {} · {}", row.model, row.duration, row.clock).into(),
            stamp: row.stamp,
        })
        .collect();
    let judged = stats.completed + stats.failed;
    let millis = |value: Option<i64>| value.map_or(-1.0, |ms| ms as f32);
    app.set_request_stats(RequestStats {
        total: stats.total as i32,
        completed: stats.completed as i32,
        failed: stats.failed as i32,
        cancelled: stats.cancelled as i32,
        success_rate: if judged == 0 {
            -1.0
        } else {
            stats.completed as f32 * 100.0 / judged as f32
        },
        p50_ms: millis(stats.p50_ms),
        p95_ms: millis(stats.p95_ms),
        first_event_ms: millis(stats.first_event_p50_ms),
    });
    sync_rows(
        app.get_request_bars(),
        bars,
        |bar| bar.id.clone(),
        |model| app.set_request_bars(model),
    );
    app.set_request_sync(sync);
    app.set_request_log(ModelRc::new(VecModel::from(log)));
    show_request_rows(app, !app.get_request_following());
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let weak = app.as_weak();
    let request_sender = sender.clone();
    app.on_refresh_requests(move || {
        // Polls leave `busy` alone and hold at most one slot in the shared queue;
        // a full queue skips this tick rather than reporting an error.
        if let Some(app) = weak.upgrade()
            && !app.get_request_loading()
            && !app.get_request_clearing()
        {
            app.set_request_loading(true);
            if request_sender.try_send(Command::RefreshRequests).is_err() {
                app.set_request_loading(false);
            }
        }
    });
    let weak = app.as_weak();
    app.on_filter_requests(move |filter| {
        if let Some(app) = weak.upgrade() {
            app.set_request_filter(filter);
            show_request_rows(&app, false);
        }
    });
    let weak = app.as_weak();
    let clear_sender = sender.clone();
    app.on_clear_requests(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_request_clearing()
        {
            app.set_request_clearing(true);
            app.set_request_clear_error("".into());
            if let Err(error) = clear_sender.try_send(Command::ClearRequests) {
                app.set_request_clearing(false);
                app.set_request_clear_error(
                    match error {
                        mpsc::error::TrySendError::Full(_) => "后台正在处理其他操作，请稍后重试。",
                        mpsc::error::TrySendError::Closed(_) => {
                            "后台已停止，请重新打开 SwitchX 后重试。"
                        }
                    }
                    .into(),
                );
            }
        }
    });
    let weak = app.as_weak();
    app.on_finish_clear_requests(move || {
        if let Some(app) = weak.upgrade()
            && app.get_request_erasing()
        {
            reset_request_history(&app);
            app.set_request_erasing(false);
            app.set_request_clearing(false);
            app.set_request_cleared(true);
            // A route can finish another request during the exit animation.
            app.invoke_refresh_requests();
        }
    });
}

fn reset_request_history(app: &AppWindow) {
    app.set_requests(ModelRc::new(VecModel::default()));
    app.set_request_log(ModelRc::new(VecModel::default()));
    app.set_request_bars(ModelRc::new(VecModel::default()));
    app.set_request_stats(RequestStats {
        success_rate: -1.0,
        p50_ms: -1.0,
        p95_ms: -1.0,
        first_event_ms: -1.0,
        ..Default::default()
    });
    app.set_request_filter(0);
    app.set_request_following(true);
    app.set_request_unseen(0);
    app.set_request_error("".into());
}

impl Worker {
    pub(super) fn clear_requests(&self) {
        let result = self.data_dir.get().and_then(app::clear_requests);
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok(_) => {
                app.set_request_clear_open(false);
                app.set_request_erasing(true);
            }
            Err(error) => {
                app.set_request_clearing(false);
                app.set_request_clear_error(
                    format!("清空失败：{}。记录已保留，请稍后重试。", error.message()).into(),
                );
            }
        });
    }

    pub(super) fn refresh_requests(&self) {
        let result = self.data_dir.get().and_then(app::load_requests);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_request_loading(false);
            match result {
                Ok(records) => {
                    show_requests(&app, records);
                    app.set_request_error("".into());
                }
                Err(error) => app.set_request_error(
                    format!("请求记录读取失败：{}；保留上次列表", error.message()).into(),
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchx::headless;

    #[test]
    fn clearing_waits_for_success_resets_paused_history_and_refreshes_new_arrivals() {
        headless::install();
        let app = AppWindow::new().unwrap();
        let (sender, mut receiver) = mpsc::channel(1);
        connect(&app, &sender);
        let rows = ModelRc::new(VecModel::from(vec![RequestRow {
            id: "retained-old-request".into(),
            ..Default::default()
        }]));
        app.set_requests(rows.clone());
        app.set_request_log(rows);
        app.set_request_filter(2);
        app.set_request_following(false);
        app.set_request_unseen(4);
        app.invoke_clear_requests();
        assert!(app.get_request_clearing());
        app.invoke_clear_requests();
        app.invoke_refresh_requests();
        assert!(matches!(receiver.try_recv(), Ok(Command::ClearRequests)));
        assert!(receiver.try_recv().is_err(), "no duplicate clears or polls");
        app.invoke_finish_clear_requests();
        assert_eq!(
            app.get_requests().row_count(),
            1,
            "wait for database success"
        );

        // Queue pressure must leave the old list available for a retry.
        app.set_request_clearing(false);
        sender
            .try_send(Command::PollRouteStatus)
            .unwrap_or_else(|_| panic!("empty queue"));
        app.invoke_clear_requests();
        assert!(!app.get_request_clearing());
        assert!(!app.get_request_clear_error().is_empty());
        assert_eq!(app.get_requests().row_count(), 1);
        receiver
            .try_recv()
            .unwrap_or_else(|_| panic!("queued poll"));

        app.invoke_clear_requests();
        receiver
            .try_recv()
            .unwrap_or_else(|_| panic!("queued clear"));
        app.set_request_erasing(true);
        app.invoke_finish_clear_requests();
        assert!(!app.get_request_clearing());
        assert!(!app.get_request_erasing());
        assert!(app.get_request_cleared());
        assert_eq!(app.get_requests().row_count(), 0);
        assert_eq!(app.get_request_log().row_count(), 0);
        assert_eq!(app.get_request_bars().row_count(), 0);
        assert_eq!(app.get_request_stats().total, 0);
        assert_eq!(app.get_request_stats().p50_ms, -1.0);
        assert_eq!(app.get_request_filter(), 0);
        assert_eq!(app.get_request_unseen(), 0);
        assert!(app.get_request_following());
        assert!(matches!(receiver.try_recv(), Ok(Command::RefreshRequests)));
    }

    #[test]
    fn paused_request_rows_keep_aged_history_and_count_arrivals() {
        let row = |id: &str| RequestRow {
            id: id.into(),
            ..Default::default()
        };
        let ids = |rows: &[RequestRow]| {
            rows.iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>()
        };
        let current = ["a", "b", "c", "d"].map(row);
        // "a" and "b" left the newest window; "c" finished late and "e" is new.
        let incoming = ["x", "c", "d", "e"].map(row).to_vec();
        let (kept, arrivals) = merge_request_rows(&current, incoming.clone(), true);
        assert_eq!(ids(&kept), ["a", "b", "x", "c", "d", "e"]);
        assert_eq!(arrivals, 2);
        let (live, _) = merge_request_rows(&current, incoming, false);
        assert_eq!(ids(&live), ["x", "c", "d", "e"]);

        let long = (0..RETAINED_REQUEST_HISTORY + 5)
            .map(|index| row(&format!("old-{index}")))
            .collect::<Vec<_>>();
        let (capped, arrivals) = merge_request_rows(&long, vec![row("new")], true);
        assert_eq!(capped.len(), RETAINED_REQUEST_HISTORY + 1);
        assert_eq!(capped[0].id, "old-5");
        assert_eq!(arrivals, 1);
        assert!(request_matches(2, 2) && request_matches(1, 2) && !request_matches(0, 2));
    }

    #[test]
    fn request_refresh_stamps_arrivals_updates_in_place_and_filters() {
        headless::install();
        let app = AppWindow::new().unwrap();
        let view = |id: &str, status| app::RequestView {
            id: id.into(),
            time: "2026-10-10 09:47:47".into(),
            model: "sx-model".into(),
            provider: "Synthetic".into(),
            upstream: "upstream".into(),
            duration_ms: 1_600,
            headers_ms: Some(120),
            first_event_ms: None,
            http_status: Some(200),
            generation: "generation".into(),
            status,
            error: String::new(),
            fallback: String::new(),
            usage: Some(switchx::usage::TokenUsage {
                input: 12_000,
                cached_input: 9_000,
                cache_write: 0,
                output: 300,
                reasoning: 0,
            }),
        };
        let done = RequestStatus::Completed;
        let ids = |app: &AppWindow| {
            app.get_requests()
                .iter()
                .map(|row| (row.id.to_string(), row.stamp))
                .collect::<Vec<_>>()
        };
        // Store order is newest first; the timeline is chronological.
        show_requests(&app, vec![view("b", done), view("a", done)]);
        assert_eq!(ids(&app), [("a".into(), 0), ("b".into(), 0)]);
        let first = app.get_requests().row_data(0).unwrap();
        assert_eq!(
            (
                first.clock.as_str(),
                first.duration.as_str(),
                first.http.as_str()
            ),
            ("09:47:47", "1.6 s", "200")
        );
        assert_eq!((first.headers_ms, first.first_event_ms), (120, -1));
        assert_eq!(
            (first.tokens.as_str(), first.token_detail.as_str()),
            ("12.3K Token", "输入 12.0K（缓存 9.0K）· 输出 300")
        );
        let model = app.get_requests();

        show_requests(
            &app,
            vec![view("c", done), view("b", done), view("a", done)],
        );
        assert_eq!(
            ids(&app),
            [("a".into(), 0), ("b".into(), 0), ("c".into(), 2)]
        );
        assert_eq!(model.row_count(), 3, "rows update in the same model");

        // Paused: aged-out rows stay above and arrivals are counted.
        app.set_request_following(false);
        show_requests(
            &app,
            vec![view("d", RequestStatus::Failed), view("c", done)],
        );
        assert_eq!(
            ids(&app).into_iter().map(|row| row.0).collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        assert_eq!(app.get_request_unseen(), 1);
        assert_eq!(app.get_request_bars().row_count(), 2);
        let stats = app.get_request_stats();
        assert_eq!((stats.total, stats.completed, stats.failed), (2, 1, 1));
        assert_eq!(stats.success_rate, 50.0);

        app.set_request_following(true);
        show_requests(
            &app,
            vec![view("d", RequestStatus::Failed), view("c", done)],
        );
        assert_eq!(
            ids(&app),
            [("c".into(), 2), ("d".into(), 3)],
            "following trims to the newest window and keeps arrival stamps"
        );
        app.set_request_filter(2);
        show_request_rows(&app, false);
        assert_eq!(ids(&app), [("d".into(), 3)]);
    }
}
