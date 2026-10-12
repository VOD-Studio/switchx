use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use switchx::headless;

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

fn draw(
    window: &MinimalSoftwareWindow,
    elapsed_ms: u64,
    name: &str,
) -> SharedPixelBuffer<Rgb8Pixel> {
    headless::advance(elapsed_ms);
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_FEEDBACK_SNAPSHOTS", name, &pixels);
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
    let window = headless::install_with_manual_time();
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
