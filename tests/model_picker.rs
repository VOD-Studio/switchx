use slint::platform::{Key, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::headless;

slint::slint! {
    import { ModelPicker } from "../ui/editors/model-picker.slint";
    import { DropdownViewport } from "../ui/components.slint";
    import { Theme } from "../ui/tokens.slint";
    export { Theme } from "../ui/tokens.slint";
    export { ModelChoices } from "../ui/editors/model-picker.slint";

    export component PickerWindow inherits Window {
        width: 360px;
        height: 420px;
        background: Theme.background;
        property <Point> viewport: { x: root.width, y: root.height };
        init => { DropdownViewport.size = root.viewport; }
        changed viewport => { DropdownViewport.size = root.viewport; }
        in-out property <[string]> models;
        in-out property <string> value: "custom-model";
        in-out property <bool> enabled: true;
        out property <bool> expanded: picker.expanded;
        out property <bool> next-focused: next.has-focus;
        in-out property <length> picker-y: 64px;
        public function focus-picker() { picker.focus(); }
        public function select-all() { picker.select-all(); }
        picker := ModelPicker {
            x: 40px;
            y: root.picker-y;
            width: 260px;
            text <=> root.value;
            model: root.models;
            enabled: root.enabled;
            accessible-label: "模型 ID";
        }
        next := FocusScope { x: 310px; y: 64px; width: 40px; height: 36px; }
    }
}

fn setup() -> (std::rc::Rc<MinimalSoftwareWindow>, PickerWindow) {
    let window = headless::install_with_manual_time();
    let app = PickerWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.global::<ModelChoices>()
        .on_filter(switchx::model_picker::filter);
    app.window().set_size(PhysicalSize::new(360, 420));
    app.show().unwrap();
    headless::render(&window);
    (window, app)
}

fn models(app: &PickerWindow, values: &[&str]) {
    app.set_models(ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| (*value).into())
            .collect::<Vec<_>>(),
    )));
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_MODEL_PICKER_SNAPSHOTS", name, &pixels);
}

#[test]
fn manual_model_ids_survive_discovery_search_and_empty_results() {
    let (window, app) = setup();
    app.invoke_focus_picker();
    app.invoke_select_all();
    headless::key(&window, "private-model");
    assert_eq!(app.get_value(), "private-model");
    headless::key(&window, Key::DownArrow);
    assert!(!app.get_expanded(), "No list leaves a normal input");

    models(&app, &["gpt-6-luna", "gpt-6-sol", "DeepSeek-Chat"]);
    assert_eq!(
        app.get_value(),
        "private-model",
        "Discovery keeps custom IDs"
    );
    for dark in [false, true] {
        app.global::<Theme>().set_dark(dark);
        let theme = if dark { "dark" } else { "light" };
        draw(&window, &format!("picker-{theme}-closed"));
        headless::click(&window, 280.0, 82.0);
        draw(&window, &format!("picker-{theme}-open"));
        assert!(app.get_expanded());
        headless::key(&window, "  DEEPSEEK  ");
        draw(&window, &format!("picker-{theme}-search"));
        assert_eq!(app.get_value(), "  DEEPSEEK  ");
        headless::key(&window, Key::DownArrow);
        headless::key(&window, Key::Return);
        assert_eq!(
            app.get_value(),
            "DeepSeek-Chat",
            "Search ignores case and outer spaces"
        );
        assert!(!app.get_expanded());

        headless::click(&window, 280.0, 82.0);
        draw(&window, "picker-reopened");
        headless::key(&window, "unlisted-model");
        draw(&window, &format!("picker-{theme}-empty"));
        headless::key(&window, Key::DownArrow);
        headless::key(&window, Key::Return);
        assert_eq!(
            app.get_value(),
            "unlisted-model",
            "No match keeps manual input"
        );
        assert!(!app.get_expanded());

        app.invoke_focus_picker();
        app.invoke_select_all();
        headless::key(&window, "another-custom-id");
        headless::key(&window, Key::DownArrow);
        draw(&window, "picker-escape-open");
        headless::key(&window, "typed-id");
        headless::key(&window, Key::Escape);
        assert_eq!(app.get_value(), "typed-id");
        assert!(!app.get_expanded());
    }
}

#[test]
fn mouse_selection_focus_and_disabling_preserve_the_editor_value() {
    let (window, app) = setup();
    models(&app, &["gpt-6-luna", "gpt-6-sol", "DeepSeek-Chat"]);
    headless::click(&window, 280.0, 82.0);
    draw(&window, "picker-mouse-open");
    headless::click(&window, 110.0, 170.0);
    assert_eq!(app.get_value(), "gpt-6-sol");
    assert!(!app.get_expanded());

    // Opening always shows the whole directory, including after a filtered search.
    headless::click(&window, 280.0, 82.0);
    draw(&window, "picker-full-directory");
    headless::key(&window, Key::Tab);
    assert!(!app.get_expanded());
    assert!(app.get_next_focused(), "Tab leaves the picker");

    app.invoke_focus_picker();
    headless::key(&window, Key::DownArrow);
    draw(&window, "picker-disable-open");
    app.set_enabled(false);
    draw(&window, "picker-disabled");
    assert!(!app.get_expanded());
    headless::click(&window, 110.0, 130.0);
    assert_eq!(app.get_value(), "gpt-6-sol");
    app.set_enabled(true);
    app.invoke_focus_picker();
    headless::key(&window, Key::DownArrow);
    draw(&window, "picker-before-clear");
    models(&app, &[]);
    draw(&window, "picker-cleared");
    assert!(!app.get_expanded());
    assert_eq!(
        app.get_value(),
        "gpt-6-sol",
        "Clearing suggestions keeps the input"
    );
}

#[test]
fn long_directories_scroll_and_open_above_near_the_window_edge() {
    let (window, app) = setup();
    app.window().set_size(PhysicalSize::new(360, 420));
    models(
        &app,
        &[
            "model-0", "model-1", "model-2", "model-3", "model-4", "model-5", "model-6", "model-7",
            "model-8",
        ],
    );
    app.set_picker_y(360.0);
    draw(&window, "picker-near-bottom");
    headless::click(&window, 280.0, 378.0);
    draw(&window, "picker-above");
    for _ in 0..9 {
        headless::key(&window, Key::DownArrow);
    }
    draw(&window, "picker-scrolled");
    headless::key(&window, Key::Return);
    assert_eq!(app.get_value(), "model-8");
    assert!(!app.get_expanded());
    headless::click(&window, 280.0, 378.0);
    draw(&window, "picker-above-reopened");
    headless::click(&window, 330.0, 20.0);
    assert!(!app.get_expanded(), "Outside clicks dismiss suggestions");
}
