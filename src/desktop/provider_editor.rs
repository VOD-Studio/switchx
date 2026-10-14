//! The API connection editor, its presets, and its Codex config preview.

use slint::{ComponentHandle, Model};
use switchx::{
    app::{self, load_snapshot},
    codex::{
        client,
        provider_config::{self, CodexOptions},
    },
    storage::{ProviderKind, ProviderRecord},
    ui::AppWindow,
};
use tokio::sync::mpsc;

use super::{
    batch_picker::picked_provider_models,
    codex_config::codex_home,
    command::{Command, ProviderForm, queue},
    common_config::common_config_for_home,
    connection_models::{directory_draft, first_directory_model},
    feedback::show_action,
    grok::{show_xai_accounts, xai_account_view},
    icon_picker::set_editor_provider_icon,
    open_url,
    provider_checks::set_provider_check_id,
    subscription_editor::set_subscription_config,
    workbench::{created_connection_id, mark_fresh_models, show_result},
    worker::Worker,
};

pub(super) fn editor_codex_options(app: &AppWindow) -> Result<CodexOptions, String> {
    let options = CodexOptions {
        remote_compaction: app.get_edit_remote_compaction(),
        use_common_config: app.get_edit_use_common_config(),
        context_1m: app.get_edit_context_1m(),
        compact_limit: if app.get_edit_context_1m() {
            app.get_edit_compact_limit()
                .trim()
                .parse()
                .map_err(|_| "压缩阈值须为小于 1000000 的正整数")?
        } else {
            900_000
        },
        config_toml: app
            .get_subscription_editor_open()
            .then(|| app.get_edit_config_preview().to_string()),
    };
    options.validate()?;
    Ok(options)
}

pub(super) fn update_provider_config_preview(app: &AppWindow) {
    if app.get_subscription_editor_open() {
        set_subscription_config(app, Ok(app.get_edit_config_preview().to_string()));
        return;
    }
    if !app.get_editor_open() {
        return;
    }
    let result = (|| {
        let options = editor_codex_options(app)?;
        let helper = std::env::current_exe().map_err(|_| "无法定位 SwitchX 凭据程序")?;
        provider_config::preview(
            app.get_edit_name().as_str(),
            app.get_edit_url().as_str(),
            app.get_edit_model().as_str(),
            app.get_edit_id().as_str(),
            &helper,
            &options,
            app.get_common_config_saved().as_str(),
        )
    })();
    match result {
        Ok(preview) => {
            app.set_edit_config_preview(preview.into());
            app.set_edit_config_error("".into());
        }
        Err(error) => {
            app.set_edit_config_error(error.into());
            app.set_edit_config_preview("".into());
        }
    }
}

pub(super) fn apply_provider_preset(app: &AppWindow, id: &str) {
    if app.get_busy() || app.get_config_managed() || !app.get_edit_id().is_empty() {
        return;
    }
    let preset = app::PROVIDER_PRESETS.iter().find(|preset| preset.id == id);
    if preset.is_none() && !id.is_empty() {
        return;
    }
    let brand = app
        .get_provider_presets()
        .iter()
        .find(|preset| preset.id == id)
        .unwrap_or_default();
    app.set_edit_key("".into());
    app.set_edit_preset_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_name(preset.map_or("", |preset| preset.name).into());
    app.set_edit_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_model(preset.map_or("", |preset| preset.model_id).into());
    app.set_edit_preset_id(brand.id);
    app.set_edit_preset_icon(brand.icon);
    app.set_edit_preset_monochrome(brand.monochrome);
    set_editor_provider_icon(app, "", ProviderKind::ApiKey, &app.get_edit_url());
}

pub(super) fn show_provider_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    options: CodexOptions,
    common: String,
) {
    app.invoke_reset_connection_draft();
    let selected_preset = (provider.is_none() && app.get_connection_picker_open())
        .then(|| app.get_connection_preset_id());
    app.set_editor_open(false);
    app.set_subscription_editor_open(false);
    app.set_xai_editor_open(false);
    app.set_common_config_editor_open(false);
    app.set_icon_picker_open(false);
    app.set_common_config_saved(common.into());
    let preset = provider
        .as_ref()
        .and_then(|provider| app::provider_preset(&provider.base_url));
    let brand = app
        .get_provider_presets()
        .iter()
        .find(|row| preset.is_some_and(|preset| row.id == preset.id))
        .unwrap_or_default();
    app.set_edit_id(
        provider
            .as_ref()
            .map_or("", |provider| provider.id.as_str())
            .into(),
    );
    app.set_edit_name(
        provider
            .as_ref()
            .map_or("", |provider| provider.name.as_str())
            .into(),
    );
    app.set_edit_preset_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_url(
        provider
            .as_ref()
            .map_or("", |provider| provider.base_url.as_str())
            .into(),
    );
    app.set_edit_model(
        provider
            .as_ref()
            .map_or("", |provider| provider.model_id.as_str())
            .into(),
    );
    app.set_edit_key("".into());
    app.set_edit_preset_id(brand.id);
    app.set_edit_preset_icon(brand.icon);
    app.set_edit_preset_monochrome(brand.monochrome);
    set_editor_provider_icon(
        app,
        provider
            .as_ref()
            .and_then(|provider| provider.icon_id.as_deref())
            .unwrap_or(""),
        ProviderKind::ApiKey,
        provider
            .as_ref()
            .map_or("", |provider| provider.base_url.as_str()),
    );
    app.set_edit_remote_compaction(options.remote_compaction);
    app.set_edit_use_common_config(options.use_common_config);
    app.set_edit_context_1m(options.context_1m);
    app.set_edit_compact_limit(options.compact_limit.to_string().into());
    app.set_edit_config_error("".into());
    app.set_delete_confirm(false);
    app.set_busy(false);
    app.set_editor_open(true);
    app.set_connection_picker_open(false);
    if let Some(preset) = selected_preset {
        apply_provider_preset(app, &preset);
    }
    update_provider_config_preview(app);
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_provider_editor(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::BeginProviderEditor {
                    id: id.into(),
                    home: app.get_config_home().into(),
                },
            );
        }
    });
    let weak = app.as_weak();
    app.on_update_provider_config_preview(move || {
        if let Some(app) = weak.upgrade() {
            update_provider_config_preview(&app);
        }
    });
    let weak = app.as_weak();
    app.on_open_provider_preset_link(move |api_key| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() {
            return;
        }
        let Some(preset) = app::PROVIDER_PRESETS
            .iter()
            .find(|preset| preset.id == app.get_edit_preset_id().as_str())
        else {
            return;
        };
        let url = if api_key {
            preset.api_key_url
        } else {
            preset.website_url
        };
        show_action(
            &app,
            open_url(url).map(|()| "已在浏览器打开供应商页面".into()),
        );
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_provider(move |id, name, url, model, key| {
        if let Some(app) = weak.upgrade() {
            // A new connection saved from its directory may not have picked a default yet.
            let model = match directory_draft(&app).and(first_directory_model(&app)) {
                Some(first) if id.is_empty() && model.trim().is_empty() => {
                    app.set_edit_model(first.as_str().into());
                    first.into()
                }
                _ => model,
            };
            let options = match editor_codex_options(&app) {
                Ok(options) => options,
                Err(error) => {
                    app.set_edit_config_error(error.clone().into());
                    show_action(&app, Err(error));
                    return;
                }
            };
            queue(
                &app,
                &callback_sender,
                Command::Save(ProviderForm {
                    id: id.into(),
                    name: name.into(),
                    url: url.into(),
                    model: model.into(),
                    key: key.into(),
                    options,
                    icon_id: app.get_edit_icon_id().into(),
                    picked: picked_provider_models(&app),
                    directory: directory_draft(&app),
                }),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_delete_provider(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::Delete(id.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_import_current(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::ImportCurrent(home.into()));
        }
    });
}

impl Worker {
    pub(super) fn begin_provider_editor(&self, id: String, target: String) {
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            let common = common_config_for_home(data_dir, &target)?;
            let (provider, options) = if id.is_empty() {
                (None, CodexOptions::for_common(&common)?)
            } else {
                let provider = app::load_provider(data_dir, &id)?;
                if provider.kind != ProviderKind::ApiKey {
                    return Err("请使用订阅连接编辑器修改名称和账号绑定".into());
                }
                let state = app::load_provider_config(data_dir, &id)?;
                let options = state
                    .options
                    .map(Ok)
                    .unwrap_or_else(|| CodexOptions::for_common(&common))?;
                (Some(provider), options)
            };
            Ok::<_, String>((provider, options, common))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok((provider, options, common)) => {
                show_provider_editor(&app, provider, options, common)
            }
            Err(error) => show_action(&app, Err(error)),
        });
    }

    pub(super) fn save_provider(&mut self, form: ProviderForm) {
        let ProviderForm {
            id,
            name,
            url,
            model,
            key,
            options,
            icon_id,
            picked,
            directory,
        } = form;
        let result = self.data_dir.path().and_then(|path| {
            if let Some(directory) = &directory {
                if !id.is_empty() {
                    return Err("已保存连接请使用模型目录编辑器".into());
                }
                return app::save_provider_with_directory(
                    path,
                    &name,
                    &url,
                    &model,
                    key,
                    &options,
                    Some(&icon_id),
                    directory,
                );
            }
            app::save_provider_with_mappings(
                path,
                (!id.is_empty()).then_some(id.as_str()),
                &name,
                &url,
                &model,
                key,
                &options,
                Some(&icon_id),
                &picked,
            )
        });
        if result.is_ok() {
            self.discard_previews();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            let created_connection = snapshot
                .as_ref()
                .and_then(|snapshot| created_connection_id(&app, snapshot));
            let created = result.clone().unwrap_or_default();
            show_action(
                &app,
                result.map(|created| match created.len() {
                    0 => "上游与 API Key 已保存到本机".into(),
                    count => format!("上游与 API Key 已保存到本机，并添加 {count} 个模型映射"),
                }),
            );
            if let Some(snapshot) = snapshot {
                set_provider_check_id(&app, &id, "");
                show_result(&app, snapshot);
                mark_fresh_models(&app, &created);
                app.set_editor_open(false);
                app.set_subscription_editor_open(false);
                app.set_edit_key("".into());
                app.invoke_reset_connection_draft();
                app.set_direct_preview_ready(false);
                app.set_route_preview_ready(false);
            }
            if let Some(id) = created_connection {
                app.invoke_begin_connection_models(id);
            }
        });
    }

    pub(super) fn delete_provider(&mut self, id: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|path| app::delete_provider(path, &id));
        let deleted = result.is_ok();
        if deleted {
            self.discard_previews();
        }
        let snapshot = self.data_dir.ok().map(|path| load_snapshot(path, false));
        let accounts = self.data_dir.ok().map(xai_account_view);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(view) = accounts {
                show_xai_accounts(&app, view);
            }
            if let Some(snapshot) = snapshot {
                show_result(&app, snapshot);
                if deleted {
                    app.set_editor_open(false);
                    app.set_xai_editor_open(false);
                    app.set_subscription_editor_open(false);
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                }
            }
            show_action(&app, result.map(|()| "上游已删除".into()));
        });
    }

    pub(super) fn import_current(&self, target: String) {
        let result = (|| {
            let candidate = client::import_candidate(&codex_home(&target)?)?;
            let data_dir = self.data_dir.path()?;
            let common = common_config_for_home(data_dir, &target)?;
            let options = CodexOptions::for_common(&common)?;
            Ok::<_, String>((candidate, options, common))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok((candidate, options, common)) => {
                let provider = ProviderRecord {
                    id: String::new(),
                    name: candidate.name,
                    base_url: candidate.base_url,
                    model_id: candidate.model_id,
                    credential_ref: None,
                    icon_id: None,
                    kind: ProviderKind::ApiKey,
                    account_binding: None,
                };
                show_provider_editor(&app, Some(provider), options, common);
                app.set_active_page(1);
                show_action(
                    &app,
                    Ok("已填入当前上游资料；请输入 API Key 后保存。原配置与凭据未改动".into()),
                );
            }
            Err(error) => show_action(&app, Err(error)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::{ModelRc, VecModel};
    use switchx::{
        credentials::Secret,
        headless,
        ui::{ProviderPresetRow, Theme},
    };

    use crate::desktop::{
        grok::show_xai_editor,
        icon_picker::initialize_provider_icons,
        subscription_editor::{close_subscription_editor, show_subscription_editor},
    };

    #[test]
    fn unified_connection_picker_routes_choices_and_clears_credentials() {
        use slint::platform::Key;
        use std::{cell::Cell, rc::Rc};

        let window = headless::install();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        app.set_active_page(1);
        initialize_provider_icons(&app).unwrap();
        app.set_provider_presets(ModelRc::new(VecModel::from(
            app::PROVIDER_PRESETS
                .iter()
                .map(|preset| ProviderPresetRow {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    icon: slint::Image::load_from_svg_data(preset.icon).unwrap(),
                    monochrome: preset.monochrome,
                })
                .collect::<Vec<_>>(),
        )));
        let calls = Rc::new(Cell::new(0));
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_provider_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_provider_editor(
                &weak.upgrade().unwrap(),
                None,
                CodexOptions::default(),
                String::new(),
            );
        });
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_subscription_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_subscription_editor(
                &weak.upgrade().unwrap(),
                None,
                Vec::new(),
                String::new(),
                CodexOptions::default(),
                String::new(),
                Secret::new(String::new()),
            );
        });
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_xai_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_xai_editor(&weak.upgrade().unwrap(), None, Vec::new());
        });
        let weak = app.as_weak();
        app.on_cancel_subscription_editor(move || {
            close_subscription_editor(&weak.upgrade().unwrap())
        });
        let click = |x, y| {
            headless::click(&window, x, y);
            slint::platform::update_timers_and_animations();
        };
        let escape = || {
            headless::key(&window, Key::Escape);
            slint::platform::update_timers_and_animations();
        };
        let draw = |name: &str| {
            headless::save_snapshot(
                "SWITCHX_CONNECTION_SNAPSHOTS",
                name,
                &headless::render(&window),
            );
        };
        app.show().unwrap();
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            let left = width as f32 - 734.0;
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                draw(&format!("connections-{suffix}"));
                click(width as f32 - 100.0, 242.0);
                assert!(app.get_connection_picker_open());
                draw(&format!("picker-{suffix}"));
                click(left + 90.0, 160.0);
                assert!(app.get_subscription_editor_open());
                assert!(!app.get_connection_picker_open());
                draw(&format!("chatgpt-{suffix}"));
                app.set_subscription_auth_json("synthetic-unsaved-credential".into());
                let subscription_left = (width as f32 - 1120.0).max(60.0) + 26.0;
                click(subscription_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                assert!(app.get_subscription_auth_json().is_empty());
                draw("back-from-chatgpt");
                click(left + 440.0, 160.0);
                assert!(app.get_xai_editor_open());
                draw(&format!("grok-{suffix}"));
                click(subscription_left + 50.0, 108.0);
                assert!(app.get_connection_picker_open());
                assert!(!app.get_xai_editor_open());
                draw("back-from-grok");
                click(left + 90.0, 280.0);
                assert!(app.get_editor_open());
                assert!(app.get_edit_url().is_empty());
                draw(&format!("custom-api-{suffix}"));
                app.set_edit_key("synthetic-unsaved-key".into());
                escape();
                assert!(!app.get_editor_open());
                assert!(app.get_edit_key().is_empty());
                for (index, preset) in app::PROVIDER_PRESETS.iter().enumerate() {
                    app.invoke_open_connection_picker();
                    draw("before-api-preset");
                    click(
                        left + 90.0 + (index % 2) as f32 * 350.0,
                        370.0 + (index / 2) as f32 * 90.0,
                    );
                    assert!(app.get_editor_open(), "{}", preset.id);
                    assert_eq!(app.get_edit_preset_id(), preset.id);
                    assert_eq!(app.get_edit_name(), preset.name);
                    assert_eq!(app.get_edit_url(), preset.base_url);
                    assert_eq!(app.get_edit_model(), preset.model_id);
                    if index == 0 {
                        draw(&format!("preset-api-{suffix}"));
                    }
                    app.set_edit_key("synthetic-unsaved-key".into());
                    draw("before-back-from-api");
                    let api_left = (width as f32 - 1120.0).max(60.0) + 26.0;
                    click(api_left + 50.0, 108.0);
                    assert!(app.get_connection_picker_open());
                    assert!(app.get_edit_key().is_empty());
                }
                escape();
                assert!(!app.get_connection_picker_open());
            }
        }
        app.invoke_open_connection_picker();
        draw("before-disabled-choices");
        let before = calls.get();
        for (busy, managed, pending) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            app.set_busy(busy);
            app.set_config_managed(managed);
            app.set_chatgpt_login_pending(pending);
            draw("disabled-choice");
            click(356.0, 160.0);
            assert_eq!(calls.get(), before);
        }
        app.set_busy(false);
        app.set_config_managed(false);
        app.set_chatgpt_login_pending(false);
        draw("before-scrim-close");
        click(80.0, 400.0);
        assert!(!app.get_connection_picker_open());
        app.invoke_open_connection_picker();
        draw("before-chatgpt-close");
        click(356.0, 160.0);
        app.set_subscription_auth_json("synthetic-unsaved-credential".into());
        escape();
        assert!(!app.get_subscription_editor_open());
        assert!(app.get_subscription_auth_json().is_empty());
        app.invoke_open_connection_picker();
        draw("before-grok-close");
        click(706.0, 160.0);
        draw("grok-before-close");
        click(964.0, 44.0);
        assert!(!app.get_xai_editor_open());
        for (busy, managed) in [(true, false), (false, true)] {
            app.set_busy(busy);
            app.set_config_managed(managed);
            app.invoke_open_connection_picker();
            assert!(!app.get_connection_picker_open());
        }
        app.set_busy(false);
        app.set_config_managed(false);
        app.invoke_open_connection_picker();
        app.set_active_page(5);
        draw("after-navigation");
        assert!(!app.get_connection_picker_open());
    }
}
