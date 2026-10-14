use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, PhysicalSize};
use switchx::{
    headless,
    ui::{AppWindow, Theme},
};

fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..45 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        headless::render(window);
    }
}

#[test]
fn shared_snippet_card_keeps_its_outline_along_the_editor_in_both_themes() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    switchx::code_highlight::connect(&app);
    app.set_active_page(1);
    let snippet = "web_search = \"live\"\n\n[features]\nshell_tool = true\n";
    app.set_common_config_saved(snippet.into());
    app.set_common_config_draft(snippet.into());
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.show().unwrap();

    for dark in [false, true] {
        app.global::<Theme>().set_dark(dark);
        app.set_common_config_editor_open(true);
        settle(&window);
        let editor = ElementHandle::find_by_accessible_label(&app, "Codex 通用配置片段")
            .find(|element| element.accessible_role() == Some(AccessibleRole::TextInput))
            .expect("the shared snippet editor");
        let position = editor.absolute_position();
        let size = editor.size();
        let pixels = headless::render(&window);
        let name = if dark {
            "outline-dark"
        } else {
            "outline-light"
        };
        headless::save_snapshot("SWITCHX_COMMON_CONFIG_SNAPSHOTS", name, &pixels);

        // The editor spans the whole card, so its outer columns are the card's
        // left and right edges. Its own background must not paint over them.
        let width = pixels.width() as usize;
        let left = position.x.round() as usize;
        let right = (position.x + size.width).round() as usize - 1;
        for fraction in [0.25, 0.5, 0.75] {
            let row = (position.y + size.height * fraction).round() as usize * width;
            for (edge, inside) in [(left, left + 6), (right, right - 6)] {
                assert_ne!(
                    pixels.as_slice()[row + edge],
                    pixels.as_slice()[row + inside],
                    "the card outline is hidden at column {edge} (dark: {dark})"
                );
            }
        }
        app.set_common_config_editor_open(false);
        settle(&window);
    }
}
