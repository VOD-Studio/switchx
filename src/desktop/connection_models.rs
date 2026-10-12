//! A connection's model directory and model discovery.

use std::collections::HashSet;

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app, catalog, chatgpt,
    credentials::Secret,
    storage::{ModelRecord, ProviderKind, ProviderRecord},
    ui::{AppWindow, ConnectionModelRow},
    upstream, xai,
};
use tokio::sync::mpsc;

use super::{
    batch_picker::{lock_batch_rows, show_batch_candidates, update_batch_rows},
    codex_config::provider_models,
    command::{Command, queue},
    feedback::show_action,
    subscription_editor::close_subscription_editor,
    workbench::show_result,
    worker::Worker,
};

pub(super) fn connection_model_row(model: &ModelRecord) -> ConnectionModelRow {
    let metadata: serde_json::Value = serde_json::from_str(&model.metadata).unwrap_or_default();
    ConnectionModelRow {
        public_id: model.public_id.clone().into(),
        display_name: model.display_name.clone().into(),
        upstream_model: model.upstream_model.clone().into(),
        context_window: metadata["context_window"]
            .as_i64()
            .map(|value| value.to_string())
            .unwrap_or_default()
            .into(),
        reasoning_levels: metadata["supported_reasoning_levels"]
            .as_array()
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| level["effort"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
            .into(),
        default_reasoning: metadata["default_reasoning_level"]
            .as_str()
            .unwrap_or_default()
            .into(),
        removing: false,
    }
}

pub(super) fn connection_directory_rows(
    saved: &[ModelRecord],
    discovered: Vec<app::ConnectionModel>,
) -> Vec<ConnectionModelRow> {
    let mut rows: Vec<_> = saved.iter().map(connection_model_row).collect();
    let mut known: HashSet<_> = saved
        .iter()
        .map(|model| model.upstream_model.clone())
        .collect();
    for model in discovered {
        if known.insert(model.upstream_model.clone()) {
            rows.push(ConnectionModelRow {
                display_name: model.display_name.into(),
                upstream_model: model.upstream_model.into(),
                context_window: model.context_window.into(),
                reasoning_levels: model.reasoning_levels.into(),
                default_reasoning: model.default_reasoning.into(),
                ..Default::default()
            });
        }
    }
    rows
}

pub(super) fn show_connection_directory(
    app: &AppWindow,
    provider: ProviderRecord,
    models: &[ModelRecord],
    discovered: Vec<app::ConnectionModel>,
) {
    app.set_connection_models_name(provider.name.into());
    app.set_connection_models_subscription(provider.kind == ProviderKind::Chatgpt);
    let rows = connection_directory_rows(models, discovered);
    let dirty = rows.len() > models.len();
    app.set_connection_models(ModelRc::new(VecModel::from(rows)));
    app.set_connection_models_dirty(dirty);
    if dirty {
        app.set_connection_models_tone(1);
        app.set_connection_models_message("获取的模型已加入目录草稿，请保存模型目录。".into());
    }
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_connection_models(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() || app.get_config_managed() {
            return;
        }
        let Some(provider) = app
            .get_providers()
            .iter()
            .find(|provider| provider.id == id)
        else {
            return;
        };
        // Capture the settings form's valid discovery before closing it clears
        // the shared picker. The directory still requires its explicit save.
        let discovered =
            if app.get_editor_open() && app.get_edit_id() == id && app.get_discovery_scope() == 0 {
                app.get_batch_models()
                    .iter()
                    .filter(|row| row.supported)
                    .map(|row| app::ConnectionModel {
                        public_id: String::new(),
                        display_name: format!("{}/{}", row.id, provider.name),
                        upstream_model: row.id.into(),
                        context_window: row.template_context.into(),
                        reasoning_levels: row.template_levels.into(),
                        default_reasoning: row.template_default.into(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
        app.set_active_page(1);
        close_subscription_editor(&app);
        app.set_editor_open(false);
        app.set_edit_key("".into());
        app.set_xai_editor_open(false);
        app.set_connection_picker_open(false);
        app.set_connection_models_open(false);
        app.invoke_reset_model_discovery();
        app.set_connection_models_id(id.clone());
        app.set_connection_models_name(provider.name);
        app.set_connection_models_subscription(provider.is_subscription && !provider.is_grok);
        app.set_connection_models(ModelRc::new(VecModel::from(Vec::new())));
        app.set_connection_models_dirty(false);
        app.set_connection_models_message("".into());
        app.set_connection_models_tone(0);
        app.set_connection_models_open(true);
        queue(
            &app,
            &callback_sender,
            Command::BeginConnectionModels {
                provider: id.into(),
                discovered,
            },
        );
    });
    let weak = app.as_weak();
    app.on_edit_connection(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() || app.get_config_managed() {
            return;
        }
        let Some(provider) = app
            .get_providers()
            .iter()
            .find(|provider| provider.id == id)
        else {
            return;
        };
        app.set_connection_models_open(false);
        app.set_active_page(1);
        if provider.is_grok {
            app.invoke_begin_xai_editor(id);
        } else if provider.is_subscription {
            app.invoke_begin_subscription_editor(id);
        } else {
            app.invoke_begin_provider_editor(id);
        }
    });
    let weak = app.as_weak();
    app.on_connection_model_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_connection_models_dirty(true);
            app.set_connection_models_message("".into());
        }
    });
    let weak = app.as_weak();
    app.on_add_connection_model(move || {
        if let Some(app) = weak.upgrade() {
            let rows = app.get_connection_models();
            if let Some(rows) = rows.as_any().downcast_ref::<VecModel<ConnectionModelRow>>() {
                rows.push(ConnectionModelRow::default());
                app.set_connection_models_dirty(true);
            }
        }
    });
    let weak = app.as_weak();
    app.on_append_discovered_models(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let rows = app.get_connection_models();
        let Some(rows) = rows.as_any().downcast_ref::<VecModel<ConnectionModelRow>>() else {
            return;
        };
        for candidate in app
            .get_batch_models()
            .iter()
            .filter(|row| row.checked && row.supported)
        {
            if rows
                .iter()
                .any(|row| row.upstream_model == candidate.id && !row.removing)
            {
                continue;
            }
            let levels = if candidate.levels.is_empty() {
                candidate.template_levels
            } else {
                candidate.levels
            };
            let default = if catalog::reasoning_levels(&levels)
                .unwrap_or_default()
                .contains(&candidate.template_default.as_str())
            {
                candidate.template_default
            } else {
                "".into()
            };
            rows.push(ConnectionModelRow {
                display_name: format!("{}/{}", candidate.id, app.get_connection_models_name())
                    .into(),
                upstream_model: candidate.id,
                context_window: if candidate.context.is_empty() {
                    candidate.template_context
                } else {
                    candidate.context
                },
                default_reasoning: default,
                reasoning_levels: levels,
                ..Default::default()
            });
            app.set_connection_models_dirty(true);
        }
        update_batch_rows(&app, |_, row| {
            if rows
                .iter()
                .any(|model| model.upstream_model == row.id && !model.removing)
            {
                row.added = true;
                row.checked = false;
                true
            } else {
                false
            }
        });
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_connection_models(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let models = app
            .get_connection_models()
            .iter()
            .filter(|row| !row.removing)
            .map(|row| app::ConnectionModel {
                public_id: row.public_id.into(),
                display_name: row.display_name.into(),
                upstream_model: row.upstream_model.into(),
                context_window: row.context_window.into(),
                reasoning_levels: row.reasoning_levels.into(),
                default_reasoning: row.default_reasoning.into(),
            })
            .collect();
        queue(
            &app,
            &callback_sender,
            Command::SaveConnectionModels {
                provider: app.get_connection_models_id().into(),
                models,
            },
        );
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_select_connection(move |id, enabled| {
        if let Some(app) = weak.upgrade() {
            let ids = app
                .get_models()
                .iter()
                .filter(|model| {
                    model.provider_id == id && (if enabled { model.ready } else { model.included })
                })
                .map(|model| model.public_id.into())
                .collect();
            queue(&app, &callback_sender, Command::SelectModels(ids, enabled));
        }
    });
}

pub(super) fn connect_discovery(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_fetch_models(move |scope| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() || app.get_config_managed() {
            return;
        }
        let (provider, url, key) = if scope == 0 {
            (
                app.get_edit_id().to_string(),
                app.get_edit_url().to_string(),
                Secret::new(app.get_edit_key().into()),
            )
        } else {
            let Some(provider) = app.get_providers().iter().find(|provider| {
                provider.id
                    == if scope == 2 {
                        app.get_connection_models_id()
                    } else {
                        app.get_model_provider_id()
                    }
            }) else {
                show_action(&app, Err("请先保存上游连接".into()));
                return;
            };
            (
                provider.id.to_string(),
                provider.base_url.to_string(),
                Secret::new(String::new()),
            )
        };
        app.invoke_reset_model_discovery();
        app.set_discovery_scope(scope);
        app.set_discovery_tone(0);
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
}

impl Worker {
    pub(super) fn begin_connection_models(
        &mut self,
        id: String,
        discovered: Vec<app::ConnectionModel>,
    ) {
        self.subscription_auth = None;
        let result = self.data_dir.path().and_then(|data_dir| {
            Ok((
                app::load_provider(data_dir, &id)?,
                provider_models(data_dir, &id)?,
            ))
        });
        self.connection_models_snapshot = result
            .as_ref()
            .ok()
            .map(|(_, models)| (id.clone(), models.clone()));
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_busy(false);
            if !app.get_connection_models_open() || app.get_connection_models_id() != id {
                return;
            }
            match result {
                Ok((provider, models)) => {
                    show_connection_directory(&app, provider, &models, discovered);
                }
                Err(error) => {
                    app.set_connection_models_tone(3);
                    app.set_connection_models_message(error.into());
                }
            }
        });
    }

    pub(super) async fn save_connection_models(
        &mut self,
        provider: String,
        models: Vec<app::ConnectionModel>,
    ) {
        let result = async {
            let data_dir = self.data_dir.path()?;
            let expected = self
                .connection_models_snapshot
                .as_ref()
                .filter(|(id, _)| id == &provider)
                .map(|(_, models)| models)
                .ok_or("请重新打开模型目录")?;
            let templates =
                if app::load_provider(data_dir, &provider)?.kind == ProviderKind::Chatgpt {
                    chatgpt::catalog().await?
                } else {
                    Vec::new()
                };
            app::save_connection_models(data_dir, &provider, expected, &models, &templates)
        }
        .await;
        if result.is_ok() {
            self.discard_previews();
            self.connection_models_snapshot = None;
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_connection_models_saving(false);
            app.set_busy(false);
            match result {
                Ok(()) => {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                    }
                    app.set_connection_models_dirty(false);
                    app.set_route_preview_ready(false);
                    app.set_direct_preview_ready(false);
                    app.invoke_begin_connection_models(provider.into());
                    app.set_connection_models_tone(1);
                    app.set_connection_models_message(
                        "模型目录已保存。可在工作台一次加入整个连接。".into(),
                    );
                }
                Err(error) => {
                    app.set_connection_models_tone(3);
                    app.set_connection_models_message(error.into());
                }
            }
        });
    }

    pub(super) async fn fetch_models(
        &self,
        scope: i32,
        generation: i32,
        provider: String,
        url: String,
        key: Secret,
    ) {
        let result = async {
            let data_dir = self.data_dir.path()?;
            if !provider.is_empty() {
                let selected = app::load_provider(data_dir, &provider)?;
                if selected.kind == ProviderKind::XaiOAuth {
                    xai::validate_provider(&selected)?;
                    let manager = xai::AccountManager::open(data_dir)?;
                    let account =
                        manager.resolve_binding(selected.account_binding.as_ref().unwrap())?;
                    let token = manager.credential(&account.id).await?;
                    return upstream::fetch_models(xai::BASE_URL, token.expose())
                        .await
                        .map(|models| (models, false, Vec::new()));
                }
            }
            let is_subscription = !provider.is_empty()
                && app::load_provider(data_dir, &provider)?.kind == ProviderKind::Chatgpt;
            if is_subscription {
                // The official templates also seed the batch picker's parameters.
                let templates = chatgpt::catalog().await?;
                return Ok((
                    templates
                        .iter()
                        .filter_map(|model| model["slug"].as_str().map(str::to_owned))
                        .collect::<Vec<_>>(),
                    true,
                    templates,
                ));
            }
            let token = if key.expose().is_empty() {
                if provider.is_empty() {
                    return Err("请先填写 API 地址和 API Key".into());
                }
                app::provider_credential(data_dir, &app::load_provider(data_dir, &provider)?)?
            } else {
                key
            };
            upstream::fetch_models(&url, token.expose())
                .await
                .map(|models| (models, false, Vec::new()))
        }
        .await
        .and_then(|(models, is_subscription, templates)| {
            let candidates = if scope == 0 {
                app::draft_candidates(&url, &models)
            } else if !provider.is_empty() {
                app::batch_candidates(self.data_dir.path()?, &provider, &models, &templates)?
            } else {
                Vec::new()
            };
            Ok((models, is_subscription, candidates))
        });
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_busy(false);
            if generation != app.get_discovery_generation()
                || scope != app.get_discovery_scope()
                || (scope == 0 && !app.get_editor_open())
                || (scope == 1 && !app.get_model_editor_open())
                || (scope == 2 && !app.get_connection_models_open())
            {
                return;
            }
            app.set_fetching_models(false);
            match result {
                Ok((models, is_subscription, candidates)) => {
                    let added = candidates.iter().filter(|model| model.added).count();
                    app.set_discovery_tone(if models.is_empty() { 2 } else { 1 });
                    app.set_discovery_message(if models.is_empty() {
                        "上游返回空列表，可手动填写模型 ID".into()
                    } else {
                        let suffix = if added > 0 {
                            format!("，其中 {added} 个已添加")
                        } else {
                            String::new()
                        };
                        if is_subscription {
                            format!(
                                "已读取 CLI 内置的 {} 个官方模型{suffix}；账号权限以实际请求为准",
                                models.len()
                            )
                            .into()
                        } else {
                            let directory_hint = if scope == 0 && !provider.is_empty() {
                                "；切换到模型目录可查看并保存"
                            } else {
                                ""
                            };
                            format!("已获取 {} 个模型{suffix}{directory_hint}", models.len()).into()
                        }
                    });
                    show_batch_candidates(&app, candidates);
                    lock_batch_rows(&app);
                    app.set_fetched_models(ModelRc::new(VecModel::from(
                        models
                            .into_iter()
                            .map(slint::SharedString::from)
                            .collect::<Vec<_>>(),
                    )));
                }
                Err(error) => {
                    app.set_discovery_tone(3);
                    app.set_discovery_message(error.into());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchx::{
        codex::provider_config::CodexOptions,
        headless,
        id::new_id,
        storage::Store,
        ui::{ModelRow, ProviderRow, Theme},
    };

    use crate::desktop::provider_editor::show_provider_editor;

    #[test]
    fn settings_model_discovery_populates_directory_drafts_and_preserves_saved_models() {
        let window = headless::install();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        app.set_active_page(1);
        slint::platform::update_timers_and_animations();
        let (sender, mut receiver) = mpsc::channel(8);
        connect(&app, &sender);
        connect_discovery(&app, &sender);
        let provider = ProviderRecord {
            id: "api".into(),
            name: "Podlink".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "existing-model".into(),
            credential_ref: None,
            kind: ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
        };
        app.set_providers(ModelRc::new(VecModel::from(vec![
            ProviderRow {
                id: provider.id.clone().into(),
                name: provider.name.clone().into(),
                ..Default::default()
            },
            ProviderRow {
                id: "other".into(),
                name: "Other connection".into(),
                ..Default::default()
            },
        ])));
        let mut metadata = catalog::mapping_metadata(
            "existing-model",
            "My custom name",
            &catalog::MappingSettings {
                context_window: "1050000",
                reasoning_levels: Some("low, medium, high"),
                default_reasoning: Some("high"),
            },
            None,
        )
        .unwrap();
        metadata["custom_field"] = serde_json::json!("preserved");
        let saved = vec![ModelRecord {
            public_id: "sx-existing".into(),
            provider_id: provider.id.clone(),
            display_name: "My custom name".into(),
            upstream_model: "existing-model".into(),
            metadata: metadata.to_string(),
            enabled: true,
            fallback_provider_id: None,
        }];
        let data =
            std::env::temp_dir().join(format!("switchx-discovery-directory-{}", new_id().unwrap()));
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        store.put_provider(&provider).unwrap();
        store.add_models(&saved).unwrap();
        let discovered = ["existing-model", "new-model", "new-model"].map(String::from);
        let show_discovery = || {
            app.set_discovery_scope(0);
            show_batch_candidates(&app, app::draft_candidates(&provider.base_url, &discovered));
        };
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                app.set_connection_models_open(false);
                show_provider_editor(
                    &app,
                    Some(provider.clone()),
                    CodexOptions::default(),
                    String::new(),
                );
                slint::platform::update_timers_and_animations();
                app.set_edit_key("synthetic-private-key".into());
                slint::platform::update_timers_and_animations();
                show_discovery();
                app.invoke_begin_connection_models(provider.id.clone().into());
                let Command::BeginConnectionModels {
                    provider: id,
                    discovered,
                } = receiver.try_recv().unwrap()
                else {
                    panic!("expected settings discovery to travel with directory opening");
                };
                assert_eq!(id, provider.id);
                assert_eq!(discovered.len(), 2);
                show_connection_directory(&app, provider.clone(), &saved, discovered);
                app.set_busy(false);
                slint::platform::update_timers_and_animations();
                assert!(app.get_connection_models_open());
                assert!(app.get_edit_key().is_empty());
                assert_eq!(app.get_batch_models().row_count(), 0);
                assert_eq!(app.get_connection_models().row_count(), 2);
                assert!(app.get_connection_models_dirty());
                assert_eq!(
                    app.get_connection_models().row_data(0).unwrap(),
                    connection_model_row(&saved[0])
                );
                let new = app.get_connection_models().row_data(1).unwrap();
                assert_eq!(new.upstream_model, "new-model");
                assert_eq!(new.display_name, "new-model/Podlink");
                assert!(new.context_window.is_empty() && new.reasoning_levels.is_empty());
                assert_eq!(
                    store.models().unwrap(),
                    saved,
                    "fetching must leave the stored directory unchanged"
                );
                if std::env::var_os("SWITCHX_CONNECTION_SNAPSHOTS").is_some() {
                    let theme = if dark { "dark" } else { "light" };
                    headless::save_snapshot(
                        "SWITCHX_CONNECTION_SNAPSHOTS",
                        &format!("discovery-directory-{theme}-{width}x{height}"),
                        &headless::render(&window),
                    );
                }
            }
        }
        app.invoke_save_connection_models();
        let Command::SaveConnectionModels {
            provider: id,
            models,
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected an explicit directory save");
        };
        app::save_connection_models(&data, &id, &saved, &models, &[]).unwrap();
        let updated = store.models().unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(
            updated
                .iter()
                .find(|model| model.public_id == "sx-existing")
                .unwrap(),
            &saved[0]
        );
        assert!(
            !updated
                .iter()
                .find(|model| model.upstream_model == "new-model")
                .unwrap()
                .enabled
        );
        assert_eq!(
            connection_directory_rows(&updated, models).len(),
            2,
            "repeated discovery must not duplicate mappings"
        );
        app.set_busy(false);
        app.set_connection_models_open(false);
        show_provider_editor(
            &app,
            Some(provider.clone()),
            CodexOptions::default(),
            String::new(),
        );
        slint::platform::update_timers_and_animations();
        show_discovery();
        app.invoke_begin_connection_models("other".into());
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::BeginConnectionModels { provider, discovered } if provider == "other" && discovered.is_empty())
        );
        app.set_busy(false);
        app.set_connection_models_open(false);
        show_provider_editor(
            &app,
            Some(provider.clone()),
            CodexOptions::default(),
            String::new(),
        );
        slint::platform::update_timers_and_animations();
        show_discovery();
        app.invoke_fetch_models(0);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Command::FetchModels { scope: 0, .. }
        ));
        assert_eq!(
            app.get_batch_models().row_count(),
            0,
            "a retry must discard the previous result before a possible failure"
        );
        app.set_busy(false);
        app.set_fetching_models(false);
        app.set_discovery_tone(3);
        app.invoke_begin_connection_models("api".into());
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::BeginConnectionModels { discovered, .. } if discovered.is_empty())
        );
        drop(store);
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn connection_directory_opens_from_workbench_clears_credentials_and_queues_one_group_selection()
    {
        headless::install();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        let (sender, mut receiver) = mpsc::channel(8);
        connect(&app, &sender);
        app.set_providers(ModelRc::new(VecModel::from(vec![ProviderRow {
            id: "api".into(),
            name: "Synthetic API".into(),
            ..Default::default()
        }])));
        app.set_active_page(0);
        app.set_subscription_auth_json("synthetic-private-editor-content".into());
        app.set_edit_key("synthetic-private-api-key".into());
        app.invoke_begin_connection_models("api".into());
        slint::platform::update_timers_and_animations();
        assert_eq!(app.get_active_page(), 1);
        assert!(
            app.get_connection_models_open(),
            "navigation must not close the directory it just opened"
        );
        assert!(app.get_subscription_auth_json().is_empty());
        assert!(app.get_edit_key().is_empty());
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::BeginConnectionModels { provider, discovered } if provider == "api" && discovered.is_empty())
        );
        assert!(!app.get_connection_models_saving());
        assert!(receiver.try_recv().is_err());

        queue(
            &app,
            &sender,
            Command::FetchModels {
                scope: 2,
                generation: 0,
                provider: "api".into(),
                url: "https://example.invalid/v1".into(),
                key: Secret::new(String::new()),
                home: "/isolated/codex".into(),
            },
        );
        assert!(!app.get_connection_models_saving());
        assert!(matches!(
            receiver.try_recv().unwrap(),
            Command::FetchModels { .. }
        ));
        app.set_busy(false);
        app.set_models(ModelRc::new(VecModel::from(vec![
            ModelRow {
                provider_id: "api".into(),
                public_id: "sx-pro".into(),
                ready: true,
                ..Default::default()
            },
            ModelRow {
                provider_id: "api".into(),
                public_id: "sx-mini".into(),
                ready: true,
                ..Default::default()
            },
            ModelRow {
                provider_id: "api".into(),
                public_id: "sx-unready".into(),
                ready: false,
                ..Default::default()
            },
            ModelRow {
                provider_id: "other".into(),
                public_id: "sx-other".into(),
                ready: true,
                ..Default::default()
            },
        ])));
        app.invoke_select_connection("api".into(), true);
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::SelectModels(ids, true) if ids == ["sx-pro", "sx-mini"])
        );
        assert!(receiver.try_recv().is_err());
        app.set_busy(false);
        app.set_connection_models(ModelRc::new(VecModel::from(vec![
            ConnectionModelRow {
                upstream_model: "coder-pro".into(),
                display_name: "Coder".into(),
                reasoning_levels: "low, high".into(),
                default_reasoning: "high".into(),
                ..Default::default()
            },
            ConnectionModelRow {
                upstream_model: "removed-model".into(),
                removing: true,
                ..Default::default()
            },
        ])));
        app.invoke_save_connection_models();
        assert!(app.get_connection_models_saving());
        let Command::SaveConnectionModels { provider, models } = receiver.try_recv().unwrap()
        else {
            panic!("expected one atomic directory save");
        };
        assert_eq!(provider, "api");
        assert_eq!(models.len(), 1);
        drop(receiver);
        app.set_busy(false);
        app.invoke_save_connection_models();
        assert!(!app.get_busy());
        assert!(!app.get_connection_models_saving());
        assert_eq!(models[0].upstream_model, "coder-pro");
        assert_eq!(models[0].default_reasoning, "high");
    }
}
