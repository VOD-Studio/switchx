//! Claude window callbacks share the same application services as agent controls.
use super::{
    command::{Command, queue},
    worker::Worker,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::path::{Path, PathBuf};
use switchx::{
    app,
    claude::{self, AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy, Roles},
    credentials::Secret,
    ui::{AppWindow, ClaudeConnectionRow, ClaudeModelRow, ClaudeRouteRow, ClaudeUi},
};
use tokio::sync::mpsc;

pub(super) enum PreparedChange {
    Direct(Box<claude::config::Prepared>),
    Route(Box<claude::route_session::PreparedRoute>),
}
impl PreparedChange {
    fn preview(&self) -> &claude::config::Preview {
        match self {
            Self::Direct(value) => &value.preview,
            Self::Route(value) => &value.preview,
        }
    }
}
fn route_options(ui: &ClaudeUi) -> Result<claude::publication::Options, String> {
    let port = if ui.get_route_port().trim().is_empty() {
        0
    } else {
        ui.get_route_port()
            .trim()
            .parse::<u16>()
            .map_err(|_| "端口须为 0–65535，0 表示自动分配")?
    };
    Ok(claude::publication::Options {
        mode: if ui.get_route_mode() == 0 {
            claude::publication::Mode::Route
        } else {
            claude::publication::Mode::Aggregate
        },
        primary: ui
            .get_route_provider_ids()
            .row_data(ui.get_route_primary() as usize)
            .unwrap_or_default()
            .into(),
        default_model: ui.get_route_default().trim().into(),
        port,
    })
}
fn show_route_models(ui: &ClaudeUi, models: Vec<claude::publication::PublishedModel>) {
    ui.set_route_models(ModelRc::new(VecModel::from(
        models
            .into_iter()
            .map(|m| ClaudeRouteRow {
                id: m.public_id.into(),
                label: m.label.into(),
                provider: m.provider_name.into(),
                upstream: m.upstream_model.into(),
                menu: m.menu,
            })
            .collect::<Vec<_>>(),
    )));
}

pub(super) struct SaveClaude {
    pub home: String,
    pub connection: Connection,
    pub key: Option<Secret>,
    pub expected: Option<i64>,
    pub generation: i32,
}
pub(super) struct FetchClaude {
    pub home: String,
    pub id: String,
    pub url: String,
    pub auth: AuthMode,
    pub key: Option<Secret>,
    pub generation: i32,
}
pub(super) fn model_row(model: &ModelOption) -> ClaudeModelRow {
    ClaudeModelRow {
        model: model.model.as_str().into(),
        label: model.label.as_str().into(),
        description: model.description.as_str().into(),
        behaves_as: model.behaves_as.as_str().into(),
        enabled: model.enabled,
    }
}
fn model_value(row: ClaudeModelRow) -> ModelOption {
    ModelOption {
        model: row.model.trim().into(),
        label: row.label.trim().into(),
        description: row.description.trim().into(),
        behaves_as: row.behaves_as.trim().into(),
        enabled: row.enabled,
    }
}
fn models(ui: &ClaudeUi) -> Vec<ClaudeModelRow> {
    ui.get_edit_models().iter().collect()
}
fn set_models(ui: &ClaudeUi, rows: Vec<ClaudeModelRow>) {
    ui.set_edit_models(ModelRc::new(VecModel::from(rows)));
}
fn edit_row(ui: &ClaudeUi, index: i32, edit: impl FnOnce(&mut ClaudeModelRow)) {
    let model = ui.get_edit_models();
    if let Some(rows) = model.as_any().downcast_ref::<VecModel<ClaudeModelRow>>()
        && let Some(mut row) = rows.row_data(index as usize)
    {
        edit(&mut row);
        rows.set_row_data(index as usize, row);
    }
}
fn form(window: &AppWindow) -> Result<SaveClaude, String> {
    let ui = window.global::<ClaudeUi>();
    let auth = AuthMode::from_index(ui.get_edit_auth())?;
    let picker = PickerPolicy::from_index(ui.get_edit_picker())?;
    let id = ui.get_edit_id().to_string();
    let expected = if id.is_empty() {
        None
    } else {
        Some(
            ui.get_edit_revision()
                .parse::<i64>()
                .map_err(|_| "连接版本无效")?,
        )
    };
    let connection = Connection {
        id: if id.is_empty() {
            switchx::id::new_id()?
        } else {
            id
        },
        workspace_id: ui.get_edit_workspace().into(),
        name: ui.get_edit_name().into(),
        base_url: ui.get_edit_url().into(),
        settings: ConnectionSettings {
            auth,
            picker,
            default_model: ui.get_edit_default().trim().into(),
            models: models(&ui)
                .into_iter()
                .map(model_value)
                .filter(|row| {
                    !(row.model.is_empty()
                        && row.label.is_empty()
                        && row.description.is_empty()
                        && row.behaves_as.is_empty())
                })
                .collect(),
            roles: Roles {
                sonnet: ui.get_role_sonnet().trim().into(),
                opus: ui.get_role_opus().trim().into(),
                haiku: ui.get_role_haiku().trim().into(),
                fable: ui.get_role_fable().trim().into(),
                subagent: ui.get_role_subagent().trim().into(),
            },
        },
        revision: expected.unwrap_or(0),
        has_key: false,
    };
    connection.settings.validate()?;
    let key = ui.get_edit_key();
    let key = (!key.trim().is_empty()).then(|| Secret::new(key.trim().into()));
    Ok(SaveClaude {
        home: window.get_claude_home().into(),
        connection,
        key,
        expected,
        generation: ui.get_editor_generation(),
    })
}

pub(super) fn connect(window: &AppWindow, sender: &mpsc::Sender<Command>) {
    let ui = window.global::<ClaudeUi>();
    ui.on_matches_query(|text, query| text.to_lowercase().contains(&query.to_lowercase()));
    macro_rules! action {
        ($method:ident,$build:expr) => {{
            let weak = window.as_weak();
            let sender = sender.clone();
            ui.$method(move || {
                if let Some(window) = weak.upgrade() {
                    let result: Result<Command, String> = ($build)(&window);
                    match result {
                        Ok(command) => queue(&window, &sender, command),
                        Err(error) => window.global::<ClaudeUi>().set_editor_error(error.into()),
                    }
                }
            });
        }};
    }
    action!(on_preview_routing, |window: &AppWindow| Ok(
        Command::ClaudeRoutePreview {
            home: window.get_claude_home().into(),
            options: route_options(&window.global::<ClaudeUi>())?
        }
    ));
    action!(on_refresh_route_models, |window: &AppWindow| Ok(
        Command::ClaudeRouteModels {
            home: window.get_claude_home().into(),
            options: route_options(&window.global::<ClaudeUi>())?
        }
    ));
    action!(on_refresh, |window: &AppWindow| Ok(Command::InspectClaude(
        window.get_claude_home().into()
    )));
    action!(on_check_cli, |window: &AppWindow| Ok(
        Command::ClaudeCheckCli {
            home: window.get_claude_home().into(),
            path: window.global::<ClaudeUi>().get_cli_setting().into()
        }
    ));
    action!(on_import_current, |window: &AppWindow| Ok(
        Command::ClaudeImport(window.get_claude_home().into())
    ));
    action!(on_save, |window: &AppWindow| {
        let form = form(window)?;
        window.global::<ClaudeUi>().set_edit_key("".into());
        Ok(Command::ClaudeSave(form))
    });
    action!(on_apply, |window: &AppWindow| Ok(Command::ClaudeApply(
        window.get_claude_home().into()
    )));
    action!(on_cancel_preview, |_: &AppWindow| Ok(
        Command::ClaudeCancelPreview
    ));
    action!(on_remove, |window: &AppWindow| {
        let ui = window.global::<ClaudeUi>();
        Ok(Command::ClaudeDelete {
            home: window.get_claude_home().into(),
            id: ui.get_delete_id().into(),
            revision: ui
                .get_delete_revision()
                .parse::<i64>()
                .map_err(|_| "连接版本无效")?,
        })
    });
    action!(on_fetch_models, |window: &AppWindow| {
        let ui = window.global::<ClaudeUi>();
        let key = ui.get_edit_key();
        let fetch = FetchClaude {
            home: window.get_claude_home().into(),
            id: ui.get_edit_id().into(),
            url: ui.get_edit_url().into(),
            auth: AuthMode::from_index(ui.get_edit_auth())?,
            key: (!key.trim().is_empty()).then(|| Secret::new(key.trim().into())),
            generation: ui.get_editor_generation(),
        };
        ui.set_fetching(true);
        ui.set_editor_error("".into());
        Ok(Command::ClaudeFetch(fetch))
    });
    action!(on_new_native, |window: &AppWindow| {
        let ui = window.global::<ClaudeUi>();
        let generation = ui.get_editor_generation().wrapping_add(1);
        ui.set_editor_generation(generation);
        Ok(Command::ClaudeEdit {
            home: window.get_claude_home().into(),
            id: String::new(),
            native: true,
            tab: 0,
            generation,
        })
    });
    let weak = window.as_weak();
    let edit_sender = sender.clone();
    ui.on_edit(move |id, tab| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let generation = ui.get_editor_generation().wrapping_add(1);
            ui.set_editor_generation(generation);
            queue(
                &window,
                &edit_sender,
                Command::ClaudeEdit {
                    home: window.get_claude_home().into(),
                    id: id.into(),
                    native: false,
                    tab,
                    generation,
                },
            );
        }
    });
    let weak = window.as_weak();
    let preview_sender = sender.clone();
    ui.on_preview(move |id| {
        if let Some(window) = weak.upgrade() {
            queue(
                &window,
                &preview_sender,
                Command::ClaudePreview {
                    home: window.get_claude_home().into(),
                    id: id.into(),
                },
            );
        }
    });
    let weak = window.as_weak();
    let restore_sender = sender.clone();
    ui.on_preview_recovery(move |keep| {
        if let Some(window) = weak.upgrade() {
            queue(
                &window,
                &restore_sender,
                Command::ClaudeRestorePreview {
                    home: window.get_claude_home().into(),
                    keep_conflicts: keep,
                },
            );
        }
    });
    let weak = window.as_weak();
    ui.on_model_edit(move |index, field, text| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let default = ui.get_edit_default();
            edit_row(&ui, index, |row| match field {
                0 => {
                    if default == row.model || default.is_empty() {
                        ui.set_edit_default(text.clone());
                    }
                    if row.label == row.model {
                        row.label = text.clone();
                    }
                    row.model = text;
                }
                1 => row.label = text,
                2 => row.description = text,
                3 => row.behaves_as = text,
                _ => {}
            });
        }
    });
    let weak = window.as_weak();
    ui.on_model_toggle(move |index, enabled| {
        if let Some(window) = weak.upgrade() {
            edit_row(&window.global::<ClaudeUi>(), index, |row| {
                row.enabled = enabled
            });
        }
    });
    let weak = window.as_weak();
    ui.on_toggle_all(move |enabled| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let mut rows = models(&ui);
            for row in &mut rows {
                row.enabled = enabled;
            }
            set_models(&ui, rows);
        }
    });
    let weak = window.as_weak();
    ui.on_add_model(move || {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let mut rows = models(&ui);
            if rows.len() < 200 {
                rows.push(model_row(&ModelOption::new("")));
                set_models(&ui, rows);
            }
        }
    });
    let weak = window.as_weak();
    ui.on_remove_model(move |index| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let mut rows = models(&ui);
            if index >= 0 && (index as usize) < rows.len() {
                rows.remove(index as usize);
                set_models(&ui, rows);
            }
        }
    });
    let weak = window.as_weak();
    ui.on_move_model(move |index, delta| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            let mut rows = models(&ui);
            let next = index.saturating_add(delta);
            if index >= 0
                && next >= 0
                && (index as usize) < rows.len()
                && (next as usize) < rows.len()
            {
                rows.swap(index as usize, next as usize);
                set_models(&ui, rows);
            }
        }
    });
    let weak = window.as_weak();
    ui.on_choose_default(move |index| {
        if let Some(window) = weak.upgrade() {
            let ui = window.global::<ClaudeUi>();
            edit_row(&ui, index, |row| {
                if !row.model.is_empty() {
                    row.enabled = true;
                    ui.set_edit_default(row.model.clone());
                }
            });
        }
    });
}

fn show_snapshot(window: &AppWindow, snapshot: app::claude::ClaudeSnapshot) {
    let ui = window.global::<ClaudeUi>();
    let selected = ui
        .get_connections()
        .row_data(ui.get_selected_connection() as usize)
        .map(|c| c.id);
    let rows: Vec<_> = snapshot
        .connections
        .iter()
        .map(|connection| ClaudeConnectionRow {
            id: connection.id.as_str().into(),
            name: connection.name.as_str().into(),
            endpoint: connection.base_url.as_str().into(),
            authentication: connection.settings.auth.label().into(),
            default_model: connection.settings.default_model.as_str().into(),
            picker: match connection.settings.picker {
                PickerPolicy::Preserve => "保留原菜单",
                PickerPolicy::Append => "追加到内置菜单",
                PickerPolicy::Replace => "由 SwitchX 管理菜单",
            }
            .into(),
            revision: connection.revision.to_string().into(),
            model_count: connection
                .settings
                .models
                .iter()
                .filter(|m| m.enabled)
                .count() as i32,
            native: connection.settings.auth == AuthMode::Native,
            models: ModelRc::new(VecModel::from(
                connection
                    .settings
                    .models
                    .iter()
                    .map(model_row)
                    .collect::<Vec<_>>(),
            )),
        })
        .collect();
    let selection = selected
        .and_then(|id| rows.iter().position(|c| c.id == id))
        .unwrap_or(0) as i32;
    if window.get_claude_workspace_id().as_str() != snapshot.workspace.id {
        ui.set_page(snapshot.workspace.last_page.clamp(0, 3));
    }
    window.set_claude_home(
        snapshot
            .workspace
            .key
            .home
            .to_string_lossy()
            .as_ref()
            .into(),
    );
    window.set_claude_workspace_id(snapshot.workspace.id.into());
    window.set_claude_status(
        if snapshot.status.pending {
            "有未完成的配置事务，请先恢复"
        } else if snapshot.status.managed {
            "用户配置已写入；当前会话请在 CLI 中核对"
        } else if snapshot.workspace.key.config_path().exists() {
            "已读取用户设置，尚未接管"
        } else {
            "用户配置尚不存在；保存连接不会创建它"
        }
        .into(),
    );
    let route_providers: Vec<_> = snapshot
        .connections
        .iter()
        .filter(|c| c.settings.auth != AuthMode::Native)
        .collect();
    ui.set_route_primary(
        route_providers
            .iter()
            .position(|c| c.id == snapshot.route_options.primary)
            .unwrap_or(0) as i32,
    );
    ui.set_route_provider_ids(ModelRc::new(VecModel::from(
        route_providers
            .iter()
            .map(|c| c.id.as_str().into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    ui.set_route_provider_names(ModelRc::new(VecModel::from(
        route_providers
            .iter()
            .map(|c| c.name.as_str().into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    ui.set_route_mode(
        if snapshot.route_options.mode == claude::publication::Mode::Route {
            0
        } else {
            1
        },
    );
    ui.set_route_default(snapshot.route_options.default_model.into());
    ui.set_route_port(snapshot.route_options.port.to_string().into());
    show_route_models(&ui, snapshot.route_models);
    ui.set_route_error(snapshot.route_error.into());
    ui.set_recent_requests(ModelRc::new(VecModel::from(
        snapshot
            .recent_requests
            .into_iter()
            .map(Into::into)
            .collect::<Vec<slint::SharedString>>(),
    )));
    ui.set_active_mode(snapshot.status.mode.into());
    ui.set_route_endpoint(snapshot.status.endpoint.unwrap_or_default().into());
    ui.set_loaded(true);
    ui.set_managed(snapshot.status.managed);
    ui.set_recovering(snapshot.status.pending);
    ui.set_active_id(snapshot.status.provider_id.into());
    ui.set_active_name(snapshot.status.provider_name.into());
    ui.set_config_error(snapshot.configuration_error.unwrap_or_default().into());
    ui.set_cli_setting(snapshot.cli_path.into());
    ui.set_connection_names(ModelRc::new(VecModel::from(
        rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
    )));
    ui.set_connections(ModelRc::new(VecModel::from(rows)));
    ui.set_selected_connection(selection);
}

fn show_editor(
    window: &AppWindow,
    workspace: &str,
    connection: Option<Connection>,
    native: bool,
    tab: i32,
) {
    let ui = window.global::<ClaudeUi>();
    let (id, revision, name, url, settings) = match connection {
        Some(c) => (c.id, c.revision.to_string(), c.name, c.base_url, c.settings),
        None => (
            String::new(),
            String::new(),
            if native {
                "本机官方登录".into()
            } else {
                String::new()
            },
            if native {
                String::new()
            } else {
                "https://api.anthropic.com".into()
            },
            if native {
                ConnectionSettings {
                    auth: AuthMode::Native,
                    picker: PickerPolicy::Preserve,
                    ..Default::default()
                }
            } else {
                ConnectionSettings {
                    models: vec![ModelOption::new("")],
                    ..Default::default()
                }
            },
        ),
    };
    ui.set_edit_workspace(workspace.into());
    ui.set_edit_id(id.into());
    ui.set_edit_revision(revision.into());
    ui.set_edit_name(name.into());
    ui.set_edit_url(url.into());
    ui.set_edit_key("".into());
    ui.set_edit_auth(settings.auth.index());
    ui.set_edit_picker(settings.picker.index());
    ui.set_edit_default(settings.default_model.into());
    ui.set_role_sonnet(settings.roles.sonnet.into());
    ui.set_role_opus(settings.roles.opus.into());
    ui.set_role_haiku(settings.roles.haiku.into());
    ui.set_role_fable(settings.roles.fable.into());
    ui.set_role_subagent(settings.roles.subagent.into());
    set_models(&ui, settings.models.iter().map(model_row).collect());
    ui.set_editor_error("".into());
    ui.set_fetching(false);
    ui.set_editor_tab(tab.clamp(0, 2));
    ui.set_editor_open(true);
}

pub(super) fn preview_text(preview: &claude::config::Preview) -> String {
    let mut text = format!(
        "目标：{}
连接：{}
操作：{}

受管字段
",
        preview.target.display(),
        preview.provider,
        match preview.operation {
            "restore" => "恢复配置",
            "route" => "发布单连接路由",
            "aggregate" => "发布模型聚合",
            _ => "应用直连",
        }
    );
    if preview.fields.is_empty() {
        text.push_str(
            "没有需要改写的字段
",
        );
    }
    for field in &preview.fields {
        text.push_str(&format!(
            "• {field}
"
        ));
    }
    if !preview.models.is_empty() {
        text.push_str(
            "
模型菜单
",
        );
        for model in &preview.models {
            text.push_str(&format!(
                "• {}  ·  {}
",
                model
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                model["model"].as_str().unwrap_or("")
            ));
        }
    }
    if !preview.conflicts.is_empty() {
        text.push_str(
            "
外部修改将保留
",
        );
        text.push_str(&preview.conflicts.join(
            "
",
        ));
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&preview.notes.join(
        "
",
    ));
    text
}

impl Worker {
    pub(super) fn inspect_claude(&mut self, home: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|data| app::claude::load_claude(data, Path::new(&home)));
        let cli = result.as_ref().ok().map(|snapshot| {
            (
                snapshot.workspace.key.home.clone(),
                snapshot.cli_path.clone(),
            )
        });
        let shown = home.clone();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            if window.get_claude_home().as_str() != shown {
                return;
            }
            window.set_busy(false);
            match result {
                Ok(snapshot) => show_snapshot(&window, snapshot),
                Err(error) => {
                    let ui = window.global::<ClaudeUi>();
                    ui.set_loaded(false);
                    ui.set_config_error(error.into());
                }
            }
        });
        if let Some((target, setting)) = cli {
            self.start_claude_cli_check(target, setting);
        }
    }

    fn start_claude_cli_check(&mut self, home: PathBuf, setting: String) {
        let id = switchx::id::new_id().unwrap_or_default();
        let weak = self.weak.clone();
        let check_id = id.clone();
        let shown = home.clone();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            if window.get_claude_home().as_str() != shown.to_string_lossy() {
                return;
            }
            let ui = window.global::<ClaudeUi>();
            ui.set_cli_checking(true);
            ui.set_cli_check_id(check_id.into());
        });
        self.provider_checks.spawn(async move {
            let result = claude::client::inspect(Some(&setting), &home).await;
            let _ = weak.upgrade_in_event_loop(move |window| {
                let ui = window.global::<ClaudeUi>();
                if ui.get_cli_check_id().as_str() != id
                    || window.get_claude_home().as_str() != home.to_string_lossy()
                {
                    return;
                }
                ui.set_cli_checking(false);
                match result {
                    Ok(info) => {
                        ui.set_cli_version(info.version.as_str().into());
                        ui.set_supports_menu(info.capabilities.model_picker);
                        ui.set_supports_behaves_as(info.capabilities.model_behaves_as);
                        ui.set_cli_message(
                            format!(
                                "Claude Code {} · {}",
                                info.version,
                                if info.capabilities.model_picker {
                                    "支持自定义模型菜单"
                                } else {
                                    "此版本不支持自定义模型菜单"
                                }
                            )
                            .into(),
                        );
                    }
                    Err(error) => {
                        ui.set_cli_version("".into());
                        ui.set_supports_menu(false);
                        ui.set_supports_behaves_as(false);
                        ui.set_cli_message(error.into());
                    }
                }
            });
        });
    }

    pub(super) fn claude_check_cli(&mut self, home: String, path: String) {
        self.discard_previews();
        let result = self
            .data_dir
            .path()
            .and_then(|data| app::claude::save_claude_cli(data, Path::new(&home), &path));
        match result {
            Ok(()) => {
                self.start_claude_cli_check(PathBuf::from(&home), path);
                let _ = self
                    .weak
                    .upgrade_in_event_loop(|window| window.set_busy(false));
            }
            Err(error) => {
                let _ = self.weak.upgrade_in_event_loop(move |window| {
                    window.set_busy(false);
                    window.global::<ClaudeUi>().set_cli_message(error.into());
                });
            }
        }
    }

    pub(super) fn begin_claude_editor(
        &mut self,
        home: String,
        id: String,
        native: bool,
        tab: i32,
        generation: i32,
    ) {
        self.discard_previews();
        let result: Result<_, String> = (|| {
            let data = self.data_dir.path()?;
            let snapshot = app::claude::load_claude(data, Path::new(&home))?;
            if !id.is_empty() && snapshot.status.managed {
                return Err("请先恢复此 Claude 工作空间的配置，再编辑已有连接".into());
            }
            let connection = if id.is_empty() {
                None
            } else {
                Some(
                    snapshot
                        .connections
                        .into_iter()
                        .find(|c| c.id == id)
                        .ok_or("连接不存在，请刷新")?,
                )
            };
            Ok((snapshot.workspace.id, connection))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            let ui = window.global::<ClaudeUi>();
            if window.get_active_page() != 6
                || window.get_claude_home().as_str() != home
                || ui.get_editor_generation() != generation
            {
                return;
            }
            match result {
                Ok((workspace, connection)) => {
                    show_editor(&window, &workspace, connection, native, tab)
                }
                Err(error) => ui.set_feedback(error.into()),
            }
        });
    }

    pub(super) fn save_claude(&mut self, form: SaveClaude) {
        self.discard_previews();
        let result = self.data_dir.path().and_then(|data| {
            let saved = app::claude::save_claude_connection(
                data,
                Path::new(&form.home),
                form.connection,
                form.key,
                form.expected,
            )?;
            Ok((
                saved.id,
                app::claude::load_claude(data, Path::new(&form.home))?,
            ))
        });
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            let ui = window.global::<ClaudeUi>();
            if window.get_claude_home().as_str() != form.home
                || ui.get_editor_generation() != form.generation
            {
                return;
            }
            match result {
                Ok((id, snapshot)) => {
                    show_snapshot(&window, snapshot);
                    ui.set_editor_open(false);
                    ui.set_edit_key("".into());
                    let index = ui
                        .get_connections()
                        .iter()
                        .position(|row| row.id.as_str() == id)
                        .unwrap_or(0);
                    ui.set_selected_connection(index as i32);
                    ui.set_feedback("连接已保存；在连接页预览并启用。".into());
                }
                Err(error) => ui.set_editor_error(error.into()),
            }
        });
    }

    pub(super) fn import_claude(&mut self, home: String) {
        self.discard_previews();
        let result = self.data_dir.path().and_then(|data| {
            app::claude::import_claude_current(data, Path::new(&home))?;
            app::claude::load_claude(data, Path::new(&home))
        });
        self.finish_claude(home, result, "已导入连接与模型，尚未应用。", false);
    }
    pub(super) fn delete_claude(&mut self, home: String, id: String, revision: i64) {
        self.discard_previews();
        let result = self.data_dir.path().and_then(|data| {
            app::claude::delete_claude_connection(data, Path::new(&home), &id, revision)?;
            app::claude::load_claude(data, Path::new(&home))
        });
        self.finish_claude(home, result, "连接已删除，客户端配置未修改。", true);
    }
    fn finish_claude(
        &self,
        home: String,
        result: Result<app::claude::ClaudeSnapshot, String>,
        message: &'static str,
        close_delete: bool,
    ) {
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            if window.get_claude_home().as_str() != home {
                return;
            }
            let ui = window.global::<ClaudeUi>();
            match result {
                Ok(snapshot) => {
                    show_snapshot(&window, snapshot);
                    ui.set_feedback(message.into());
                    if close_delete {
                        ui.set_delete_open(false);
                    }
                }
                Err(error) => ui.set_feedback(error.into()),
            }
        });
    }

    pub(super) fn fetch_claude_models(&mut self, fetch: FetchClaude) {
        let key = fetch.key.map(Ok).unwrap_or_else(|| {
            self.data_dir.path().and_then(|data| {
                app::claude::claude_connection_key(data, Path::new(&fetch.home), &fetch.id)
            })
        });
        let weak = self.weak.clone();
        let _ = self
            .weak
            .upgrade_in_event_loop(|window| window.set_busy(false));
        self.provider_checks.spawn(async move {
            let result = match key {
                Ok(key) => app::claude::discover_claude_models(&fetch.url, fetch.auth, &key).await,
                Err(error) => Err(error),
            };
            let _ = weak.upgrade_in_event_loop(move |window| {
                let ui = window.global::<ClaudeUi>();
                if !ui.get_editor_open()
                    || ui.get_editor_generation() != fetch.generation
                    || window.get_claude_home().as_str() != fetch.home
                {
                    return;
                }
                ui.set_fetching(false);
                match result {
                    Ok(found) => {
                        let mut rows = models(&ui);
                        rows.retain(|row| !row.model.is_empty());
                        for model in found {
                            if rows.len() < 200
                                && !rows.iter().any(|row| row.model.as_str() == model.model)
                            {
                                rows.push(model_row(&model));
                            }
                        }
                        if ui.get_edit_default().is_empty()
                            && let Some(row) = rows.first()
                        {
                            ui.set_edit_default(row.model.clone());
                        }
                        set_models(&ui, rows);
                        ui.set_editor_error("".into());
                        ui.set_editor_tab(1);
                    }
                    Err(error) => ui.set_editor_error(error.into()),
                }
            });
        });
    }

    pub(super) fn claude_route_models(
        &mut self,
        home: String,
        options: claude::publication::Options,
    ) {
        self.discard_previews();
        let result = (|| -> Result<_, String> {
            let data = self.data_dir.path()?;
            app::claude::route_plan(data, Path::new(&home), options)
        })();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            if window.get_claude_home().as_str() != home {
                return;
            }
            let ui = window.global::<ClaudeUi>();
            match result {
                Ok(plan) => {
                    show_route_models(&ui, plan.models);
                    ui.set_route_error("".into());
                }
                Err(error) => ui.set_route_error(error.into()),
            }
        });
    }
    pub(super) async fn preview_claude_route(
        &mut self,
        home: String,
        options: claude::publication::Options,
    ) -> Result<claude::config::Preview, String> {
        self.discard_previews();
        let result = async {
            let data = self.data_dir.path()?;
            let cli = app::claude::inspect_claude_cli(data, Path::new(&home)).await?;
            let helper = switchx::credential_helper::executable()?;
            self.claude_routes
                .prepare(data, Path::new(&home), options, &helper, cli.capabilities)
                .await
        }
        .await;
        self.publish_claude_preview(
            home,
            result.map(|prepared| PreparedChange::Route(Box::new(prepared))),
        )
    }
    pub(super) fn publish_claude_route_status(&self) {
        let statuses = self.claude_routes.statuses();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            let ui = window.global::<ClaudeUi>();
            let status = statuses
                .iter()
                .find(|(id, _, _)| id == window.get_claude_workspace_id().as_str());
            let running = status.is_some_and(|(_, running, _)| *running);
            ui.set_route_running(running);
            ui.set_route_recording_failed(status.is_some_and(|(_, _, failed)| *failed));
            if matches!(ui.get_active_mode().as_str(), "route" | "aggregate") && !running {
                ui.set_recovering(true);
                window.set_claude_status("路由未运行，请先恢复配置；不会自动启动旧路由".into());
            }
        });
    }

    pub(super) async fn preview_claude(
        &mut self,
        home: String,
        id: String,
    ) -> Result<claude::config::Preview, String> {
        self.discard_previews();
        let result = async {
            let data = self.data_dir.path()?;
            let helper = switchx::credential_helper::executable()?;
            app::claude::preview_claude_direct(data, Path::new(&home), &id, &helper).await
        }
        .await;
        self.publish_claude_preview(
            home,
            result.map(|prepared| PreparedChange::Direct(Box::new(prepared))),
        )
    }
    pub(super) fn preview_claude_restore(
        &mut self,
        home: String,
        keep: bool,
    ) -> Result<claude::config::Preview, String> {
        self.discard_previews();
        let result = self
            .data_dir
            .path()
            .and_then(|data| app::claude::preview_claude_restore(data, Path::new(&home), keep));
        self.publish_claude_preview(
            home,
            result.map(|prepared| PreparedChange::Direct(Box::new(prepared))),
        )
    }
    fn publish_claude_preview(
        &mut self,
        home: String,
        result: Result<PreparedChange, String>,
    ) -> Result<claude::config::Preview, String> {
        let result = result.map(|prepared| {
            let preview = prepared.preview().clone();
            self.claude_prepared = Some(prepared);
            preview
        });
        let shown = result.clone();
        let _ = self.weak.upgrade_in_event_loop(move |window| {
            window.set_busy(false);
            if window.get_claude_home().as_str() != home {
                return;
            }
            let ui = window.global::<ClaudeUi>();
            match shown {
                Ok(preview) => {
                    ui.set_preview_text(preview_text(&preview).into());
                    ui.set_preview_restore(preview.operation == "restore");
                    ui.set_preview_route(matches!(preview.operation, "route" | "aggregate"));
                    ui.set_preview_conflicts(!preview.conflicts.is_empty());
                    ui.set_preview_ready(true);
                    ui.set_feedback("".into());
                    ui.set_preview_open(true);
                }
                Err(error) => {
                    ui.set_preview_ready(false);
                    ui.set_feedback(error.into());
                }
            }
        });
        result
    }
    pub(super) async fn apply_claude(&mut self, home: String) -> Result<Vec<String>, String> {
        let result = async {
            let prepared = self
                .claude_prepared
                .take()
                .ok_or("请先预览 Claude 配置变更")?;
            if self.recovery_only && prepared.preview().operation != "restore" {
                return Err("旧版资料有配置待恢复，请先恢复".into());
            }
            let data = self.data_dir.path()?;
            match prepared {
                PreparedChange::Direct(prepared) => {
                    if prepared.preview.operation == "restore" {
                        self.claude_routes
                            .restore(data, Path::new(&home), *prepared)
                            .await
                    } else {
                        let cli = app::claude::inspect_claude_cli(data, Path::new(&home)).await?;
                        prepared.validate_capabilities(cli.capabilities)?;
                        app::claude::apply_claude_change(data, Path::new(&home), *prepared)
                    }
                }
                PreparedChange::Route(prepared) => {
                    let workspace = app::claude::claude_workspace(data, Path::new(&home))?;
                    if prepared.workspace_id() != workspace.id {
                        return Err("预览属于其他 Claude 工作空间".into());
                    }
                    let cli = app::claude::inspect_claude_cli(data, Path::new(&home)).await?;
                    self.claude_routes
                        .apply(data, *prepared, cli.capabilities)
                        .await?;
                    Ok(Vec::new())
                }
            }
        }
        .await;
        let snapshot = self
            .data_dir
            .path()
            .and_then(|data| app::claude::load_claude(data, Path::new(&home)));
        if self.recovery_only && snapshot.is_ok() {
            self.recovery_only = self
                .data_dir
                .path()
                .and_then(|data| {
                    switchx::storage::Store::needs_recovery_before_migration(
                        &data.join("switchx.sqlite"),
                    )
                    .map_err(|_| "无法检查恢复状态".into())
                })
                .unwrap_or(true);
        }
        let recovery_only = self.recovery_only;
        let shown = result.clone();
        let _=self.weak.upgrade_in_event_loop(move |window|{
            window.set_busy(false);window.set_recovery_only(recovery_only);if window.get_claude_home().as_str()!=home{return;}let ui=window.global::<ClaudeUi>();ui.set_preview_ready(false);
            if let Ok(snapshot)=snapshot{show_snapshot(&window,snapshot);}
            match shown{Ok(conflicts)=>{ui.set_preview_open(false);ui.set_feedback(if conflicts.is_empty(){"配置操作已完成；当前会话请用 /model 与 /status 核对。".into()}else{format!("已恢复可还原字段，外部修改已保留：{}。可再次预览并选择保留外部值结束管理。",conflicts.join("、")).into()});},Err(error)=>ui.set_feedback(error.into())}
        });
        result
    }
    pub(super) fn cancel_claude_preview(&mut self) {
        self.claude_prepared = None;
        let _ = self.weak.upgrade_in_event_loop(|window| {
            window.set_busy(false);
            window.global::<ClaudeUi>().set_preview_ready(false);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use i_slint_backend_testing::ElementHandle;
    use switchx::{headless, ui::Theme};

    #[test]
    fn editor_preserves_order_defaults_and_drops_secrets_on_navigation() {
        let window = headless::install_with_manual_time();
        let app = AppWindow::new().unwrap();
        let (sender, mut receiver) = mpsc::channel(16);
        connect(&app, &sender);
        app.set_loading(false);
        app.set_active_client(1);
        app.set_active_page(6);
        app.set_claude_home("/isolated/claude".into());
        app.set_claude_status("用户配置已写入；当前会话请在 CLI 中核对".into());
        app.set_app_version(env!("CARGO_PKG_VERSION").into());
        let ui = app.global::<ClaudeUi>();
        ui.set_loaded(true);
        ui.set_supports_menu(true);
        ui.set_supports_behaves_as(true);
        let mut sonnet = ModelOption::new("team/sonnet");
        sonnet.label = "Sonnet · 日常编码".into();
        sonnet.description = "主力模型，适合代码修改与工具调用".into();
        sonnet.behaves_as = "claude-sonnet-4-6".into();
        let mut opus = ModelOption::new("team/opus");
        opus.label = "Opus · 深度推理".into();
        let connection = Connection {
            id: "a".repeat(32),
            workspace_id: "b".repeat(32),
            name: "工作网关".into(),
            base_url: "https://api.example.test/anthropic".into(),
            settings: ConnectionSettings {
                auth: AuthMode::Helper,
                picker: PickerPolicy::Replace,
                models: vec![sonnet, opus],
                default_model: "team/sonnet".into(),
                ..Default::default()
            },
            revision: 1,
            has_key: true,
        };
        ui.set_connections(ModelRc::new(VecModel::from(vec![ClaudeConnectionRow {
            id: connection.id.as_str().into(),
            name: connection.name.as_str().into(),
            endpoint: connection.base_url.as_str().into(),
            authentication: "SwitchX 凭据 helper".into(),
            default_model: "team/sonnet".into(),
            picker: "由 SwitchX 管理菜单".into(),
            revision: "1".into(),
            model_count: 2,
            native: false,
            models: ModelRc::new(VecModel::from(
                connection
                    .settings
                    .models
                    .iter()
                    .map(model_row)
                    .collect::<Vec<_>>(),
            )),
        }])));
        ui.set_connection_names(ModelRc::new(VecModel::from(vec!["工作网关".into()])));
        ui.set_managed(true);
        ui.set_active_id(connection.id.as_str().into());
        ui.set_active_name("工作网关".into());
        app.show().unwrap();
        let settle = || {
            for _ in 0..3 {
                headless::advance(400);
                headless::render(&window);
            }
        };
        for (width, height) in [(1000, 680), (1400, 900)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                app.global::<Theme>().set_animations_enabled(true);
                ui.set_editor_open(false);
                ui.set_page(0);
                settle();
                let label = "预览 Claude 连接 工作网关";
                let button = ElementHandle::find_by_accessible_label(&app, label)
                    .next()
                    .unwrap();
                let pos = button.absolute_position();
                let size = button.size();
                assert!(
                    pos.x >= 0.0 && pos.x + size.width <= width as f32,
                    "connection action exceeds window"
                );
                headless::save_snapshot(
                    "SWITCHX_CLAUDE_SNAPSHOTS",
                    &format!(
                        "connections-{width}-{}",
                        if dark { "dark" } else { "light" }
                    ),
                    &headless::render(&window),
                );
                show_editor(
                    &app,
                    &connection.workspace_id,
                    Some(connection.clone()),
                    false,
                    1,
                );
                settle();
                for label in ["Claude 模型 ID 0", "默认 Claude 模型 0", "保存 Claude 连接"]
                {
                    let item = ElementHandle::find_by_accessible_label(&app, label)
                        .next()
                        .unwrap();
                    let pos = item.absolute_position();
                    let size = item.size();
                    assert!(
                        pos.x >= 0.0 && pos.x + size.width <= width as f32,
                        "{label} exceeds width"
                    );
                    assert!(
                        pos.y >= 0.0 && pos.y + size.height <= height as f32,
                        "{label} exceeds height"
                    );
                }
                headless::save_snapshot(
                    "SWITCHX_CLAUDE_SNAPSHOTS",
                    &format!("models-{width}-{}", if dark { "dark" } else { "light" }),
                    &headless::render(&window),
                );
            }
        }
        ui.invoke_model_edit(0, 1, "自定义名称".into());
        ui.invoke_model_edit(0, 0, "team/manual".into());
        assert_eq!(ui.get_edit_default(), "team/manual");
        assert_eq!(models(&ui)[0].label, "自定义名称");
        ui.invoke_move_model(0, 1);
        assert_eq!(models(&ui)[1].model, "team/manual");
        ui.invoke_toggle_all(false);
        assert_eq!(ui.get_edit_default(), "team/manual");
        ui.invoke_choose_default(0);
        assert!(models(&ui)[0].enabled);
        assert_eq!(ui.get_edit_default(), "team/opus");
        ui.set_edit_key("synthetic-ui-key".into());
        ui.invoke_save();
        let Command::ClaudeSave(saved) = receiver.try_recv().unwrap() else {
            panic!("expected save");
        };
        assert_eq!(saved.connection.settings.models[1].model, "team/manual");
        assert_eq!(saved.key.unwrap().expose(), "synthetic-ui-key");
        assert_eq!(saved.expected, Some(1));
        assert!(ui.get_edit_key().is_empty());
        app.set_busy(false);
        ui.set_edit_key("synthetic-cancel-key".into());
        ui.set_editor_open(false);
        settle();
        assert!(ui.get_edit_key().is_empty());
        show_editor(
            &app,
            &connection.workspace_id,
            Some(connection.clone()),
            false,
            0,
        );
        settle();
        ui.set_edit_key("synthetic-navigation-key".into());
        app.set_active_page(5);
        settle();
        assert!(!ui.get_editor_open());
        assert!(ui.get_edit_key().is_empty());
        app.set_active_page(6);
        show_editor(&app, "synthetic-workspace", None, false, 0);
        ui.set_edit_auth(AuthMode::Native.index());
        let official = form(&app).unwrap();
        assert!(official.connection.settings.models.is_empty());
        assert!(official.key.is_none());
        super::super::clients::connect(&app, &sender);
        app.set_claude_workspace_id("synthetic-workspace".into());
        ui.set_page(2);
        app.invoke_client_navigation(1, 6);
        let Command::ClientNavigation { workspace_id, page } = receiver.try_recv().unwrap() else {
            panic!("expected navigation");
        };
        assert_eq!(workspace_id, "synthetic-workspace");
        assert_eq!(page, 2);
    }
    #[test]
    fn routing_form_keeps_client_targets_and_survives_compact_layouts() {
        let window = headless::install_with_manual_time();
        let app = AppWindow::new().unwrap();
        app.set_app_version(env!("CARGO_PKG_VERSION").into());
        let (sender, mut receiver) = mpsc::channel(16);
        connect(&app, &sender);
        app.set_loading(false);
        app.set_active_page(6);
        app.set_active_client(1);
        app.set_claude_home("/isolated/claude".into());
        app.set_claude_workspace_id("synthetic-workspace".into());
        app.set_route_running(true);
        app.set_config_home("/isolated/codex".into());
        let ui = app.global::<ClaudeUi>();
        ui.set_loaded(true);
        ui.set_managed(true);
        ui.set_active_mode("aggregate".into());
        ui.set_route_running(true);
        ui.set_route_endpoint("http://127.0.0.1:18732".into());
        ui.set_route_provider_ids(ModelRc::new(VecModel::from(vec![
            "alpha".into(),
            "beta".into(),
        ])));
        ui.set_route_provider_names(ModelRc::new(VecModel::from(vec![
            "工作网关".into(),
            "备用开发网关".into(),
        ])));
        ui.set_route_port("18732".into());
        ui.set_route_default("shared-model".into());
        ui.set_route_models(ModelRc::new(VecModel::from(vec![
            ClaudeRouteRow {
                id: "sx-claude-synthetic-a".into(),
                label: "Sonnet · 工作网关".into(),
                provider: "工作网关".into(),
                upstream: "shared-model".into(),
                menu: true,
            },
            ClaudeRouteRow {
                id: "sx-claude-synthetic-b".into(),
                label: "Sonnet · 备用开发网关".into(),
                provider: "备用开发网关".into(),
                upstream: "shared-model".into(),
                menu: true,
            },
        ])));
        ui.set_recent_requests(ModelRc::new(VecModel::from(vec![
            "正常完成 · 工作网关 · shared-model · 1.2 s".into(),
        ])));
        app.show().unwrap();
        let settle = || {
            for _ in 0..3 {
                headless::advance(400);
                headless::render(&window);
            }
        };
        for (width, height) in [(1000, 680), (1400, 900)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                ui.set_page(3);
                settle();
                for label in [
                    "预览 Claude 模型路由",
                    "Claude 路由默认连接",
                    "Claude 路由默认模型",
                    "预览恢复 Claude 配置",
                ] {
                    let item = ElementHandle::find_by_accessible_label(&app, label)
                        .next()
                        .unwrap();
                    let pos = item.absolute_position();
                    let size = item.size();
                    assert!(
                        pos.x >= 0.0 && pos.x + size.width <= width as f32,
                        "{label} outside width"
                    );
                    assert!(
                        pos.y >= 0.0 && pos.y + size.height <= height as f32,
                        "{label} outside height"
                    );
                }
                headless::save_snapshot(
                    "SWITCHX_CLAUDE_ROUTE_SNAPSHOTS",
                    &format!("routing-{width}-{}", if dark { "dark" } else { "light" }),
                    &headless::render(&window),
                );
            }
        }
        ui.invoke_preview_routing();
        let Command::ClaudeRoutePreview { home, options } = receiver.try_recv().unwrap() else {
            panic!("expected route preview");
        };
        assert_eq!(home, "/isolated/claude");
        assert_eq!(options.primary, "alpha");
        assert_eq!(options.mode, claude::publication::Mode::Aggregate);
        assert_eq!(options.port, 18732);
        assert!(app.get_route_running());
        app.set_busy(false);
        ui.set_route_primary(1);
        ui.set_route_mode(0);
        ui.invoke_refresh_route_models();
        let Command::ClaudeRouteModels { options, .. } = receiver.try_recv().unwrap() else {
            panic!("expected model refresh");
        };
        assert_eq!(options.primary, "beta");
        assert_eq!(options.mode, claude::publication::Mode::Route);
        app.set_busy(false);
        ui.set_page(2);
        settle();
        let item = ElementHandle::find_by_accessible_label(&app, "Claude Code 配置目录绝对路径")
            .next()
            .unwrap();
        let pos = item.absolute_position();
        let size = item.size();
        headless::click(&window, pos.x + size.width / 2.0, pos.y + size.height / 2.0);
        headless::key(&window, "x");
        settle();
        assert!(app.get_claude_workspace_id().is_empty());
        assert!(!ui.get_managed());
        assert!(app.get_route_running());
        assert_eq!(app.get_config_home(), "/isolated/codex");
    }
}
