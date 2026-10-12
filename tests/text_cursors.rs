use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel};
use switchx::{code_highlight, headless};

slint::slint! {
    import { QuietLineEdit } from "../ui/components.slint";
    import { CodeEditor } from "../ui/code-editor.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";
    export { SyntaxHighlighting, CodeSpan } from "../ui/code-editor.slint";

    export component CursorWindow inherits Window {
        preferred-width: 1000px;
        preferred-height: 680px;
        background: Theme.background;
        in-out property <string> value;
        in-out property <string> code;
        public function focus-line() { field.focus(); }
        public function focus-password() { secret.focus(); }
        public function focus-code() { editor.focus(); }
        public function focus-away() { next.focus(); }
        public function place-caret(target: int, offset: int) {
            if target == 0 { field.set-selection-offsets(offset, offset); }
            else if target == 1 { secret.set-selection-offsets(offset, offset); }
            else { editor.accessible-action-set-selection-offsets(offset, offset); }
        }

        Text { x: 32px; y: 28px; text: "文本光标"; color: Theme.primary-text; font-size: 18px; }
        field := QuietLineEdit {
            x: 32px; y: 80px; width: root.width - 64px;
            text <=> root.value;
        }
        secret := QuietLineEdit {
            x: 32px; y: 144px; width: root.width - 64px;
            text: "synthetic-password";
            input-type: InputType.password;
        }
        editor := CodeEditor {
            x: 32px; y: 208px; width: root.width - 64px; height: root.height - 240px;
            text <=> root.code;
            show-line-numbers: true;
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

// Compare rendered frames inside the field, excluding its focus border. This
// catches native caret color and clipping rather than just checking Slint values.
fn assert_readable_caret(
    blurred: &SharedPixelBuffer<Rgb8Pixel>,
    focused: &SharedPixelBuffer<Rgb8Pixel>,
    region: (usize, usize, usize, usize),
    name: &str,
) {
    let (left, top, width, height) = region;
    let stride = focused.width() as usize;
    let readable = |x: usize, y: usize| {
        let before = blurred.as_slice()[y * stride + x];
        let after = focused.as_slice()[y * stride + x];
        let a = luminance(before);
        let b = luminance(after);
        before != after && (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 4.5
    };
    let tallest = (left..left + width - 1)
        .map(|x| {
            (top..top + height)
                .filter(|&y| readable(x, y) && readable(x + 1, y))
                .count()
        })
        .max()
        .unwrap_or(0);
    assert!(
        tallest >= 10,
        "{name}: expected a visible 2px caret with at least 4.5:1 contrast in {region:?}; only {tallest} rows were readable"
    );
}

#[test]
fn carets_are_readable_in_both_themes_and_stay_visible_when_scrolling() {
    let window = headless::install_with_manual_time();
    let app = CursorWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    let highlighting = app.global::<SyntaxHighlighting>();
    highlighting.on_line_count(|text| text.as_str().split('\n').count() as i32);
    highlighting.on_spans(|source, language| {
        ModelRc::new(VecModel::from(
            code_highlight::spans(source.as_str(), language.as_str())
                .into_iter()
                .map(|span| CodeSpan {
                    text: span.text.into(),
                    prefix: span.prefix.into(),
                    line_text: span.line_text.into(),
                    line: span.line,
                    kind: match span.kind {
                        code_highlight::TokenKind::Plain => 0,
                        code_highlight::TokenKind::Key => 1,
                        code_highlight::TokenKind::String => 2,
                        code_highlight::TokenKind::Number => 3,
                        code_highlight::TokenKind::Literal => 4,
                        code_highlight::TokenKind::Comment => 5,
                        code_highlight::TokenKind::Section => 6,
                    },
                })
                .collect::<Vec<_>>(),
        ))
    });
    app.show().unwrap();

    for (width, height) in [(1000, 680), (1200, 820)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_dark(dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for long in [false, true] {
                app.set_value(
                    if long {
                        "Podlink ".repeat(200)
                    } else {
                        "Podlink".into()
                    }
                    .into(),
                );
                app.set_code(
                    if long {
                        format!("model = \"{}\"", "synthetic model ".repeat(200))
                    } else {
                        "model_reasoning_effort = \"max\"".into()
                    }
                    .into(),
                );
                for target in ["line", "password", "code"] {
                    let focus = || match target {
                        "line" => app.invoke_focus_line(),
                        "password" => app.invoke_focus_password(),
                        _ => app.invoke_focus_code(),
                    };
                    focus();
                    let (target_index, offset) = match target {
                        "line" => (0, app.get_value().len()),
                        "password" => (1, "synthetic-password".len()),
                        _ => (2, app.get_code().len()),
                    };
                    app.invoke_place_caret(target_index, offset as i32);
                    headless::render(&window);
                    app.invoke_focus_away();
                    let blurred = headless::render(&window);
                    focus();
                    let focused = headless::render(&window);
                    let region = match target {
                        "line" => (44, 81, width as usize - 88, 34),
                        "password" => (44, 145, width as usize - 88, 34),
                        _ => (78, 216, width as usize - 132, 28),
                    };
                    let name = format!(
                        "{target}-{}-{suffix}",
                        if long { "scrolled" } else { "end" }
                    );
                    headless::save_snapshot("SWITCHX_CURSOR_SNAPSHOTS", &name, &focused);
                    headless::save_snapshot(
                        "SWITCHX_CURSOR_SNAPSHOTS",
                        &format!("{name}-blurred"),
                        &blurred,
                    );
                    assert_readable_caret(&blurred, &focused, region, &name);

                    // Moving the caret still uses the native editing behavior.
                    headless::key(&window, slint::platform::Key::Backspace);
                    if target == "line" {
                        assert!(app.get_value().as_str().ends_with(if long {
                            "Podlink"
                        } else {
                            "Podlin"
                        }));
                    } else if target == "code" {
                        assert!(!app.get_code().as_str().ends_with('"'));
                    }
                }
            }
        }
    }
}
