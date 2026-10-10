use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use std::{cell::Cell, io::Write, rc::Rc, time::Duration};

slint::slint! {
    import { FeedbackBanner } from "../ui/feedback-banner.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component FeedbackWindow inherits Window {
        width: 620px;
        height: 160px;
        background: Theme.background;
        in-out property <bool> shown;
        in-out property <bool> loading: true;
        in-out property <string> message: "正在获取模型列表…";
        if root.shown: FeedbackBanner {
            x: 20px; y: 20px; width: parent.width - 40px;
            loading: root.loading;
            message: root.message;
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

fn draw(
    window: &MinimalSoftwareWindow,
    elapsed_ms: u64,
    name: &str,
) -> SharedPixelBuffer<Rgb8Pixel> {
    PREVIEW_TIME.with(|time| time.set(time.get() + Duration::from_millis(elapsed_ms)));
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_FEEDBACK_SNAPSHOTS") {
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
    for _ in 0..80 {
        draw(window, 16, "settling");
    }
}

fn assert_moving(window: &MinimalSoftwareWindow, name: &str) {
    let first = draw(window, 0, &format!("{name}-a"));
    // The sweep briefly leaves the clipped card at each end of its cycle.
    let moving = (0..4).any(|index| {
        let next = draw(window, 137, &format!("{name}-{index}"));
        first.as_bytes() != next.as_bytes()
    });
    assert!(moving, "{name} must keep moving");
}

fn assert_still(window: &MinimalSoftwareWindow, name: &str) {
    let first = draw(window, 0, &format!("{name}-a"));
    let second = draw(window, 137, &format!("{name}-b"));
    assert!(
        first.as_bytes() == second.as_bytes(),
        "{name} must stay still"
    );
}

#[test]
fn dynamically_created_loading_feedback_runs_until_finished_and_respects_motion_preferences() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
    let app = FeedbackWindow::new().unwrap();
    app.show().unwrap();

    for width in [620, 420] {
        app.window().set_size(PhysicalSize::new(width, 160));
        for dark in [false, true] {
            app.set_shown(false);
            draw(&window, 0, "hidden");
            let theme = app.global::<Theme>();
            theme.set_dark(dark);
            theme.set_animations_enabled(true);
            theme.set_system_reduced_motion(false);
            app.set_loading(true);
            app.set_shown(true);
            draw(&window, 0, "created-loading");
            settle(&window);
            let suffix = format!("{}-{width}", if dark { "dark" } else { "light" });
            assert_moving(&window, &format!("first-loading-{suffix}"));
            draw(&window, 2400, "later");
            assert_moving(&window, &format!("multiple-cycles-{suffix}"));

            theme.set_animations_enabled(false);
            draw(&window, 0, "motion-disabled");
            assert_still(&window, &format!("disabled-{suffix}"));
            theme.set_system_reduced_motion(true);
            theme.set_animations_enabled(true);
            draw(&window, 0, "reduced-motion");
            assert_still(&window, &format!("reduced-{suffix}"));
            theme.set_system_reduced_motion(false);
            draw(&window, 0, "motion-restored");
            assert_moving(&window, &format!("restored-{suffix}"));

            app.set_loading(false);
            draw(&window, 0, "completed");
            settle(&window);
            assert_still(&window, &format!("completed-{suffix}"));
            app.set_loading(true);
            draw(&window, 0, "retry");
            settle(&window);
            assert_moving(&window, &format!("retry-{suffix}"));

            app.set_message("".into());
            app.set_loading(false);
            draw(&window, 0, "dismissed");
            settle(&window);
            assert_still(&window, &format!("dismissed-{suffix}"));
            app.set_message("正在获取模型列表…".into());
        }
    }
}
