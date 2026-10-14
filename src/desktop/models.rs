//! Model mappings: the single mapping editor, publication selection, and fallbacks.

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{app, catalog, chatgpt, id::new_id, storage::ProviderKind, ui::AppWindow};
use tokio::sync::mpsc;

use super::{
    batch_picker::update_batch_reasoning_options,
    command::{BatchForm, Command, ModelForm, queue, queue_on},
    feedback::show_action,
    workbench::{mark_fresh_models, show_result},
    worker::Worker,
};

pub(super) fn update_model_display_name(app: &AppWindow) {
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

pub(super) fn open_model_editor(
    app: &AppWindow,
    provider_id: &str,
    original_id: &str,
) -> Result<(), String> {
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
    app.set_model_user_agent("".into());
    app.set_model_header_overrides("".into());
    app.set_model_body_overrides("".into());
    app.set_model_request_open(false);
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
        None => format!("sx-{}", new_id()?).into(),
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
    app.set_model_batch_mode(original_id.is_empty());
    app.set_batch_saving(false);
    if original_id.is_empty() {
        app.set_batch_query("".into());
        app.set_batch_context("".into());
        app.set_batch_levels("".into());
        app.set_batch_default("未设置".into());
        update_batch_reasoning_options(app);
    }
    app.set_model_editor_open(true);
    app.set_fallback_editor_open(false);
    app.set_editor_open(false);
    app.set_edit_key("".into());
    app.set_active_page(0);
    update_reasoning_options(app);
    Ok(())
}

pub(super) fn update_reasoning_options(app: &AppWindow) {
    let text = app.get_model_reasoning_levels();
    let levels = catalog::reasoning_levels(&text).unwrap_or_default();
    let mut options = vec![slint::SharedString::from("未设置")];
    options.extend(levels.into_iter().map(slint::SharedString::from));
    if !options.contains(&app.get_model_default_reasoning()) {
        app.set_model_default_reasoning("未设置".into());
    }
    app.set_model_reasoning_options(ModelRc::new(VecModel::from(options)));
}

pub(super) fn request_draft(app: &AppWindow) -> (String, String, String) {
    (
        app.get_model_user_agent().into(),
        app.get_model_header_overrides().into(),
        app.get_model_body_overrides().into(),
    )
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    app.on_save_model(queue_on(app, sender, |app| {
        Command::SaveModel(ModelForm {
            request_overrides: request_draft(app),
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
        })
    }));
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_model_editor(move |provider, original_id| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::BeginModelEditor {
                    provider: provider.into(),
                    original_id: original_id.into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_add_selected_model(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(provider) = app
            .get_model_provider_ids()
            .row_data(app.get_model_provider_choice() as usize)
        {
            queue(
                &app,
                &callback_sender,
                Command::BeginModelEditor {
                    provider: provider.into(),
                    original_id: String::new(),
                },
            );
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
    app.on_delete_model(queue_on(app, sender, |app| {
        Command::DeleteModel(app.get_model_original_id().into())
    }));
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_select_model(move |provider, enabled| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SelectModels(vec![provider.into()], enabled),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_select_models(move |enabled, selected_only| {
        if let Some(app) = weak.upgrade() {
            let public_ids = app
                .get_models()
                .iter()
                .filter(|model| model.ready && (!selected_only || model.included))
                .map(|model| model.public_id.into())
                .collect();
            queue(
                &app,
                &callback_sender,
                Command::SelectModels(public_ids, enabled),
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
        let providers = app.get_providers();
        if providers
            .iter()
            .any(|provider| provider.id == primary.provider_id && provider.is_subscription)
        {
            show_action(
                &app,
                Err("订阅账号不参与自动备用；请恢复后明确切换账号或 API 上游".into()),
            );
            return;
        }
        let mut ids = vec![slint::SharedString::default()];
        let mut labels = vec![slint::SharedString::from("不使用备用上游")];
        let mut selected = 0;
        for model in models.iter().filter(|model| {
            model.ready
                && providers
                    .iter()
                    .any(|provider| provider.id == model.provider_id && !provider.is_subscription)
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
}

impl Worker {
    pub(super) fn begin_model_editor(&self, provider: String, original_id: String) {
        let result = (|| {
            if original_id.is_empty() {
                return Ok(Default::default());
            }
            app::load_request_overrides(self.data_dir.path()?, &provider, &original_id)
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            let result =
                result.and_then(|overrides: switchx::request_overrides::RequestOverrides| {
                    open_model_editor(&app, &provider, &original_id)?;
                    app.set_model_user_agent(overrides.user_agent().into());
                    app.set_model_header_overrides(overrides.headers_json().into());
                    app.set_model_body_overrides(overrides.body_json().into());
                    app.set_model_request_open(!overrides.is_empty());
                    Ok(())
                });
            app.set_busy(false);
            if let Err(error) = result {
                show_action(&app, Err(error));
            }
        });
    }

    pub(super) async fn save_model(&mut self, form: ModelForm) {
        let ModelForm {
            provider,
            original_id,
            public_id,
            name,
            upstream_model,
            path,
            context,
            levels,
            default_reasoning,
            request_overrides,
        } = form;
        let result = async {
            let data_dir = self.data_dir.path()?;
            let overrides = switchx::request_overrides::RequestOverrides::parse(
                &request_overrides.0,
                &request_overrides.1,
                &request_overrides.2,
            )?;
            let keep_imported_reasoning = !path.trim().is_empty() && levels.trim().is_empty();
            let input = app::ModelInput {
                request_overrides: Some(&overrides),
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
            if app::load_provider(data_dir, &provider)?.kind == ProviderKind::Chatgpt {
                chatgpt::save_mapping(data_dir, input).await
            } else {
                app::save_mapping(data_dir, input)
            }
        }
        .await;
        if result.is_ok() {
            self.route_session.discard_preview();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(snapshot) = snapshot {
                show_result(&app, snapshot);
                app.set_model_editor_open(false);
                app.set_route_preview_ready(false);
            }
            show_action(
                &app,
                result.map(|()| "模型映射已保存；预览发布后可开启路由并刷新 Codex 模型菜单".into()),
            );
        });
    }

    pub(super) async fn save_models(&mut self, form: BatchForm) {
        let BatchForm {
            provider,
            models,
            context,
            levels,
            default_reasoning,
            request_overrides,
        } = form;
        let result = async {
            let data_dir = self.data_dir.path()?;
            let overrides = switchx::request_overrides::RequestOverrides::parse(
                &request_overrides.0,
                &request_overrides.1,
                &request_overrides.2,
            )?;
            let models = models
                .iter()
                .map(
                    |(upstream_model, context_window, reasoning_levels)| app::BatchModel {
                        upstream_model,
                        context_window,
                        reasoning_levels,
                    },
                )
                .collect::<Vec<_>>();
            let input = app::BatchInput {
                request_overrides: Some(&overrides),
                provider_id: &provider,
                models: &models,
                context_window: &context,
                reasoning_levels: &levels,
                default_reasoning: &default_reasoning,
            };
            if app::load_provider(data_dir, &provider)?.kind == ProviderKind::Chatgpt {
                chatgpt::save_mappings(data_dir, &input).await
            } else {
                app::save_mappings(data_dir, &input)
            }
        }
        .await;
        if result.is_ok() {
            self.route_session.discard_preview();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_batch_saving(false);
            if let Some(snapshot) = snapshot {
                show_result(&app, snapshot);
                app.set_model_editor_open(false);
                app.set_route_preview_ready(false);
            }
            if let Ok(public_ids) = &result {
                mark_fresh_models(&app, public_ids);
            }
            show_action(
                &app,
                result.map(|public_ids| {
                    format!(
                        "已添加 {} 个模型映射；预览发布后可开启路由并刷新 Codex 模型菜单",
                        public_ids.len()
                    )
                }),
            );
        });
    }

    pub(super) fn delete_model(&mut self, public_id: String) {
        let result = self
            .data_dir
            .path()
            .and_then(|data_dir| app::delete_model(data_dir, &public_id));
        if result.is_ok() {
            self.route_session.discard_preview();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(snapshot) = snapshot {
                show_result(&app, snapshot);
                app.set_model_editor_open(false);
                app.set_route_preview_ready(false);
            }
            show_action(&app, result.map(|()| "模型映射已删除".into()));
        });
    }

    pub(super) fn select_models(&mut self, public_ids: Vec<String>, enabled: bool) {
        let result = self
            .data_dir
            .path()
            .and_then(|data_dir| app::select_models(data_dir, &public_ids, enabled));
        if result.is_ok() {
            self.route_session.discard_preview();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
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

    pub(super) fn save_fallback(&mut self, public_id: String, fallback: Option<String>) {
        let result = self
            .data_dir
            .path()
            .and_then(|data_dir| app::save_fallback(data_dir, &public_id, fallback.as_deref()));
        if result.is_ok() {
            self.route_session.discard_preview();
        }
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
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
}
