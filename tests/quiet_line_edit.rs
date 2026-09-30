use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::{cell::Cell, cell::RefCell, io::Write, rc::Rc, time::Duration};

slint::slint! {
    import { QuietLineEdit } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component InputWindow inherits Window {
        width: 400px;
        height: 390px;
        background: Theme.background;
        in-out property <string> value;
        in-out property <string> password: "abc123";
        in-out property <string> number;
        in-out property <bool> enabled: true;
        in-out property <bool> read-only;
        out property <bool> input-focused: field.has-focus;
        out property <bool> next-focused: next.has-focus;
        out property <string> protected-value: secret.accessible-value;
        out property <int> edits;
        out property <string> accepted-value;
        public function focus-input() { field.focus(); }
        public function focus-next() { next.focus(); }
        public function select-input() { field.select-all(); }
        public function copy-input() { field.copy(); }
        public function cut-input() { field.cut(); }
        public function paste-input() { field.paste(); }
        public function undo-input() { field.undo(); }
        public function redo-input() { field.redo(); }

        Text { x: 32px; y: 28px; text: "共用输入框"; color: Theme.primary-text; font-size: 18px; }
        field := QuietLineEdit {
            x: 32px; y: 80px; width: 300px; height: 36px;
            text <=> root.value;
            enabled: root.enabled;
            read-only: root.read-only;
            icon: @image-url("../assets/ui/search.svg");
            placeholder-text: "搜索连接";
            accessible-label: "搜索连接";
            edited(value) => { root.edits += 1; }
            accepted(value) => { root.accepted-value = value; }
        }
        secret := QuietLineEdit {
            x: 32px; y: 144px; width: 300px; height: 36px;
            text <=> root.password;
            input-type: InputType.password;
            accessible-label: "合成密码";
        }
        numeric := QuietLineEdit {
            x: 32px; y: 208px; width: 110px; height: 36px;
            text <=> root.number;
            input-type: InputType.number;
            placeholder-text: "端口";
            accessible-label: "数值";
        }
        QuietLineEdit {
            x: 32px; y: 272px; width: 300px; height: 36px;
            text: "只读文本，可以选择复制";
            read-only: true;
        }
        QuietLineEdit {
            x: 32px; y: 336px; width: 300px; height: 36px;
            placeholder-text: "当前不可编辑";
            enabled: false;
        }
        next := FocusScope { x: 360px; y: 80px; width: 10px; height: 36px; }
    }
}

thread_local! {
    static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static CLIPBOARD: RefCell<String> = const { RefCell::new(String::new()) };
}

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn duration_since_start(&self) -> Duration {
        PREVIEW_TIME.with(Cell::get)
    }
    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if clipboard == Clipboard::DefaultClipboard {
            CLIPBOARD.with(|value| *value.borrow_mut() = text.into());
        }
    }
    fn clipboard_text(&self, _: Clipboard) -> Option<String> {
        Some(CLIPBOARD.with(|value| value.borrow().clone()))
    }
}

fn advance(milliseconds: u64) {
    PREVIEW_TIME.with(|time| time.set(time.get() + Duration::from_millis(milliseconds)));
    slint::platform::update_timers_and_animations();
}

fn move_pointer(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
}

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    move_pointer(window, x, y);
    let position = LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn key(window: &MinimalSoftwareWindow, text: slint::SharedString) {
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_INPUT_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
    pixels
}

fn pixel(pixels: &SharedPixelBuffer<Rgb8Pixel>, x: usize, y: usize) -> [u8; 3] {
    let value = pixels.as_slice()[y * pixels.width() as usize + x];
    [value.r, value.g, value.b]
}

#[test]
fn themed_input_preserves_editing_and_respects_motion_preferences() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = InputWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.window().set_size(PhysicalSize::new(400, 390));
    app.show().unwrap();
    app.invoke_focus_next();
    draw(&window, "initial");

    app.set_value("外部更新".into());
    assert_eq!(app.get_edits(), 0);
    click(&window, 100.0, 98.0);
    assert!(app.get_input_focused());
    app.invoke_select_input();
    key(&window, "连接".into());
    assert_eq!(app.get_value(), "连接");
    key(&window, Key::Return.into());
    assert_eq!(app.get_accepted_value(), "连接");
    app.invoke_select_input();
    app.invoke_copy_input();
    assert_eq!(CLIPBOARD.with(|value| value.borrow().clone()), "连接");
    app.invoke_cut_input();
    assert_eq!(app.get_value(), "");
    app.invoke_undo_input();
    assert_eq!(app.get_value(), "连接");
    app.invoke_redo_input();
    assert_eq!(app.get_value(), "");
    app.invoke_paste_input();
    assert_eq!(app.get_value(), "连接");
    assert!(app.get_edits() > 0);

    app.set_read_only(true);
    key(&window, "不可写入".into());
    assert_eq!(app.get_value(), "连接");
    app.set_read_only(false);
    app.set_enabled(false);
    click(&window, 100.0, 98.0);
    key(&window, "不可写入".into());
    assert_eq!(app.get_value(), "连接");
    app.set_enabled(true);
    app.invoke_focus_input();
    key(&window, Key::Tab.into());
    assert!(!app.get_input_focused());
    app.invoke_focus_next();
    assert!(app.get_next_focused());

    click(&window, 60.0, 226.0);
    key(&window, "abc".into());
    assert_eq!(app.get_number(), "");
    key(&window, "18731".into());
    assert_eq!(app.get_number(), "18731");
    app.invoke_focus_next();
    assert_eq!(app.get_protected_value(), "");
    let masked = draw(&window, "password-first");
    app.set_password("key789".into());
    let masked_again = draw(&window, "password-second");
    assert_eq!(masked.as_bytes(), masked_again.as_bytes());

    app.set_value("".into());
    for dark in [false, true] {
        let theme = app.global::<Theme>();
        theme.set_dark(dark);
        theme.set_system_reduced_motion(false);
        theme.set_animations_enabled(true);
        app.invoke_focus_next();
        move_pointer(&window, 380.0, 380.0);
        draw(&window, "reset");
        advance(200);
        let suffix = if dark { "dark" } else { "light" };
        let idle = draw(&window, &format!("idle-{suffix}"));
        move_pointer(&window, 100.0, 98.0);
        let hover_start = draw(&window, "hover-start");
        advance(60);
        let hover_mid = draw(&window, "hover-mid");
        advance(100);
        let hover_end = draw(&window, &format!("hover-{suffix}"));
        assert_eq!(pixel(&idle, 331, 98), pixel(&hover_start, 331, 98));
        assert_ne!(pixel(&idle, 331, 98), pixel(&hover_mid, 331, 98));
        assert_ne!(pixel(&hover_mid, 331, 98), pixel(&hover_end, 331, 98));

        // Clicking the padded icon region must focus the underlying input too.
        click(&window, 45.0, 98.0);
        assert!(app.get_input_focused());
        let focus_start = draw(&window, "focus-start");
        advance(60);
        let focus_mid = draw(&window, "focus-mid");
        advance(100);
        let focus_end = draw(&window, &format!("focus-{suffix}"));
        assert_ne!(pixel(&focus_start, 29, 98), pixel(&focus_mid, 29, 98));
        assert_ne!(pixel(&focus_mid, 29, 98), pixel(&focus_end, 29, 98));

        app.invoke_focus_next();
        draw(&window, "blur-start");
        advance(200);
        theme.set_system_reduced_motion(true);
        app.invoke_focus_input();
        let reduced = draw(&window, &format!("reduced-motion-{suffix}"));
        assert_eq!(pixel(&focus_end, 29, 98), pixel(&reduced, 29, 98));
        theme.set_system_reduced_motion(false);
        theme.set_animations_enabled(false);
        app.invoke_focus_next();
        draw(&window, "animations-off");
        app.invoke_focus_input();
        let off = draw(&window, &format!("no-animation-{suffix}"));
        assert_eq!(pixel(&focus_end, 29, 98), pixel(&off, 29, 98));
    }
}
