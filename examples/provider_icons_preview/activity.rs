use std::{
    cell::{Cell, RefCell},
    error::Error,
    path::Path,
    rc::Rc,
};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless,
    ui::{AppWindow, RequestBar, RequestRow, RequestStats, Theme},
};

use crate::support::{frame, play, set_theme, snapshot_now};

pub(super) fn activity_row(index: usize, stamp: i32) -> RequestRow {
    let kind = match index % 9 {
        3 => 1,
        6 => 3,
        8 => 2,
        _ => 0,
    };
    let ms = match kind {
        1 => 0,
        3 => 1_600 + (index as i32 % 4) * 700,
        _ => 380 + (index as i32 * 2_731) % 11_000,
    };
    let (model, provider, upstream) = [
        ("gpt-6-luna", "ChatGPT 订阅", "gpt-6-luna"),
        (
            "sx-chatgpt-01b7c588f74e632446a86553f624da4d-gpt-6-luna",
            "ChatGPT 订阅",
            "gpt-6-luna",
        ),
        ("sx-podlink-gpt-6-luna", "Podlink", "gpt-6-luna"),
        ("grok-5-fast", "Grok 账号", "grok-5-fast-reasoning"),
    ][index % 4];
    let (provider, upstream) = if kind == 1 {
        ("未选择上游", "—")
    } else {
        (provider, upstream)
    };
    let clock = format!("09:{:02}:{:02}", 20 + index / 60, index % 60);
    RequestRow {
        id: format!("synthetic-{index:04}").into(),
        time: format!("2026-10-10 {clock}").into(),
        clock: clock.into(),
        model: model.into(),
        provider: provider.into(),
        upstream: upstream.into(),
        duration: if ms >= 1_000 {
            format!("{:.1} s", ms as f64 / 1_000.0)
        } else {
            format!("{ms} ms")
        }
        .into(),
        duration_ms: ms,
        level: ((ms as f64 + 1.0).log10() / 60_001_f64.log10()) as f32,
        headers_ms: if kind == 1 { -1 } else { ms / 9 },
        first_event_ms: if kind == 1 || kind == 3 { -1 } else { ms / 4 },
        http: if kind == 1 { "" } else { "200" }.into(),
        generation: "synthetic-generation".into(),
        status: ["正常完成", "请求失败", "流中断", "用户取消 / 客户端断开"][kind as usize].into(),
        kind,
        error: [
            "",
            "模型未发布 · unknown_model",
            "响应结束，但未收到正常完成信号 · missing_completion",
            "完成前客户端断开；可能是用户取消或连接丢失 · client_disconnected",
        ][kind as usize]
            .into(),
        fallback: if index % 13 == 5 {
            "主连接 建立连接失败（请求未发送）→ 尝试备用 Podlink".into()
        } else {
            "".into()
        },
        stamp,
    }
}

pub(super) fn activity_bar(row: &RequestRow) -> RequestBar {
    RequestBar {
        id: row.id.clone(),
        level: row.level,
        kind: row.kind,
        label: format!("{} · {} · {}", row.model, row.duration, row.clock).into(),
        stamp: row.stamp,
    }
}

pub(super) fn activity_stats(rows: &[RequestRow]) -> RequestStats {
    let count = |kind: &[i32]| rows.iter().filter(|row| kind.contains(&row.kind)).count() as i32;
    let mut done = rows
        .iter()
        .filter(|row| row.kind == 0)
        .map(|row| row.duration_ms as f32)
        .collect::<Vec<_>>();
    done.sort_by(f32::total_cmp);
    let rank = |p: usize| {
        if done.is_empty() {
            -1.0
        } else {
            done[(done.len() * p).div_ceil(100).max(1) - 1]
        }
    };
    let (completed, failed) = (count(&[0]), count(&[1, 2]));
    RequestStats {
        total: rows.len() as i32,
        completed,
        failed,
        cancelled: count(&[3]),
        success_rate: if completed + failed == 0 {
            -1.0
        } else {
            completed as f32 * 100.0 / (completed + failed) as f32
        },
        p50_ms: rank(50),
        p95_ms: rank(95),
        first_event_ms: rank(50) / 4.0,
    }
}

pub(super) fn render_activity_design(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let filters = Rc::new(RefCell::new(Vec::<RequestRow>::new()));
    let log = filters.clone();
    let weak = app.as_weak();
    app.on_filter_requests(move |filter| {
        let app = weak.upgrade().unwrap();
        let rows = log
            .borrow()
            .iter()
            .filter(|row| match filter {
                1 => row.kind == 0,
                2 => row.kind == 1 || row.kind == 2,
                3 => row.kind == 3,
                _ => true,
            })
            .cloned()
            .collect::<Vec<_>>();
        app.set_requests(ModelRc::new(VecModel::from(rows)));
    });
    let polls = Rc::new(Cell::new(0));
    let counter = polls.clone();
    app.on_refresh_requests(move || counter.set(counter.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(false);
            app.set_active_page(0);
            set_theme(app, window, dark);
            let rows = (0..36)
                .map(|index| activity_row(index, 0))
                .collect::<Vec<_>>();
            *filters.borrow_mut() = rows.clone();
            let model = Rc::new(VecModel::from(rows.clone()));
            let bars = Rc::new(VecModel::from(
                rows.iter()
                    .rev()
                    .take(48)
                    .rev()
                    .map(activity_bar)
                    .collect::<Vec<_>>(),
            ));
            app.set_requests(ModelRc::from(model.clone()));
            app.set_request_bars(ModelRc::from(bars.clone()));
            app.set_request_stats(activity_stats(&rows));
            app.set_request_sync(1);
            app.set_request_filter(0);
            app.set_route_running(true);

            app.global::<Theme>().set_animations_enabled(true);
            frame(600);
            headless::render(window);
            let before = polls.get();
            app.set_active_page(3);
            headless::render(window);
            assert_eq!(
                polls.get(),
                before + 1,
                "entering the page refreshes at once"
            );
            // Entry motion starts only after the page has painted.
            play(
                window,
                output,
                &format!("activity-enter-{suffix}"),
                &[64, 144, 256, 416, 640, 1008, 1600],
            )?;
            frame(2_000);
            assert!(polls.get() >= before + 2, "the page polls while it is open");
            assert!(app.get_request_following());
            snapshot_now(window, output, &format!("activity-settled-{suffix}"))?;

            // A live arrival lands at the bottom and the view follows it.
            for index in 36..38 {
                app.set_request_sync(app.get_request_sync() + 1);
                let row = activity_row(index, app.get_request_sync());
                filters.borrow_mut().push(row.clone());
                bars.push(activity_bar(&row));
                model.push(row);
            }
            app.set_request_stats(activity_stats(&filters.borrow()));
            play(
                window,
                output,
                &format!("activity-arrival-{suffix}"),
                &[0, 80, 176, 320, 528, 896, 1408],
            )?;
            assert!(app.get_request_following(), "arrivals keep following");

            // Scrolling back pauses following; arrivals then wait behind the pill.
            window.dispatch_event(WindowEvent::PointerScrolled {
                position: slint::LogicalPosition::new(width as f32 * 0.6, height as f32 * 0.6),
                delta_x: 0.0,
                delta_y: 420.0,
            });
            frame(32);
            assert!(
                !app.get_request_following(),
                "scrolling back pauses following"
            );
            for index in 38..41 {
                app.set_request_sync(app.get_request_sync() + 1);
                let row = activity_row(index, app.get_request_sync());
                bars.push(activity_bar(&row));
                model.push(row);
            }
            app.set_request_unseen(3);
            play(
                window,
                output,
                &format!("activity-paused-{suffix}"),
                &[0, 128, 400],
            )?;
            let pill = (
                180.0 + (width as f32 - 180.0) / 2.0 - 8.0,
                height as f32 - 16.0 - 14.0 - 14.0 - 17.0 - 14.0,
            );
            headless::click(window, pill.0, pill.1);
            frame(16);
            assert!(app.get_request_following(), "the pill resumes following");
            assert_eq!(app.get_request_unseen(), 0);
            play(
                window,
                output,
                &format!("activity-resumed-{suffix}"),
                &[160, 896],
            )?;

            // Expand the newest entry; it stays in view while following.
            let last_row = height as f32 - 16.0 - 14.0 - 14.0 - 34.0;
            headless::click(window, width as f32 * 0.6, last_row);
            play(
                window,
                output,
                &format!("activity-details-{suffix}"),
                &[0, 128, 256, 608],
            )?;

            // Hovering a pulse bar names its request.
            let mut position = slint::LogicalPosition::new(width as f32 - 90.0, 250.0);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            frame(200);
            snapshot_now(window, output, &format!("activity-pulse-hover-{suffix}"))?;
            position.x = 10.0;
            window.dispatch_event(WindowEvent::PointerMoved { position });

            app.invoke_filter_requests(2);
            app.set_request_filter(2);
            frame(700);
            snapshot_now(window, output, &format!("activity-filter-{suffix}"))?;
            app.set_request_filter(0);

            app.set_requests(ModelRc::new(VecModel::from(Vec::new())));
            app.set_request_bars(ModelRc::new(VecModel::from(Vec::new())));
            app.set_request_stats(activity_stats(&[]));
            app.set_active_page(0);
            play(window, output, "unused", &[])?;
            app.set_active_page(3);
            play(
                window,
                output,
                &format!("activity-empty-{suffix}"),
                &[400, 1200],
            )?;

            // Leaving the page stops polling.
            app.set_active_page(0);
            headless::render(window);
            let left = polls.get();
            frame(6_000);
            assert_eq!(polls.get(), left, "polling stops once the page is left");
            app.global::<Theme>().set_animations_enabled(false);
        }
    }
    println!("Synthetic activity timeline frames: {}", output.display());
    Ok(())
}
