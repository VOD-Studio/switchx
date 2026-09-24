slint::include_modules!();

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app::{self, AppError, Snapshot, data_directory, load_snapshot},
    client,
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    storage::ProviderRecord,
};
use tokio::sync::mpsc::{self, error::TrySendError};

enum Command {
    Refresh(bool),
    Save {
        id: String,
        name: String,
        url: String,
        model: String,
        key: String,
    },
    Delete(String),
    Check(String),
    InspectDirect {
        id: String,
        home: String,
    },
    ApplyDirect(String),
    RestoreDirect(String),
    InspectConfig(String),
    ImportCurrent(String),
    CheckLogin(String),
}

fn matching_providers(providers: &ModelRc<ProviderRow>, query: &str) -> Vec<ProviderRow> {
    let query = query.trim().to_lowercase();
    providers
        .iter()
        .filter(|provider| provider.name.to_lowercase().contains(&query))
        .collect()
}

fn filter_providers(app: &AppWindow, query: &str) {
    let rows = matching_providers(&app.get_providers(), query);
    app.set_filtered_providers(ModelRc::new(VecModel::from(rows)));
}

fn show_result(app: &AppWindow, result: Result<Snapshot, AppError>) {
    app.set_loading(false);
    app.set_busy(false);
    match result {
        Ok(snapshot) => {
            let count = snapshot.providers.len();
            let rows = snapshot
                .providers
                .into_iter()
                .map(|provider| ProviderRow {
                    id: provider.id.into(),
                    name: provider.name.into(),
                    endpoint: provider.endpoint.into(),
                    base_url: provider.base_url.into(),
                    model_id: provider.model_id.into(),
                    credential_status: provider.credential_status.into(),
                })
                .collect::<Vec<_>>();
            app.set_providers(ModelRc::new(VecModel::from(rows)));
            filter_providers(app, &app.get_provider_query());
            app.set_status_text(
                format!(
                    "本地资料已读取 · {count} 个上游记录{}",
                    if snapshot.credentials_checked {
                        " · 凭据状态已检查"
                    } else {
                        " · 凭据尚未检查"
                    }
                )
                .into(),
            );
            app.set_error_code("".into());
            app.set_error_message("".into());
            app.set_error_action("".into());
        }
        Err(error) => {
            app.set_error_code(error.code().into());
            app.set_error_message(error.message().into());
            app.set_error_action(error.action().into());
            app.set_status_text("上次成功读取的记录仍保留在窗口中".into());
        }
    }
}

fn show_action(app: &AppWindow, result: Result<String, String>) {
    app.set_busy(false);
    match result {
        Ok(message) => {
            app.set_action_message(message.into());
            app.set_error_code("".into());
            app.set_error_message("".into());
            app.set_error_action("".into());
        }
        Err(message) => {
            app.set_error_code("operation_failed".into());
            app.set_error_message(message.into());
            app.set_error_action("检查输入或刷新后重试；发生配置冲突时先检查目标文件。".into());
        }
    }
}

fn queue(app: &AppWindow, sender: &mpsc::Sender<Command>, command: Command) {
    app.set_busy(true);
    if let Err(error) = sender.try_send(command) {
        app.set_busy(false);
        let message = match error {
            TrySendError::Full(_) => "操作仍在执行，请稍后重试",
            TrySendError::Closed(_) => "后台状态通道已停止",
        };
        show_action(app, Err(message.into()));
    }
}

fn home(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    client::config_path(&path)?;
    Ok(path)
}

fn credential(provider: &ProviderRecord) -> Result<switchx::credentials::Secret, String> {
    let reference = provider
        .credential_ref
        .as_deref()
        .ok_or("上游未配置 API Key")?;
    CredentialStore::new(PROVIDER_KEY_SERVICE)
        .and_then(|store| store.get(reference))
        .map_err(|_| "无法读取上游 API Key".into())
}

async fn worker(
    mut receiver: mpsc::Receiver<Command>,
    weak: slint::Weak<AppWindow>,
    directory: Result<PathBuf, AppError>,
) {
    let mut prepared: Option<(PreparedDirectSwitch, ProviderRecord)> = None;
    while let Some(command) = receiver.recv().await {
        match command {
            Command::Refresh(check_credentials) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| *error)
                    .and_then(|path| load_snapshot(path, check_credentials));
                let _ = weak.upgrade_in_event_loop(move |app| show_result(&app, result));
            }
            Command::Save {
                id,
                name,
                url,
                model,
                key,
            } => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| {
                        app::save_provider(
                            path,
                            (!id.is_empty()).then_some(id.as_str()),
                            &name,
                            &url,
                            &model,
                            key,
                        )
                    });
                if result.is_ok() {
                    prepared = None;
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_action(
                        &app,
                        result.map(|()| "上游已保存；API Key 未写入 SQLite".into()),
                    );
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        app.set_editor_open(false);
                        app.set_direct_preview_ready(false);
                    }
                });
            }
            Command::Delete(id) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::delete_provider(path, &id));
                let was_deleted =
                    result.is_ok() || result.as_ref().is_err_and(|error| error.contains("已删除"));
                if was_deleted {
                    prepared = None;
                }
                let snapshot = directory
                    .as_ref()
                    .ok()
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        if was_deleted {
                            app.set_editor_open(false);
                            app.set_direct_preview_ready(false);
                        }
                    }
                    show_action(&app, result.map(|()| "上游已删除".into()));
                });
            }
            Command::Check(id) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::load_provider(path, &id));
                let result = match result
                    .and_then(|provider| credential(&provider).map(|token| (provider, token)))
                {
                    Ok((provider, token)) => direct::check_models(&provider, token.expose())
                        .await
                        .map(|()| {
                            format!(
                                "{}：/models 已连通，目录包含 {}；Responses 工具调用尚未验证",
                                provider.name, provider.model_id
                            )
                        }),
                    Err(error) => Err(error),
                };
                let _ = weak.upgrade_in_event_loop(move |app| show_action(&app, result));
            }
            Command::InspectDirect { id, home: target } => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let provider = app::load_provider(data_dir, &id)?;
                    credential(&provider)?;
                    let target = client::config_path(&home(&target)?)?;
                    let helper = std::env::current_exe()
                        .map_err(|_| "无法定位 SwitchX credential helper")?;
                    let prepared =
                        PreparedDirectSwitch::inspect(&target, data_dir, &provider, &helper)?;
                    Ok::<_, String>((prepared, provider))
                })();
                match result {
                    Ok((switch, provider)) => {
                        let summary = format!(
                            "目标：{} · 模型：{} · 受管变更：{}",
                            provider.name,
                            provider.model_id,
                            switch.changes.join("、")
                        );
                        prepared = Some((switch, provider));
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            app.set_busy(false);
                            app.set_direct_preview(summary.into());
                            app.set_direct_preview_ready(true);
                            app.set_action_message("差异已准备；应用前将再次检查上游目录".into());
                        });
                    }
                    Err(error) => {
                        prepared = None;
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            app.set_direct_preview_ready(false);
                            show_action(&app, Err(error));
                        });
                    }
                }
            }
            Command::ApplyDirect(target) => {
                let result = match prepared.take() {
                    Some((switch, selected)) => {
                        let checked = (|| {
                            let target_path = client::config_path(&home(&target)?)?;
                            if target_path != switch.target() {
                                return Err("配置目录已变化，请重新预览".into());
                            }
                            let data_dir = directory.as_ref().map_err(|error| error.message())?;
                            let latest = app::load_provider(data_dir, &selected.id)?;
                            if latest != selected {
                                return Err("上游资料已变化，请重新预览".into());
                            }
                            let token = credential(&latest)?;
                            Ok::<_, String>((data_dir.clone(), latest, token))
                        })();
                        match checked {
                            Ok((data_dir, provider, token)) => {
                                match direct::check_models(&provider, token.expose()).await {
                                    Ok(()) => switch.apply().map(|()| {
                                        let status =
                                            client::inspect(Path::new(&target), &data_dir).ok();
                                        (
                                            "直连配置已写入；请重启目标 Codex 客户端后验证实际请求"
                                                .to_owned(),
                                            status,
                                        )
                                    }),
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => Err("请先预览直连变更".into()),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    match result {
                        Ok((message, status)) => {
                            show_action(&app, Ok(message));
                            if let Some(status) = status {
                                app.set_direct_active(status.direct_active);
                                app.set_config_status(
                                    format!(
                                        "{} · {} · {}",
                                        status.mode, status.provider, status.model
                                    )
                                    .into(),
                                );
                            }
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::RestoreDirect(target) => {
                prepared = None;
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let target_path = client::config_path(&home(&target)?)?;
                    let result = direct_config::restore(&target_path, data_dir)?;
                    let status = client::inspect(Path::new(&target), data_dir)?;
                    Ok::<_, String>((result, status))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    match result {
                        Ok((restored, status)) => {
                            app.set_direct_active(status.direct_active);
                            app.set_config_status(
                                format!("{} · {} · {}", status.mode, status.provider, status.model)
                                    .into(),
                            );
                            if restored.conflicts.is_empty() {
                                show_action(
                                    &app,
                                    Ok("原配置的受管字段已恢复；请重启目标 Codex 客户端".into()),
                                );
                            } else {
                                show_action(
                                    &app,
                                    Err(format!(
                                        "已保留外部改动，以下字段有冲突：{}；恢复 journal 已保留",
                                        restored.conflicts.join("、")
                                    )),
                                );
                            }
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::InspectConfig(target) => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    client::inspect(&home(&target)?, data_dir)
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(status) => {
                        app.set_direct_active(status.direct_active);
                        app.set_config_status(
                            format!("{} · {} · {}", status.mode, status.provider, status.model)
                                .into(),
                        );
                        show_action(
                            &app,
                            Ok(if status.config_exists {
                                "已读取目标 config.toml；未修改配置"
                            } else {
                                "目标 config.toml 尚不存在；未修改配置"
                            }
                            .into()),
                        );
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::ImportCurrent(target) => {
                let result = home(&target).and_then(|path| client::import_candidate(&path));
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(candidate) => {
                        app.set_edit_id("".into());
                        app.set_edit_name(candidate.name.into());
                        app.set_edit_url(candidate.base_url.into());
                        app.set_edit_model(candidate.model_id.into());
                        app.set_edit_key("".into());
                        app.set_editor_open(true);
                        app.set_active_page(1);
                        show_action(
                            &app,
                            Ok(
                                "已填入当前上游资料；请输入 API Key 后保存。原配置与凭据未改动"
                                    .into(),
                            ),
                        );
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::CheckLogin(target) => {
                let result = match home(&target) {
                    Ok(path) => client::login_status(&path).await,
                    Err(error) => Err(error),
                };
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(status) => {
                        app.set_auth_status(status.into());
                        show_action(
                            &app,
                            Ok("官方登录状态已由 Codex CLI 报告；未读取 auth.json".into()),
                        );
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
        }
    }
}

fn credential_command() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    args.next();
    let Some(command) = args.next() else {
        return Ok(false);
    };
    if command != "credential" {
        return Err("unknown SwitchX command".into());
    }
    let reference = args.next().ok_or("credential reference is missing")?;
    if args.next().is_some() {
        return Err("unexpected credential command argument".into());
    }
    let reference = reference
        .to_str()
        .ok_or("credential reference is invalid")?;
    let store = CredentialStore::new(PROVIDER_KEY_SERVICE)?;
    let secret = store.get(reference)?;
    println!("{}", secret.expose());
    Ok(true)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if credential_command()? {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = AppWindow::new()?;
    let tray = SwitchXTray::new()?;
    let window = app.as_weak();
    tray.on_show_app(move || {
        if let Some(app) = window.upgrade() {
            let _ = app.show();
        }
    });
    let directory = data_directory();
    app.set_data_path(
        directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "不可用".into())
            .into(),
    );
    app.set_config_home(
        client::default_home()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    let (sender, receiver) = mpsc::channel::<Command>(8);
    runtime.spawn(worker(receiver, app.as_weak(), directory));
    let window = app.as_weak();
    let restore_sender = sender.clone();
    tray.on_restore_config(move || {
        if let Some(app) = window.upgrade() {
            let _ = app.show();
            queue(
                &app,
                &restore_sender,
                Command::RestoreDirect(app.get_config_home().to_string()),
            );
        }
    });
    tray.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });
    let weak = app.as_weak();
    let refresh_sender = sender.clone();
    app.on_refresh(move |check_credentials| {
        if let Some(app) = weak.upgrade() {
            app.set_loading(true);
            queue(&app, &refresh_sender, Command::Refresh(check_credentials));
        }
    });
    let weak = app.as_weak();
    app.on_filter_providers(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_providers(&app, &query);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_provider(move |id, name, url, model, key| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::Save {
                    id: id.into(),
                    name: name.into(),
                    url: url.into(),
                    model: model.into(),
                    key: key.into(),
                },
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
    app.on_check_provider(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::Check(id.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_inspect_direct(move |id, home| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::InspectDirect {
                    id: id.into(),
                    home: home.into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_apply_direct(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::ApplyDirect(app.get_config_home().into()),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_restore_direct(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::RestoreDirect(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_inspect_config(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::InspectConfig(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_import_current(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::ImportCurrent(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_check_login(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::CheckLogin(home.into()));
        }
    });
    queue(&app, &sender, Command::Refresh(false));
    queue(
        &app,
        &sender,
        Command::InspectConfig(app.get_config_home().to_string()),
    );
    let result = app.run();
    drop(app);
    drop(tray);
    drop(sender);
    drop(runtime);
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_filter_matches_chinese_names_and_ignores_case() {
        let providers = ModelRc::new(VecModel::from(vec![
            ProviderRow {
                id: "a".into(),
                name: "DeepSeek 官方".into(),
                endpoint: "https://example.invalid".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "a".into(),
                credential_status: "凭据未检查".into(),
            },
            ProviderRow {
                id: "b".into(),
                name: "备用上游".into(),
                endpoint: "https://backup.invalid".into(),
                base_url: "https://backup.invalid/v1".into(),
                model_id: "b".into(),
                credential_status: "凭据未检查".into(),
            },
        ]));
        assert_eq!(matching_providers(&providers, "deep").len(), 1);
        assert_eq!(matching_providers(&providers, " 上游 ").len(), 1);
        assert_eq!(matching_providers(&providers, "  ").len(), 2);
        assert!(matching_providers(&providers, "missing").is_empty());
    }
}
