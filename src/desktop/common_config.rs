//! The shared Codex configuration editor.

use std::path::Path;

use slint::ComponentHandle;
use switchx::{
    app,
    codex::provider_config::{self, CodexOptions},
    ui::AppWindow,
};
use tokio::sync::mpsc;

use super::{
    codex_config::codex_home,
    command::{Command, queue, queue_on},
    feedback::show_action,
    provider_editor::update_provider_config_preview,
    subscription_editor::set_subscription_config,
    worker::Worker,
};

pub(super) fn replace_common_config(app: &AppWindow, common: String) {
    let previous = app.get_common_config_saved();
    let draft = if app.get_subscription_editor_open()
        && app.get_edit_use_common_config()
        && previous.as_str() != common
    {
        Some(
            provider_config::set_subscription_common(
                app.get_edit_config_preview().as_str(),
                previous.as_str(),
                false,
            )
            .and_then(|config| {
                provider_config::subscription_editor_config(
                    "",
                    &CodexOptions {
                        config_toml: Some(config),
                        ..CodexOptions::default()
                    },
                    &common,
                )
            }),
        )
    } else {
        None
    };
    app.set_common_config_saved(common.into());
    if let Some(draft) = draft {
        set_subscription_config(app, draft);
    } else {
        update_provider_config_preview(app);
    }
}

pub(super) fn common_config_for_home(data_dir: &Path, target: &str) -> Result<String, String> {
    match codex_home(target).and_then(|target| app::initialize_common_config(data_dir, &target)) {
        Ok(common) => Ok(common),
        Err(error) => {
            eprintln!("未自动提取 Codex 通用配置：{error}");
            app::load_common_config(data_dir)
        }
    }
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    app.on_open_common_config(queue_on(app, sender, |app| {
        Command::OpenCommonConfig(app.get_config_home().into())
    }));
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_extract_common_config(move || {
        if let Some(app) = weak.upgrade() {
            update_provider_config_preview(&app);
            if !app.get_edit_config_error().is_empty() {
                let error = app.get_edit_config_error().to_string();
                app.set_common_config_error(error.clone().into());
                show_action(&app, Err(error));
                return;
            }
            if app.get_edit_config_preview().trim().is_empty() {
                let error = "供应商配置预览为空，请先检查表单内容".to_owned();
                app.set_common_config_error(error.clone().into());
                show_action(&app, Err(error));
                return;
            }
            queue(
                &app,
                &callback_sender,
                Command::ExtractCommonConfig(app.get_edit_config_preview().into()),
            );
        }
    });
    app.on_save_common_config(queue_on(app, sender, |app| {
        Command::SaveCommonConfig(app.get_common_config_draft().into())
    }));
    let weak = app.as_weak();
    app.on_cancel_common_config(move || {
        if let Some(app) = weak.upgrade() {
            if app.get_busy() {
                return;
            }
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_error("".into());
            app.set_common_config_message("".into());
            app.set_common_config_editor_open(false);
        }
    });
}

impl Worker {
    pub(super) fn open_common_config(&self, target: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|path| common_config_for_home(path, &target));
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_busy(false);
            match result {
                Ok(common) => {
                    replace_common_config(&app, common.clone());
                    app.set_common_config_draft(common.into());
                    app.set_common_config_error("".into());
                    app.set_common_config_message("".into());
                    app.set_common_config_current_source("供应商表单当前 config.toml".into());
                    app.set_common_config_editor_open(true);
                }
                Err(error) => show_action(&app, Err(error)),
            }
        });
    }

    pub(super) fn extract_common_config(&mut self, form_toml: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|path| app::extract_and_save_common_config(path, &form_toml));
        if result.is_ok() {
            self.discard_previews();
        }
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_busy(false);
            match result {
                Ok(snippet) => {
                    app.set_common_config_current_source("供应商表单当前 config.toml".into());
                    replace_common_config(&app, snippet.clone());
                    app.set_common_config_draft(snippet.into());
                    app.set_common_config_error("".into());
                    app.set_common_config_message(
                        "已从供应商表单提取并保存通用配置；取消不会撤销这次提取。".into(),
                    );
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                    show_action(&app, Ok("已从供应商表单提取并保存 Codex 通用配置".into()));
                }
                Err(error) => {
                    app.set_common_config_error(error.clone().into());
                    show_action(&app, Err(error));
                }
            }
        });
    }

    pub(super) fn save_common_config(&mut self, snippet: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|path| app::save_common_config(path, &snippet));
        if result.is_ok() {
            self.discard_previews();
        }
        let _ =
            self.weak.upgrade_in_event_loop(move |app| {
                app.set_busy(false);
                match result {
                    Ok(()) => {
                        replace_common_config(&app, snippet);
                        app.set_common_config_error("".into());
                        app.set_common_config_editor_open(false);
                        app.set_direct_preview_ready(false);
                        app.set_route_preview_ready(false);
                        show_action(
                        &app,
                        Ok("Codex 通用配置已保存；勾选应用通用配置的供应商将在下次切换时使用。"
                            .into()),
                    );
                    }
                    Err(error) => app.set_common_config_error(error.into()),
                }
            });
    }
}
