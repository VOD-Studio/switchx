slint::include_modules!();

#[cfg(target_os = "macos")]
mod macos;

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app::{self, AppError, Snapshot, data_directory, load_snapshot},
    catalog, chatgpt, client, config_transaction,
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE, ROUTER_TOKEN_SERVICE, Secret},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    provider_config::{self, CodexOptions},
    routed::RouteSession,
    storage::ProviderRecord,
};
use tokio::sync::{
    mpsc::{self, error::TrySendError},
    watch,
};

enum Command {
    Refresh(bool),
    RefreshRequests,
    PollRouteStatus,
    Save {
        id: String,
        name: String,
        url: String,
        model: String,
        key: String,
        options: CodexOptions,
    },
    BeginProviderEditor {
        id: String,
        home: String,
    },
    OpenCommonConfig(String),
    ExtractCommonConfig(String),
    SaveCommonConfig(String),
    Delete(String),
    Check(String, String),
    FetchModels {
        scope: i32,
        generation: i32,
        provider: String,
        url: String,
        key: Secret,
        home: String,
    },
    InspectDirect {
        id: String,
        home: String,
    },
    ApplyDirect(String),
    RestoreConfig(String),
    InspectConfig(String),
    ImportCurrent(String),
    CheckLogin(String),
    Subscription {
        action: i32,
        home: String,
        port: String,
    },
    SaveModel {
        provider: String,
        original_id: String,
        public_id: String,
        name: String,
        upstream_model: String,
        path: String,
        context: String,
        levels: String,
        default_reasoning: String,
    },
    DeleteModel(String),
    SelectModel(String, bool),
    SaveFallback(String, Option<String>),
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
    LaunchCodex {
        home: String,
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
                        context_window: model.context_window.into(),
                        reasoning_levels: model.reasoning_levels.into(),
                        default_reasoning: model.default_reasoning.into(),
                        saved: model.saved,
                        ready: model.ready,
                        included: model.enabled,
                        fallback_provider_id: model.fallback_provider_id.into(),
                        fallback_label: model.fallback_label.into(),
                    })
                    .collect::<Vec<_>>(),
            )));
            let count = snapshot.providers.len();
            app.set_model_provider_ids(ModelRc::new(VecModel::from(
                snapshot
                    .providers
                    .iter()
                    .map(|provider| provider.id.clone().into())
                    .collect::<Vec<slint::SharedString>>(),
            )));
            app.set_model_provider_options(ModelRc::new(VecModel::from(
                snapshot
                    .providers
                    .iter()
                    .map(|provider| provider.name.clone().into())
                    .collect::<Vec<slint::SharedString>>(),
            )));
            let presets = app.get_provider_presets();
            let rows = snapshot
                .providers
                .into_iter()
                .map(|provider| {
                    let brand = presets
                        .iter()
                        .find(|preset| preset.id == provider.preset_id)
                        .unwrap_or_default();
                    ProviderRow {
                        id: provider.id.into(),
                        name: provider.name.into(),
                        endpoint: provider.endpoint.into(),
                        base_url: provider.base_url.into(),
                        model_id: provider.model_id.into(),
                        credential_status: provider.credential_status.into(),
                        preset_id: brand.id,
                        icon: brand.icon,
                        monochrome: brand.monochrome,
                    }
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

fn editor_codex_options(app: &AppWindow) -> Result<CodexOptions, String> {
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
    };
    options.validate()?;
    Ok(options)
}

fn update_provider_config_preview(app: &AppWindow) {
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

fn show_provider_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    options: CodexOptions,
    common: String,
) {
    app.set_editor_open(false);
    app.set_common_config_editor_open(false);
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
    app.set_edit_remote_compaction(options.remote_compaction);
    app.set_edit_use_common_config(options.use_common_config);
    app.set_edit_context_1m(options.context_1m);
    app.set_edit_compact_limit(options.compact_limit.to_string().into());
    app.set_edit_config_error("".into());
    app.set_delete_confirm(false);
    app.set_busy(false);
    app.set_editor_open(true);
    update_provider_config_preview(app);
}

fn open_provider_link(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法打开系统浏览器，请检查默认浏览器设置".into())
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

fn common_config_for_home(data_dir: &Path, target: &str) -> Result<String, String> {
    match home(target).and_then(|target| app::initialize_common_config(data_dir, &target)) {
        Ok(common) => Ok(common),
        Err(error) => {
            eprintln!("未自动提取 Codex 通用配置：{error}");
            app::load_common_config(data_dir)
        }
    }
}

async fn restore_connections(
    session: &mut RouteSession,
    data_dir: &Path,
    target: &Path,
) -> Result<(), String> {
    if config_transaction::recovery(data_dir)?.is_some() {
        session.restore(data_dir, target).await?;
    } else if direct_config::active_target(data_dir)?.is_some() {
        let result = direct_config::restore(&client::config_path(target)?, data_dir)?;
        if !result.conflicts.is_empty() {
            return Err(format!(
                "配置恢复有冲突：{}；登录操作未开始",
                result.conflicts.join("、")
            ));
        }
    }
    Ok(())
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

fn update_model_display_name(app: &AppWindow) {
    if !app.get_model_display_name_custom() {
        app.set_model_display_name(
            format!(
                "{}/{}",
                app.get_model_upstream_id(),
                app.get_model_provider_name()
            )
            .into(),
        );
    }
}

fn open_model_editor(app: &AppWindow, provider_id: &str, original_id: &str) -> Result<(), String> {
    let provider = app
        .get_providers()
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or("上游不存在，请刷新后重试")?;
    let model = if original_id.is_empty() {
        None
    } else {
        Some(
            app.get_models()
                .iter()
                .find(|model| {
                    model.public_id == original_id
                        && model.saved
                        && model.provider_id == provider_id
                })
                .ok_or("模型映射不存在，请刷新后重试")?,
        )
    };
    app.set_model_provider_id(provider.id.clone());
    app.set_model_provider_name(provider.name);
    app.set_model_original_id(original_id.into());
    app.set_model_display_name_custom(model.is_some());
    app.set_model_upstream_id(
        model
            .as_ref()
            .map(|model| model.upstream_model.clone())
            .unwrap_or(provider.model_id),
    );
    app.set_model_public_id(match &model {
        Some(model) => model.public_id.clone(),
        None => format!("sx-{}", app::new_id()?).into(),
    });
    app.set_model_display_name(
        model
            .as_ref()
            .map(|model| model.display_name.clone())
            .unwrap_or_default(),
    );
    update_model_display_name(app);
    app.set_model_context_window(
        model
            .as_ref()
            .map(|model| model.context_window.clone())
            .unwrap_or_default(),
    );
    app.set_model_reasoning_levels(
        model
            .as_ref()
            .map(|model| model.reasoning_levels.clone())
            .unwrap_or_default(),
    );
    app.set_model_default_reasoning(
        model
            .as_ref()
            .map(|model| model.default_reasoning.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "未设置".into()),
    );
    app.set_model_source_path("".into());
    app.set_model_delete_confirm(false);
    app.set_model_editor_open(true);
    app.set_fallback_editor_open(false);
    app.set_editor_open(false);
    app.set_edit_key("".into());
    app.set_active_page(2);
    update_reasoning_options(app);
    Ok(())
}

fn update_reasoning_options(app: &AppWindow) {
    let text = app.get_model_reasoning_levels();
    let levels = catalog::reasoning_levels(&text).unwrap_or_default();
    let mut options = vec![slint::SharedString::from("未设置")];
    options.extend(levels.into_iter().map(slint::SharedString::from));
    if !options.contains(&app.get_model_default_reasoning()) {
        app.set_model_default_reasoning("未设置".into());
    }
    app.set_model_reasoning_options(ModelRc::new(VecModel::from(options)));
}

async fn worker(
    mut receiver: mpsc::Receiver<Command>,
    weak: slint::Weak<AppWindow>,
    directory: Result<PathBuf, AppError>,
) -> Result<(), String> {
    let mut prepared: Option<(
        PreparedDirectSwitch,
        ProviderRecord,
        app::ProviderConfigState,
    )> = None;
    let mut route_session = RouteSession::default();
    let mut pending_login: Option<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> = None;
    while let Some(command) = receiver.recv().await {
        match command {
            Command::PollRouteStatus => {}
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
                                        fallback: record.fallback.into(),
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
            Command::BeginProviderEditor { id, home: target } => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let common = common_config_for_home(data_dir, &target)?;
                    let (provider, options) = if id.is_empty() {
                        (None, CodexOptions::for_common(&common)?)
                    } else {
                        let provider = app::load_provider(data_dir, &id)?;
                        if provider.id == chatgpt::PROVIDER_ID {
                            return Err("订阅连接由目标 Codex 管理".into());
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
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((provider, options, common)) => {
                        show_provider_editor(&app, provider, options, common)
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::OpenCommonConfig(target) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| common_config_for_home(path, &target));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(common) => {
                            app.set_common_config_saved(common.clone().into());
                            app.set_common_config_draft(common.into());
                            app.set_common_config_error("".into());
                            app.set_common_config_message("".into());
                            app.set_common_config_current_source(
                                "供应商表单当前 config.toml".into(),
                            );
                            update_provider_config_preview(&app);
                            app.set_common_config_editor_open(true);
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::ExtractCommonConfig(form_toml) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::extract_and_save_common_config(path, &form_toml));
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(snippet) => {
                            app.set_common_config_current_source(
                                "供应商表单当前 config.toml".into(),
                            );
                            app.set_common_config_saved(snippet.clone().into());
                            app.set_common_config_draft(snippet.into());
                            app.set_common_config_error("".into());
                            app.set_common_config_message(
                                "已从供应商表单提取并保存通用配置；取消不会撤销这次提取。".into(),
                            );
                            app.set_direct_preview_ready(false);
                            app.set_route_preview_ready(false);
                            update_provider_config_preview(&app);
                            show_action(&app, Ok("已从供应商表单提取并保存 Codex 通用配置".into()));
                        }
                        Err(error) => {
                            app.set_common_config_error(error.clone().into());
                            show_action(&app, Err(error));
                        }
                    }
                });
            }
            Command::SaveCommonConfig(snippet) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::save_common_config(path, &snippet));
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(()) => {
                            app.set_common_config_saved(snippet.into());
                            app.set_common_config_error("".into());
                            app.set_common_config_editor_open(false);
                            app.set_direct_preview_ready(false);
                            app.set_route_preview_ready(false);
                            update_provider_config_preview(&app);
                            show_action(&app, Ok("Codex 通用配置已保存；勾选应用通用配置的供应商将在下次切换时使用。".into()));
                        }
                        Err(error) => app.set_common_config_error(error.into()),
                    }
                });
            }
            Command::Save {
                id,
                name,
                url,
                model,
                key,
                options,
            } => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| {
                        app::save_provider_with_codex_options(
                            path,
                            (!id.is_empty()).then_some(id.as_str()),
                            &name,
                            &url,
                            &model,
                            key,
                            &options,
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
            Command::Check(id, target) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::load_provider(path, &id));
                let result = match result {
                    Ok(provider) if provider.id == chatgpt::PROVIDER_ID => match home(&target) {
                        Ok(target) => chatgpt::account(&target, false)
                            .await
                            .map(|status| status.label().into()),
                        Err(error) => Err(error),
                    },
                    Ok(provider) => match credential(&provider) {
                        Ok(token) => direct::check_models(&provider, token.expose()).await.map(
                            |()| {
                                format!(
                                    "{}：/models 已连通，目录包含 {}；Responses 工具调用尚未验证",
                                    provider.name, provider.model_id
                                )
                            },
                        ),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                };
                let _ = weak.upgrade_in_event_loop(move |app| show_action(&app, result));
            }
            Command::FetchModels {
                scope,
                generation,
                provider,
                url,
                key,
                home: target,
            } => {
                let result = async {
                    if provider == chatgpt::PROVIDER_ID {
                        chatgpt::require_login(&home(&target)?).await?;
                        return Ok(chatgpt::catalog()
                            .await?
                            .iter()
                            .filter_map(|model| model["slug"].as_str().map(str::to_owned))
                            .collect::<Vec<_>>());
                    }
                    let token = if key.expose().is_empty() {
                        let data_dir = directory.as_ref().map_err(|error| error.message())?;
                        if provider.is_empty() {
                            return Err("请先填写 API 地址和 API Key".into());
                        }
                        credential(&app::load_provider(data_dir, &provider)?)?
                    } else {
                        key
                    };
                    direct::fetch_models(&url, token.expose()).await
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    if generation != app.get_discovery_generation()
                        || scope != app.get_discovery_scope()
                        || (scope == 0 && !app.get_editor_open())
                        || (scope == 1 && !app.get_model_editor_open())
                    {
                        return;
                    }
                    app.set_fetching_models(false);
                    match result {
                        Ok(models) => {
                            app.set_discovery_message(if models.is_empty() {
                                "上游返回空列表，可手动填写模型 ID".into()
                            } else {
                                if provider == chatgpt::PROVIDER_ID {
                                    format!(
                                        "已读取 CLI 内置的 {} 个官方模型；账号权限以实际请求为准",
                                        models.len()
                                    )
                                    .into()
                                } else {
                                    format!("已获取 {} 个模型，可从列表选择", models.len()).into()
                                }
                            });
                            app.set_fetched_models(ModelRc::new(VecModel::from(
                                models
                                    .into_iter()
                                    .map(slint::SharedString::from)
                                    .collect::<Vec<_>>(),
                            )));
                        }
                        Err(error) => app.set_discovery_message(error.into()),
                    }
                });
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
                    let state = app::load_provider_config(data_dir, &id)?;
                    let prepared =
                        PreparedDirectSwitch::inspect(&target, data_dir, &provider, &helper)?;
                    let prepared = if let Some(options) = &state.options {
                        prepared.with_codex_options(options, &state.common)?
                    } else if !state.common.is_empty() {
                        prepared.with_common_config(&state.common)?
                    } else {
                        prepared
                    };
                    Ok::<_, String>((prepared, provider, state))
                })();
                match result {
                    Ok((switch, provider, state)) => {
                        let summary = format!(
                            "目标：{} · 模型：{} · 受管变更：{}",
                            provider.name,
                            provider.model_id,
                            switch.changes.join("、")
                        );
                        prepared = Some((switch, provider, state));
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
                    Some((switch, selected, selected_config)) => {
                        let checked = (|| {
                            let target_path = client::config_path(&home(&target)?)?;
                            if target_path != switch.target() {
                                return Err("配置目录已变化，请重新预览".into());
                            }
                            let data_dir = directory.as_ref().map_err(|error| error.message())?;
                            let latest = app::load_provider(data_dir, &selected.id)?;
                            if latest != selected
                                || app::load_provider_config(data_dir, &selected.id)?
                                    != selected_config
                            {
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
                    common_config_for_home(data_dir, &target.to_string_lossy())?;
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
                let result = (|| {
                    let candidate = client::import_candidate(&home(&target)?)?;
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let common = common_config_for_home(data_dir, &target)?;
                    let options = CodexOptions::for_common(&common)?;
                    Ok::<_, String>((candidate, options, common))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((candidate, options, common)) => {
                        let provider = ProviderRecord {
                            id: String::new(),
                            name: candidate.name,
                            base_url: candidate.base_url,
                            model_id: candidate.model_id,
                            credential_ref: None,
                        };
                        show_provider_editor(&app, Some(provider), options, common);
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
            Command::Subscription {
                action,
                home: target,
                port,
            } => {
                let result: Result<String, String> = async {
                    let target_home = home(&target)?;
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    if pending_login
                        .as_ref()
                        .is_some_and(|(_, task)| !task.is_finished())
                        && action != 3
                    {
                        return Err("登录仍在进行；可先完成或取消登录".into());
                    }
                    match action {
                        0 => {
                            let models = chatgpt::catalog().await?;
                            chatgpt::save_connection(data_dir, &models)?;
                            route_session.discard_preview();
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                show_result(&app, Ok(snapshot));
                                app.set_route_preview_ready(false);
                                app.set_active_page(5);
                            });
                            Ok("已添加订阅连接与 CLI 内置模型资料；请登录后预览发布".to_owned())
                        }
                        1 => {
                            // Reauthentication is an explicit restore-then-login operation.
                            restore_connections(&mut route_session, data_dir, &target_home).await?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            let login =
                                chatgpt::Session::start(&target_home).await?.login().await?;
                            open_provider_link(&login.url)?;
                            let (cancel, receiver) = watch::channel(false);
                            let id = app::new_id()?;
                            let login_id = id.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_chatgpt_login_id(id.into());
                                app.set_chatgpt_login_pending(true);
                                app.set_auth_status("正在等待官方浏览器登录…".into());
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            let window = weak.clone();
                            let task = tokio::spawn(async move {
                                let result = login.finish(receiver).await;
                                let _ = window.upgrade_in_event_loop(move |app| {
                                    if app.get_chatgpt_login_id() != login_id.as_str() {
                                        return;
                                    }
                                    app.set_chatgpt_login_pending(false);
                                    match result {
                                        Ok(status) => {
                                            app.set_auth_status(status.label().into());
                                            app.set_action_message(
                                                "ChatGPT 登录已完成；可重新预览并开启路由".into(),
                                            );
                                        }
                                        Err(error) => {
                                            app.set_auth_status(error.clone().into());
                                            app.set_action_message(error.into());
                                        }
                                    }
                                });
                            });
                            pending_login = Some((cancel, task));
                            Ok("已打开官方登录页；原配置已恢复，可在此取消登录".to_owned())
                        }
                        2 => {
                            let status = chatgpt::account(&target_home, true).await?;
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_auth_status(status.label().into())
                            });
                            status.require_chatgpt()?;
                            Ok(
                                "Codex 已检查订阅登录并请求续期；活跃会话的自动续期由 Codex 管理"
                                    .to_owned(),
                            )
                        }
                        3 => {
                            if let Some((cancel, _)) = &pending_login {
                                cancel.send_replace(true);
                            }
                            Ok("已请求取消官方登录".to_owned())
                        }
                        4 => {
                            let port = route_port(&port)?;
                            let helper =
                                std::env::current_exe().map_err(|_| "无法定位 SwitchX 程序")?;
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
                            let model = snapshot
                                .models
                                .iter()
                                .find(|model| {
                                    model.enabled && model.provider_id != chatgpt::PROVIDER_ID
                                })
                                .ok_or("请先保存并选择至少一个 API 模型；当前路由与配置已保留")?
                                .public_id
                                .clone();
                            restore_connections(&mut route_session, data_dir, &target_home).await?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            chatgpt::deselect_models(data_dir)?;
                            let summary = route_session
                                .prepare(data_dir, &target_home, port, &model, &helper)
                                .await;
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
                            let ready = summary.is_ok();
                            let preview = summary.as_ref().ok().cloned().unwrap_or_default();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_route_preview_model(model.as_str().into());
                                show_result(&app, Ok(snapshot));
                                app.set_default_model(model.into());
                                app.set_route_preview(preview.into());
                                app.set_route_preview_ready(ready);
                                app.set_active_page(2);
                            });
                            summary?;
                            Ok("已准备 API 路由预览；点击“开启路由”，无需订阅登录".to_owned())
                        }
                        _ => Err("未知订阅操作".into()),
                    }
                }
                .await;
                let status = home(&target).ok().and_then(|path| {
                    directory
                        .as_ref()
                        .ok()
                        .and_then(|data_dir| client::inspect(&path, data_dir).ok())
                });
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(status) = status {
                        show_config_status(&app, status);
                    }
                    if matches!(action, 1 | 2)
                        && let Err(error) = &result
                    {
                        app.set_auth_status(error.as_str().into());
                    }
                    show_action(&app, result);
                });
            }
            Command::SaveModel {
                provider,
                original_id,
                public_id,
                name,
                upstream_model,
                path,
                context,
                levels,
                default_reasoning,
            } => {
                let result = async {
                    let directory = directory
                        .as_ref()
                        .map_err(|error| error.message().to_owned())?;
                    let keep_imported_reasoning =
                        !path.trim().is_empty() && levels.trim().is_empty();
                    let input = app::ModelInput {
                        provider_id: &provider,
                        original_id: &original_id,
                        public_id: &public_id,
                        display_name: &name,
                        upstream_model: &upstream_model,
                        catalog_path: &path,
                        settings: Some(catalog::MappingSettings {
                            context_window: &context,
                            reasoning_levels: (!keep_imported_reasoning).then_some(levels.as_str()),
                            default_reasoning: (!keep_imported_reasoning)
                                .then_some(default_reasoning.as_str()),
                        }),
                    };
                    if provider == chatgpt::PROVIDER_ID {
                        chatgpt::save_mapping(directory, input).await
                    } else {
                        app::save_mapping(directory, input)
                    }
                }
                .await;
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
                            "模型映射已保存；预览发布后可开启路由并刷新 Codex 模型菜单".into()
                        }),
                    );
                });
            }
            Command::DeleteModel(public_id) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| app::delete_model(directory, &public_id));
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
                    show_action(&app, result.map(|()| "模型映射已删除".into()));
                });
            }
            Command::SaveFallback(provider, fallback) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| {
                        app::save_fallback(directory, &provider, fallback.as_deref())
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
                        app.set_fallback_editor_open(false);
                        app.set_route_preview_ready(false);
                    }
                    show_action(
                        &app,
                        result.map(|()| "备用策略已保存，请重新预览发布".into()),
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
                    app.set_route_preview_model(model.as_str().into());
                    app.set_route_preview_ready(result.is_ok());
                    match result {
                        Ok(summary) => {
                            app.set_route_preview(summary.into());
                            show_action(
                                &app,
                                Ok(
                                    "发布预览已准备；开启路由后点击“启动 Codex”，在新会话选择模型"
                                        .into(),
                                ),
                            );
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
                            "模型路由已开启，目录和配置已发布；点击“启动 Codex”后在新会话选择模型".into()
                        }),
                    );
                });
            }
            Command::LaunchCodex { home: target } => {
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    let path = route_session
                        .codex_launcher(directory, &home(&target)?)
                        .await?;
                    client::open_codex_launcher(&path)?;
                    Ok("已打开新的 Codex 会话；输入 /model 选择已发布模型".into())
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| show_action(&app, result));
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
                if let Some((cancel, task)) = pending_login.take() {
                    cancel.send_replace(true);
                    let _ = task.await;
                }
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
        let chatgpt_error = route_session.chatgpt_error();
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
            app.set_route_auth_error(chatgpt_error.unwrap_or("").into());
            if recording_failed {
                app.set_request_error("部分请求记录写入失败；请检查数据库权限和磁盘空间".into());
            }
            app.set_route_managed(route_managed);
            app.set_config_managed(managed);
            app.set_route_status(
                if running {
                    format!(
                        "正在路由 · {address}{}",
                        chatgpt_error
                            .map(|error| format!("\n{error}"))
                            .unwrap_or_default()
                    )
                } else if route_managed {
                    "路由未运行 · 有配置待恢复".into()
                } else {
                    "路由未启用".into()
                }
                .into(),
            );
        });
    }
    if let Some((cancel, task)) = pending_login {
        cancel.send_replace(true);
        let _ = task.await;
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
    let presets = app::PROVIDER_PRESETS
        .iter()
        .map(|preset| {
            slint::Image::load_from_svg_data(preset.icon).map(|icon| ProviderPresetRow {
                id: preset.id.into(),
                name: preset.name.into(),
                icon,
                monochrome: preset.monochrome,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    app.set_provider_presets(ModelRc::new(VecModel::from(presets)));
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_open_common_config(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::OpenCommonConfig(app.get_config_home().into()),
            );
        }
    });
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_common_config(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SaveCommonConfig(app.get_common_config_draft().into()),
            );
        }
    });
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
    let weak = app.as_weak();
    app.on_apply_provider_preset(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() || app.get_config_managed() || !app.get_edit_id().is_empty() {
            return;
        }
        let preset = app::PROVIDER_PRESETS
            .iter()
            .find(|preset| preset.id == id.as_str());
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
            open_provider_link(url).map(|()| "已在浏览器打开供应商页面".into()),
        );
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_provider(move |id, name, url, model, key| {
        if let Some(app) = weak.upgrade() {
            let options = match editor_codex_options(&app) {
                Ok(options) => options,
                Err(error) => {
                    app.set_edit_config_error(error.into());
                    return;
                }
            };
            queue(
                &app,
                &callback_sender,
                Command::Save {
                    id: id.into(),
                    name: name.into(),
                    url: url.into(),
                    model: model.into(),
                    key: key.into(),
                    options,
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
            queue(
                &app,
                &callback_sender,
                Command::Check(id.into(), app.get_config_home().into()),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_fetch_models(move |scope| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let (provider, url, key) = if scope == 0 {
            (
                app.get_edit_id().to_string(),
                app.get_edit_url().to_string(),
                Secret::new(app.get_edit_key().into()),
            )
        } else {
            let Some(provider) = app
                .get_providers()
                .iter()
                .find(|provider| provider.id == app.get_model_provider_id())
            else {
                show_action(&app, Err("请先保存上游连接".into()));
                return;
            };
            (
                provider.id.to_string(),
                provider.base_url.to_string(),
                Secret::new(String::new()),
            )
        };
        app.set_discovery_scope(scope);
        app.set_discovery_message("正在获取模型列表…".into());
        queue(
            &app,
            &callback_sender,
            Command::FetchModels {
                scope,
                generation: app.get_discovery_generation(),
                provider,
                url,
                key,
                home: app.get_config_home().into(),
            },
        );
        app.set_fetching_models(app.get_busy());
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
    app.on_subscription_action(move |action| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::Subscription {
                    action,
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_poll_route_status(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_busy()
        {
            let _ = callback_sender.try_send(Command::PollRouteStatus);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_model(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SaveModel {
                    provider: app.get_model_provider_id().into(),
                    original_id: app.get_model_original_id().into(),
                    public_id: app.get_model_public_id().into(),
                    name: app.get_model_display_name().into(),
                    upstream_model: app.get_model_upstream_id().into(),
                    path: app.get_model_source_path().into(),
                    context: app.get_model_context_window().into(),
                    levels: app.get_model_reasoning_levels().into(),
                    default_reasoning: if app.get_model_default_reasoning() == "未设置" {
                        String::new()
                    } else {
                        app.get_model_default_reasoning().into()
                    },
                },
            );
        }
    });
    let weak = app.as_weak();
    app.on_begin_model_editor(move |provider, original_id| {
        if let Some(app) = weak.upgrade()
            && let Err(error) = open_model_editor(&app, &provider, &original_id)
        {
            show_action(&app, Err(error));
        }
    });
    let weak = app.as_weak();
    app.on_add_selected_model(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(provider) = app
            .get_model_provider_ids()
            .row_data(app.get_model_provider_choice() as usize)
            && let Err(error) = open_model_editor(&app, &provider, "")
        {
            show_action(&app, Err(error));
        }
    });
    let weak = app.as_weak();
    app.on_update_model_display_name(move || {
        if let Some(app) = weak.upgrade() {
            update_model_display_name(&app);
        }
    });
    let weak = app.as_weak();
    app.on_update_reasoning_options(move || {
        if let Some(app) = weak.upgrade() {
            update_reasoning_options(&app);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_delete_model(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::DeleteModel(app.get_model_original_id().into()),
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
    let weak = app.as_weak();
    app.on_edit_fallback(move |public_id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let models = app.get_models();
        let Some(primary) = models.iter().find(|model| model.public_id == public_id) else {
            return;
        };
        if primary.provider_id == chatgpt::PROVIDER_ID {
            show_action(
                &app,
                Err("订阅账号不参与自动备用；请恢复后明确切换账号或 API 上游".into()),
            );
            return;
        }
        let providers = app.get_providers();
        let mut ids = vec![slint::SharedString::default()];
        let mut labels = vec![slint::SharedString::from("不使用备用上游")];
        let mut selected = 0;
        for model in models.iter().filter(|model| {
            model.ready
                && model.provider_id != chatgpt::PROVIDER_ID
                && model.provider_id != primary.provider_id
                && model.upstream_model == primary.upstream_model
        }) {
            if model.provider_id == primary.fallback_provider_id {
                selected = ids.len() as i32;
            }
            let endpoint = providers
                .iter()
                .find(|provider| provider.id == model.provider_id)
                .map(|provider| provider.endpoint)
                .unwrap_or_default();
            labels.push(format!("{} · {}", model.provider_name, endpoint).into());
            ids.push(model.provider_id);
        }
        app.set_fallback_primary_id(public_id);
        app.set_fallback_primary_label(
            format!("{} / {}", primary.provider_name, primary.upstream_model).into(),
        );
        app.set_fallback_ids(ModelRc::new(VecModel::from(ids)));
        app.set_fallback_options(ModelRc::new(VecModel::from(labels)));
        app.set_fallback_choice(selected);
        app.set_fallback_consent(false);
        app.set_fallback_editor_open(true);
        app.set_model_editor_open(false);
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_fallback(move || {
        if let Some(app) = weak.upgrade() {
            let Some(id) = app
                .get_fallback_ids()
                .row_data(app.get_fallback_choice() as usize)
            else {
                return;
            };
            if !id.is_empty() && !app.get_fallback_consent() {
                return;
            }
            queue(
                &app,
                &callback_sender,
                Command::SaveFallback(
                    app.get_fallback_primary_id().into(),
                    if id.is_empty() { None } else { Some(id.into()) },
                ),
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
    app.on_launch_codex(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::LaunchCodex {
                    home: app.get_config_home().into(),
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
                ..ProviderRow::default()
            },
            ProviderRow {
                id: "b".into(),
                name: "备用上游".into(),
                endpoint: "https://backup.invalid".into(),
                base_url: "https://backup.invalid/v1".into(),
                model_id: "b".into(),
                credential_status: "凭据未检查".into(),
                ..ProviderRow::default()
            },
        ]));
        assert_eq!(matching_providers(&providers, "deep").len(), 1);
        assert_eq!(matching_providers(&providers, " 上游 ").len(), 1);
        assert_eq!(matching_providers(&providers, "  ").len(), 2);
        assert!(matching_providers(&providers, "missing").is_empty());
    }

    #[test]
    fn embedded_provider_logos_decode() {
        for preset in app::PROVIDER_PRESETS {
            let image = slint::Image::load_from_svg_data(preset.icon).unwrap();
            assert!(image.size().width > 0, "{}", preset.id);
            assert!(image.size().height > 0, "{}", preset.id);
        }
    }
}
