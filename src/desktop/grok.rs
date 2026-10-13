//! Saved Grok accounts and the Grok connection editor.

use std::path::Path;

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app,
    id::new_id,
    storage::{AccountBinding, ProviderKind, ProviderRecord, Store},
    ui::{AccountRow, AppWindow, XaiQuotaRow},
    xai,
};
use tokio::sync::{mpsc, watch};

use super::{
    accounts::account_initial,
    codex_config::clear_previews,
    command::{Command, XaiAccountAction, queue, queue_on},
    connection_models::directory_draft,
    feedback::show_action,
    open_url,
    provider_checks::set_provider_check_id,
    quota::{refresh_xai_quota, update_xai_quota_times},
    rows::sync_account_rows,
    workbench::{created_connection_id, show_result},
    worker::{PendingLogin, Worker},
};

pub(super) fn xai_account_view(data_dir: &Path) -> Result<Vec<AccountRow>, String> {
    let accounts = xai::AccountManager::open(data_dir)?.list()?;
    let default_id = accounts
        .iter()
        .find(|a| a.is_default)
        .map(|a| a.id.as_str());
    let providers = Store::open_read_only(&data_dir.join("switchx.sqlite"))
        .map_err(|_| "无法读取 Grok 上游引用")?
        .providers()
        .map_err(|_| "无法读取 Grok 上游引用")?;
    Ok(accounts
        .iter()
        .map(|account| {
            let names: Vec<_> = providers
                .iter()
                .filter(|p| {
                    p.kind == ProviderKind::XaiOAuth
                        && match &p.account_binding {
                            Some(AccountBinding::Fixed(id)) => id == &account.id,
                            Some(AccountBinding::Default) => {
                                default_id == Some(account.id.as_str())
                            }
                            _ => false,
                        }
                })
                .map(|p| p.name.clone())
                .collect();
            AccountRow {
                id: account.id.clone().into(),
                label: account.label.clone().into(),
                initial: account_initial(&account.label).into(),
                workspace: if account.requires_reauth {
                    "凭据失效，请重新登录"
                } else {
                    "已保存授权"
                }
                .into(),
                is_default: account.is_default,
                is_active: false,
                requires_reauth: account.requires_reauth,
                bound_provider_count: names.len() as i32,
                bound_provider_names: names.join("、").into(),
                ..Default::default()
            }
        })
        .collect())
}

pub(super) fn show_xai_accounts(app: &AppWindow, view: Result<Vec<AccountRow>, String>) {
    match view {
        Ok(mut rows) => {
            let previous = app.get_xai_accounts();
            for row in &mut rows {
                if let Some(old) = previous.iter().find(|old| old.id == row.id) {
                    row.quota = old.quota;
                }
            }
            app.set_xai_status(format!("已保存 {} 个 Grok 账号", rows.len()).into());
            app.set_xai_account_error("".into());
            sync_account_rows(app.get_xai_accounts(), rows, |model| {
                app.set_xai_accounts(model)
            });
            if app.get_active_page() == 1 && app.get_connections_tab() == 1 {
                let unread = app
                    .get_xai_accounts()
                    .iter()
                    .filter(|row| row.quota.request_id.is_empty())
                    .map(|row| row.id)
                    .collect::<Vec<_>>();
                for id in unread {
                    app.invoke_xai_account_action(XaiAccountAction::REFRESH_QUOTA, id);
                }
            }
        }
        Err(error) => {
            app.set_xai_status(error.clone().into());
            app.set_xai_account_error(error.into());
        }
    }
}

pub(super) fn show_xai_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    accounts: Vec<xai::AccountInfo>,
) {
    app.invoke_reset_connection_draft();
    let mut ids = vec![slint::SharedString::from("@default")];
    let mut options = vec![slint::SharedString::from(
        "跟随默认 Grok 账号（发布时固定）",
    )];
    let mut choice = 0;
    for account in accounts.into_iter().filter(|a| !a.requires_reauth) {
        if provider.as_ref().and_then(|p| p.account_binding.as_ref())
            == Some(&AccountBinding::Fixed(account.id.clone()))
        {
            choice = ids.len() as i32;
        }
        ids.push(account.id.into());
        options.push(account.label.into());
    }
    // A deleted/expired fixed account must be explicitly rebound, never silently defaulted.
    if let Some(AccountBinding::Fixed(id)) =
        provider.as_ref().and_then(|p| p.account_binding.as_ref())
        && choice == 0
    {
        choice = ids.len() as i32;
        ids.push(id.clone().into());
        options.push("原账号不可用，请重新选择".into());
    }
    app.set_xai_provider_id(
        provider
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or_default()
            .into(),
    );
    app.set_xai_provider_name(
        provider
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or("Grok".into())
            .into(),
    );
    app.set_xai_account_ids(ModelRc::new(VecModel::from(ids)));
    app.set_xai_account_options(ModelRc::new(VecModel::from(options)));
    app.set_xai_account_choice(choice);
    app.set_delete_confirm(false);
    app.set_error_code("".into());
    app.set_error_message("".into());
    app.set_busy(false);
    app.set_editor_open(false);
    app.set_subscription_editor_open(false);
    app.set_xai_editor_open(true);
    app.set_connection_picker_open(false);
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_xai_editor(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::BeginXaiEditor(id.into()));
        }
    });
    app.on_save_xai(queue_on(app, sender, |app| {
        let account_id = app
            .get_xai_account_ids()
            .row_data(app.get_xai_account_choice().max(0) as usize)
            .map(|v| v.to_string())
            .unwrap_or_default();
        Command::SaveXai {
            id: app.get_xai_provider_id().into(),
            name: app.get_xai_provider_name().into(),
            account_id,
            directory: directory_draft(app),
        }
    }));
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_xai_account_action(move |code, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        match code {
            XaiAccountAction::REFRESH_QUOTA => refresh_xai_quota(&app, &callback_sender, &id),
            XaiAccountAction::TICK_QUOTA => {
                update_xai_quota_times(&app, chrono::Utc::now().timestamp())
            }
            code => match XaiAccountAction::from_code(code) {
                Some(action) => queue(
                    &app,
                    &callback_sender,
                    Command::XaiAccount {
                        action,
                        id: id.into(),
                    },
                ),
                None => show_action(&app, Err("未知 Grok 账号操作".into())),
            },
        }
    });
}

impl Worker {
    pub(super) fn begin_xai_editor(&self, id: String) {
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            app::ensure_editable(data_dir)?;
            let provider = if id.is_empty() {
                None
            } else {
                let provider = app::load_provider(data_dir, &id)?;
                xai::validate_provider(&provider)?;
                Some(provider)
            };
            Ok::<_, String>((provider, xai::AccountManager::open(data_dir)?.list()?))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok((provider, accounts)) => show_xai_editor(&app, provider, accounts),
            Err(error) => show_action(&app, Err(error)),
        });
    }

    pub(super) fn save_xai(
        &mut self,
        id: String,
        name: String,
        account_id: String,
        directory: Option<Vec<app::ConnectionModel>>,
    ) {
        let result = self.data_dir.path().and_then(|data_dir| {
            let binding = if account_id == "@default" {
                AccountBinding::Default
            } else {
                AccountBinding::Fixed(account_id)
            };
            xai::save_provider_with_directory(
                data_dir,
                (!id.is_empty()).then_some(id.as_str()),
                &name,
                binding,
                directory.as_deref(),
            )
        });
        let snapshot = self.snapshot_after(&result);
        if result.is_ok() {
            self.discard_previews();
        }
        let accounts = self.data_dir.ok().map(xai_account_view);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            let created_connection = snapshot
                .as_ref()
                .and_then(|snapshot| created_connection_id(&app, snapshot));
            if let Some(view) = accounts {
                show_xai_accounts(&app, view);
            }
            if let Some(snapshot) = snapshot {
                set_provider_check_id(&app, &id, "");
                show_result(&app, snapshot);
                app.set_xai_editor_open(false);
                app.invoke_reset_connection_draft();
                clear_previews(&app);
            }
            show_action(
                &app,
                result.map(|()| "Grok 连接与账号绑定已保存；在工作台预览并启用路由".into()),
            );
            if let Some(id) = created_connection {
                app.invoke_begin_connection_models(id);
            }
        });
    }

    /// Opens the Grok account manager once; later commands share it.
    pub(super) fn xai_manager(&mut self) -> Result<xai::AccountManager, String> {
        if let Some(manager) = &self.xai_manager {
            return Ok(manager.clone());
        }
        let manager = xai::AccountManager::open(self.data_dir.path()?)?;
        self.xai_manager = Some(manager.clone());
        Ok(manager)
    }

    pub(super) async fn xai_account(&mut self, action: XaiAccountAction, id: String) {
        let result = self.run_xai_account_action(action, &id).await;
        let accounts = self.data_dir.ok().map(xai_account_view);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(view) = accounts {
                show_xai_accounts(&app, view);
            }
            clear_previews(&app);
            show_action(&app, result);
            if action == XaiAccountAction::Remove {
                app.set_account_removing_id("".into());
            }
            if action == XaiAccountAction::Refresh {
                app.invoke_xai_account_action(XaiAccountAction::REFRESH_QUOTA, "".into());
            }
        });
    }

    async fn run_xai_account_action(
        &mut self,
        action: XaiAccountAction,
        id: &str,
    ) -> Result<String, String> {
        let data_dir = self.data_dir.path()?.to_path_buf();
        if action == XaiAccountAction::CancelLogin {
            if let Some(login) = self.pending_xai_login.take() {
                login.cancel().await;
            }
            let _ = self.weak.upgrade_in_event_loop(|app| {
                app.set_xai_pending(false);
                app.set_xai_user_code("".into());
                app.set_xai_login_url("".into());
                app.set_xai_login_id("".into());
            });
            return Ok("Grok 登录已取消".into());
        }
        if self
            .pending_xai_login
            .as_ref()
            .is_some_and(PendingLogin::is_running)
        {
            return Err("请先完成或取消 Grok 登录".into());
        }
        let manager = self.xai_manager()?;
        match action {
            XaiAccountAction::Refresh => Ok("已刷新 Grok 账号列表".into()),
            XaiAccountAction::Login => {
                app::ensure_editable(&data_dir)?;
                let login = manager.start_login().await?;
                open_url(&login.verification_url)?;
                let login_id = new_id()?;
                let ui_id = login_id.clone();
                let url = login.verification_url.clone();
                let code = login.user_code.clone();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_xai_login_id(ui_id.into());
                    app.set_xai_pending(true);
                    app.set_xai_login_url(url.into());
                    app.set_xai_user_code(code.into());
                });
                let (cancel, receiver) = watch::channel(false);
                let window = self.weak.clone();
                let task = tokio::spawn(async move {
                    let result = login.finish(receiver).await;
                    let view = xai_account_view(&data_dir);
                    let _ = window.upgrade_in_event_loop(move |app| {
                        if app.get_xai_login_id() != login_id.as_str() {
                            return;
                        }
                        app.set_xai_pending(false);
                        app.set_xai_user_code("".into());
                        app.set_xai_login_url("".into());
                        // Reauthorization supersedes queries using previous credentials.
                        if let Ok(account) = &result {
                            let mut rows = app.get_xai_accounts().iter().collect::<Vec<_>>();
                            if let Some(row) = rows.iter_mut().find(|row| row.id == account.id) {
                                row.quota = XaiQuotaRow::default();
                            }
                            sync_account_rows(app.get_xai_accounts(), rows, |model| {
                                app.set_xai_accounts(model)
                            });
                        }
                        show_xai_accounts(&app, view);
                        match result {
                            Ok(account) => {
                                app.set_action_message(
                                    format!(
                                        "Grok 账号 {} 已保存，可绑定到 Grok 上游",
                                        account.label
                                    )
                                    .into(),
                                );
                                app.invoke_xai_account_action(
                                    XaiAccountAction::REFRESH_QUOTA,
                                    account.id.into(),
                                );
                            }
                            Err(error) => {
                                app.set_xai_status(error.clone().into());
                                app.set_xai_account_error(error.clone().into());
                                app.set_action_message(error.into());
                            }
                        }
                        clear_previews(&app);
                    });
                });
                self.pending_xai_login = Some(PendingLogin::new(cancel, task));
                Ok("已打开 xAI 官方页面，等待验证码授权".into())
            }
            XaiAccountAction::SetDefault => {
                app::ensure_editable(&data_dir)?;
                manager.set_default(id).await?;
                self.discard_previews();
                Ok("默认 Grok 账号已更新，请重新预览路由".into())
            }
            XaiAccountAction::Remove => {
                app::ensure_editable(&data_dir)?;
                let rows = xai_account_view(&data_dir)?;
                if rows
                    .iter()
                    .any(|a| a.id == id && a.bound_provider_count > 0)
                {
                    return Err("Grok 账号仍被上游引用，请先重新绑定或删除连接".into());
                }
                manager.remove(id).await?;
                self.discard_previews();
                let _ = self.weak.upgrade_in_event_loop(|app| {
                    app.set_delete_account_confirm(false);
                    app.set_delete_account_id("".into());
                });
                Ok("Grok 账号已移除".into())
            }
            XaiAccountAction::CancelLogin => unreachable!("cancelled before the login guard"),
        }
    }
}
