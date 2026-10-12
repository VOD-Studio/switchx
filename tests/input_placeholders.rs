use slint::{ComponentHandle, PhysicalSize, Rgb8Pixel, SharedPixelBuffer};
use switchx::headless;

slint::slint! {
    import { QuietLineEdit } from "../ui/components.slint";
    import { CodeEditor } from "../ui/code-editor.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";

    export component PlaceholderWindow inherits Window {
        preferred-width: 1000px;
        preferred-height: 680px;
        background: Theme.background;
        in-out property <bool> show-placeholders: true;
        in-out property <string> value;
        in-out property <string> code;
        public function focus-line() { field.focus(); }
        public function focus-code() { editor.focus(); }
        public function focus-away() { next.focus(); }

        Text { x: 32px; y: 28px; text: "输入框占位提示"; color: Theme.primary-text; font-size: 18px; }
        field := QuietLineEdit {
            x: 32px; y: 80px; width: root.width - 64px;
            text <=> root.value;
            placeholder-text: root.show-placeholders ? "例如 DeepSeek" : "";
        }
        QuietLineEdit {
            x: 32px; y: 144px; width: 260px;
            icon: @image-url("../assets/ui/search.svg");
            placeholder-text: root.show-placeholders ? "搜索图标名称，例如 OpenAI、DeepSeek" : "";
        }
        QuietLineEdit {
            x: 32px; y: 208px; width: 260px;
            input-type: InputType.password;
            placeholder-text: root.show-placeholders ? "留空保留已保存的 Key" : "";
        }
        editor := CodeEditor {
            x: 32px; y: 272px; width: root.width - 64px; height: root.height - 304px;
            text <=> root.code;
            show-line-numbers: true;
            placeholder-text: root.show-placeholders ? "# 在这里写下你的 Codex 配置\n\nweb_search = \"live\"\n\n[features]" : "";
        }
        next := FocusScope { x: 0px; y: 0px; width: 1px; height: 1px; }
    }
}

fn luminance(pixel: Rgb8Pixel) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(pixel.r) + 0.7152 * linear(pixel.g) + 0.0722 * linear(pixel.b)
}

fn assert_readable_hint(
    blank: &SharedPixelBuffer<Rgb8Pixel>,
    hint: &SharedPixelBuffer<Rgb8Pixel>,
    region: (usize, usize, usize, usize),
) {
    let (left, top, width, height) = region;
    let stride = hint.width() as usize;
    let mut changed = 0;
    let mut contrast = 1.0_f64;
    for y in top..top + height {
        for x in left..left + width {
            let before = blank.as_slice()[y * stride + x];
            let after = hint.as_slice()[y * stride + x];
            if before != after {
                changed += 1;
                let a = luminance(before);
                let b = luminance(after);
                contrast = contrast.max((a.max(b) + 0.05) / (a.min(b) + 0.05));
            }
        }
    }
    assert!(
        changed > 60,
        "the hint must render at the input's text origin in {region:?}"
    );
    assert!(
        contrast >= 4.5,
        "placeholder contrast is only {contrast:.2}:1"
    );
}

#[test]
fn placeholders_are_readable_at_the_text_origin_and_hide_when_typing() {
    let window = headless::install_with_manual_time();
    let app = PlaceholderWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.show().unwrap();

    for (width, height) in [(1000, 680), (1200, 820)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_dark(dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for state in ["idle", "line-focus", "code-focus"] {
                app.invoke_focus_away();
                if state == "line-focus" {
                    app.invoke_focus_line();
                } else if state == "code-focus" {
                    app.invoke_focus_code();
                }
                app.set_show_placeholders(false);
                let blank = headless::render(&window);
                app.set_show_placeholders(true);
                let hint = headless::render(&window);
                headless::save_snapshot(
                    "SWITCHX_PLACEHOLDER_SNAPSHOTS",
                    &format!("{state}-{suffix}"),
                    &hint,
                );
                for region in [
                    (44, 81, 240, 34),
                    (70, 145, 230, 34),
                    (44, 209, 240, 34),
                    // The first code line must start beside line 1, at the top.
                    (78, 280, 280, 24),
                    (78, 304, 280, 56),
                ] {
                    assert_readable_hint(&blank, &hint, region);
                }
            }

            app.invoke_focus_line();
            headless::key(&window, "DeepSeek");
            assert_eq!(app.get_value(), "DeepSeek");
            app.invoke_focus_code();
            headless::key(&window, "web_search = \"live\"");
            assert_eq!(app.get_code(), "web_search = \"live\"");
            let typed = headless::render(&window);
            app.set_show_placeholders(false);
            let without_hints = headless::render(&window);
            // Only the other empty fields should change after disabling hints.
            for (top, height) in [(81, 34), (280, height as usize - 312)] {
                for y in top..top + height {
                    let start = y * width as usize + 44;
                    let end = y * width as usize + width as usize - 44;
                    assert_eq!(
                        &typed.as_slice()[start..end],
                        &without_hints.as_slice()[start..end],
                        "typing must replace the hint without overlapping it"
                    );
                }
            }
            app.set_value("".into());
            app.set_code("".into());
        }
    }
}
