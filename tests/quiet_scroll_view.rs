use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel,
};
use std::{io::Write, rc::Rc};

slint::slint! {
    import { QuietScrollView, ListView } from "../ui/scroll-view.slint";
    import { CodeEditor } from "../ui/code-editor.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component ScrollWindow inherits Window {
        width: 360px;
        height: 700px;
        background: Theme.panel;
        in-out property <length> body-height: 420px;
        in-out property <bool> enabled: true;
        in-out property <ScrollBarPolicy> vertical-policy: as-needed;
        in-out property <ScrollBarPolicy> horizontal-policy: as-needed;
        in-out property <[int]> rows;
        in-out property <string> editor-text <=> editor.text;
        out property <length> visible-width: vertical.visible-width;
        out property <length> visible-height: both.visible-height;
        out property <length> scroll-y: vertical.content-y;
        out property <length> scroll-x: both.content-x;
        out property <length> list-y: list.content-y;
        out property <length> list-width: list.visible-width;
        out property <int> clicks;
        out property <int> instantiated;
        public function focus-editor() { editor.focus(); }
        public function hide-bars() { root.vertical-policy = always-off; root.horizontal-policy = always-off; }
        public function show-vertical() { root.vertical-policy = always-on; }
        vertical := QuietScrollView {
            x: 20px;
            y: 20px;
            width: 320px;
            height: 140px;
            enabled: root.enabled;
            vertical-scrollbar-policy: root.vertical-policy;
            horizontal-scrollbar-policy: always-off;
            content-width: self.visible-width;
            content-height: root.body-height;
            Rectangle {
                width: vertical.visible-width;
                height: root.body-height;
                background: #ba3b91;
                TouchArea { clicked => { root.clicks += 1; } }
            }
        }
        both := QuietScrollView {
            x: 20px;
            y: 200px;
            width: 320px;
            height: 140px;
            horizontal-scrollbar-policy: root.horizontal-policy;
            content-width: 640px;
            content-height: 420px;
            Rectangle { width: 640px; height: 420px; background: #ba3b91; }
        }
        list := ListView {
            x: 20px;
            y: 370px;
            width: 320px;
            height: 140px;
            horizontal-scrollbar-policy: always-off;
            for row in root.rows: Rectangle {
                init => { root.instantiated += 1; }
                height: 40px;
                background: #ba3b91;
                Text { text: row; }
            }
        }
        editor := CodeEditor {
            x: 20px;
            y: 550px;
            width: 320px;
            height: 120px;
        }
    }
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_SCROLL_SNAPSHOTS") {
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

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn scrollbars_reserve_space_outside_content() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = ScrollWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.set_rows(ModelRc::new(VecModel::from((0..1000).collect::<Vec<_>>())));
    app.set_editor_text(
        (0..40)
            .map(|row| format!("line {row:02}: {}\n", "synthetic content ".repeat(6)))
            .collect::<String>()
            .into(),
    );
    app.window().set_size(PhysicalSize::new(360, 700));
    app.show().unwrap();
    draw(&window, "initial");
    assert!(
        app.get_visible_width() <= 308.0,
        "vertical scrollbar overlays content: viewport {}px inside a 320px scroll area",
        app.get_visible_width()
    );
    assert!(
        app.get_visible_height() <= 128.0,
        "horizontal scrollbar overlays content: viewport {}px inside a 140px scroll area",
        app.get_visible_height()
    );

    assert!(app.get_list_width() <= 308.0);
    assert!(
        app.get_instantiated() > 0 && app.get_instantiated() < 20,
        "list rows must stay virtualized"
    );
    for dark in [false, true] {
        app.global::<Theme>().set_dark(dark);
        // Hover expands the thumb inside its gutter, never over the content.
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(333.0, 40.0),
        });
        let pixels = draw(&window, if dark { "hover-dark" } else { "hover-light" });
        assert_eq!(pixel(&pixels, 322, 40), [0xba, 0x3b, 0x91]);
        assert_ne!(pixel(&pixels, 328, 40), [0xba, 0x3b, 0x91]);
        assert_eq!(pixel(&pixels, 150, 322), [0xba, 0x3b, 0x91]);
        assert_ne!(pixel(&pixels, 150, 328), [0xba, 0x3b, 0x91]);
    }

    click(&window, 322.0, 90.0);
    assert_eq!(
        app.get_clicks(),
        1,
        "content's right edge must remain clickable"
    );
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(150.0, 80.0),
        delta_x: 0.0,
        delta_y: -55.0,
    });
    assert!(app.get_scroll_y() < 0.0);

    // Clicking the track reaches the end; dragging the thumb returns to the start.
    click(&window, 333.0, 155.0);
    assert_eq!(app.get_scroll_y(), -280.0);
    window.dispatch_event(WindowEvent::PointerPressed {
        position: LogicalPosition::new(333.0, 145.0),
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(333.0, 30.0),
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(333.0, 30.0),
        button: PointerEventButton::Left,
    });
    assert_eq!(app.get_scroll_y(), 0.0);

    // Virtualized rows may change the estimated content height during a drag.
    window.dispatch_event(WindowEvent::PointerPressed {
        position: LogicalPosition::new(333.0, 40.0),
        button: PointerEventButton::Left,
    });
    app.set_body_height(840.0);
    draw(&window, "height-changed-during-drag");
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(333.0, 45.0),
    });
    assert_eq!(
        app.get_scroll_y(),
        0.0,
        "changed content height must not make the thumb jump"
    );
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(333.0, 65.0),
    });
    assert!(app.get_scroll_y() < 0.0);
    window.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(333.0, 65.0),
        button: PointerEventButton::Left,
    });
    app.set_body_height(420.0);
    click(&window, 320.0, 333.0);
    assert!(app.get_scroll_x() < -300.0);

    let width = app.get_visible_width();
    app.set_body_height(60.0);
    draw(&window, "short-content");
    assert_eq!(
        app.get_visible_width(),
        width,
        "overflow must not shift the form width"
    );
    app.invoke_hide_bars();
    draw(&window, "hidden-bars");
    assert_eq!(app.get_visible_width(), width + 14.0);
    assert!(app.get_visible_height() >= 136.0);
    app.invoke_show_vertical();
    app.set_body_height(420.0);
    app.set_enabled(false);
    draw(&window, "disabled");
    click(&window, 333.0, 155.0);
    assert_eq!(
        app.get_scroll_y(),
        0.0,
        "disabled scrollbar must ignore dragging and track clicks"
    );
    app.set_enabled(true);
    click(&window, 333.0, 155.0);
    app.set_body_height(60.0);
    draw(&window, "shrunk-content");
    assert_eq!(
        app.get_scroll_y(),
        0.0,
        "shrinking content must clamp its offset"
    );

    let instantiated = app.get_instantiated();
    window.dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(150.0, 440.0),
        delta_x: 0.0,
        delta_y: -4000.0,
    });
    draw(&window, "scrolled-list");
    assert!(app.get_list_y() < -3000.0);
    assert!(app.get_instantiated() - instantiated < 20);

    app.invoke_focus_editor();
    for _ in 0..40 {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::DownArrow.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::DownArrow.into(),
        });
    }
    window.dispatch_event(WindowEvent::KeyPressed {
        text: "END_MARKER".into(),
    });
    window.dispatch_event(WindowEvent::KeyReleased {
        text: "END_MARKER".into(),
    });
    assert!(app.get_editor_text().ends_with("END_MARKER"));
    draw(&window, "editor-end");
}
