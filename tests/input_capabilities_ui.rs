use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::{
    ComponentHandle, Model, ModelRc, PhysicalSize, VecModel,
    platform::{Key, software_renderer::MinimalSoftwareWindow},
};
use switchx::{
    headless,
    ui::{AppWindow, ConnectionModelRow, Theme},
};

fn draw(window: &MinimalSoftwareWindow, name: &str) {
    headless::save_snapshot(
        "SWITCHX_CAPABILITIES_SNAPSHOTS",
        name,
        &headless::render(window),
    );
}
fn settle(window: &MinimalSoftwareWindow) {
    for _ in 0..24 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        headless::render(window);
    }
}
fn click(app: &AppWindow, window: &MinimalSoftwareWindow, label: &str, role: AccessibleRole) {
    let control = ElementHandle::find_by_accessible_label(app, label)
        .find(|e| e.accessible_role() == Some(role))
        .unwrap_or_else(|| panic!("missing control: {label}"));
    let pos = control.absolute_position();
    let size = control.size();
    assert!(size.width > 0. && size.height > 0.);
    headless::click(window, pos.x + size.width / 2., pos.y + size.height / 2.);
    slint::platform::update_timers_and_animations();
}

#[test]
fn capabilities_support_themes_sizes_keyboard_inheritance_disabled_states_and_motion() {
    let window = headless::install_with_manual_time();
    let app = AppWindow::new().unwrap();
    app.set_loading(false);
    let weak = app.as_weak();
    app.on_connection_model_edited(move || weak.unwrap().set_connection_models_dirty(true));
    app.set_model_provider_name("合成连接".into());
    app.set_model_display_name("视觉助手 / 合成连接".into());
    app.set_fetched_models(ModelRc::new(VecModel::from(vec!["synthetic-model".into()])));
    app.set_model_upstream_id("synthetic-model".into());
    app.set_model_original_id("sx-test".into());
    app.set_model_context_window("128000".into());
    app.show().unwrap();
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.invoke_set_appearance(dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.set_active_page(0);
            app.set_connection_models_open(false);
            app.set_model_editor_open(true);
            app.set_model_inherited_modalities(4);
            app.set_model_input_modalities(0);
            settle(&window);
            draw(&window, &format!("model-inherited-{suffix}"));
            click(
                &app,
                &window,
                "模型输入能力：图片",
                AccessibleRole::Checkbox,
            );
            assert_eq!(app.get_model_input_modalities(), 3);
            headless::key(&window, " ");
            assert_eq!(
                app.get_model_input_modalities(),
                4,
                "focused capability toggles from the keyboard"
            );
            click(
                &app,
                &window,
                "模型输入能力：音频",
                AccessibleRole::Checkbox,
            );
            assert_eq!(app.get_model_input_modalities(), 2);
            draw(&window, &format!("model-image-{suffix}"));
            click(
                &app,
                &window,
                "模型输入能力：图片",
                AccessibleRole::Checkbox,
            );
            assert_eq!(app.get_model_input_modalities(), 1);
            click(
                &app,
                &window,
                "模型输入能力：沿用资料",
                AccessibleRole::Button,
            );
            assert_eq!(app.get_model_input_modalities(), 0);
            app.set_config_managed(true);
            click(
                &app,
                &window,
                "模型输入能力：图片",
                AccessibleRole::Checkbox,
            );
            assert_eq!(
                app.get_model_input_modalities(),
                0,
                "published configuration cannot be edited"
            );
            app.set_config_managed(false);
            app.set_model_editor_open(false);
            app.set_active_page(1);
            app.set_connection_models_name("合成连接".into());
            app.set_connection_models(ModelRc::new(VecModel::from(vec![ConnectionModelRow {
                public_id: "sx-test".into(),
                display_name: "视觉助手".into(),
                upstream_model: "synthetic-model".into(),
                context_window: "128000".into(),
                inherited_modalities: 2,
                ..Default::default()
            }])));
            app.set_connection_models_open(true);
            settle(&window);
            click(&app, &window, "输入能力 1", AccessibleRole::Button);
            settle(&window);
            draw(&window, &format!("directory-popup-{suffix}"));
            click(&app, &window, "输入能力 1：音频", AccessibleRole::Checkbox);
            assert_eq!(
                app.get_connection_models()
                    .row_data(0)
                    .unwrap()
                    .input_modalities,
                4
            );
            headless::key(&window, Key::Escape);
            settle(&window);
            draw(&window, &format!("directory-saved-{suffix}"));
        }
    }
    app.global::<Theme>().set_animations_enabled(true);
    click(&app, &window, "输入能力 1", AccessibleRole::Button);
    for frame in 0..24 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        draw(&window, &format!("popup-frame-{frame:03}"));
    }
    click(&app, &window, "输入能力 1：图片", AccessibleRole::Checkbox);
    for frame in 0..24 {
        headless::advance(16);
        slint::platform::update_timers_and_animations();
        draw(&window, &format!("selection-frame-{frame:03}"));
    }
    assert_eq!(
        app.get_connection_models()
            .row_data(0)
            .unwrap()
            .input_modalities,
        3
    );
    app.global::<Theme>().set_system_reduced_motion(true);
    click(
        &app,
        &window,
        "输入能力 1：沿用资料",
        AccessibleRole::Button,
    );
    assert_eq!(
        app.get_connection_models()
            .row_data(0)
            .unwrap()
            .input_modalities,
        0
    );
    draw(&window, "reduced-motion");

    // The batch picker keeps row choices above shared choices and templates.
    headless::key(&window, Key::Escape);
    settle(&window);
    app.set_connection_models_open(false);
    app.set_active_page(0);
    app.set_model_original_id("".into());
    app.set_model_batch_mode(true);
    app.set_model_editor_open(true);
    settle(&window);
    let weak = app.as_weak();
    app.on_batch_edit(move |index, context, levels, modalities| {
        let app = weak.unwrap();
        let rows = app.get_batch_models();
        let mut row = rows.row_data(index as usize).unwrap();
        row.context = context;
        row.levels = levels;
        row.input_modalities = modalities;
        rows.set_row_data(index as usize, row);
    });
    app.set_batch_models(ModelRc::new(VecModel::from(vec![
        switchx::ui::BatchModelRow {
            id: "synthetic-vision".into(),
            supported: true,
            matched: true,
            checked: true,
            source: "预设资料".into(),
            template_modalities: 2,
            ..Default::default()
        },
    ])));
    app.set_batch_summary(switchx::ui::BatchSummary {
        total: 1,
        matched: 1,
        selected: 1,
        visible_selectable: 1,
        visible_selected: 1,
        ..Default::default()
    });
    settle(&window);
    click(
        &app,
        &window,
        "调整 synthetic-vision 的参数",
        AccessibleRole::Button,
    );
    settle(&window);
    click(
        &app,
        &window,
        "synthetic-vision 的输入能力",
        AccessibleRole::Button,
    );
    settle(&window);
    draw(&window, "batch-row-popup-dark-1000x680");
    click(
        &app,
        &window,
        "synthetic-vision 的输入能力：图片",
        AccessibleRole::Checkbox,
    );
    assert_eq!(
        app.get_batch_models().row_data(0).unwrap().input_modalities,
        1
    );
    app.set_batch_input_modalities(4);
    assert_eq!(
        app.get_batch_models().row_data(0).unwrap().input_modalities,
        1
    );
    click(
        &app,
        &window,
        "synthetic-vision 的输入能力：沿用资料",
        AccessibleRole::Button,
    );
    assert_eq!(
        app.get_batch_models().row_data(0).unwrap().input_modalities,
        0
    );
    headless::key(&window, Key::Escape);
    settle(&window);
    headless::scroll(&window, 960., 490., -3000.);
    settle(&window);
    draw(&window, "batch-shared-dark-1000x680");
    click(
        &app,
        &window,
        "统一输入能力：音频",
        AccessibleRole::Checkbox,
    );
    assert_eq!(app.get_batch_input_modalities(), 2);
    click(
        &app,
        &window,
        "统一输入能力：仅文字",
        AccessibleRole::Button,
    );
    assert_eq!(app.get_batch_input_modalities(), 1);
}
