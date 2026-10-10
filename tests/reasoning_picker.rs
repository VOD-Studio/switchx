use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, SharedPixelBuffer, VecModel};
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};
use switchx::ui::{AppWindow, ConnectionModelRow, ReasoningChoices, Theme};

thread_local! { static TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) }; }
struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        TIME.with(Cell::get)
    }
}
fn advance(ms: u64) {
    TIME.with(|time| time.set(time.get() + Duration::from_millis(ms)));
    slint::platform::update_timers_and_animations();
}
fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    slint::platform::update_timers_and_animations();
}
fn text(window: &MinimalSoftwareWindow, text: slint::SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    slint::platform::update_timers_and_animations();
}
fn key(window: &MinimalSoftwareWindow, key: Key) {
    text(window, key.into());
}
fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<slint::Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_REASONING_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
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
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
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
            let trigger = width as f32 - 140.0;
            let panel_x = width as f32 - 280.0;
            click(&window, trigger, 338.0);
            draw(&window, "open");
            click(&window, panel_x, 160.0);
            assert!(
                row(&app).reasoning_levels.starts_with("minimal, low"),
                "click must add a level"
            );
            assert_eq!(row(&app).default_reasoning, "high");
            assert!(app.get_connection_models_dirty());
            // The search keeps focus after each toggle and accepts uppercase queries.
            text(&window, "HIGH".into());
            draw(&window, "search");
            click(&window, panel_x, 120.0);
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
            click(&window, trigger, 338.0);
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
            click(&window, trigger, 338.0);
            draw(&window, "default-open");
            click(&window, panel_x, 475.0);
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
            click(&window, trigger, 338.0);
            draw(&window, "clear-open");
            click(&window, width as f32 - 110.0, 447.0);
            assert!(row(&app).reasoning_levels.is_empty());
            assert!(row(&app).default_reasoning.is_empty());
            click(&window, 300.0, 600.0);
            draw(&window, "clear-closed");
            reset(&app);
            app.set_config_managed(true);
            draw(&window, "managed");
            click(&window, trigger, 338.0);
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
    click(&window, 1060.0, 338.0);
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
    click(&window, 1060.0, 338.0);
    advance(48);
    draw(&window, "reversal-start");
    click(&window, 300.0, 600.0);
    advance(32);
    draw(&window, "reversal-exiting");
    key(&window, Key::Return);
    settle(&window);
    click(&window, 920.0, 160.0);
    assert!(
        row(&app).reasoning_levels.starts_with("minimal, low"),
        "rapid keyboard reopen must cancel the stale close timer"
    );
    click(&window, 300.0, 600.0);
    settle(&window);
    click(&window, 1060.0, 338.0);
    settle(&window);
    click(&window, 920.0, 160.0);
    assert_eq!(row(&app).reasoning_levels, "low, medium, high, xhigh, max");
    app.set_busy(true);
    settle(&window);
    click(&window, 920.0, 160.0);
    assert_eq!(row(&app).reasoning_levels, "low, medium, high, xhigh, max");
    app.set_busy(false);
    app.global::<Theme>().set_system_reduced_motion(true);
    draw(&window, "reduced-closed");
    click(&window, 1060.0, 338.0);
    draw(&window, "reduced-open");
    click(&window, 920.0, 160.0);
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
