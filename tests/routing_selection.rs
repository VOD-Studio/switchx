use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use std::{cell::Cell, rc::Rc};
use switchx::headless;

use switchx::ui::{AppWindow, ModelRow, ProviderRow, Theme};

fn click(window: &MinimalSoftwareWindow, x: f32, y: f32) {
    headless::click(window, x, y);
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) {
    let pixels = headless::render(window);
    headless::save_snapshot("SWITCHX_SELECTION_SNAPSHOTS", name, &pixels);
}

fn set_selection(app: &AppWindow, selected: usize) {
    app.set_models(ModelRc::new(VecModel::from(
        (0..11)
            .map(|index| ModelRow {
                provider_id: "synthetic".into(),
                provider_name: "ChatGPT 订阅".into(),
                public_id: format!("sx-synthetic-{index}").into(),
                display_name: format!("合成模型 {index}/ChatGPT 订阅").into(),
                ready: index < 10,
                included: index < selected,
                saved: true,
                is_subscription: true,
                binding_label: "订阅账号".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )));
    app.set_providers(ModelRc::new(VecModel::from(vec![ProviderRow {
        id: "synthetic".into(),
        name: "ChatGPT 订阅".into(),
        is_subscription: true,
        model_count: 11,
        ready_model_count: 10,
        selected_model_count: selected as i32,
        models: app.get_models(),
        ..Default::default()
    }])));
    app.set_selected_model_count(selected as i32);
    app.set_selectable_model_count(10);
}

#[test]
fn model_header_tracks_selection_and_respects_disabled_states() {
    let window = headless::install();
    let app = AppWindow::new().unwrap();
    app.global::<Theme>().set_animations_enabled(false);
    app.set_loading(false);
    app.set_status_text("合成模型选择检查".into());
    app.set_model_provider_options(ModelRc::new(VecModel::from(vec!["ChatGPT 订阅".into()])));
    app.window().set_size(PhysicalSize::new(1200, 820));
    app.show().unwrap();
    let calls = Rc::new(Cell::new(0));
    let last = Rc::new(Cell::new((false, false)));
    let callback_calls = calls.clone();
    let callback_last = last.clone();
    app.on_select_models(move |selected, selected_only| {
        callback_calls.set(callback_calls.get() + 1);
        callback_last.set((selected, selected_only));
    });

    set_selection(&app, 3);
    draw(&window, "before-failed-save");
    for _ in 0..2 {
        click(&window, 240.0, 238.0);
        assert_eq!(last.get(), (true, false));
    }
    assert_eq!(calls.get(), 2);

    for dark in [false, true] {
        app.invoke_set_appearance(dark);
        set_selection(&app, 3);
        draw(
            &window,
            if dark {
                "partial-dark"
            } else {
                "partial-light"
            },
        );
        let before = calls.get();
        click(&window, 240.0, 238.0);
        assert_eq!(calls.get(), before + 1);
        assert_eq!(last.get(), (true, false));
        set_selection(&app, 10);
        draw(&window, if dark { "all-dark" } else { "all-light" });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Space.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Space.into(),
        });
        assert_eq!(calls.get(), before + 2);
        assert_eq!(last.get(), (false, false));
        set_selection(&app, 0);
        draw(&window, if dark { "none-dark" } else { "none-light" });
    }

    set_selection(&app, 3);
    draw(&window, "before-selected-filter");
    click(&window, 1015.0, 186.0);
    draw(&window, "selected-filter-dark");
    click(&window, 240.0, 238.0);
    assert_eq!(last.get(), (false, true));
    set_selection(&app, 0);
    draw(&window, "empty-selected-dark");
    let before = calls.get();
    click(&window, 240.0, 238.0);
    assert_eq!(calls.get(), before);
    click(&window, 935.0, 186.0);

    for (busy, managed, loading) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        set_selection(&app, 3);
        app.set_busy(busy);
        app.set_config_managed(managed);
        app.set_loading(loading);
        draw(&window, "disabled-dark");
        click(&window, 240.0, 238.0);
        assert_eq!(calls.get(), before);
    }
    app.set_busy(false);
    app.set_config_managed(false);
    app.set_loading(false);
    app.set_models(ModelRc::new(VecModel::from(Vec::new())));
    app.set_selected_model_count(0);
    app.set_selectable_model_count(0);
    draw(&window, "empty-dark");
    click(&window, 240.0, 238.0);
    assert_eq!(calls.get(), before);
    app.set_models(ModelRc::new(VecModel::from(vec![ModelRow {
        display_name: "待补充资料的模型".into(),
        ..Default::default()
    }])));
    draw(&window, "unavailable-dark");
    click(&window, 240.0, 238.0);
    assert_eq!(calls.get(), before);

    set_selection(&app, 3);
    app.window().set_size(PhysicalSize::new(1000, 680));
    for dark in [false, true] {
        app.invoke_set_appearance(dark);
        draw(
            &window,
            if dark {
                "compact-dark"
            } else {
                "compact-light"
            },
        );
        click(&window, 240.0, 238.0);
        assert_eq!(last.get(), (true, false));
    }
    assert_eq!(calls.get(), before + 2);
}
