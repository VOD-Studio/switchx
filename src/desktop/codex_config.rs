//! The target Codex configuration: direct switches, routes, and recovery.

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app::{self, load_snapshot},
    codex::{
        self, client,
        direct_switch::{self, PreparedDirectSwitch},
        route_switch,
    },
    route_session::{RoutePreview, RouteSession},
    storage::{ModelRecord, ProviderKind, Store},
    ui::{AppWindow, PreviewBindingRow, PreviewFact, PreviewMappingRow, RoutePreviewData},
    upstream,
};
use tokio::sync::mpsc;

use super::{
    accounts::{account_view, show_accounts},
    command::{Command, queue, queue_on},
    common_config::common_config_for_home,
    feedback::show_action,
    workbench::{filter_providers, show_result},
    worker::Worker,
};

pub(super) fn codex_home(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    client::config_path(&path)?;
    Ok(path)
}

pub(super) fn route_preview_data(preview: &RoutePreview) -> RoutePreviewData {
    let strings = |items: &[String]| {
        ModelRc::new(VecModel::from(
            items.iter().map(|item| item.into()).collect::<Vec<_>>(),
        ))
    };
    RoutePreviewData {
        client: preview.client_version.as_str().into(),
        endpoint: preview.endpoint.as_str().into(),
        target: preview.target.to_string_lossy().as_ref().into(),
        mappings: ModelRc::new(VecModel::from(
            preview
                .mappings
                .iter()
                .map(|mapping| {
                    let fallback = mapping.fallback.as_ref();
                    PreviewMappingRow {
                        model: mapping.model.as_str().into(),
                        provider: mapping.destination.name.as_str().into(),
                        origin: mapping.destination.origin.as_str().into(),
                        fallback: fallback.map_or("", |it| it.name.as_str()).into(),
                        fallback_origin: fallback.map_or("", |it| it.origin.as_str()).into(),
                    }
                })
                .collect::<Vec<_>>(),
        )),
        changes: strings(&preview.changed_fields),
        bindings: ModelRc::new(VecModel::from(
            preview
                .bindings
                .iter()
                .map(|binding| PreviewBindingRow {
                    provider: binding.provider.as_str().into(),
                    grok: binding.grok,
                    facts: ModelRc::new(VecModel::from(
                        binding
                            .facts
                            .iter()
                            .map(|fact| PreviewFact {
                                label: fact.label.as_str().into(),
                                value: fact.value.as_str().into(),
                            })
                            .collect::<Vec<_>>(),
                    )),
                })
                .collect::<Vec<_>>(),
        )),
        notes: strings(&preview.notes),
    }
}

pub(super) fn route_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "本地端口必须在 1–65535 之间".into())
}

pub(super) async fn restore_connections(
    session: &mut RouteSession,
    data_dir: &Path,
    target: &Path,
) -> Result<(), String> {
    if route_switch::recovery(data_dir)?.is_some() {
        session.restore(data_dir, target).await?;
    } else if direct_switch::active_target(data_dir)?.is_some() {
        let result = direct_switch::restore(&client::config_path(target)?, data_dir)?;
        if !result.conflicts.is_empty() {
            return Err(format!(
                "配置恢复有冲突：{}；登录操作未开始",
                result.conflicts.join("、")
            ));
        }
    }
    Ok(())
}

pub(super) fn show_config_status(app: &AppWindow, status: client::ConfigStatus) {
    app.set_direct_active(status.direct_active);
    app.set_route_managed(status.route_managed);
    app.set_config_mode(status.mode.as_str().into());
    app.set_config_provider(status.provider.as_str().into());
    app.set_config_model(status.model.as_str().into());
    app.set_config_exists(status.config_exists);
    app.set_config_status(
        format!("{} · {} · {}", status.mode, status.provider, status.model).into(),
    );
}

pub(super) fn clear_previews(app: &AppWindow) {
    app.set_direct_preview_ready(false);
    app.set_route_preview_ready(false);
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
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
    app.on_apply_direct(queue_on(app, sender, |app| {
        Command::ApplyDirect(app.get_config_home().into())
    }));
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
    app.on_poll_route_status(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_busy()
        {
            let _ = callback_sender.try_send(Command::PollRouteStatus);
        }
    });
    app.on_inspect_route(queue_on(app, sender, |app| {
        // Invalidate the previous result before the worker begins checking again.
        app.set_route_preview_ready(false);
        app.set_route_preview(Default::default());
        app.set_action_message("".into());
        app.set_error_code("".into());
        app.set_error_message("".into());
        app.set_error_action("".into());
        Command::InspectRoute {
            home: app.get_config_home().into(),
            port: app.get_route_port().into(),
            model: app.get_default_model().into(),
        }
    }));
    app.on_apply_route(queue_on(app, sender, |app| Command::ApplyRoute {
        home: app.get_config_home().into(),
        port: app.get_route_port().into(),
        model: app.get_default_model().into(),
    }));
    app.on_launch_codex(queue_on(app, sender, |app| Command::LaunchCodex {
        home: app.get_config_home().into(),
    }));
    app.on_cancel_route_preview(queue_on(app, sender, |_| Command::CancelRoutePreview));
}

impl Worker {
    pub(super) fn inspect_direct(&mut self, id: String, target: String) {
        self.route_session.discard_preview();
        let _ = self
            .weak
            .upgrade_in_event_loop(|app| app.set_route_preview_ready(false));
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            let provider = app::load_provider(data_dir, &id)?;
            app::provider_credential(data_dir, &provider)?;
            let target = client::config_path(&codex_home(&target)?)?;
            let helper = switchx::credential_helper::executable()?;
            let state = app::load_provider_config(data_dir, &id)?;
            let prepared = PreparedDirectSwitch::inspect(&target, data_dir, &provider, &helper)?;
            let prepared = if let Some(options) = &state.options {
                prepared.with_codex_options(options, &state.common)?
            } else if !state.common.is_empty() {
                prepared.with_common_config(&state.common)?
            } else {
                prepared
            };
            let prepared = prepared.with_model_directory(&provider_models(data_dir, &id)?)?;
            Ok::<_, String>((prepared, provider, state))
        })();
        match result {
            Ok((switch, provider, state)) => {
                let summary = format!(
                    "目标：{} · 模型：{} · 受管变更：{}",
                    provider.name,
                    switch.default_model(),
                    switch.changes.join("、")
                );
                self.prepared = Some((switch, provider, state));
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    app.set_direct_preview(summary.into());
                    app.set_direct_preview_ready(true);
                    app.set_action_message("差异已准备；应用前将再次检查上游目录".into());
                });
            }
            Err(error) => {
                self.prepared = None;
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    show_action(&app, Err(error));
                });
            }
        }
    }

    pub(super) async fn apply_direct(&mut self, target: String) {
        let result = async {
            let (switch, selected, selected_config) =
                self.prepared.take().ok_or("请先预览直连变更")?;
            let (data_dir, provider, token) = (|| {
                let target_path = client::config_path(&codex_home(&target)?)?;
                if target_path != switch.target() {
                    return Err("配置目录已变化，请重新预览".into());
                }
                let data_dir = self.data_dir.path()?;
                let mut latest = app::load_provider(data_dir, &selected.id)?;
                if latest != selected
                    || app::load_provider_config(data_dir, &selected.id)? != selected_config
                {
                    return Err("上游资料已变化，请重新预览".into());
                }
                switch.validate_model_directory(&provider_models(data_dir, &selected.id)?)?;
                latest.model_id = switch.default_model().into();
                let token = app::provider_credential(data_dir, &latest)?;
                Ok::<_, String>((data_dir.to_path_buf(), latest, token))
            })()?;
            upstream::check_models(&provider, token.expose()).await?;
            switch.apply()?;
            let status = client::inspect(Path::new(&target), &data_dir).ok();
            Ok::<_, String>((
                "直连配置已写入；请重启目标 Codex 客户端后验证实际请求".to_owned(),
                status,
            ))
        }
        .await;
        let _ = self.weak.upgrade_in_event_loop(move |app| {
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

    pub(super) async fn restore_config(&mut self, target: String) {
        self.prepared = None;
        let result = async {
            let data_dir = self.data_dir.path()?;
            let target_home = codex_home(&target)?;
            if route_switch::recovery(data_dir)?.is_some() {
                self.route_session.restore(data_dir, &target_home).await?;
            } else {
                self.route_session.discard_preview();
                let result = direct_switch::restore(&client::config_path(&target_home)?, data_dir)?;
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
        let fallback_status = self
            .data_dir
            .ok()
            .and_then(|path| client::inspect(Path::new(&target), path).ok());
        let restored_data = result
            .as_ref()
            .ok()
            .and(self.data_dir.ok())
            .map(|data_dir| {
                let _ = common_config_for_home(data_dir, &target);
                (
                    load_snapshot(data_dir, false),
                    codex_home(&target).map(|target| account_view(data_dir, &target)),
                )
            });
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_direct_preview_ready(false);
            app.set_route_preview_ready(false);
            if let Some(status) = fallback_status {
                show_config_status(&app, status);
            }
            match result {
                Ok(status) => {
                    app.set_recovery_only(false);
                    show_config_status(&app, status);
                    show_action(
                        &app,
                        Ok(
                            "原配置的受管字段已恢复，本地路由已停止；请重启目标 Codex 客户端"
                                .into(),
                        ),
                    );
                    if let Some((snapshot, accounts)) = restored_data {
                        show_result(&app, snapshot);
                        if let Ok(accounts) = accounts {
                            show_accounts(&app, accounts);
                        }
                    }
                }
                Err(error) => show_action(&app, Err(error)),
            }
        });
    }

    pub(super) fn inspect_config(&mut self, target: String) {
        let recovery_only = self.recovery_only;
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            let managed = direct_switch::active_target(data_dir)?
                .or(route_switch::recovery(data_dir)?.map(|route| route.config_path));
            let target = managed
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .unwrap_or(codex_home(&target)?);
            if !recovery_only {
                common_config_for_home(data_dir, &target.to_string_lossy())?;
            }
            client::inspect(&target, data_dir).map(|status| (status, target))
        })();
        let accounts = result
            .as_ref()
            .ok()
            .filter(|_| !recovery_only)
            .and_then(|(_, target)| {
                self.account_home = Some(target.clone());
                self.data_dir
                    .ok()
                    .map(|data_dir| account_view(data_dir, target))
            });
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok((status, target)) => {
                app.set_recovery_only(recovery_only);
                if recovery_only {
                    app.set_active_page(5);
                }
                if let Some(accounts) = accounts {
                    show_accounts(&app, accounts);
                }
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

    pub(super) async fn inspect_route(&mut self, target: String, port: String, model: String) {
        self.prepared = None;
        let result = async {
            let data_dir = self.data_dir.path()?;
            let target = codex_home(&target)?;
            let helper = switchx::credential_helper::executable()?;
            self.route_session
                .prepare(data_dir, &target, route_port(&port)?, &model, &helper)
                .await
        }
        .await;
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_direct_preview_ready(false);
            app.set_route_preview_model(model.as_str().into());
            app.set_route_preview_ready(result.is_ok());
            match result {
                Ok(summary) => {
                    app.set_route_preview(route_preview_data(&summary));
                    show_action(
                        &app,
                        Ok("发布预览已准备；确认启用后点击“启动 Codex”，在新会话选择模型".into()),
                    );
                }
                Err(error) => {
                    app.set_route_preview(Default::default());
                    show_action(&app, Err(error));
                }
            }
        });
    }

    pub(super) async fn apply_route(&mut self, target: String, port: String, model: String) {
        let result = async {
            let data_dir = self.data_dir.path()?;
            self.route_session
                .apply(data_dir, &codex_home(&target)?, route_port(&port)?, &model)
                .await
        }
        .await;
        let status = self
            .data_dir
            .ok()
            .and_then(|path| client::inspect(Path::new(&target), path).ok());
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_route_preview_ready(false);
            if let Some(status) = status {
                show_config_status(&app, status);
            }
            show_action(
                &app,
                result.map(|()| {
                    "模型路由已开启；点击“启动 Codex”或运行 codex --no-daemon 新建会话。已有会话的连接不会随 /model 切换。".into()
                }),
            );
        });
    }

    pub(super) async fn launch_codex(&mut self, target: String) {
        let result = async {
            let data_dir = self.data_dir.path()?;
            let path = self
                .route_session
                .codex_launcher(data_dir, &codex_home(&target)?)
                .await?;
            client::open_codex_launcher(&path)?;
            Ok("已打开新的 Codex 会话；输入 /model 选择已发布模型".into())
        }
        .await;
        let _ = self
            .weak
            .upgrade_in_event_loop(move |app| show_action(&app, result));
    }

    pub(super) fn cancel_route_preview(&mut self) {
        self.route_session.discard_preview();
        let _ = self.weak.upgrade_in_event_loop(|app| {
            app.set_route_preview_ready(false);
            app.set_route_preview(Default::default());
            show_action(&app, Ok("已取消发布预览".into()));
        });
    }

    /// Reports the router and journal state after every command.
    pub(super) fn publish_route_status(&self) {
        let running = self.route_session.is_running();
        let recording_failed = self.route_session.recording_failed();
        let chatgpt_error = self.route_session.chatgpt_error();
        let provider_errors = self
            .data_dir
            .ok()
            .filter(|_| !self.recovery_only)
            .and_then(|path| Store::open_read_only(&path.join("switchx.sqlite")).ok())
            .and_then(|store| store.providers().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|provider| provider.kind == ProviderKind::Chatgpt)
            .map(|provider| {
                let error = self
                    .route_session
                    .chatgpt_error_for(&provider.id)
                    .unwrap_or("")
                    .to_owned();
                (provider.id, error)
            })
            .collect::<Vec<_>>();
        let address = self
            .route_session
            .address()
            .map(|address| format!("http://{address}/v1"))
            .unwrap_or_default();
        let route_managed = self
            .data_dir
            .ok()
            .is_some_and(|path| path.join(codex::ROUTE_JOURNAL).exists());
        let managed = self.data_dir.ok().is_some_and(codex::config_managed);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            let mut changed = false;
            let rows = app
                .get_providers()
                .iter()
                .map(|mut provider| {
                    let error = provider_errors
                        .iter()
                        .find(|(id, _)| id == provider.id.as_str())
                        .map(|(_, error)| error.as_str())
                        .unwrap_or("");
                    if provider.auth_error != error {
                        provider.auth_error = error.into();
                        changed = true;
                    }
                    provider
                })
                .collect::<Vec<_>>();
            if changed {
                app.set_providers(ModelRc::new(VecModel::from(rows)));
                filter_providers(&app, &app.get_provider_query());
            }
            app.set_route_running(running);
            app.set_route_auth_error(chatgpt_error.unwrap_or("").into());
            app.set_request_write_failed(recording_failed);
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
}

/// A connection's saved model mappings, read for previews and directory edits.
pub(super) fn provider_models(data_dir: &Path, id: &str) -> Result<Vec<ModelRecord>, String> {
    Ok(Store::open(&data_dir.join("switchx.sqlite"))
        .map_err(|_| "无法读取模型目录")?
        .models()
        .map_err(|_| "无法读取模型目录")?
        .into_iter()
        .filter(|model| model.provider_id == id)
        .collect())
}
