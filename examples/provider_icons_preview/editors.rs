use std::{cell::Cell, error::Error, path::Path, rc::Rc, time::Duration};

use slint::platform::{WindowEvent, software_renderer::MinimalSoftwareWindow};
use slint::{ComponentHandle, ModelRc, PhysicalSize, VecModel};
use switchx::{
    headless, provider_icons,
    ui::{AppWindow, Theme},
};

use crate::{
    models::{batch_rows, summarize},
    support::{frame, play, render, set_theme, snapshot, snapshot_now},
};

pub(super) fn render_xai_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    app.set_active_page(1);
    app.set_xai_provider_name("Grok · 工作账号".into());
    app.set_xai_account_ids(ModelRc::new(VecModel::from(vec![
        "@default".into(),
        "synthetic-grok-account".into(),
    ])));
    app.set_xai_account_options(ModelRc::new(VecModel::from(vec![
        "跟随默认 Grok 账号（发布时固定）".into(),
        "工作账号 · preview@example.invalid".into(),
    ])));
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_xai(move || saved.set(saved.get() + 1));
    let models = Rc::new(Cell::new(0));
    let model = models.clone();
    app.on_begin_connection_models(move |id| {
        assert_eq!(id, "preview-grok");
        model.set(model.get() + 1);
    });
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_provider(move |id| {
        assert_eq!(id, "preview-grok");
        deleted.set(deleted.get() + 1);
    });
    let accounts = Rc::new(Cell::new(0));
    let managed = accounts.clone();
    app.on_xai_account_action(move |action, _| {
        if action == 0 {
            managed.set(managed.get() + 1);
        }
    });
    for (width, height) in [(1200, 820), (1000, 680), (1600, 1000)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let left = (width as f32 - 1120.0).max(60.0) + 26.0;
        let right = left + 312.0;
        for dark in [false, true] {
            set_theme(app, window, dark);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            app.set_xai_provider_id("preview-grok".into());
            app.set_delete_confirm(false);
            app.set_xai_account_choice(1);
            app.set_xai_editor_open(true);
            snapshot(window, output, &format!("xai-editor-{suffix}"))?;
            headless::click(
                window,
                left + 245.0,
                if height < 760 { 354.0 } else { 386.0 },
            );
            render(window);
            for key in [slint::platform::Key::Home, slint::platform::Key::Return] {
                window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
                window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            }
            render(window);
            assert_eq!(
                app.get_xai_account_choice(),
                0,
                "the account picker supports keyboard selection"
            );
            app.set_xai_account_choice(1);
            render(window);
            let count = models.get();
            headless::click(
                window,
                right + 90.0,
                height as f32 - if height < 760 { 133.0 } else { 137.0 },
            );
            assert_eq!(models.get(), count + 1, "the model directory is reachable");
            let count = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "save remains in the fixed footer");
            let model_count = models.get();
            app.set_config_managed(true);
            snapshot(window, output, &format!("xai-managed-{suffix}"))?;
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            headless::click(
                window,
                right + 90.0,
                height as f32 - if height < 760 { 133.0 } else { 137.0 },
            );
            assert_eq!(saves.get(), count + 1);
            assert_eq!(models.get(), model_count, "managed models cannot be edited");
            app.set_config_managed(false);
            app.set_busy(true);
            snapshot(window, output, &format!("xai-saving-{suffix}"))?;
            headless::click(window, width as f32 - 202.0, height as f32 - 46.0);
            assert!(app.get_xai_editor_open(), "saving prevents cancellation");
            app.set_busy(false);
            let count = deletes.get();
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert!(app.get_delete_confirm());
            assert_eq!(deletes.get(), count, "deletion requires confirmation");
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert_eq!(deletes.get(), count + 1);
            app.set_delete_confirm(false);
            app.set_xai_account_choice(0);
            snapshot(window, output, &format!("xai-default-{suffix}"))?;
            let count = accounts.get();
            headless::click(
                window,
                left + 140.0,
                if height < 760 { 492.0 } else { 538.0 },
            );
            assert_eq!(accounts.get(), count + 1, "account management is reachable");
            assert_eq!(app.get_connections_tab(), 1);
            assert!(!app.get_xai_editor_open());
            app.set_connections_tab(0);
            app.set_xai_editor_open(true);
            app.set_error_code("synthetic-preview-error".into());
            app.set_error_message("合成保存错误：绑定的 Grok 账号不可用，请重新选择。".into());
            app.set_error_action("在账号管理中检查后重试".into());
            snapshot(window, output, &format!("xai-error-{suffix}"))?;
            app.set_error_code("".into());
            app.set_error_message("".into());
            app.set_error_action("".into());
            app.set_xai_editor_open(false);
            app.set_xai_provider_id("".into());
            app.set_xai_editor_open(true);
            snapshot(window, output, &format!("xai-new-{suffix}"))?;
            let count = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "new connections can be saved");
            headless::click(window, width as f32 - 220.0, height as f32 - 46.0);
            assert!(!app.get_xai_editor_open(), "cancel remains reachable");
        }
    }
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.set_xai_provider_id("preview-grok".into());
    app.global::<Theme>().set_animations_enabled(true);
    app.set_xai_editor_open(true);
    play(
        window,
        output,
        "xai-enter",
        &[0, 64, 144, 256, 416, 640, 960],
    )?;
    app.set_xai_account_choice(1);
    play(window, output, "xai-binding", &[0, 64, 144, 256, 416, 640])?;
    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        app.set_xai_editor_open(false);
        headless::render(window);
        app.set_xai_editor_open(true);
        let immediate = headless::render(window);
        frame(960);
        assert!(
            immediate.as_bytes() == headless::render(window).as_bytes(),
            "disabled entry motion settles immediately"
        );
    }
    println!(
        "Synthetic Grok subscription layout and interaction checks: {}",
        output.display()
    );
    Ok(())
}

pub(super) fn render_subscription_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    switchx::code_highlight::connect(app);
    app.set_active_page(1);
    app.set_subscription_id("preview-subscription".into());
    app.set_subscription_name("ChatGPT · 工作账号".into());
    app.set_subscription_account_ids(ModelRc::new(VecModel::from(vec![
        "".into(),
        "synthetic-account".into(),
    ])));
    app.set_subscription_account_options(ModelRc::new(VecModel::from(vec![
        "跟随 Codex 登录".into(),
        "工作账号 · Plus".into(),
    ])));
    app.set_subscription_binding_label(
        "跟随所选 Codex 目录的登录；也可选择保存账号或粘贴完整 auth.json。".into(),
    );
    app.set_edit_icon_name("OpenAI".into());
    app.set_edit_icon(provider_icons::load_image(
        provider_icons::icon("openai").unwrap(),
    )?);
    app.set_edit_use_common_config(true);
    app.set_edit_context_1m(true);
    app.set_edit_compact_limit("900000".into());
    app.set_edit_config_preview(
        concat!(
            "model = \"synthetic-codex\"\n",
            "model_reasoning_effort = \"high\"\n",
            "model_context_window = 1000000\n",
            "model_auto_compact_token_limit = 900000\n\n",
            "web_search = \"live\"\n",
            "service_tier = \"default\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n"
        )
        .into(),
    );
    const SYNTHETIC_AUTH: &str = "{\n  \"auth_mode\": \"chatgpt\",\n  \"tokens\": {\n    \"access_token\": \"synthetic-preview-only\"\n  }\n}";
    let weak = app.as_weak();
    app.on_load_subscription_auth(move || {
        weak.upgrade()
            .unwrap()
            .set_subscription_auth_json(SYNTHETIC_AUTH.into());
    });
    let weak = app.as_weak();
    app.on_cancel_subscription_editor(move || {
        let app = weak.upgrade().unwrap();
        app.set_subscription_editor_open(false);
        app.set_subscription_auth_json("".into());
    });
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_subscription(move |id, name, account| {
        assert_eq!(id, "preview-subscription");
        assert_eq!(name, "ChatGPT · 工作账号");
        assert_eq!(account, "synthetic-account");
        saved.set(saved.get() + 1);
    });
    let formats = Rc::new(Cell::new(0));
    let formatted = formats.clone();
    app.on_format_subscription_auth(move || formatted.set(formatted.get() + 1));
    let commons = Rc::new(Cell::new(0));
    let common = commons.clone();
    app.on_update_subscription_common(move || common.set(common.get() + 1));
    let contexts = Rc::new(Cell::new(0));
    let context = contexts.clone();
    app.on_update_subscription_context(move || context.set(context.get() + 1));
    let icons = Rc::new(Cell::new(0));
    let icon = icons.clone();
    app.on_open_provider_icon_picker(move || icon.set(icon.get() + 1));
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_provider(move |id| {
        assert_eq!(id, "preview-subscription");
        deleted.set(deleted.get() + 1);
    });
    let models = Rc::new(Cell::new(0));
    let model = models.clone();
    app.on_begin_connection_models(move |id| {
        assert_eq!(id, "preview-subscription");
        model.set(model.get() + 1);
    });
    for (width, height) in [(1200, 820), (1000, 680), (1600, 1000)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let left = (width as f32 - 1120.0).max(60.0) + 26.0;
        let right = left + 312.0;
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_delete_confirm(false);
            app.set_edit_use_common_config(true);
            app.set_edit_context_1m(true);
            app.set_subscription_account_choice(0);
            app.set_subscription_binding_label(
                "跟随所选 Codex 目录的登录；也可选择保存账号或粘贴完整 auth.json。".into(),
            );
            app.set_subscription_editor_open(true);
            snapshot(window, output, &format!("subscription-editor-{suffix}"))?;

            // A credential draft cannot change the public configuration view.
            let public = headless::render(window);
            app.set_subscription_auth_json("synthetic-hidden-credential".into());
            assert!(
                public.as_bytes() == headless::render(window).as_bytes(),
                "credentials stay out of the configuration view"
            );
            app.set_subscription_auth_json(SYNTHETIC_AUTH.into());

            let count = icons.get();
            headless::click(window, left + 40.0, 180.0);
            assert_eq!(icons.get(), count + 1, "the avatar opens the icon picker");
            window.dispatch_event(WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(10.0, 10.0),
            });
            headless::click(window, width as f32 - 126.0, 164.0);
            snapshot(window, output, &format!("subscription-options-{suffix}"))?;
            let count = commons.get();
            headless::click(window, right + 70.0, 273.0);
            assert!(!app.get_edit_use_common_config());
            assert_eq!(
                commons.get(),
                count + 1,
                "the switch updates the configuration"
            );
            headless::click(window, right + 70.0, 273.0);
            let count = contexts.get();
            let context_x = right + (width as f32 - 26.0 - right) / 2.0 + 70.0;
            headless::click(window, context_x, 273.0);
            assert!(!app.get_edit_context_1m());
            assert_eq!(contexts.get(), count + 1);
            snapshot(
                window,
                output,
                &format!("subscription-context-off-{suffix}"),
            )?;
            headless::click(window, context_x, 273.0);
            headless::click(window, width as f32 - 126.0, 164.0);
            render(window);

            headless::click(window, right + 195.0, 215.0);
            snapshot(window, output, &format!("subscription-auth-{suffix}"))?;
            let count = formats.get();
            headless::click(window, width as f32 - 110.0, 164.0);
            assert_eq!(formats.get(), count + 1, "JSON formatting stays reachable");
            app.set_subscription_auth_error("合成 JSON 错误：请检查格式。".into());
            snapshot(window, output, &format!("subscription-auth-error-{suffix}"))?;
            let count = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count, "invalid JSON cannot be saved");
            app.set_subscription_auth_error("".into());
            headless::click(window, right + 65.0, 215.0);
            render(window);

            headless::click(window, width as f32 - 44.0, 164.0);
            snapshot(window, output, &format!("subscription-expanded-{suffix}"))?;
            headless::click(window, width as f32 - 44.0, 164.0);
            render(window);
            app.set_subscription_account_choice(1);
            app.set_subscription_binding_label(
                "绑定到已保存的工作账号；发布路由时核对账号。".into(),
            );
            snapshot(window, output, &format!("subscription-bound-{suffix}"))?;
            let count = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save uses the selected account at every size"
            );
            app.set_config_managed(true);
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "managed configuration prevents saving"
            );
            app.set_config_managed(false);
            app.set_edit_config_error("合成配置错误：压缩阈值需要修正。".into());
            headless::click(window, width as f32 - 126.0, 164.0);
            snapshot(
                window,
                output,
                &format!("subscription-config-error-{suffix}"),
            )?;
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "invalid TOML cannot be saved");
            app.set_edit_config_error("".into());
            app.set_subscription_name("".into());
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(saves.get(), count + 1, "a connection needs a name");
            app.set_subscription_name("ChatGPT · 工作账号".into());
            let count = deletes.get();
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert!(app.get_delete_confirm());
            assert_eq!(deletes.get(), count, "delete requires confirmation");
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert_eq!(deletes.get(), count + 1);
            if height >= 820 {
                let count = models.get();
                headless::click(window, left + 115.0, 646.0);
                assert_eq!(
                    models.get(),
                    count + 1,
                    "the model directory shortcut works"
                );
            }
            app.set_subscription_auth_json("synthetic-unsaved-credential".into());
            headless::click(window, width as f32 - 202.0, height as f32 - 46.0);
            assert!(
                !app.get_subscription_editor_open(),
                "cancel stays reachable"
            );
            assert!(app.get_subscription_auth_json().is_empty());
            render(window);
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.global::<Theme>().set_animations_enabled(true);
    app.set_subscription_editor_open(true);
    play(
        window,
        output,
        "subscription-enter",
        &[0, 64, 144, 256, 416, 640, 960],
    )?;
    window.dispatch_event(WindowEvent::PointerMoved {
        position: slint::LogicalPosition::new(150.0, 180.0),
    });
    play(window, output, "subscription-avatar", &[0, 80, 160, 320])?;
    headless::click(window, 590.0, 215.0);
    play(window, output, "subscription-tabs", &[0, 64, 144, 256, 416])?;
    headless::click(window, 460.0, 215.0);
    play(
        window,
        output,
        "subscription-tabs-return",
        &[0, 64, 144, 416],
    )?;
    headless::click(window, 1074.0, 164.0);
    play(
        window,
        output,
        "subscription-options",
        &[0, 64, 144, 256, 416],
    )?;
    headless::click(window, 1074.0, 164.0);
    play(
        window,
        output,
        "subscription-options-close",
        &[0, 64, 144, 416],
    )?;
    headless::click(window, 1156.0, 164.0);
    let first = headless::render(window);
    play(
        window,
        output,
        "subscription-expand",
        &[0, 64, 144, 256, 416, 640],
    )?;
    let last = headless::render(window);
    assert!(
        first.as_bytes() != last.as_bytes(),
        "the editor width animates"
    );
    // Reverse twice during the transition and compare with a clean settle.
    headless::click(window, 1156.0, 164.0);
    headless::render(window);
    frame(96);
    headless::click(window, 1156.0, 164.0);
    headless::render(window);
    frame(640);
    assert!(
        last.as_bytes() == headless::render(window).as_bytes(),
        "an interrupted transition settles to the same layout"
    );
    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        headless::click(window, 1156.0, 164.0);
        let immediate = headless::render(window);
        frame(640);
        assert!(
            immediate.as_bytes() == headless::render(window).as_bytes(),
            "disabled motion settles immediately"
        );
        app.set_subscription_editor_open(false);
        headless::render(window);
        app.set_subscription_editor_open(true);
        let immediate = headless::render(window);
        frame(960);
        assert!(
            immediate.as_bytes() == headless::render(window).as_bytes(),
            "disabled entry motion settles immediately"
        );
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);
    app.set_subscription_editor_open(false);
    app.set_subscription_id("".into());
    app.set_subscription_name("ChatGPT · 新连接".into());
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            app.set_subscription_editor_open(true);
            snapshot(
                window,
                output,
                &format!(
                    "subscription-new-{}-{width}x{height}",
                    if dark { "dark" } else { "light" }
                ),
            )?;
            app.set_subscription_editor_open(false);
        }
    }
    Ok(())
}

pub(super) fn render_common_config(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    switchx::code_highlight::connect(app);
    app.set_active_page(1);
    app.set_common_config_saved(
        concat!(
            "# Shared across your Codex connections\n",
            "web_search = \"live\"\n",
            "model_reasoning_effort = \"high\"\n",
            "service_tier = \"default\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n\n",
            "[history]\n",
            "persistence = \"save-all\"\n\n",
            "[shell_environment_policy]\n",
            "inherit = \"core\"\n",
            "exclude = [\"EXAMPLE_PRIVATE_*\"]\n"
        )
        .into(),
    );
    app.set_common_config_draft(app.get_common_config_saved());
    app.set_common_config_current_source("连接表单当前 config.toml".into());
    let native = std::env::args().any(|arg| arg == "--common-config-native");
    let saves = Rc::new(Cell::new(0));
    let extracts = Rc::new(Cell::new(0));
    let saved = saves.clone();
    let weak = app.as_weak();
    app.on_save_common_config(move || {
        saved.set(saved.get() + 1);
        if let Some(app) = weak.upgrade() {
            app.set_busy(true);
            if native {
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_millis(900), move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_common_config_saved(app.get_common_config_draft());
                        app.set_busy(false);
                        app.set_common_config_editor_open(false);
                    }
                });
            }
        }
    });
    let extracted = extracts.clone();
    let weak = app.as_weak();
    app.on_extract_common_config(move || {
        extracted.set(extracted.get() + 1);
        if let Some(app) = weak.upgrade() {
            app.set_busy(true);
            if native {
                let weak = app.as_weak();
                slint::Timer::single_shot(Duration::from_millis(1100), move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_common_config_draft(app.get_common_config_saved());
                        app.set_busy(false);
                        app.set_common_config_message(
                            "已从合成连接表单提取并保存；取消不会撤销此次提取。".into(),
                        );
                    }
                });
            }
        }
    });
    let weak = app.as_weak();
    app.on_cancel_common_config(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_busy()
        {
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_error("".into());
            app.set_common_config_message("".into());
            app.set_common_config_editor_open(false);
        }
    });
    if native {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(true);
        app.set_common_config_editor_open(true);
        return Ok(app.run()?);
    }
    for (width, height) in [(1200, 820), (1000, 680)] {
        app.window().set_size(PhysicalSize::new(width, height));
        for dark in [false, true] {
            set_theme(app, window, dark);
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_editor_open(true);
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            snapshot(window, output, &format!("common-config-{suffix}"))?;
            let left = width as f32 - 920.0 + 26.0;
            let help_y = if height < 760 { 230.0 } else { 258.0 };
            headless::click(window, width as f32 - 80.0, help_y);
            snapshot(window, output, &format!("common-help-{suffix}"))?;
            headless::click(window, width as f32 - 80.0, help_y);
            render(window);
            headless::click(window, width as f32 - 56.0, help_y + 60.0);
            snapshot(window, output, &format!("common-focus-{suffix}"))?;
            headless::click(
                window,
                width as f32 - 56.0,
                help_y + 60.0 - if height < 760 { 124.0 } else { 148.0 },
            );
            render(window);
            // Exercise native input focus and the text binding, then save.
            headless::click(window, left + 140.0, help_y + 110.0);
            window.dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
            assert_ne!(
                app.get_common_config_draft(),
                app.get_common_config_saved(),
                "the editor must accept keyboard input"
            );
            snapshot(window, output, &format!("common-dirty-{suffix}"))?;
            let count = saves.get();
            headless::click(window, width as f32 - 89.0, height as f32 - 47.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save remains reachable at every size"
            );
            assert!(app.get_busy());
            snapshot(window, output, &format!("common-saving-{suffix}"))?;
            headless::click(window, width as f32 - 89.0, height as f32 - 47.0);
            headless::click(window, left + 93.0, height as f32 - 47.0);
            window.dispatch_event(WindowEvent::KeyPressed {
                text: slint::platform::Key::Escape.into(),
            });
            assert!(
                app.get_common_config_editor_open(),
                "busy operations cannot be dismissed"
            );
            assert_eq!(
                saves.get(),
                count + 1,
                "busy operations cannot be submitted twice"
            );
            app.set_busy(false);

            let count = extracts.get();
            headless::click(window, left + 93.0, height as f32 - 47.0);
            assert_eq!(
                extracts.get(),
                count + 1,
                "extract remains reachable at every size"
            );
            snapshot(window, output, &format!("common-extracting-{suffix}"))?;
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_busy(false);
            app.set_common_config_message("已从连接表单提取并保存；取消不会撤销此次提取。".into());
            snapshot(window, output, &format!("common-extracted-{suffix}"))?;
            app.set_common_config_message("".into());
            app.set_common_config_error("TOML 第 3 行缺少右引号，请检查后重新保存。".into());
            snapshot(window, output, &format!("common-error-{suffix}"))?;
            app.set_common_config_error("".into());
            app.set_config_managed(true);
            snapshot(window, output, &format!("common-managed-{suffix}"))?;
            let saved = saves.get();
            let extracted = extracts.get();
            headless::click(window, width as f32 - 89.0, height as f32 - 47.0);
            headless::click(window, left + 93.0, height as f32 - 47.0);
            assert_eq!(saves.get(), saved, "managed configuration cannot be saved");
            assert_eq!(
                extracts.get(),
                extracted,
                "managed configuration cannot be extracted"
            );
            app.set_config_managed(false);
            app.set_common_config_draft("".into());
            snapshot(window, output, &format!("common-empty-{suffix}"))?;
            headless::click(window, width as f32 - 205.0, height as f32 - 47.0);
            assert!(!app.get_common_config_editor_open());
            assert_eq!(
                app.get_common_config_draft(),
                app.get_common_config_saved(),
                "cancel discards only the manual draft"
            );
            app.set_common_config_editor_open(false);
            render(window);
        }
    }

    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.global::<Theme>().set_animations_enabled(true);
    headless::render(window);
    app.set_common_config_editor_open(true);
    let first = headless::render(window);
    let mut elapsed = 0;
    for at in [0, 16, 48, 96, 160, 240, 340, 480, 640] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("common-enter-{at:03}ms"))?;
    }
    assert_ne!(
        first.as_bytes(),
        headless::render(window).as_bytes(),
        "the drawer and its sections must animate in"
    );
    headless::click(window, 1120.0, 258.0);
    headless::render(window);
    frame(96);
    snapshot_now(window, output, "common-help-opening-096ms")?;
    // Reverse twice before settling; controls must stay reachable.
    headless::click(window, 1120.0, 258.0);
    headless::render(window);
    frame(48);
    headless::click(window, 1120.0, 258.0);
    headless::render(window);
    frame(480);
    snapshot_now(window, output, "common-help-interrupted-settled")?;
    app.set_common_config_editor_open(false);
    headless::render(window);
    frame(96);
    snapshot_now(window, output, "common-exit-096ms")?;
    app.set_common_config_editor_open(true);
    headless::render(window);
    frame(640);
    snapshot_now(window, output, "common-reopened")?;

    for reduced in [false, true] {
        app.set_common_config_editor_open(false);
        frame(640);
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        app.set_common_config_editor_open(true);
        let still = headless::render(window);
        frame(1500);
        assert_eq!(
            still.as_bytes(),
            headless::render(window).as_bytes(),
            "disabled or reduced motion settles immediately, including the floating artwork"
        );
        snapshot_now(
            window,
            output,
            if reduced {
                "common-system-reduced-motion"
            } else {
                "common-motion-disabled"
            },
        )?;
        app.set_common_config_editor_open(false);
        headless::render(window);
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(true);
    headless::render(window);
    // A deterministic animation reel for visual review, using only fixtures.
    for index in 0..120 {
        match index {
            1 => app.set_common_config_editor_open(true),
            24 | 44 => headless::click(window, 1120.0, 258.0),
            60 => headless::click(window, 1144.0, 318.0),
            80 => app.set_common_config_draft(
                format!(
                    "{}\n# A fresh shared preference\n",
                    app.get_common_config_saved()
                )
                .into(),
            ),
            90 => headless::click(window, 1111.0, 773.0),
            105 => {
                app.set_busy(false);
                app.set_common_config_editor_open(false);
            }
            _ => {}
        }
        snapshot_now(window, output, &format!("common-reel-{index:03}"))?;
        frame(40);
    }
    println!(
        "Synthetic shared configuration previews: {}",
        output.display()
    );
    Ok(())
}

pub(super) fn render_provider_editor(
    app: &AppWindow,
    window: &MinimalSoftwareWindow,
    output: &Path,
) -> Result<(), Box<dyn Error>> {
    switchx::code_highlight::connect(app);
    app.set_active_page(1);
    app.set_edit_id("preview-api".into());
    app.set_edit_name("Podlink".into());
    app.set_edit_url("https://api.example.invalid/v1".into());
    app.set_edit_model("gpt-6-luna".into());
    app.set_edit_icon_name("Anthropic".into());
    app.set_edit_icon(provider_icons::load_image(
        provider_icons::icon("anthropic").unwrap(),
    )?);
    app.set_edit_use_common_config(true);
    app.set_edit_context_1m(true);
    app.set_edit_compact_limit("900000".into());
    app.set_edit_config_preview(
        concat!(
            "model_provider = \"switchx_direct_preview\"\n",
            "model = \"gpt-6-luna\"\n",
            "web_search = \"live\"\n",
            "model_context_window = 1000000\n",
            "model_auto_compact_token_limit = 900000\n\n",
            "approval_policy = \"never\"\n",
            "sandbox_mode = \"danger-full-access\"\n",
            "model_reasoning_effort = \"high\"\n",
            "service_tier = \"default\"\n\n",
            "[model_providers.switchx_direct_preview]\n",
            "name = \"Podlink\"\n",
            "base_url = \"https://api.example.invalid/v1\"\n",
            "wire_api = \"responses\"\n\n",
            "[features]\n",
            "shell_tool = true\n",
            "apply_patch_freeform = true\n"
        )
        .into(),
    );
    if std::env::args().any(|arg| arg == "--provider-editor-native") {
        app.window().set_size(PhysicalSize::new(1200, 820));
        app.global::<Theme>().set_animations_enabled(true);
        app.invoke_set_appearance(true);
        app.set_editor_open(true);
        return Ok(app.run()?);
    }
    let saves = Rc::new(Cell::new(0));
    let saved = saves.clone();
    app.on_save_provider(move |_, _, _, _, _| saved.set(saved.get() + 1));
    let deletes = Rc::new(Cell::new(0));
    let deleted = deletes.clone();
    app.on_delete_provider(move |_| deleted.set(deleted.get() + 1));
    let fetches = Rc::new(Cell::new(0));
    let fetched = fetches.clone();
    app.on_fetch_models(move |_| fetched.set(fetched.get() + 1));
    for (width, height) in [(1200, 820), (1000, 680), (1600, 1000)] {
        app.window().set_size(PhysicalSize::new(width, height));
        let left = (width as f32 - 1120.0).max(60.0) + 26.0;
        let right = left + 312.0;
        for dark in [false, true] {
            let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
            set_theme(app, window, dark);
            app.set_delete_confirm(false);
            app.set_editor_open(true);
            snapshot(window, output, &format!("provider-editor-{suffix}"))?;

            // Configuration preferences expand without moving the footer.
            headless::click(window, width as f32 - 126.0, 164.0);
            render(window);
            headless::click(window, right + 72.0, 239.0);
            assert!(
                !app.get_edit_use_common_config(),
                "whole-row switch must toggle"
            );
            headless::click(window, right + 72.0, 239.0);
            assert!(app.get_edit_use_common_config());
            headless::click(window, right + 70.0, 292.0);
            assert!(!app.get_edit_context_1m(), "context control must toggle");
            snapshot(window, output, &format!("provider-context-off-{suffix}"))?;
            headless::click(window, right + 70.0, 292.0);
            assert!(app.get_edit_context_1m());
            snapshot(window, output, &format!("provider-options-{suffix}"))?;
            headless::click(window, width as f32 - 126.0, 164.0);
            render(window);

            headless::click(window, width as f32 - 44.0, 164.0);
            snapshot(window, output, &format!("provider-expanded-{suffix}"))?;
            headless::click(window, width as f32 - 44.0, 164.0);
            render(window);

            if width == 1200 {
                let count = fetches.get();
                headless::click(window, left + 136.0, 588.0);
                assert_eq!(
                    fetches.get(),
                    count + 1,
                    "model discovery must be reachable"
                );
            }
            app.set_edit_key("synthetic-clear-check".into());
            let count = saves.get();
            headless::click(window, width as f32 - 90.0, height as f32 - 46.0);
            assert_eq!(
                saves.get(),
                count + 1,
                "save must remain visible at every size"
            );
            assert!(
                app.get_edit_key().is_empty(),
                "saving clears the credential draft"
            );
            app.set_delete_confirm(false);
            let count = deletes.get();
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert!(app.get_delete_confirm());
            assert_eq!(deletes.get(), count, "delete still requires a second click");
            headless::click(window, left + 56.0, height as f32 - 46.0);
            assert_eq!(deletes.get(), count + 1);
            app.set_edit_key("synthetic-clear-check".into());
            headless::click(window, width as f32 - 202.0, height as f32 - 46.0);
            assert!(!app.get_editor_open(), "cancel must remain reachable");
            assert!(
                app.get_edit_key().is_empty(),
                "cancelling clears the credential draft"
            );
            render(window);
        }
    }

    // Capture a reversible width transition and verify reduced-motion settling.
    app.window().set_size(PhysicalSize::new(1200, 820));
    set_theme(app, window, true);
    app.set_delete_confirm(false);
    app.set_editor_open(true);
    render(window);
    app.global::<Theme>().set_animations_enabled(true);
    headless::click(window, 1156.0, 164.0);
    let first = headless::render(window);
    let mut elapsed = 0;
    for at in [0, 16, 48, 96, 160, 240, 340, 480] {
        frame(at - elapsed);
        elapsed = at;
        snapshot_now(window, output, &format!("provider-expand-{at:03}ms"))?;
    }
    let last = headless::render(window);
    assert_ne!(first.as_bytes(), last.as_bytes(), "expansion must animate");
    headless::click(window, 1156.0, 164.0);
    headless::render(window);
    frame(96);
    headless::click(window, 1156.0, 164.0);
    headless::render(window);
    frame(480);
    assert_eq!(
        last.as_bytes(),
        headless::render(window).as_bytes(),
        "interrupted expansion settles to the same layout"
    );
    for reduced in [false, true] {
        app.global::<Theme>().set_animations_enabled(reduced);
        app.global::<Theme>().set_system_reduced_motion(reduced);
        headless::click(window, 1156.0, 164.0);
        let immediate = headless::render(window);
        frame(600);
        assert_eq!(
            immediate.as_bytes(),
            headless::render(window).as_bytes(),
            "disabled motion settles immediately"
        );
    }
    app.global::<Theme>().set_system_reduced_motion(false);
    app.global::<Theme>().set_animations_enabled(false);
    headless::click(window, 1156.0, 164.0);
    render(window);
    app.set_edit_config_error("合成配置错误：请检查压缩阈值。".into());
    snapshot(window, output, "provider-config-error-dark")?;
    let count = saves.get();
    headless::click(window, 1110.0, 774.0);
    assert_eq!(saves.get(), count, "invalid configuration cannot be saved");
    app.set_edit_config_error("".into());
    app.set_editor_open(false);
    render(window);

    app.set_edit_id("".into());
    app.set_edit_key("synthetic-new-connection".into());
    app.set_editor_open(true);
    render(window);
    let rows = batch_rows("");
    app.set_batch_summary(summarize(&rows));
    app.set_batch_models(ModelRc::new(VecModel::from(rows)));
    snapshot(window, output, "provider-new-discovered-dark")?;
    app.set_editor_open(false);
    println!("Synthetic API editor previews: {}", output.display());
    Ok(())
}
