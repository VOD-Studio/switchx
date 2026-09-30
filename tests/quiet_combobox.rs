use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};
use std::{io::Write, rc::Rc};

slint::slint! {
    import { QuietComboBox } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component DropdownWindow inherits Window {
        preferred-width: 360px;
        preferred-height: 420px;
        background: Theme.background;
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

fn draw(window: &MinimalSoftwareWindow, name: &str) {
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
