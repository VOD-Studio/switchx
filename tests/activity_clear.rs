use std::{cell::Cell, rc::Rc};

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel, platform::Key};
use switchx::{
    headless,
    ui::{AppWindow, RequestBar, RequestRow, RequestStats, Theme},
};

fn frame(window: &slint::platform::software_renderer::MinimalSoftwareWindow, ms: u64) {
    for _ in 0..ms.div_ceil(16) {
        headless::advance(16);
        headless::render(window);
    }
}

fn center(app: &AppWindow, label: &str) -> (f32, f32) {
    let control = ElementHandle::find_by_accessible_label(app, label)
        .next()
        .unwrap_or_else(|| panic!("missing visible control: {label}"));
    let position = control.absolute_position();
    let size = control.size();
    assert!(size.width > 0.0 && size.height > 0.0);
    (
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    )
}

fn seed(app: &AppWindow) {
    app.set_requests(ModelRc::new(VecModel::from(
        (0..8)
            .map(|index| RequestRow {
                id: format!("synthetic-{index}").into(),
                model: "gpt-6-luna".into(),
                provider: "合成连接".into(),
                upstream: "gpt-6-luna".into(),
                clock: format!("09:47:{index:02}").into(),
                duration: "2.9 s".into(),
                duration_ms: 2_900,
                level: 0.7,
                status: "完成".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    app.set_request_stats(RequestStats {
        total: 8,
        completed: 8,
        success_rate: 100.0,
        p50_ms: 2_900.0,
        p95_ms: 2_900.0,
        first_event_ms: 210.0,
        ..Default::default()
    });
    app.set_request_bars(ModelRc::new(VecModel::from(
        (0..8)
            .map(|index| RequestBar {
                id: format!("bar-{index}").into(),
                level: 0.35 + index as f32 * 0.06,
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    app.set_request_cleared(false);
}

#[test]
fn clear_modal_cancels_retries_blocks_duplicates_and_finishes_with_motion_preferences() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_loading(false);
    app.set_status_text("合成资料 · 活动页预览".into());
    app.set_route_running(true);
    app.show().unwrap();
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let weak = app.as_weak();
    app.on_clear_requests(move || {
        counter.set(counter.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_request_clear_error("".into());
        app.set_request_clearing(true);
    });
    let finishes = Rc::new(Cell::new(0));
    let counter = finishes.clone();
    let weak = app.as_weak();
    app.on_finish_clear_requests(move || {
        counter.set(counter.get() + 1);
        let app = weak.upgrade().unwrap();
        app.set_requests(ModelRc::new(VecModel::default()));
        app.set_request_bars(ModelRc::new(VecModel::default()));
        app.set_request_stats(RequestStats {
            success_rate: -1.0,
            p50_ms: -1.0,
            p95_ms: -1.0,
            first_event_ms: -1.0,
            ..Default::default()
        });
        app.set_request_erasing(false);
        app.set_request_clearing(false);
        app.set_request_cleared(true);
    });

    for (width, height, dark, motion, reduced) in [
        (1200, 820, true, true, false),
        (1200, 820, false, true, false),
        (1000, 680, true, true, false),
        (1000, 680, false, true, false),
        (1000, 680, true, false, false),
        (1000, 680, false, true, true),
    ] {
        app.window().set_size(PhysicalSize::new(width, height));
        let theme = app.global::<Theme>();
        theme.set_dark(dark);
        theme.set_animations_enabled(false);
        theme.set_system_reduced_motion(reduced);
        app.set_active_page(0);
        headless::render(&window);
        seed(&app);
        app.set_active_page(3);
        frame(&window, 1000);
        theme.set_animations_enabled(motion);
        let suffix = format!(
            "{}-{width}x{height}-{motion}-{reduced}",
            if dark { "dark" } else { "light" }
        );
        let snapshot = |name: &str| {
            let pixels = headless::render(&window);
            headless::save_snapshot(
                "SWITCHX_ACTIVITY_CLEAR_SNAPSHOTS",
                &format!("{name}-{suffix}"),
                &pixels,
            );
        };
        snapshot("ready");
        let (x, y) = center(&app, "清空记录");
        assert!(x < width as f32 - 12.0 && y < height as f32);
        window.dispatch_event(slint::platform::WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(x, y),
        });
        frame(&window, 240);
        snapshot("hover");
        headless::click(&window, x, y);
        frame(&window, 600);
        assert!(app.get_request_clear_open());
        snapshot("confirm");
        let before = calls.get();
        headless::key(&window, Key::Escape);
        frame(&window, 400);
        assert!(!app.get_request_clear_open());
        assert_eq!(calls.get(), before);
        assert_eq!(app.get_requests().row_count(), 8);

        headless::click(&window, x, y);
        frame(&window, 600);
        let (keep_x, keep_y) = center(&app, "保留记录");
        headless::click(&window, keep_x, keep_y);
        frame(&window, 400);
        assert!(!app.get_request_clear_open());
        assert_eq!(calls.get(), before);
        headless::click(&window, x, y);
        frame(&window, 600);
        let (confirm_x, confirm_y) = center(&app, "确认清空");
        headless::click(&window, confirm_x, confirm_y);
        frame(&window, 240);
        snapshot("loading");
        headless::click(&window, confirm_x, confirm_y);
        headless::key(&window, Key::Escape);
        assert_eq!(calls.get(), before + 1);
        assert!(app.get_request_clear_open(), "busy modal stays open");
        assert_eq!(app.get_requests().row_count(), 8);

        app.set_request_clearing(false);
        app.set_request_clear_error("清空失败：合成数据库错误。记录已保留，请稍后重试。".into());
        frame(&window, 400);
        snapshot("error");
        let (confirm_x, confirm_y) = center(&app, "确认清空");
        headless::click(&window, confirm_x, confirm_y);
        assert_eq!(calls.get(), before + 2);
        app.set_request_clear_open(false);
        app.set_request_erasing(true);
        headless::render(&window);
        let finished_before = finishes.get();
        if motion && !reduced {
            frame(&window, 208);
            snapshot("erasing");
            assert_eq!(
                app.get_requests().row_count(),
                8,
                "rows stay mounted for exit motion"
            );
            assert_eq!(finishes.get(), finished_before);
        }
        frame(&window, 600);
        snapshot("empty-enter");
        assert_eq!(finishes.get(), finished_before + 1);
        assert!(app.get_request_cleared());
        assert_eq!(app.get_requests().row_count(), 0);
        frame(&window, 800);
        snapshot("empty");
        assert_eq!(app.get_request_stats().total, 0);
        headless::click(&window, x, y);
        assert!(
            !app.get_request_clear_open(),
            "empty history disables clear"
        );
        assert_eq!(calls.get(), before + 2);
    }

    // The window owns completion, even if the page is removed mid-animation.
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(true);
    seed(&app);
    app.set_request_erasing(true);
    headless::render(&window);
    app.set_active_page(0);
    let before = finishes.get();
    frame(&window, 700);
    assert_eq!(finishes.get(), before + 1);
    assert_eq!(app.get_requests().row_count(), 0);
}
