slint::include_modules!();

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::app::{AppError, Snapshot, data_directory, load_snapshot};
use tokio::sync::mpsc::error::TrySendError;

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
    match result {
        Ok(snapshot) => {
            let count = snapshot.providers.len();
            let rows = snapshot
                .providers
                .into_iter()
                .map(|provider| ProviderRow {
                    name: provider.name.into(),
                    endpoint: provider.endpoint.into(),
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
            let retained = app.get_providers().row_count() != 0;
            app.set_error_code(error.code().into());
            app.set_error_message(error.message().into());
            app.set_error_action(
                format!(
                    "{}{}",
                    if retained {
                        "仍显示上次成功读取的资料。"
                    } else {
                        ""
                    },
                    error.action()
                )
                .into(),
            );
            app.set_status_text(
                if !retained {
                    "本地资料未读取"
                } else {
                    "读取失败，仍显示上次成功读取的资料"
                }
                .into(),
            );
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    tray.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });
    let directory = data_directory();
    app.set_data_path(
        directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "不可用".into())
            .into(),
    );
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<bool>(4);
    let weak = app.as_weak();
    runtime.handle().spawn_blocking(move || {
        while let Some(check_credentials) = receiver.blocking_recv() {
            let result = directory
                .as_ref()
                .map_err(|error| *error)
                .and_then(|path| load_snapshot(path, check_credentials));
            let _ = weak.upgrade_in_event_loop(move |app| show_result(&app, result));
        }
    });
    let weak = app.as_weak();
    let refresh_sender = sender.clone();
    app.on_refresh(move |check_credentials| {
        if let Some(app) = weak.upgrade() {
            app.set_loading(true);
            app.set_status_text("正在读取本地资料…".into());
            match refresh_sender.try_send(check_credentials) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => show_result(&app, Err(AppError::Busy)),
                Err(TrySendError::Closed(_)) => {
                    show_result(&app, Err(AppError::WorkerStopped));
                }
            }
        }
    });
    let weak = app.as_weak();
    app.on_filter_providers(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_providers(&app, &query);
        }
    });
    sender.try_send(false)?;

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
                name: "DeepSeek 官方".into(),
                endpoint: "https://example.invalid".into(),
                credential_status: "凭据未检查".into(),
            },
            ProviderRow {
                name: "备用上游".into(),
                endpoint: "https://backup.invalid".into(),
                credential_status: "凭据未检查".into(),
            },
        ]));
        assert_eq!(matching_providers(&providers, "deep").len(), 1);
        assert_eq!(matching_providers(&providers, " 上游 ").len(), 1);
        assert_eq!(matching_providers(&providers, "  ").len(), 2);
        assert!(matching_providers(&providers, "missing").is_empty());
    }
}
