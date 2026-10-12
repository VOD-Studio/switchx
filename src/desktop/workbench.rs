//! The connection workbench: saved connections, their models, and search.

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app::{AppError, Snapshot, load_snapshot},
    storage::ProviderKind,
    ui::{AppWindow, ModelRow, ProviderRow},
};
use tokio::sync::mpsc;

use super::{
    command::{Command, queue},
    grok::{show_xai_accounts, xai_account_view},
    icon_picker::{provider_icon_row, resolved_provider_icon_id},
    rows::sync_rows,
    worker::Worker,
};

pub(super) fn matching_providers(
    providers: &ModelRc<ProviderRow>,
    query: &str,
) -> Vec<ProviderRow> {
    let query = query.trim().to_lowercase();
    providers
        .iter()
        .filter(|provider| provider.name.to_lowercase().contains(&query))
        .collect()
}

pub(super) fn filter_providers(app: &AppWindow, query: &str) {
    let rows = matching_providers(&app.get_providers(), query);
    sync_rows(
        app.get_filtered_providers(),
        rows,
        |row| row.id.clone(),
        |model| app.set_filtered_providers(model),
    );
}

pub(super) fn show_result(app: &AppWindow, result: Result<Snapshot, AppError>) {
    app.set_loading(false);
    app.set_busy(false);
    match result {
        Ok(snapshot) => {
            app.set_local_data_ready(true);
            app.set_credentials_checked(snapshot.credentials_checked);
            let checking = app.get_providers();
            let selected_count = snapshot.models.iter().filter(|model| model.enabled).count();
            let selectable_count = snapshot.models.iter().filter(|model| model.ready).count();
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
            app.set_selected_connection_count(
                snapshot
                    .models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(|model| &model.provider_id)
                    .collect::<std::collections::HashSet<_>>()
                    .len() as i32,
            );
            app.set_selectable_model_count(selectable_count as i32);
            app.set_models(ModelRc::new(VecModel::from(
                snapshot
                    .models
                    .into_iter()
                    .map(|model| ModelRow {
                        is_subscription: snapshot.providers.iter().any(|provider| {
                            provider.id == model.provider_id
                                && provider.kind != ProviderKind::ApiKey
                        }),
                        binding_label: snapshot
                            .providers
                            .iter()
                            .find(|provider| provider.id == model.provider_id)
                            .map(|provider| provider.binding_label.clone())
                            .unwrap_or_default()
                            .into(),
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
                        fresh: false,
                    })
                    .collect::<Vec<_>>(),
            )));
            app.invoke_update_default_model_label();
            let model_rows = app.get_models();
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
            let rows = snapshot
                .providers
                .into_iter()
                .map(|provider| {
                    let previous = checking
                        .iter()
                        .find(|row| {
                            row.id == provider.id
                                && row.name == provider.name
                                && row.base_url == provider.base_url
                                && row.model_id == provider.model_id
                                && row.binding_label == provider.binding_label
                        })
                        .unwrap_or_default();
                    let brand = provider_icon_row(
                        app,
                        resolved_provider_icon_id(
                            provider.kind,
                            &provider.base_url,
                            &provider.icon_id,
                        ),
                    );
                    let models: Vec<_> = model_rows
                        .iter()
                        .filter(|model| model.provider_id == provider.id)
                        .collect();
                    let mut directory = previous.models.clone();
                    sync_rows(
                        directory.clone(),
                        models.clone(),
                        |model| model.public_id.clone(),
                        |rows| directory = rows,
                    );
                    ProviderRow {
                        model_count: models.iter().filter(|model| model.saved).count() as i32,
                        ready_model_count: models.iter().filter(|model| model.ready).count() as i32,
                        selected_model_count: models.iter().filter(|model| model.included).count()
                            as i32,
                        models: directory,
                        id: provider.id.into(),
                        name: provider.name.into(),
                        endpoint: provider.endpoint.into(),
                        base_url: provider.base_url.into(),
                        model_id: provider.model_id.into(),
                        credential_status: provider.credential_status.into(),
                        is_subscription: provider.kind != ProviderKind::ApiKey,
                        is_grok: provider.kind == ProviderKind::XaiOAuth,
                        binding_label: provider.binding_label.into(),
                        auth_error: "".into(),
                        preset_id: provider.preset_id.into(),
                        icon_id: brand.id,
                        icon: brand.icon,
                        monochrome: brand.monochrome,
                        ..previous
                    }
                })
                .collect::<Vec<_>>();
            sync_rows(
                app.get_providers(),
                rows,
                |row| row.id.clone(),
                |rows| app.set_providers(rows),
            );
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
            app.set_local_data_ready(false);
            app.set_credentials_checked(false);
            app.set_error_code(error.code().into());
            app.set_error_message(error.message().into());
            app.set_error_action(error.action().into());
            app.set_status_text("上次成功读取的记录仍保留在窗口中".into());
        }
    }
}

pub(super) fn created_connection_id(
    app: &AppWindow,
    snapshot: &Result<Snapshot, AppError>,
) -> Option<slint::SharedString> {
    snapshot
        .as_ref()
        .ok()?
        .providers
        .iter()
        .find(|provider| !app.get_providers().iter().any(|row| row.id == provider.id))
        .map(|provider| provider.id.clone().into())
}

pub(super) fn mark_fresh_models(app: &AppWindow, public_ids: &[String]) {
    let models = app.get_models();
    for index in 0..models.row_count() {
        if let Some(mut model) = models.row_data(index)
            && public_ids.iter().any(|id| model.public_id == id.as_str())
        {
            model.fresh = true;
            models.set_row_data(index, model);
        }
    }
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
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
    let weak = app.as_weak();
    app.on_update_default_model_label(move || {
        if let Some(app) = weak.upgrade() {
            let label = app
                .get_models()
                .iter()
                .find(|model| model.public_id == app.get_default_model())
                .map(|model| model.display_name)
                .unwrap_or_else(|| "尚未选择".into());
            app.set_default_model_label(label);
        }
    });
}

impl Worker {
    pub(super) fn refresh(&self, check_credentials: bool) {
        let result = self
            .data_dir
            .get()
            .and_then(|path| load_snapshot(path, check_credentials));
        let accounts = self.data_dir.ok().map(xai_account_view);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            show_result(&app, result);
            if let Some(view) = accounts {
                show_xai_accounts(&app, view);
            }
        });
    }
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
}
