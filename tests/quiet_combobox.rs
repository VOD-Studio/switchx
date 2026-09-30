use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel,
};
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};

thread_local! { static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) }; }

slint::slint! {
    import { QuietComboBox, DropdownViewport } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component DropdownWindow inherits Window {
        preferred-width: 360px;
        preferred-height: 420px;
        background: Theme.background;
        property <Point> dropdown-viewport: { x: root.width, y: root.height };
        init => { DropdownViewport.size = root.dropdown-viewport; }
        changed dropdown-viewport => { DropdownViewport.size = root.dropdown-viewport; }
        in-out property <[string]> options: ["ChatGPT 订阅", "DeepSeek"];
        in-out property <int> choice;
        in-out property <string> value: "DeepSeek";
        in-out property <bool> enabled: true;
        out property <bool> expanded: chooser.accessible-expanded;
        out property <int> selections;
        out property <string> selected-value;
        out property <bool> next-focused: next.has-focus;
        public function focus-choice() { chooser.focus(); }
        chooser := QuietComboBox {
            x: 40px;
            y: 64px;
            width: 155px;
            height: 36px;
            model: root.options;
            current-index <=> root.choice;
            current-value <=> root.value;
            enabled: root.enabled;
            accessible-label: "选择连接";
            selected(value) => { root.selections += 1; root.selected-value = value; }
        }
        next := FocusScope { x: 280px; y: 64px; width: 40px; height: 36px; }
    }
}

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }

    fn duration_since_start(&self) -> Duration {
        PREVIEW_TIME.with(Cell::get)
    }
}

fn advance(milliseconds: u64) {
    PREVIEW_TIME.with(|clock| clock.set(clock.get() + Duration::from_millis(milliseconds)));
    slint::platform::update_timers_and_animations();
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
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

fn key(window: &MinimalSoftwareWindow, key: Key) {
    window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_DROPDOWN_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
    pixels
}

// Measure only the menu region, excluding the field's focus and arrow animation.
fn menu_difference(
    frame: &SharedPixelBuffer<Rgb8Pixel>,
    blank: &SharedPixelBuffer<Rgb8Pixel>,
) -> u64 {
    (110..196)
        .flat_map(|y| {
            let start = (y * frame.width() as usize + 40) * 3;
            let end = start + 200 * 3;
            frame.as_bytes()[start..end]
                .iter()
                .zip(&blank.as_bytes()[start..end])
                .map(|(left, right)| u64::from(left.abs_diff(*right)))
        })
        .sum()
}

#[test]
fn themed_dropdown_preserves_bindings_and_interaction() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = DropdownWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.window().set_size(PhysicalSize::new(360, 420));
    app.show().unwrap();
    draw(&window, "initial");
    assert_eq!(app.get_choice(), 1);
    assert_eq!(app.get_value(), "DeepSeek");
    app.set_value("ChatGPT 订阅".into());
    slint::platform::update_timers_and_animations();
    assert_eq!(app.get_choice(), 0);
    app.set_choice(1);
    slint::platform::update_timers_and_animations();
    assert_eq!(app.get_value(), "DeepSeek");
    assert_eq!(app.get_selections(), 0);

    for dark in [false, true] {
        app.global::<Theme>().set_dark(dark);
        app.set_choice(0);
        draw(&window, if dark { "closed-dark" } else { "closed-light" });
        click(&window, 120.0, 82.0);
        assert!(app.get_expanded());
        draw(&window, if dark { "open-dark" } else { "open-light" });
        let selections = app.get_selections();
        key(&window, Key::DownArrow);
        assert_eq!(app.get_choice(), 1);
        assert_eq!(app.get_selected_value(), "DeepSeek");
        assert_eq!(app.get_selections(), selections + 1);
        key(&window, Key::Escape);
        assert!(!app.get_expanded());
        key(&window, Key::Return);
        assert!(app.get_expanded());
        click(&window, 120.0, 130.0);
        assert_eq!(app.get_choice(), 0);
        assert!(!app.get_expanded());
        key(&window, Key::Space);
        assert!(app.get_expanded());
        click(&window, 320.0, 350.0);
        assert!(!app.get_expanded());
    }

    app.invoke_focus_choice();
    key(&window, Key::Return);
    key(&window, Key::Tab);
    assert!(!app.get_expanded());
    assert!(app.get_next_focused());

    app.set_options(ModelRc::new(VecModel::from(
        (0..12)
            .map(|index| format!("连接 {index}").into())
            .collect::<Vec<_>>(),
    )));
    app.invoke_focus_choice();
    key(&window, Key::Return);
    key(&window, Key::End);
    assert_eq!(app.get_choice(), 11);
    draw(&window, "long-list-dark");
    key(&window, Key::Home);
    assert_eq!(app.get_choice(), 0);
    draw(&window, "long-list-home-dark");
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(120.0, 150.0),
        delta_x: 0.0,
        delta_y: -100.0,
    });
    assert_eq!(app.get_choice(), 0);
    key(&window, Key::Escape);

    let selections = app.get_selections();
    app.set_enabled(false);
    draw(&window, "disabled-dark");
    click(&window, 120.0, 82.0);
    key(&window, Key::DownArrow);
    assert!(!app.get_expanded());
    assert_eq!(app.get_selections(), selections);
    app.set_enabled(true);
    app.invoke_focus_choice();
    key(&window, Key::Return);
    app.set_options(ModelRc::new(VecModel::from(Vec::new())));
    slint::platform::update_timers_and_animations();
    assert!(!app.get_expanded());
    assert_eq!(app.get_value(), "");
    click(&window, 120.0, 82.0);
    assert!(!app.get_expanded());
}

#[test]
fn dropdown_animates_both_directions_and_reverses_without_stale_closes() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = DropdownWindow::new().unwrap();
    app.window().set_size(PhysicalSize::new(360, 420));
    app.show().unwrap();
    app.invoke_focus_choice();

    for dark in [false, true] {
        app.global::<Theme>().set_dark(dark);
        app.set_choice(0);
        advance(200);
        let theme = if dark { "dark" } else { "light" };
        let blank = draw(&window, &format!("motion-{theme}-closed"));
        key(&window, Key::Return);
        assert!(app.get_expanded());
        let start = draw(&window, &format!("motion-{theme}-enter-start"));
        assert_eq!(menu_difference(&start, &blank), 0);
        advance(16);
        draw(&window, &format!("motion-{theme}-enter-ready"));
        advance(40);
        let entering = draw(&window, &format!("motion-{theme}-enter-mid"));
        advance(150);
        let open = draw(&window, &format!("motion-{theme}-open"));
        let full = menu_difference(&open, &blank);
        assert!(full > 0);
        let middle = menu_difference(&entering, &blank);
        assert!(middle > 0 && middle < full, "enter: {middle}/{full}");

        key(&window, Key::Escape);
        assert!(!app.get_expanded());
        let exiting = draw(&window, &format!("motion-{theme}-exit-start"));
        assert_eq!(menu_difference(&exiting, &blank), full);
        advance(40);
        let exiting = draw(&window, &format!("motion-{theme}-exit-mid"));
        let middle = menu_difference(&exiting, &blank);
        assert!(middle > 0 && middle < full, "exit: {middle}/{full}");
        let selections = app.get_selections();
        click(&window, 120.0, 130.0);
        key(&window, Key::DownArrow);
        assert_eq!(app.get_selections(), selections);

        // Reopen before the exit timer fires; it must not close this new opening.
        if dark {
            click(&window, 120.0, 82.0);
        } else {
            key(&window, Key::Return);
        }
        assert!(app.get_expanded());
        advance(200);
        let reopened = draw(&window, &format!("motion-{theme}-reopened"));
        assert_eq!(menu_difference(&reopened, &blank), full);
        assert!(app.get_expanded());

        // The outside-click surface must follow the host window after a resize.
        app.window().set_size(PhysicalSize::new(500, 500));
        draw(&window, "resized-open");
        click(&window, 450.0, 450.0);
        assert!(!app.get_expanded());
        advance(200);
        app.window().set_size(PhysicalSize::new(360, 420));
        slint::platform::update_timers_and_animations();
        key(&window, Key::Return);
        advance(16);
        draw(&window, "resize-enter-ready");
        advance(200);
        draw(&window, "resize-open");

        click(&window, 320.0, 350.0);
        assert!(!app.get_expanded());
        advance(40);
        let outside_exit = draw(&window, &format!("motion-{theme}-outside-exit-mid"));
        let middle = menu_difference(&outside_exit, &blank);
        assert!(middle > 0 && middle < full);
        advance(120);
        let closed = draw(&window, &format!("motion-{theme}-exit-end"));
        assert_eq!(menu_difference(&closed, &blank), 0);

        // Closing before the entry delay expires must not produce a late flash.
        key(&window, Key::Return);
        key(&window, Key::Escape);
        advance(200);
        assert!(!app.get_expanded());
        assert_eq!(menu_difference(&draw(&window, "quick-close"), &blank), 0);

        key(&window, Key::Return);
        key(&window, Key::Tab);
        assert!(!app.get_expanded());
        assert!(app.get_next_focused());
        advance(200);
        assert!(app.get_next_focused());
        app.invoke_focus_choice();

        for reduced in [false, true] {
            app.global::<Theme>().set_animations_enabled(reduced);
            app.global::<Theme>().set_system_reduced_motion(reduced);
            key(&window, Key::Return);
            let open = draw(&window, "motion-disabled-open");
            assert!(app.get_expanded());
            assert_eq!(menu_difference(&open, &blank), full);
            key(&window, Key::Escape);
            assert!(!app.get_expanded());
            let closed = draw(&window, "motion-disabled-closed");
            assert_eq!(menu_difference(&closed, &blank), 0);
        }
        app.global::<Theme>().set_system_reduced_motion(false);
        app.global::<Theme>().set_animations_enabled(true);
    }
}
