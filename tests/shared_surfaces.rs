use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};

slint::slint! {
    import { QuietButton, SurfaceCard } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component SurfaceWindow inherits Window {
        width: 400px;
        height: 240px;
        background: Theme.background;
        in-out property <bool> enabled: true;
        in-out property <bool> selected: false;
        in-out property <color> card-fill: Theme.panel;
        in-out property <color> card-border: Theme.border;
        in-out property <length> card-border-width: 2px;
        out property <int> clicks;
        out property <string> action-label: action.accessible-label;
        out property <bool> action-enabled: action.accessible-enabled;
        public function focus-action() { action.focus(); }
        public function accessible-click() { action.accessible-action-default(); }

        SurfaceCard {
            x: 20px; y: 20px; width: 360px; height: 200px;
            background: root.card-fill;
            border-color: root.card-border;
            border-width: root.card-border-width;
            border-radius: 12px;
            Rectangle { x: 16px; y: 150px; width: 40px; height: 20px; background: Theme.brand; }
            action := QuietButton {
                x: 24px; y: 30px; width: 160px; height: 36px;
                text: "保存连接";
                enabled: root.enabled;
                selected: root.selected;
                clicked => { root.clicks += 1; }
            }
            QuietButton {
                x: 210px; y: 30px; width: 100px; height: 36px;
                text: "主要操作";
                primary: true;
            }
            QuietButton {
                x: 24px; y: 90px; width: 36px;
                text: "添加连接";
                icon-only: true;
                enabled: root.enabled;
            }
            QuietButton {
                x: 90px; y: 90px; width: 120px;
                text: "导航操作";
                navigation: true;
                selected: true;
            }
        }
    }
}

thread_local! { static PREVIEW_TIME: Cell<Duration> = const { Cell::new(Duration::ZERO) }; }

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
    PREVIEW_TIME.with(|time| time.set(time.get() + Duration::from_millis(milliseconds)));
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) -> SharedPixelBuffer<Rgb8Pixel> {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_SURFACE_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
    pixels
}

fn key(window: &MinimalSoftwareWindow, key: Key) {
    window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
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
}

#[test]
fn shared_surfaces_preserve_styling_motion_and_accessible_button_actions() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = SurfaceWindow::new().unwrap();
    app.window().set_size(PhysicalSize::new(400, 240));
    app.show().unwrap();

    for dark in [false, true] {
        let theme = app.global::<Theme>();
        theme.set_dark(dark);
        theme.set_animations_enabled(true);
        app.set_card_fill(theme.get_panel());
        app.set_card_border(theme.get_border());
        app.set_card_border_width(2.0);
        app.set_enabled(true);
        app.set_selected(false);
        advance(250);
        let suffix = if dark { "dark" } else { "light" };
        draw(&window, &format!("enabled-{suffix}"));
        assert_eq!(app.get_action_label(), "保存连接");
        assert!(app.get_action_enabled());

        let clicks = app.get_clicks();
        click(&window, 80.0, 68.0);
        key(&window, Key::Space);
        key(&window, Key::Return);
        app.invoke_accessible_click();
        assert_eq!(app.get_clicks(), clicks + 4);
        advance(200);
        draw(&window, &format!("focused-{suffix}"));

        app.set_enabled(false);
        draw(&window, &format!("disabled-start-{suffix}"));
        advance(40);
        let middle = draw(&window, &format!("disabled-mid-{suffix}"));
        advance(200);
        let disabled = draw(&window, &format!("disabled-end-{suffix}"));
        assert_ne!(middle.as_bytes(), disabled.as_bytes());
        assert!(!app.get_action_enabled());
        click(&window, 80.0, 68.0);
        key(&window, Key::Space);
        app.invoke_accessible_click();
        assert_eq!(app.get_clicks(), clicks + 4);

        app.set_enabled(true);
        app.invoke_focus_action();
        key(&window, Key::Return);
        assert_eq!(app.get_clicks(), clicks + 5);
        app.set_selected(true);
        advance(250);
        let start = draw(&window, &format!("card-start-{suffix}"));
        app.set_card_fill(slint::Color::from_rgb_u8(60, 100, 140));
        app.set_card_border(slint::Color::from_rgb_u8(180, 100, 40));
        draw(&window, &format!("card-transition-start-{suffix}"));
        advance(60);
        let middle = draw(&window, &format!("card-mid-{suffix}"));
        advance(250);
        let end = draw(&window, &format!("card-end-{suffix}"));
        assert_ne!(start.as_bytes(), middle.as_bytes());
        assert_ne!(middle.as_bytes(), end.as_bytes());
        assert_eq!(
            end.as_slice()[150 * 400 + 200],
            Rgb8Pixel::new(60, 100, 140)
        );

        for reduced_motion in [false, true] {
            theme.set_animations_enabled(reduced_motion);
            theme.set_system_reduced_motion(reduced_motion);
            app.set_card_fill(slint::Color::from_rgb_u8(140, 100, 60));
            app.set_enabled(false);
            let immediate = draw(&window, &format!("reduced-start-{suffix}-{reduced_motion}"));
            assert_eq!(
                immediate.as_slice()[150 * 400 + 200],
                Rgb8Pixel::new(140, 100, 60)
            );
            advance(250);
            let settled = draw(&window, &format!("reduced-end-{suffix}-{reduced_motion}"));
            assert_eq!(immediate.as_bytes(), settled.as_bytes());
            app.set_card_fill(slint::Color::from_rgb_u8(60, 100, 140));
            draw(&window, "reset-card");
        }
        theme.set_system_reduced_motion(false);
    }
}
