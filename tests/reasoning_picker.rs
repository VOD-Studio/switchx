use i_slint_backend_testing::ElementHandle;
use slint::platform::Key;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, SharedPixelBuffer, VecModel};
use switchx::headless;
use switchx::ui::{AppWindow, ConnectionModelRow, ReasoningChoices, Theme};

fn advance(ms: u64) {
    headless::advance(ms);
    slint::platform::update_timers_and_animations();
}
fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    headless::click(window, x, y);
    slint::platform::update_timers_and_animations();
}
fn element(app: &AppWindow, label: &str) -> ElementHandle {
    let mut matches = ElementHandle::find_by_accessible_label(app, label);
    let element = matches
        .next()
        .unwrap_or_else(|| panic!("visible control missing: {label}"));
    assert!(matches.next().is_none(), "ambiguous control label: {label}");
    element
}
fn click_control(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str) {
    let control = element(app, label);
    let position = control.absolute_position();
    let size = control.size();
    click(
        window,
        position.x + size.width / 2.0,
        position.y + size.height / 2.0,
    );
}
fn text(window: &MinimalSoftwareWindow, text: slint::SharedString) {
    headless::key(window, text);
    slint::platform::update_timers_and_animations();
}
fn key(window: &MinimalSoftwareWindow, key: Key) {
    text(window, key.into());
}
fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<slint::Rgb8Pixel> {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_REASONING_SNAPSHOTS", name, &pixels);
    pixels
}
fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..20 {
        advance(16);
        draw(window, "settling");
    }
}
fn row(app: &AppWindow) -> ConnectionModelRow {
    app.get_connection_models().row_data(0).unwrap()
}
fn reset(app: &AppWindow) {
    app.set_connection_models(ModelRc::new(VecModel::from(vec![ConnectionModelRow {
        public_id: "sx-synthetic".into(),
        display_name: "合成模型/ChatGPT 订阅".into(),
        upstream_model: "synthetic-coder".into(),
        context_window: "272000".into(),
        reasoning_levels: "low, medium, high, xhigh, max".into(),
        default_reasoning: "high".into(),
        ..Default::default()
    }])));
    app.set_connection_models_dirty(false);
}

#[test]
fn picker_supports_search_multiselect_defaults_keyboard_motion_and_disabled_states() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    switchx::reasoning_picker::connect(&app);
    app.set_loading(false);
    app.set_active_page(1);
    app.set_connection_models_open(true);
    app.set_connection_models_name("ChatGPT 订阅".into());
    app.set_connection_models_subscription(true);
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        weak.unwrap().set_connection_models_dirty(true);
    });
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.show().unwrap();
    let choices = app.global::<ReasoningChoices>();
    assert_eq!(
        choices.invoke_label("high，low medium high xhigh max".into()),
        "low → max"
    );
    assert_eq!(
        choices
            .invoke_choices("low, xhigh".into(), "HIGH".into())
            .row_count(),
        2
    );
    assert_eq!(
        choices
            .invoke_choices("".into(), "均衡".into())
            .row_data(0)
            .unwrap()
            .effort,
        "medium"
    );

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.invoke_set_appearance(dark);
            reset(&app);
            draw(&window, "closed");
            click_control(&window, &app, "思考等级 1");
            draw(&window, "open");
            click_control(&window, &app, "minimal，最少");
            assert!(
                row(&app).reasoning_levels.starts_with("minimal, low"),
                "click must add a level"
            );
            assert_eq!(row(&app).default_reasoning, "high");
            assert!(app.get_connection_models_dirty());
            // The search keeps focus after each toggle and accepts uppercase queries.
            text(&window, "HIGH".into());
            draw(&window, "search");
            click_control(&window, &app, "high，深入");
            assert_eq!(
                row(&app).reasoning_levels,
                "minimal, low, medium, xhigh, max"
            );
            assert!(
                row(&app).default_reasoning.is_empty(),
                "removing the default must clear it"
            );
            key(&window, Key::Escape);
            draw(&window, "closed-after-search");
            click_control(&window, &app, "思考等级 1");
            draw(&window, "reopened");
            key(&window, Key::DownArrow);
            key(&window, Key::Return);
            assert_eq!(
                row(&app).reasoning_levels,
                "low, medium, xhigh, max",
                "arrow/enter must toggle the highlighted level"
            );
            // A nonmatching search cannot toggle an unrelated level.
            text(&window, "no-such-level".into());
            key(&window, Key::Return);
            assert_eq!(row(&app).reasoning_levels, "low, medium, xhigh, max");
            key(&window, Key::Escape);
            draw(&window, "no-results-closed");
            reset(&app);
            draw(&window, "default-reset");
            click_control(&window, &app, "思考等级 1");
            draw(&window, "default-open");
            click_control(&window, &app, "默认思考等级");
            draw(&window, "default-menu");
            key(&window, Key::UpArrow);
            assert_eq!(
                row(&app).default_reasoning,
                "medium",
                "the default selector must write back to the draft"
            );
            key(&window, Key::Escape);
            draw(&window, "default-selected");
            click(&window, 300.0, 600.0);
            draw(&window, "dismissed");
            click_control(&window, &app, "思考等级 1");
            draw(&window, "clear-open");
            click_control(&window, &app, "清空选择");
            assert!(row(&app).reasoning_levels.is_empty());
            assert!(row(&app).default_reasoning.is_empty());
            click(&window, 300.0, 600.0);
            draw(&window, "clear-closed");
            reset(&app);
            app.set_config_managed(true);
            draw(&window, "managed");
            click_control(&window, &app, "思考等级 1");
            key(&window, Key::Return);
            assert!(!app.get_connection_models_dirty());
            app.set_config_managed(false);
            draw(&window, "managed-reset");
        }
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    reset(&app);
    draw(&window, "motion-closed");
    app.global::<Theme>().set_animations_enabled(true);
    click_control(&window, &app, "思考等级 1");
    draw(&window, "motion-start");
    advance(48);
    let entering = draw(&window, "motion-entering");
    settle(&window);
    let opened = draw(&window, "motion-open");
    assert_ne!(
        entering.as_bytes(),
        opened.as_bytes(),
        "opening must animate"
    );
    click(&window, 300.0, 600.0);
    advance(48);
    let exiting = draw(&window, "motion-exiting");
    settle(&window);
    let closed = draw(&window, "motion-closed");
    assert_ne!(
        exiting.as_bytes(),
        closed.as_bytes(),
        "closing must animate"
    );
    click_control(&window, &app, "思考等级 1");
    advance(48);
    draw(&window, "reversal-start");
    click(&window, 300.0, 600.0);
    advance(32);
    draw(&window, "reversal-exiting");
    key(&window, Key::Return);
    settle(&window);
    click_control(&window, &app, "minimal，最少");
    assert!(
        row(&app).reasoning_levels.starts_with("minimal, low"),
        "rapid keyboard reopen must cancel the stale close timer"
    );
    click(&window, 300.0, 600.0);
    settle(&window);
    click_control(&window, &app, "思考等级 1");
    settle(&window);
    click_control(&window, &app, "minimal，最少");
    assert_eq!(row(&app).reasoning_levels, "low, medium, high, xhigh, max");
    app.set_busy(true);
    settle(&window);
    assert_eq!(
        element(&app, "思考等级 1").accessible_expanded(),
        Some(false)
    );
    click_control(&window, &app, "思考等级 1");
    key(&window, Key::Return);
    assert_eq!(row(&app).reasoning_levels, "low, medium, high, xhigh, max");
    app.set_busy(false);
    app.global::<Theme>().set_system_reduced_motion(true);
    draw(&window, "reduced-closed");
    click_control(&window, &app, "思考等级 1");
    draw(&window, "reduced-open");
    click_control(&window, &app, "minimal，最少");
    assert_eq!(
        row(&app).reasoning_levels,
        "minimal, low, medium, high, xhigh, max"
    );
    key(&window, Key::Escape);
    let reduced = draw(&window, "reduced-close");
    advance(48);
    assert_eq!(
        reduced.as_bytes(),
        draw(&window, "reduced-settled").as_bytes()
    );
}
