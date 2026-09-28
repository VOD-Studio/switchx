slint::include_modules!();

#[cfg(target_os = "macos")]
mod macos;

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app::{self, AppError, Snapshot, data_directory, load_snapshot},
    client, config_transaction,
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE, ROUTER_TOKEN_SERVICE},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    routed::RouteSession,
    storage::ProviderRecord,
};
use tokio::sync::mpsc::{self, error::TrySendError};

enum Command {
    Refresh(bool),
    RefreshRequests,
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
    RestoreConfig(String),
    InspectConfig(String),
    ImportCurrent(String),
    CheckLogin(String),
    SaveModel {
        provider: String,
        public_id: String,
        name: String,
        path: String,
    },
    SelectModel(String, bool),
    InspectRoute {
        home: String,
        port: String,
        model: String,
    },
    ApplyRoute {
        home: String,
        port: String,
        model: String,
    },
    CancelRoutePreview,
    Quit,
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
            let selected_count = snapshot.models.iter().filter(|model| model.enabled).count();
            if !snapshot
                .models
                .iter()
                .any(|model| model.enabled && model.public_id == app.get_default_model().as_str())
            {
                app.set_default_model(
                    snapshot
                        .models
                        .iter()
                        .find(|model| model.enabled)
                        .map(|model| model.public_id.as_str())
                        .unwrap_or("")
                        .into(),
                );
            }
            app.set_selected_model_count(selected_count as i32);
            app.set_models(ModelRc::new(VecModel::from(
                snapshot
                    .models
                    .into_iter()
                    .map(|model| ModelRow {
                        provider_id: model.provider_id.into(),
                        provider_name: model.provider_name.into(),
                        upstream_model: model.upstream_model.into(),
                        public_id: model.public_id.into(),
                        display_name: model.display_name.into(),
                        detail: model.detail.into(),
                        ready: model.ready,
                        included: model.enabled,
                    })
                    .collect::<Vec<_>>(),
            )));
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
    app::provider_credential(provider)
}

fn route_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "本地端口必须在 1–65535 之间".into())
}

fn show_config_status(app: &AppWindow, status: client::ConfigStatus) {
    app.set_direct_active(status.direct_active);
    app.set_route_managed(status.route_managed);
    app.set_config_status(
        format!("{} · {} · {}", status.mode, status.provider, status.model).into(),
    );
}

async fn worker(
    mut receiver: mpsc::Receiver<Command>,
    weak: slint::Weak<AppWindow>,
    directory: Result<PathBuf, AppError>,
) -> Result<(), String> {
    let mut prepared: Option<(PreparedDirectSwitch, ProviderRecord)> = None;
    let mut route_session = RouteSession::default();
    while let Some(command) = receiver.recv().await {
        match command {
            Command::RefreshRequests => {
                let result = directory
                    .as_ref()
                    .map_err(|error| *error)
                    .and_then(|path| app::load_requests(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(records) => {
                            app.set_requests(ModelRc::new(VecModel::from(
                                records
                                    .into_iter()
                                    .map(|record| RequestRow {
                                        time: record.time.into(),
                                        route: record.route.into(),
                                        timing: record.timing.into(),
                                        detail: record.detail.into(),
                                        status: record.status.label().into(),
                                        completed: record.status
                                            == switchx::storage::RequestStatus::Completed,
                                        cancelled: record.status
                                            == switchx::storage::RequestStatus::Cancelled,
                                        error: record.error.into(),
                                    })
                                    .collect::<Vec<_>>(),
                            )));
                            app.set_request_error("".into());
                        }
                        Err(error) => app.set_request_error(
                            format!("请求记录读取失败：{}；保留上次列表", error.message()).into(),
                        ),
                    }
                });
            }
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
                    route_session.discard_preview();
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
                        app.set_route_preview_ready(false);
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
                    route_session.discard_preview();
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
                            app.set_route_preview_ready(false);
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
                route_session.discard_preview();
                let _ = weak.upgrade_in_event_loop(|app| app.set_route_preview_ready(false));
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
                                show_config_status(&app, status);
                            }
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::RestoreConfig(target) => {
                prepared = None;
                let result = async {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let target_home = home(&target)?;
                    if config_transaction::recovery(data_dir)?.is_some() {
                        route_session.restore(data_dir, &target_home).await?;
                    } else {
                        route_session.discard_preview();
                        let result =
                            direct_config::restore(&client::config_path(&target_home)?, data_dir)?;
                        if !result.conflicts.is_empty() {
                            return Err(format!(
                                "已保留外部改动，以下字段有冲突：{}；恢复记录已保留",
                                result.conflicts.join("、")
                            ));
                        }
                    }
                    client::inspect(&target_home, data_dir)
                }
                .await;
                let fallback_status = directory
                    .as_ref()
                    .ok()
                    .and_then(|path| client::inspect(Path::new(&target), path).ok());
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                    if let Some(status) = fallback_status { show_config_status(&app, status); }
                    match result {
                        Ok(status) => {
                            show_config_status(&app, status);
                            show_action(&app, Ok("原配置的受管字段已恢复，本地路由已停止；请重启目标 Codex 客户端".into()));
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::InspectConfig(target) => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let managed = direct_config::active_target(data_dir)?
                        .or(config_transaction::recovery(data_dir)?.map(|route| route.config_path));
                    let target = managed
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                        .unwrap_or(home(&target)?);
                    client::inspect(&target, data_dir).map(|status| (status, target))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((status, target)) => {
                        let config_exists = status.config_exists;
                        app.set_config_home(target.to_string_lossy().into_owned().into());
                        show_config_status(&app, status);
                        show_action(
                            &app,
                            Ok(if config_exists {
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
            Command::SaveModel {
                provider,
                public_id,
                name,
                path,
            } => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| {
                        app::save_model(directory, &provider, &public_id, &name, &path)
                    });
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        app.set_model_editor_open(false);
                        app.set_route_preview_ready(false);
                    }
                    show_action(
                        &app,
                        result.map(|()| {
                            "模型资料已保存；开启路由前会检查 Codex 兼容性和上游目录".into()
                        }),
                    );
                });
            }
            Command::SelectModel(provider, enabled) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| app::select_model(directory, &provider, enabled));
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        app.set_route_preview_ready(false);
                    }
                    show_action(
                        &app,
                        result.map(|()| "模型选择已保存，请重新预览发布".into()),
                    );
                });
            }
            Command::InspectRoute {
                home: target,
                port,
                model,
            } => {
                prepared = None;
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    let target = home(&target)?;
                    let helper = std::env::current_exe().map_err(|_| "无法定位 SwitchX 程序")?;
                    route_session
                        .prepare(directory, &target, route_port(&port)?, &model, &helper)
                        .await
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(result.is_ok());
                    match result {
                        Ok(summary) => {
                            app.set_route_preview(summary.into());
                            show_action(&app, Ok("发布预览已准备；点击“开启路由”后检查凭据和上游，并写入目标配置".into()));
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::ApplyRoute {
                home: target,
                port,
                model,
            } => {
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    route_session
                        .apply(directory, &home(&target)?, route_port(&port)?, &model)
                        .await
                }
                .await;
                let status = directory
                    .as_ref()
                    .ok()
                    .and_then(|path| client::inspect(Path::new(&target), path).ok());
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_route_preview_ready(false);
                    if let Some(status) = status {
                        show_config_status(&app, status);
                    }
                    show_action(
                        &app,
                        result.map(|()| {
                            "API 路由已开启，目录和配置已发布；请重启目标 Codex 后选择模型".into()
                        }),
                    );
                });
            }
            Command::CancelRoutePreview => {
                route_session.discard_preview();
                let _ = weak.upgrade_in_event_loop(|app| {
                    app.set_route_preview_ready(false);
                    app.set_route_preview("".into());
                    show_action(&app, Ok("已取消发布预览".into()));
                });
            }
            Command::Quit => {
                let result = async {
                    if let Ok(directory) = &directory
                        && let Some(recovery) = config_transaction::recovery(directory)?
                    {
                        route_session
                            .restore(directory, recovery.config_path.parent().unwrap())
                            .await?;
                    }
                    Ok::<_, String>(())
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(()) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        let _ = app.show();
                        app.set_active_page(5);
                        show_action(&app, Err(format!("退出前恢复未完成：{error}")));
                    }
                });
            }
        }
        let running = route_session.is_running();
        let recording_failed = route_session.recording_failed();
        let address = route_session
            .address()
            .map(|address| format!("http://{address}/v1"))
            .unwrap_or_default();
        let route_managed = directory
            .as_ref()
            .is_ok_and(|path| path.join("switch-journal.json").exists());
        let managed = route_managed
            || directory
                .as_ref()
                .is_ok_and(|path| path.join("direct-journal.json").exists());
        let _ = weak.upgrade_in_event_loop(move |app| {
            app.set_route_running(running);
            if recording_failed {
                app.set_request_error("部分请求记录写入失败；请检查数据库权限和磁盘空间".into());
            }
            app.set_route_managed(route_managed);
            app.set_config_managed(managed);
            app.set_route_status(
                if running {
                    format!("正在路由 · {address}")
                } else if route_managed {
                    "路由未运行 · 有配置待恢复".into()
                } else {
                    "路由未启用".into()
                }
                .into(),
            );
        });
    }
    // Also recover when the platform exits the event loop without using our tray.
    if let Ok(directory) = &directory
        && let Some(recovery) = config_transaction::recovery(directory)?
    {
        route_session
            .restore(directory, recovery.config_path.parent().unwrap())
            .await?;
    }
    Ok(())
}

fn credential_command() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args_os();
    args.next();
    let Some(command) = args.next() else {
        return Ok(false);
    };
    let service = if command == "credential" {
        PROVIDER_KEY_SERVICE
    } else if command == "local-token" {
        ROUTER_TOKEN_SERVICE
    } else {
        return Err("unknown SwitchX command".into());
    };
    let reference = args.next().ok_or("credential reference is missing")?;
    if args.next().is_some() {
        return Err("unexpected credential command argument".into());
    }
    let reference = reference
        .to_str()
        .ok_or("credential reference is invalid")?;
    let store = CredentialStore::new(service)?;
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
    let background = runtime.spawn(worker(receiver, app.as_weak(), directory));
    let window = app.as_weak();
    let restore_sender = sender.clone();
    tray.on_restore_config(move || {
        if let Some(app) = window.upgrade() {
            let _ = app.show();
            queue(
                &app,
                &restore_sender,
                Command::RestoreConfig(app.get_config_home().to_string()),
            );
        }
    });
    let quit_sender = sender.clone();
    let weak = app.as_weak();
    tray.on_quit_app(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &quit_sender, Command::Quit);
        }
    });
    #[cfg(target_os = "macos")]
    {
        let quit_sender = sender.clone();
        let weak = app.as_weak();
        macos::install_quit_handler(move || {
            if let Some(app) = weak.upgrade() {
                queue(&app, &quit_sender, Command::Quit);
            }
        })?;
    }
    let weak = app.as_weak();
    let refresh_sender = sender.clone();
    app.on_refresh(move |check_credentials| {
        if let Some(app) = weak.upgrade() {
            app.set_loading(true);
            queue(&app, &refresh_sender, Command::Refresh(check_credentials));
        }
    });
    let weak = app.as_weak();
    let request_sender = sender.clone();
    app.on_refresh_requests(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &request_sender, Command::RefreshRequests);
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
    app.on_restore_config(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::RestoreConfig(home.into()));
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_model(move |provider, public_id, name, path| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SaveModel {
                    provider: provider.into(),
                    public_id: public_id.into(),
                    name: name.into(),
                    path: path.into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_select_model(move |provider, enabled| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SelectModel(provider.into(), enabled),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_inspect_route(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::InspectRoute {
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                    model: app.get_default_model().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_apply_route(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::ApplyRoute {
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                    model: app.get_default_model().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_cancel_route_preview(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::CancelRoutePreview);
        }
    });
    queue(&app, &sender, Command::Refresh(false));
    queue(
        &app,
        &sender,
        Command::InspectConfig(app.get_config_home().to_string()),
    );
    let result = app.run();
    #[cfg(target_os = "macos")]
    macos::clear_quit_handler();
    drop(app);
    drop(tray);
    drop(sender);
    runtime.block_on(background)??;
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
