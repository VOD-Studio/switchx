use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Key, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, ModelRc, PhysicalSize, VecModel};
use std::{cell::Cell, io::Write, rc::Rc};

use switchx::ui::{AppWindow, ModelRow, ProviderRow, Theme};

struct PreviewPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for PreviewPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
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
    slint::platform::update_timers_and_animations();
}

fn draw(window: &MinimalSoftwareWindow, name: &str) {
    slint::platform::update_timers_and_animations();
    let size = WindowAdapter::size(window);
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(pixels.make_mut_slice(), size.width as usize);
    });
    if let Some(output) = std::env::var_os("SWITCHX_SELECTION_SNAPSHOTS") {
        let output = std::path::PathBuf::from(output);
        assert!(output.is_absolute());
        std::fs::create_dir_all(&output).unwrap();
        let mut file = std::fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
        write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
        file.write_all(pixels.as_bytes()).unwrap();
    }
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
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
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
