use std::{cell::Cell, error::Error, fs, path::Path, rc::Rc, time::Duration};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, Model, ModelRc, PhysicalSize, VecModel};
#[cfg(target_os = "macos")]
use switchx::macos;
use switchx::{
    headless, provider_icons,
    ui::{AppWindow, ConnectionModelRow, ModelRow, ProviderPresetRow, ProviderRow, Theme},
};

use crate::support::{frame, play, render, set_theme, snapshot, snapshot_now};

/// Connection cards: ready, hover, checks, long text, and managed states.
pub(super) fn render_cards(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    providers: &ModelRc<ProviderRow>,
    native: bool,
) -> Result<(), Box<dyn Error>> {
    let mut rows: Vec<_> = providers.iter().collect();
    rows[0].name = "ChatGPT 订阅".into();
    rows[0].binding_label = "绑定：原生 Codex 登录".into();
    rows[0].credential_status = "订阅凭据由绑定账号提供".into();
    rows[1].binding_label = "绑定：Personal · 合成账号".into();
    rows[2].name = "Gemini API".into();
    rows[2].credential_status = "API Key 已保存 · 尚未检查".into();
    app.set_action_message("连接卡片预览 · 合成资料".into());
    let model = Rc::new(VecModel::from(rows.clone()));
    app.set_providers(model.clone().into());
    app.set_filtered_providers(model.clone().into());
    if native {
        app.set_native_titlebar_overlay(cfg!(target_os = "macos"));
        #[cfg(target_os = "macos")]
        app.global::<Theme>()
            .set_system_reduced_motion(macos::prefers_reduced_motion());
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(true);
        app.on_check_provider(move |id| {
            if let Some(index) = model.iter().position(|row| row.id == id) {
                let mut row = model.row_data(index).unwrap();
                row.check_id = "synthetic-check".into();
                row.check_stage = "检查凭据".into();
                model.set_row_data(index, row);
                let model = model.clone();
                slint::Timer::single_shot(Duration::from_millis(1800), move || {
                    let mut row = model.row_data(index).unwrap();
                    row.check_id = "".into();
                    row.check_message = "合成检查完成".into();
                    row.check_elapsed_ms = 1800;
                    model.set_row_data(index, row);
                });
            }
        });
        return Ok(app.run()?);
    }
    app.show()?;
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            model.set_vec(rows.clone());
            snapshot(window, output, &format!("connections-ready-{suffix}"))?;
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(500.0, 270.0),
            });
            snapshot(window, output, &format!("connections-hover-{suffix}"))?;
            window.dispatch_event(WindowEvent::PointerExited);
            let mut pending = rows[0].clone();
            pending.check_id = "synthetic-check".into();
            pending.check_stage = "检查凭据".into();
            pending.check_elapsed_ms = 12500;
            model.set_row_data(0, pending);
            let mut failed = rows[1].clone();
            failed.check_failed = true;
            failed.check_message = "检查失败".into();
            failed.check_detail = "授权已失效，请在管理连接中重新绑定账号。".into();
            model.set_row_data(1, failed);
            snapshot(window, output, &format!("connections-checks-{suffix}"))?;
            let mut long = rows[2].clone();
            long.name = "团队的 API 连接 · 一个需要截断的非常长的连接名称".into();
            long.endpoint = "https://example.invalid/a/very/long/path/to/an/api/endpoint/v1".into();
            long.auth_error = "无法读取凭据。请打开管理连接重新填写 API Key，然后再次检查。".into();
            model.set_vec(vec![long]);
            snapshot(window, output, &format!("connections-long-{suffix}"))?;
            app.set_config_managed(true);
            snapshot(window, output, &format!("connections-managed-{suffix}"))?;
            app.set_config_managed(false);
        }
    }
    println!("Synthetic connection-card previews: {}", output.display());
    Ok(())
}

pub(super) fn render_connections_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    let checks = Rc::new(Cell::new(0));
    let check_count = checks.clone();
    app.on_refresh(move |_| check_count.set(check_count.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            app.set_connections_tab(0);
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.global::<Theme>().set_animations_enabled(true);
            // Settle the existing provider cards before measuring a tab switch.
            for _ in 0..24 {
                frame(32);
                headless::render(window);
            }
            let initial = headless::render(window);
            for (target, name) in [(1, "accounts"), (0, "upstreams")] {
                headless::click(window, if target == 1 { 360.0 } else { 260.0 }, 182.0);
                assert_eq!(app.get_connections_tab(), target);
                let mut elapsed = 0;
                for at in [
                    0, 16, 32, 64, 96, 128, 160, 192, 224, 256, 288, 340, 400, 480, 600, 760, 920,
                ] {
                    frame(at - elapsed);
                    elapsed = at;
                    let pixels = headless::render(window);
                    // Tab changes keep the title/subtitle still throughout the transition.
                    for y in 20..154 {
                        let start = (y * width as usize + 208) * 3;
                        let end = (y * width as usize + width as usize - 30) * 3;
                        assert_eq!(
                            &pixels.as_bytes()[start..end],
                            &initial.as_bytes()[start..end]
                        );
                    }
                    snapshot_now(window, output, &format!("tabs-{name}-{suffix}-{at:03}ms"))?;
                    if target == 1 && at == 32 {
                        // The still-visible outgoing toolbar must not dispatch actions.
                        headless::click(window, width as f32 - 208.0, 244.0);
                        assert_eq!(checks.get(), 0);
                    }
                }
            }
            // Reverse twice while both pages are still mounted, then resize mid-flight.
            app.set_connections_tab(1);
            headless::render(window);
            frame(96);
            snapshot_now(window, output, &format!("tabs-interrupt-before-{suffix}"))?;
            app.set_connections_tab(0);
            headless::render(window);
            frame(48);
            app.set_connections_tab(1);
            headless::render(window);
            app.window().set_size(PhysicalSize::new(width - 20, height));
            frame(920);
            snapshot_now(window, output, &format!("tabs-interrupt-settled-{suffix}"))?;
            assert_eq!(app.get_connections_tab(), 1);
            app.window().set_size(PhysicalSize::new(width, height));
            // Both motion settings must switch immediately, without a timer tick.
            for reduced in [false, true] {
                app.global::<Theme>().set_animations_enabled(reduced);
                app.global::<Theme>().set_system_reduced_motion(reduced);
                app.set_connections_tab(0);
                let upstreams = headless::render(window);
                app.set_connections_tab(1);
                let accounts = headless::render(window);
                assert_ne!(upstreams.as_bytes(), accounts.as_bytes());
                frame(1000);
                let settled = headless::render(window);
                assert_eq!(
                    accounts.as_bytes(),
                    settled.as_bytes(),
                    "Motion-off content must be complete immediately"
                );
                snapshot_now(window, output, &format!("tabs-static-{reduced}-{suffix}"))?;
            }
            app.global::<Theme>().set_system_reduced_motion(false);
        }
    }
    println!(
        "Synthetic tab transitions and interruption checks: {}",
        output.display()
    );
    Ok(())
}

pub(super) fn render_connection_workbench(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
    hover_only: bool,
) -> Result<(), Box<dyn Error>> {
    let make_provider =
        |id: &str, name: &str, icon_id: &str, subscription: bool, grok: bool, selected: bool| {
            let icon = provider_icons::icon(icon_id).unwrap();
            let models: Vec<_> = ["coder-pro", "coder-mini", "reasoner-max", "chat-flash"]
                .iter()
                .enumerate()
                .map(|(index, model)| ModelRow {
                    provider_id: id.into(),
                    provider_name: name.into(),
                    upstream_model: (*model).into(),
                    public_id: format!("sx-{id}-{index}").into(),
                    display_name: format!("{model}/{name}").into(),
                    context_window: "1000000".into(),
                    reasoning_levels: "low, medium, high, max".into(),
                    saved: true,
                    ready: true,
                    included: selected,
                    is_subscription: subscription,
                    ..Default::default()
                })
                .collect();
            ProviderRow {
                id: id.into(),
                name: name.into(),
                icon: slint::Image::load_from_svg_data(icon.data).unwrap(),
                monochrome: icon.monochrome,
                endpoint: "https://example.invalid/v1".into(),
                base_url: "https://example.invalid/v1".into(),
                binding_label: "绑定：工作账号".into(),
                is_subscription: subscription,
                is_grok: grok,
                model_count: 4,
                ready_model_count: 4,
                selected_model_count: if selected { 4 } else { 0 },
                models: ModelRc::new(VecModel::from(models)),
                ..Default::default()
            }
        };
    let providers = vec![
        make_provider("chatgpt", "ChatGPT · 工作", "openai", true, false, true),
        make_provider("grok", "Grok", "grok", true, true, false),
        make_provider("podlink", "Podlink", "anthropic", false, false, false),
    ];
    app.set_providers(ModelRc::new(VecModel::from(providers.clone())));
    app.set_models(ModelRc::new(VecModel::from(
        providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .collect::<Vec<_>>(),
    )));
    app.set_selected_model_count(4);
    app.set_selected_connection_count(1);
    app.set_default_model_label("coder-pro/ChatGPT · 工作".into());
    app.set_selectable_model_count(12);
    app.set_default_model("sx-chatgpt-0".into());
    app.set_config_home("/isolated/codex".into());
    app.set_status_text("合成连接与模型 · 未读取账号或修改 Codex".into());
    let toggles = Rc::new(Cell::new(0));
    let calls = toggles.clone();
    app.on_select_connection(move |id, selected| {
        assert_eq!(id, "chatgpt");
        assert!(!selected);
        calls.set(calls.get() + 1);
    });
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_connection_models_dirty(true);
        }
    });
    let saves = Rc::new(Cell::new(0));
    let save_calls = saves.clone();
    app.on_save_connection_models(move || save_calls.set(save_calls.get() + 1));
    let weak = app.as_weak();
    app.on_edit_connection(move |id| {
        let app = weak.upgrade().unwrap();
        assert_eq!(id, "chatgpt");
        app.set_busy(true);
        app.set_connection_models_open(false);
        app.set_subscription_editor_open(true);
    });
    let weak = app.as_weak();
    app.on_begin_connection_models(move |id| {
        let app = weak.upgrade().unwrap();
        assert_eq!(id, "chatgpt");
        app.set_busy(true);
        app.set_subscription_editor_open(false);
        app.set_connection_models_open(true);
    });
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_active_page(0);
            snapshot(window, output, &format!("workbench-connections-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(true);
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(500.0, 290.0),
            });
            snapshot(window, output, &format!("workbench-hover-{suffix}"))?;
            window.dispatch_event(WindowEvent::PointerExited);
            app.global::<Theme>().set_animations_enabled(false);
            if width == 1200 && !dark {
                headless::click(window, 1090.0, 290.0);
                render(window);
                assert_eq!(
                    toggles.get(),
                    1,
                    "whole-connection selection must be reachable"
                );
            }
            if hover_only {
                continue;
            }
            let expand_x = width as f32 - 100.0;
            headless::click(window, expand_x, 350.0);
            snapshot(window, output, &format!("workbench-expanded-{suffix}"))?;
            let pixels = render(window);
            let sample_x = width as usize - 430;
            let pixel = |y: usize| pixels.as_slice()[y * width as usize + sample_x];
            // Empty space beside the model label must stay clear through the
            // row midpoint; the separator belongs below all row content.
            for y in 401..411 {
                assert_eq!(
                    pixel(y),
                    pixel(399),
                    "divider crosses the model row at {suffix}, y={y}"
                );
            }
            if height == 820 {
                assert_ne!(
                    pixel(439),
                    pixel(399),
                    "row boundary separator is missing at {suffix}"
                );
            }
            headless::click(window, expand_x, 350.0);
            if width == 1200 && dark {
                headless::click(window, 860.0, 186.0);
                snapshot(window, output, &format!("workbench-direct-{suffix}"))?;
                headless::click(window, 785.0, 186.0);
            }
            app.set_active_page(1);
            app.set_connection_models_id("podlink".into());
            app.set_connection_models_name("Podlink".into());
            app.set_connection_models_open(true);
            app.set_connection_models(ModelRc::new(VecModel::from(
                (0..4)
                    .map(|index| ConnectionModelRow {
                        public_id: format!("sx-podlink-{index}").into(),
                        display_name: [
                            "Claude Opus · Podlink",
                            "Claude Sonnet · Podlink",
                            "Coder Pro · Podlink",
                            "Reasoner Max · Podlink",
                        ][index]
                            .into(),
                        upstream_model: [
                            "claude-opus",
                            "claude-sonnet",
                            "coder-pro",
                            "reasoner-max",
                        ][index]
                            .into(),
                        context_window: "1000000".into(),
                        reasoning_levels: "low, medium, high, max".into(),
                        default_reasoning: "high".into(),
                        ..Default::default()
                    })
                    .collect::<Vec<_>>(),
            )));
            snapshot(window, output, &format!("connection-directory-{suffix}"))?;
            assert!(app.get_connection_models_open());
            app.set_busy(true);
            app.set_discovery_scope(2);
            app.set_fetching_models(true);
            app.set_discovery_message("正在获取模型列表…".into());
            app.global::<Theme>().set_animations_enabled(true);
            play(
                window,
                output,
                &format!("connection-directory-loading-{suffix}"),
                &[640, 1280, 1424],
            )?;
            assert_ne!(
                fs::read(output.join(format!("connection-directory-loading-{suffix}-1280ms.ppm")))?,
                fs::read(output.join(format!("connection-directory-loading-{suffix}-1424ms.ppm")))?,
                "model discovery feedback must keep moving at {suffix}"
            );
            app.global::<Theme>().set_animations_enabled(false);
            app.set_fetching_models(false);
            app.set_discovery_message("".into());
            app.set_connection_models_saving(true);
            snapshot(
                window,
                output,
                &format!("connection-directory-saving-{suffix}"),
            )?;
            app.set_connection_models_saving(false);
            app.set_busy(false);
            render(window);
            if width == 1200 && dark {
                headless::click(window, 1132.0, 338.0);
                render(window);
                assert!(
                    app.get_connection_models().row_data(0).unwrap().removing,
                    "directory removal must stay in the draft"
                );
                assert!(app.get_connection_models_dirty());
                snapshot(window, output, "connection-directory-remove-dark")?;
                headless::click(window, 1110.0, 325.0);
                render(window);
                assert!(
                    !app.get_connection_models().row_data(0).unwrap().removing,
                    "removal must be undoable before save"
                );
                headless::click(window, 1095.0, 775.0);
                render(window);
                assert_eq!(saves.get(), 1, "directory save must remain reachable");
            }
            app.set_connection_models_dirty(false);
            app.set_connection_models_open(false);
            app.set_subscription_id("chatgpt".into());
            app.set_subscription_name("ChatGPT · 工作".into());
            app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec!["".into()])));
            app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
                "跟随 Codex 登录".into(),
            ])));
            app.set_edit_icon(providers[0].icon.clone());
            app.set_subscription_binding_label("跟随所选 Codex 目录的登录。".into());
            app.set_subscription_editor_open(true);
            snapshot(window, output, &format!("subscription-clean-{suffix}"))?;
            app.set_connection_models_id("chatgpt".into());
            app.set_connection_models_name("ChatGPT · 工作".into());
            app.set_connection_models_subscription(true);
            let left = width as f32 - 820.0 + 26.0;
            for directory in [true, false, true, false] {
                headless::click(window, left + if directory { 240.0 } else { 80.0 }, 106.0);
                // Match the asynchronous editor load: focus moves while controls
                // are disabled, then the new editor becomes ready.
                render(window);
                headless::click(window, left + if directory { 80.0 } else { 240.0 }, 106.0);
                assert_eq!(app.get_connection_models_open(), directory);
                app.set_busy(false);
                render(window);
                assert_eq!(app.get_connection_models_open(), directory);
                assert_eq!(app.get_subscription_editor_open(), !directory);
                snapshot(
                    window,
                    output,
                    &format!(
                        "connection-tabs-{}-{suffix}",
                        if directory { "models" } else { "settings" }
                    ),
                )?;
            }
            for (directory, key) in [
                (true, slint::platform::Key::RightArrow),
                (false, slint::platform::Key::LeftArrow),
            ] {
                // Focus the current segment, then select its neighbor by keyboard.
                headless::click(window, left + if directory { 80.0 } else { 240.0 }, 106.0);
                window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
                window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
                render(window);
                app.set_busy(false);
                render(window);
                assert_eq!(app.get_connection_models_open(), directory);
                assert_eq!(app.get_subscription_editor_open(), !directory);
            }
            app.set_config_managed(true);
            headless::click(window, left + 240.0, 106.0);
            render(window);
            assert!(app.get_subscription_editor_open());
            assert!(!app.get_connection_models_open());
            snapshot(window, output, &format!("connection-tabs-managed-{suffix}"))?;
            app.set_config_managed(false);
            app.set_subscription_editor_open(false);
        }
    }
    if hover_only {
        println!("Synthetic workbench hover previews: {}", output.display());
        return Ok(());
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.set_active_page(0);
    app.global::<Theme>().set_animations_enabled(true);
    render(window);
    headless::click(window, 1100.0, 418.0);
    for index in 0..12 {
        frame(30);
        snapshot_now(window, output, &format!("workbench-motion-{index:02}"))?;
    }
    assert_ne!(
        fs::read(output.join("workbench-motion-00.ppm"))?,
        fs::read(output.join("workbench-motion-11.ppm"))?,
        "disclosure must animate between captured frames"
    );
    app.global::<Theme>().set_system_reduced_motion(true);
    snapshot(window, output, "workbench-reduced-motion")?;
    Ok(())
}

pub(super) fn render_connection_picker_motion(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    // Synthetic callbacks follow production's open-form / close-picker ordering.
    app.set_active_page(1);
    app.set_provider_presets(ModelRc::new(VecModel::from(
        switchx::app::PROVIDER_PRESETS
            .iter()
            .map(|preset| {
                Ok(ProviderPresetRow {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    icon: slint::Image::load_from_svg_data(preset.icon)?,
                    monochrome: preset.monochrome,
                })
            })
            .collect::<Result<Vec<_>, slint::LoadImageError>>()?,
    )));
    let selections = Rc::new(Cell::new(0));
    let selected = selections.clone();
    let weak = app.as_weak();
    app.on_begin_provider_editor(move |_| {
        let app = weak.upgrade().unwrap();
        selected.set(selected.get() + 1);
        app.set_edit_id("".into());
        app.set_edit_name("DeepSeek".into());
        app.set_edit_url("https://api.example.invalid/v1".into());
        app.set_edit_model("synthetic-model".into());
        app.set_edit_key("".into());
        app.set_edit_preset_id("deepseek".into());
        app.set_edit_icon_name("DeepSeek".into());
        app.set_edit_icon(
            provider_icons::load_image(provider_icons::icon("deepseek").unwrap()).unwrap(),
        );
        app.set_edit_config_preview("model = \"synthetic-model\"\nweb_search = \"live\"\n".into());
        app.set_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let weak = app.as_weak();
    app.on_begin_subscription_editor(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_subscription_id("".into());
        app.set_subscription_name("ChatGPT".into());
        app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec!["".into()])));
        app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
            "跟随 Codex 登录".into(),
        ])));
        app.set_subscription_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let weak = app.as_weak();
    app.on_cancel_subscription_editor(move || {
        let app = weak.upgrade().unwrap();
        app.set_subscription_editor_open(false);
        app.set_subscription_editor_pending(false);
        app.set_subscription_auth_json("".into());
    });
    let weak = app.as_weak();
    app.on_begin_xai_editor(move |_| {
        let app = weak.upgrade().unwrap();
        app.set_xai_provider_id("".into());
        app.set_xai_provider_name("Grok".into());
        app.set_xai_editor_open(true);
        app.set_connection_picker_open(false);
    });
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_provider(move |_, _, _, _, _| saved.set(saved.get() + 1));

    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let picker_left = width as f32 - 820.0 + 26.0;
        let api_left = (width as f32 - 1120.0).max(60.0) + 26.0;
        for dark in [false, true] {
            app.global::<Theme>().set_animations_enabled(false);
            set_theme(app, window, dark);
            app.invoke_open_connection_picker();
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("connection-picker-{suffix}"))?;
            app.global::<Theme>().set_animations_enabled(true);
            headless::click(window, picker_left + 90.0, 280.0);
            assert!(app.get_editor_open());
            let mut elapsed = 0;
            for at in [0, 16, 48, 96, 160, 240, 340, 480] {
                frame(at - elapsed);
                elapsed = at;
                snapshot_now(
                    window,
                    output,
                    &format!("connection-forward-{suffix}-{at:03}ms"),
                )?;
            }
            app.set_edit_key("synthetic-unsaved-key".into());
            headless::render(window);
            headless::click(window, api_left + 50.0, 108.0);
            assert!(
                app.get_connection_picker_open(),
                "back returns one level to the picker"
            );
            assert!(!app.get_editor_open());
            assert!(
                app.get_edit_key().is_empty(),
                "back clears the draft immediately"
            );
            let before = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), before, "outgoing save is inert");
            let mut elapsed = 0;
            for at in [0, 16, 48, 96, 160, 240, 340, 480] {
                frame(at - elapsed);
                elapsed = at;
                snapshot_now(
                    window,
                    output,
                    &format!("connection-back-{suffix}-{at:03}ms"),
                )?;
            }
            assert_ne!(
                fs::read(output.join(format!("connection-back-{suffix}-016ms.ppm")))?,
                fs::read(output.join(format!("connection-back-{suffix}-480ms.ppm")))?,
                "return has intermediate animation frames"
            );

            // Rapid retargeting and resize settle on a usable picker.
            headless::click(window, picker_left + 90.0, 280.0);
            headless::render(window);
            frame(96);
            app.invoke_return_to_connection_picker();
            headless::render(window);
            frame(48);
            app.invoke_begin_provider_editor("".into());
            headless::render(window);
            frame(48);
            app.invoke_return_to_connection_picker();
            app.window().set_size(PhysicalSize::new(width + 40, height));
            headless::render(window);
            frame(600);
            app.window().set_size(PhysicalSize::new(width, height));
            headless::render(window);
            frame(600);
            let count = selections.get();
            headless::click(window, picker_left + 90.0, 280.0);
            assert_eq!(
                selections.get(),
                count + 1,
                "picker works after rapid reversal and resize"
            );
            headless::render(window);
            frame(600);
            app.invoke_return_to_connection_picker();
            headless::render(window);
            frame(600);

            // The same back affordance applies to both subscription forms.
            for (x, chatgpt) in [(picker_left + 90.0, true), (picker_left + 440.0, false)] {
                headless::click(window, x, 160.0);
                headless::render(window);
                frame(600);
                assert_eq!(app.get_subscription_editor_open(), chatgpt);
                assert_eq!(app.get_xai_editor_open(), !chatgpt);
                app.set_subscription_auth_json("synthetic-unsaved-credential".into());
                headless::click(window, api_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                assert!(app.get_subscription_auth_json().is_empty());
                assert!(!app.get_subscription_editor_open());
                assert!(!app.get_xai_editor_open());
                headless::render(window);
                frame(600);
            }

            for reduced in [false, true] {
                app.global::<Theme>().set_animations_enabled(reduced);
                app.global::<Theme>().set_system_reduced_motion(reduced);
                headless::click(window, picker_left + 90.0, 280.0);
                headless::render(window);
                headless::click(window, api_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(5.0, 5.0),
                });
                let immediate = headless::render(window);
                frame(1000);
                assert_eq!(
                    immediate.as_bytes(),
                    headless::render(window).as_bytes(),
                    "disabled/reduced motion returns immediately"
                );
            }
            app.global::<Theme>().set_system_reduced_motion(false);
        }
    }
    println!(
        "Synthetic connection navigation and motion checks: {}",
        output.display()
    );
    Ok(())
}

/// Per-connection check progress and results.
pub(super) fn render_check_progress(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    let original = app.get_providers().iter().collect::<Vec<_>>();
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            for state in ["checking", "slow", "results", "concurrent"] {
                let mut rows = original.clone();
                rows[0].name = "ChatGPT 订阅".into();
                rows[0].binding_label = "绑定：原生 Codex 登录（合成资料）".into();
                if state == "results" {
                    rows[0].check_message = "登录状态可读取".into();
                    rows[0].check_detail = "尚未验证实际官方请求权限".into();
                    rows[0].check_elapsed_ms = 2300;
                    rows[1].check_failed = true;
                    rows[1].check_message = "检查失败".into();
                    rows[1].check_detail = "上游模型目录返回 HTTP 401".into();
                    rows[1].check_elapsed_ms = 1600;
                } else {
                    rows[0].check_id = "synthetic-check-a".into();
                    rows[0].check_stage = "读取登录状态".into();
                    rows[0].check_elapsed_ms = if state == "slow" { 12500 } else { 2300 };
                    if state == "concurrent" {
                        rows[1].check_id = "synthetic-check-b".into();
                        rows[1].check_stage = "读取模型目录".into();
                        rows[1].check_elapsed_ms = 1800;
                    }
                }
                let rows = ModelRc::new(VecModel::from(rows));
                app.set_providers(rows.clone());
                app.set_filtered_providers(rows);
                snapshot(window, output, &format!("check-{state}-{suffix}"))?;
            }
        }
    }
    println!("Synthetic connection-check previews: {}", output.display());
    Ok(())
}
