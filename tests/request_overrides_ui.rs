use std::{cell::Cell, rc::Rc};

use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless,
    ui::{AppWindow, BatchModelRow, BatchSummary, RequestRules, Theme},
};

fn control(app: &AppWindow, label: &str, role: AccessibleRole) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| element.accessible_role() == Some(role))
        .unwrap_or_else(|| panic!("missing control: {label}"))
}
fn draw(window: &MinimalSoftwareWindow, name: &str) {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_REQUEST_OVERRIDES_SNAPSHOTS", name, &pixels);
}
fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..45 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        headless::render(window);
    }
}
fn click(window: &MinimalSoftwareWindow, app: &AppWindow, label: &str, role: AccessibleRole) {
    let element = control(app, label, role);
    let position = element.absolute_position();
    let size = element.size();
    headless::click(
        window,
        position.x + size.width / 2.,
        position.y + size.height / 2.,
    );
    settle(window);
}
fn open(app: &AppWindow) {
    app.set_active_page(0);
    app.set_model_provider_name("示例连接".into());
    app.set_model_original_id("sx-synthetic".into());
    app.set_model_upstream_id("synthetic-coder".into());
    app.set_model_display_name("synthetic-coder / 示例连接".into());
    app.set_model_context_window("128000".into());
    app.set_model_batch_mode(false);
    app.set_model_editor_open(true);
}

#[test]
fn request_editor_validates_preserves_tabs_clears_on_close_and_animates_in_both_themes() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    switchx::request_overrides::connect(&app);
    switchx::code_highlight::connect(&app);
    switchx::reasoning_picker::connect(&app);
    app.set_loading(false);
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_model(move || saved.set(saved.get() + 1));
    app.show().unwrap();
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.invoke_set_appearance(dark);
            open(&app);
            settle(&window);
            headless::scroll(&window, width as f32 - 650., height as f32 / 2., -4000.);
            settle(&window);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            draw(&window, &format!("request-collapsed-{suffix}"));
            click(&window, &app, "请求定制", AccessibleRole::Button);
            assert!(app.get_model_request_open());
            app.set_model_user_agent("claude-cli/2.1.161 (external, cli)".into());
            app.set_model_header_overrides(
                "{\n  \"x-provider\": \"switchx\",\n  \"x-project\": \"my-workspace\"\n}".into(),
            );
            app.set_model_body_overrides(
                "{\n  \"temperature\": 0.2,\n  \"reasoning\": { \"effort\": \"high\" }\n}".into(),
            );
            control(&app, "Header 覆盖", AccessibleRole::Tab).invoke_accessible_default_action();
            settle(&window);
            draw(&window, &format!("request-headers-{suffix}"));
            click(&window, &app, "Body 覆盖", AccessibleRole::Tab);
            assert_eq!(
                control(&app, "Body 覆盖 JSON", AccessibleRole::TextInput)
                    .accessible_value()
                    .unwrap(),
                app.get_model_body_overrides()
            );
            draw(&window, &format!("request-body-{suffix}"));
            control(&app, "Body 覆盖 JSON", AccessibleRole::TextInput)
                .set_accessible_value("{\"temperature\":");
            settle(&window);
            assert_eq!(
                control(&app, "保存映射", AccessibleRole::Button).accessible_enabled(),
                Some(false)
            );
            draw(&window, &format!("request-invalid-{suffix}"));
            app.set_model_body_overrides("{\"stream\":false}".into());
            settle(&window);
            assert!(
                !app.global::<RequestRules>()
                    .invoke_validate("".into(), "".into(), app.get_model_body_overrides())
                    .is_empty()
            );
            app.set_model_body_overrides("{\"temperature\":0.2}".into());
            settle(&window);
            assert_eq!(
                control(&app, "保存映射", AccessibleRole::Button).accessible_enabled(),
                Some(true)
            );
            click(&window, &app, "格式化", AccessibleRole::Button);
            assert!(app.get_model_body_overrides().contains('\n'));
            let previous = saves.get();
            click(&window, &app, "保存映射", AccessibleRole::Button);
            assert_eq!(saves.get(), previous + 1);
            // Cancel/navigation clears the private draft independently of visual exit motion.
            app.global::<Theme>().set_animations_enabled(true);
            app.set_model_editor_open(false);
            settle(&window);
            assert!(app.get_model_user_agent().is_empty());
            assert!(app.get_model_header_overrides().is_empty());
            assert!(app.get_model_body_overrides().is_empty());
        }
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.global::<Theme>().set_animations_enabled(false);
    app.invoke_set_appearance(true);
    open(&app);
    settle(&window);
    headless::scroll(&window, 550., 400., -4000.);
    settle(&window);
    app.global::<Theme>().set_animations_enabled(true);
    control(&app, "请求定制", AccessibleRole::Button).invoke_accessible_default_action();
    for frame in 0..40 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        if [0, 4, 10, 24, 39].contains(&frame) {
            draw(&window, &format!("request-motion-{}ms", (frame + 1) * 16));
        } else {
            headless::render(&window);
        }
    }
    click(&window, &app, "选择预设", AccessibleRole::Button);
    click(&window, &app, "Kilo Code", AccessibleRole::Button);
    assert_eq!(app.get_model_user_agent(), "Kilo-Code/1.0");
    draw(&window, "request-presets-dark");
    click(&window, &app, "清空定制", AccessibleRole::Button);
    assert!(app.get_model_user_agent().is_empty());
    // Closing then reopening mid-transition settles in the requested state.
    control(&app, "请求定制", AccessibleRole::Button).invoke_accessible_default_action();
    headless::advance(64);
    headless::render(&window);
    control(&app, "请求定制", AccessibleRole::Button).invoke_accessible_default_action();
    settle(&window);
    assert!(app.get_model_request_open());
    app.set_model_user_agent("temporary/1".into());
    app.set_active_page(1);
    settle(&window);
    assert!(app.get_model_user_agent().is_empty());
    app.global::<Theme>().set_animations_enabled(false);
    open(&app);
    app.set_model_original_id("".into());
    app.set_model_batch_mode(true);
    settle(&window);
    app.set_batch_models(ModelRc::new(VecModel::from(vec![BatchModelRow {
        id: "synthetic-bulk-model".into(),
        supported: true,
        matched: true,
        checked: true,
        ..Default::default()
    }])));
    app.set_batch_summary(BatchSummary {
        total: 1,
        matched: 1,
        selected: 1,
        visible_selectable: 1,
        visible_selected: 1,
        ..Default::default()
    });
    app.set_model_request_open(true);
    app.set_model_user_agent("batch-client/1".into());
    app.set_model_body_overrides("{\"temperature\":0.2}".into());
    settle(&window);
    headless::scroll(&window, 550., 400., -4000.);
    settle(&window);
    draw(&window, "request-batch-dark");
    assert_eq!(
        control(&app, "添加 1 个模型", AccessibleRole::Button).accessible_enabled(),
        Some(true)
    );
    app.set_model_header_overrides("[]".into());
    settle(&window);
    assert_eq!(
        control(&app, "添加 1 个模型", AccessibleRole::Button).accessible_enabled(),
        Some(false)
    );
    app.set_model_batch_mode(false);
    settle(&window);
    assert_eq!(app.get_model_user_agent(), "batch-client/1");
    assert_eq!(app.get_model_body_overrides(), "{\"temperature\":0.2}");
}

#[test]
fn connection_editor_card_replaces_the_preview_validates_and_clears_on_close() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    switchx::request_overrides::connect(&app);
    switchx::code_highlight::connect(&app);
    app.set_loading(false);
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_provider(move |_, _, _, _, _| saved.set(saved.get() + 1));
    app.show().unwrap();
    let open = |app: &AppWindow| {
        app.set_active_page(1);
        app.set_edit_id("synthetic-api".into());
        app.set_edit_name("示例连接".into());
        app.set_edit_url("https://api.example.invalid/v1".into());
        app.set_edit_model("synthetic-coder".into());
        app.set_edit_config_preview("model = \"synthetic-coder\"\n".into());
        app.set_editor_open(true);
    };
    let preview_shown = |app: &AppWindow| {
        ElementHandle::find_by_accessible_label(app, "Codex config.toml 配置预览")
            .next()
            .is_some()
    };
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.invoke_set_appearance(dark);
            open(&app);
            settle(&window);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            assert!(preview_shown(&app));
            draw(&window, &format!("connection-collapsed-{suffix}"));
            click(&window, &app, "请求定制", AccessibleRole::Button);
            assert!(app.get_edit_request_open());
            assert!(
                !preview_shown(&app),
                "the opened card takes the preview's place"
            );
            app.set_edit_user_agent("claude-cli/2.1.161 (external, cli)".into());
            app.set_edit_header_overrides("{\n  \"x-provider\": \"switchx\"\n}".into());
            settle(&window);
            draw(&window, &format!("connection-open-{suffix}"));
            app.set_edit_header_overrides("{\"Authorization\":\"synthetic\"}".into());
            settle(&window);
            assert_eq!(
                control(&app, "保存连接", AccessibleRole::Button).accessible_enabled(),
                Some(false)
            );
            app.set_edit_header_overrides("{\"x-provider\":\"switchx\"}".into());
            settle(&window);
            let previous = saves.get();
            click(&window, &app, "保存连接", AccessibleRole::Button);
            assert_eq!(saves.get(), previous + 1);
            click(&window, &app, "请求定制", AccessibleRole::Button);
            assert!(preview_shown(&app), "collapsing restores the preview");
            // The keyboard focus ring outlines the whole collapsed card.
            draw(&window, &format!("connection-focused-collapsed-{suffix}"));
            app.set_editor_open(false);
            settle(&window);
            assert!(app.get_edit_user_agent().is_empty());
            assert!(app.get_edit_header_overrides().is_empty());
            assert!(app.get_edit_body_overrides().is_empty());
            assert!(!app.get_edit_request_open());
        }
    }
}
