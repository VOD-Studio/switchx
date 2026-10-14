//! Saved ChatGPT accounts and the native Codex login.

use std::path::Path;

use slint::{ComponentHandle, Model};
use switchx::{
    app::load_snapshot,
    chatgpt::{self, accounts::AccountManager},
    codex::{self, client},
    id::new_id,
    storage::{AccountBinding, ProviderKind, ProviderRecord, Store},
    ui::{AccountRow, AppWindow, CodexQuotaRow},
    upstream,
};
use tokio::sync::{mpsc, watch};

use super::{
    codex_config::{
        clear_previews, codex_home, restore_connections, route_port, route_preview_data,
        show_config_status,
    },
    command::{AccountAction, Command, SubscriptionAction, queue},
    feedback::show_action,
    open_url,
    quota::{refresh_codex_quota, tick_codex_quotas},
    rows::sync_account_rows,
    workbench::show_result,
    worker::{PendingLogin, Worker},
};

pub(super) struct AccountView {
    pub(super) rows: Vec<AccountRow>,
    pub(super) selected: String,
    pub(super) status: String,
}

pub(super) fn provider_account_id(
    provider: &ProviderRecord,
    default_id: Option<&str>,
) -> Option<String> {
    if provider.kind != ProviderKind::Chatgpt {
        return None;
    }
    match &provider.account_binding {
        Some(AccountBinding::Fixed(id)) => Some(id.clone()),
        Some(AccountBinding::Default) => default_id.map(str::to_owned),
        _ => None,
    }
}

pub(super) fn account_provider_names(
    providers: &[ProviderRecord],
    account_id: &str,
    default_id: Option<&str>,
) -> Vec<String> {
    providers
        .iter()
        .filter(|provider| provider_account_id(provider, default_id).as_deref() == Some(account_id))
        .map(|provider| provider.name.clone())
        .collect()
}

pub(super) fn account_view(data_dir: &Path, home: &Path) -> Result<AccountView, String> {
    let manager = AccountManager::open(data_dir)?;
    let active = manager.active_id(home)?;
    let accounts = manager.list()?;
    let providers = Store::open_read_only(&data_dir.join("switchx.sqlite"))
        .map_err(|_| "无法读取上游账号引用")?
        .providers()
        .map_err(|_| "无法读取上游账号引用")?;
    let default_id = manager.default_id()?;
    let selected = active.clone().unwrap_or_default();
    let status = format!(
        "已保存 {} 个账号 · Codex 入口登录{} · 各上游绑定独立管理",
        accounts.len(),
        active
            .as_ref()
            .and_then(|id| accounts.iter().find(|account| &account.id == id))
            .map(|account| format!("：{}", account.label))
            .unwrap_or_else(|| "尚未关联到保存的账号".into())
    );
    Ok(AccountView {
        rows: accounts
            .into_iter()
            .map(|account| {
                let names = account_provider_names(&providers, &account.id, default_id.as_deref());
                AccountRow {
                    is_active: active.as_ref() == Some(&account.id),
                    initial: account_initial(&account.label).into(),
                    id: account.id.into(),
                    label: account.label.into(),
                    workspace: account.workspace_id.into(),
                    is_default: account.is_default,
                    credential_generation: account.authenticated_at_ms.to_string().into(),
                    requires_reauth: false,
                    bound_provider_count: names.len() as i32,
                    bound_provider_names: names.join("、").into(),
                    ..Default::default()
                }
            })
            .collect(),
        selected,
        status,
    })
}

/// First visible character of an account label, shown in the card avatar.
pub(super) fn account_initial(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .or_else(|| label.chars().next())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

pub(super) fn show_accounts(app: &AppWindow, view: Result<AccountView, String>) {
    match view {
        Ok(mut view) => {
            let previous = app.get_accounts();
            for row in &mut view.rows {
                if let Some(old) = previous.iter().find(|old| {
                    old.id == row.id && old.credential_generation == row.credential_generation
                }) {
                    row.codex_quota = old.codex_quota;
                }
            }
            sync_account_rows(app.get_accounts(), view.rows, |model| {
                app.set_accounts(model)
            });
            app.set_selected_account_id(view.selected.into());
            app.set_account_status(view.status.into());
            app.set_account_error("".into());
            if app.get_active_page() == 1 && app.get_connections_tab() == 1 {
                let unread = app
                    .get_accounts()
                    .iter()
                    .filter(|row| row.codex_quota.request_id.is_empty())
                    .map(|row| row.id)
                    .collect::<Vec<_>>();
                for id in unread {
                    app.invoke_account_action(AccountAction::REFRESH_QUOTA, id);
                }
            }
        }
        Err(error) => {
            app.set_account_status(error.clone().into());
            app.set_account_error(error.into());
        }
    }
}

pub(super) fn sync_owned_account(data_dir: &Path, home: &Path) -> Result<(), String> {
    let manager = AccountManager::open(data_dir)?;
    // An external/native login has no matching ownership marker. Following it
    // must not adopt that credential into another saved account.
    if manager.active_id(home)?.is_some() {
        manager.sync_current(home)?;
    }
    Ok(())
}

pub(super) fn ensure_accounts_editable(data_dir: &Path) -> Result<(), String> {
    if codex::config_managed(data_dir) {
        return Err("请先恢复并停止路由，再修改默认账号或移除账号".into());
    }
    Ok(())
}

pub(super) fn ensure_account_unbound(data_dir: &Path, account_id: &str) -> Result<(), String> {
    let providers = Store::open_read_only(&data_dir.join("switchx.sqlite"))
        .and_then(|store| store.providers())
        .map_err(|_| "无法检查账号关联的上游；账号未移除")?;
    let default_id = AccountManager::open(data_dir)?.default_id()?;
    let names = account_provider_names(&providers, account_id, default_id.as_deref());
    if !names.is_empty() {
        return Err(format!(
            "此账号仍被上游引用：{}；请先重新绑定或删除这些上游，再移除账号",
            names.join("、")
        ));
    }
    Ok(())
}

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_check_login(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::CheckLogin(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_subscription_action(move |code| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(action) = SubscriptionAction::from_code(code) else {
            show_action(&app, Err("未知订阅操作".into()));
            return;
        };
        queue(
            &app,
            &callback_sender,
            Command::Subscription {
                action,
                home: app.get_config_home().into(),
                port: app.get_route_port().into(),
            },
        );
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_account_action(move |code, id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        match code {
            AccountAction::REFRESH_QUOTA => refresh_codex_quota(&app, &callback_sender, &id),
            AccountAction::TICK_QUOTA => tick_codex_quotas(&app, chrono::Utc::now().timestamp()),
            code => match AccountAction::from_code(code) {
                Some(action) => queue(
                    &app,
                    &callback_sender,
                    Command::Account {
                        action,
                        id: id.into(),
                        home: app.get_config_home().into(),
                    },
                ),
                None => show_action(&app, Err("未知账号操作".into())),
            },
        }
    });
}

impl Worker {
    pub(super) async fn account(&mut self, action: AccountAction, id: String, target: String) {
        let result = self.run_account_action(action, id, &target).await;
        let accounts = self.data_dir.ok().and_then(|data_dir| {
            codex_home(&target)
                .ok()
                .map(|home| account_view(data_dir, &home))
        });
        let status = self.data_dir.ok().and_then(|data_dir| {
            codex_home(&target)
                .ok()
                .and_then(|home| client::inspect(&home, data_dir).ok())
        });
        let snapshot = self.snapshot_after(&result);
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(snapshot) = snapshot {
                show_result(&app, snapshot);
            }
            if action == AccountAction::ImportCurrent && result.is_ok() {
                let rows = app
                    .get_accounts()
                    .iter()
                    .map(|row| AccountRow {
                        codex_quota: CodexQuotaRow::default(),
                        ..row
                    })
                    .collect::<Vec<_>>();
                sync_account_rows(app.get_accounts(), rows, |model| app.set_accounts(model));
            }
            if let Some(accounts) = accounts {
                show_accounts(&app, accounts);
            }
            if let Some(status) = status {
                show_config_status(&app, status);
            }
            if action == AccountAction::Refresh && result.is_ok() {
                app.invoke_account_action(AccountAction::REFRESH_QUOTA, "".into());
            }
            if action == AccountAction::Remove {
                app.set_account_removing_id("".into());
            }
            show_action(&app, result);
        });
    }

    async fn run_account_action(
        &mut self,
        action: AccountAction,
        id: String,
        target: &str,
    ) -> Result<String, String> {
        let target_home = codex_home(target)?;
        let data_dir = self.data_dir.path()?;
        let manager = AccountManager::open(data_dir)?;
        if self
            .pending_login
            .as_ref()
            .is_some_and(PendingLogin::is_running)
        {
            return Err("登录仍在进行；请先完成或取消登录".into());
        }
        match action {
            AccountAction::Refresh => Ok("已刷新保存的账号列表".into()),
            AccountAction::Login => {
                let login_id = new_id()?;
                let login = manager.start_login().await?;
                open_url(&login.verification_url)?;
                let code = login.user_code.clone();
                let url = login.verification_url.clone();
                let ui_id = login_id.clone();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_chatgpt_login_id(ui_id.into());
                    app.set_chatgpt_login_pending(true);
                    app.set_login_user_code(code.into());
                    app.set_login_url(url.into());
                    app.set_account_status("正在等待官方设备验证码登录…".into());
                });
                let (cancel, receiver) = watch::channel(false);
                let window = self.weak.clone();
                let data_dir = data_dir.to_path_buf();
                let task = tokio::spawn(async move {
                    let result = login.finish(receiver).await;
                    let view = account_view(&data_dir, &target_home);
                    let _ = window.upgrade_in_event_loop(move |app| {
                        if app.get_chatgpt_login_id() != login_id.as_str() {
                            return;
                        }
                        app.set_chatgpt_login_pending(false);
                        app.set_login_user_code("".into());
                        app.set_login_url("".into());
                        show_accounts(&app, view);
                        match result {
                            Ok(account) => app.set_action_message(
                                format!(
                                    "已保存账号 {}；可绑定到订阅上游，或显式写入 Codex 登录",
                                    account.label
                                )
                                .into(),
                            ),
                            Err(error) => {
                                app.set_account_status(error.clone().into());
                                app.set_account_error(error.clone().into());
                                app.set_action_message(error.into());
                            }
                        }
                    });
                });
                self.pending_login = Some(PendingLogin::new(cancel, task));
                Ok("已打开官方登录页；完成验证码登录后保存账号".into())
            }
            AccountAction::SetDefault => {
                ensure_accounts_editable(data_dir)?;
                manager.set_default(&id)?;
                self.route_session.discard_preview();
                let _ = self
                    .weak
                    .upgrade_in_event_loop(|app| app.set_route_preview_ready(false));
                Ok("已设为默认账号；固定绑定保持不变，旧的跟随默认账号连接会使用此默认选择".into())
            }
            AccountAction::Activate => {
                let id = if id.is_empty() {
                    manager.default_id()?.ok_or("请先添加或设置默认账号")?
                } else {
                    id
                };
                restore_connections(&mut self.route_session, data_dir, &target_home).await?;
                self.discard_previews();
                let _ = self.weak.upgrade_in_event_loop(|app| clear_previews(&app));
                manager.activate(&id, &target_home).await?;
                let _ = self.weak.upgrade_in_event_loop(|app| {
                    app.set_auth_status("已设置 Codex 入口登录；各上游绑定独立管理".into());
                });
                Ok("账号已写入 Codex 入口登录；各上游绑定保持不变，请重新发布并启动新会话".into())
            }
            AccountAction::Remove => {
                ensure_accounts_editable(data_dir)?;
                ensure_account_unbound(data_dir, &id)?;
                self.discard_previews();
                manager.remove(&id, &target_home).await?;
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_route_preview_ready(false);
                    app.set_delete_account_confirm(false);
                    app.set_delete_account_id("".into());
                    if app.get_selected_account_id() == id.as_str() {
                        app.set_auth_status("请重新选择账号并检查登录".into());
                    }
                });
                Ok("账号已移除；固定绑定此账号的请求不会自动改用其他账号".into())
            }
            AccountAction::ImportCurrent => {
                let account = manager.import_current(&target_home)?;
                Ok(format!(
                    "已保存此 Codex 目录的账号 {}；当前登录保持不变",
                    account.label
                ))
            }
        }
    }

    pub(super) async fn subscription(
        &mut self,
        action: SubscriptionAction,
        target: String,
        port: String,
    ) {
        let result = self.run_subscription_action(action, &target, &port).await;
        let status = codex_home(&target).ok().and_then(|path| {
            self.data_dir
                .ok()
                .and_then(|data_dir| client::inspect(&path, data_dir).ok())
        });
        let accounts = self.data_dir.ok().and_then(|data_dir| {
            codex_home(&target)
                .ok()
                .map(|home| account_view(data_dir, &home))
        });
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            if let Some(accounts) = accounts {
                show_accounts(&app, accounts);
            }
            if let Some(status) = status {
                show_config_status(&app, status);
            }
            if matches!(
                action,
                SubscriptionAction::Login | SubscriptionAction::CheckLogin
            ) && let Err(error) = &result
            {
                app.set_auth_status(error.as_str().into());
            }
            show_action(&app, result);
        });
    }

    async fn run_subscription_action(
        &mut self,
        action: SubscriptionAction,
        target: &str,
        port: &str,
    ) -> Result<String, String> {
        let target_home = codex_home(target)?;
        let data_dir = self.data_dir.path()?;
        if self
            .pending_login
            .as_ref()
            .is_some_and(PendingLogin::is_running)
            && action != SubscriptionAction::CancelLogin
        {
            return Err("登录仍在进行；可先完成或取消登录".into());
        }
        match action {
            SubscriptionAction::AddConnection => {
                let models = chatgpt::catalog().await?;
                chatgpt::save_connection(data_dir, &models)?;
                self.route_session.discard_preview();
                let snapshot = load_snapshot(data_dir, false).map_err(|error| error.message())?;
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    show_result(&app, Ok(snapshot));
                    app.set_route_preview_ready(false);
                    app.set_connections_tab(1);
                    app.set_active_page(1);
                });
                Ok("已添加订阅连接与 CLI 内置模型资料；请登录后预览发布".to_owned())
            }
            SubscriptionAction::Login => {
                // Reauthentication is an explicit restore-then-login operation.
                restore_connections(&mut self.route_session, data_dir, &target_home).await?;
                sync_owned_account(data_dir, &target_home)?;
                self.prepared = None;
                self.route_session.discard_preview();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                });
                let login = chatgpt::Session::start(&target_home).await?.login().await?;
                open_url(&login.url)?;
                let (cancel, receiver) = watch::channel(false);
                let id = new_id()?;
                let login_id = id.clone();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_chatgpt_login_id(id.into());
                    app.set_chatgpt_login_pending(true);
                    app.set_login_user_code("".into());
                    app.set_login_url("".into());
                    app.set_auth_status("正在等待官方浏览器登录…".into());
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                });
                let window = self.weak.clone();
                let account_data_dir = data_dir.to_path_buf();
                let account_target = target_home.clone();
                let task = tokio::spawn(async move {
                    let result = login.finish(receiver).await;
                    let accounts = account_view(&account_data_dir, &account_target);
                    let _ = window.upgrade_in_event_loop(move |app| {
                        if app.get_chatgpt_login_id() != login_id.as_str() {
                            return;
                        }
                        app.set_chatgpt_login_pending(false);
                        show_accounts(&app, accounts);
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
                self.pending_login = Some(PendingLogin::new(cancel, task));
                Ok("已打开官方登录页；原配置已恢复，可在此取消登录".to_owned())
            }
            SubscriptionAction::CheckLogin => {
                let manager = AccountManager::open(data_dir)?;
                if let Some(id) = manager.active_id(&target_home)? {
                    manager.refresh(&id, &target_home).await?;
                }
                let status = chatgpt::account(&target_home, true).await?;
                sync_owned_account(data_dir, &target_home)?;
                let _ = self
                    .weak
                    .upgrade_in_event_loop(move |app| app.set_auth_status(status.label().into()));
                status.require_chatgpt()?;
                Ok("已检查订阅登录并同步续期凭据；活跃会话也可由 Codex 自动续期".to_owned())
            }
            SubscriptionAction::CancelLogin => {
                if let Some(login) = &self.pending_login {
                    login.request_cancel();
                }
                Ok("已请求取消官方登录".to_owned())
            }
            SubscriptionAction::SwitchToApi => {
                let port = route_port(port)?;
                let helper = std::env::current_exe().map_err(|_| "无法定位 SwitchX 程序")?;
                // API routes record this helper; refuse before restoring the current route.
                upstream::ensure_persistent_helper(&helper)?;
                let snapshot = load_snapshot(data_dir, false).map_err(|error| error.message())?;
                let model = snapshot
                    .models
                    .iter()
                    .find(|model| {
                        model.enabled
                            && snapshot.providers.iter().any(|provider| {
                                provider.id == model.provider_id
                                    && provider.kind == ProviderKind::ApiKey
                            })
                    })
                    .ok_or("请先保存并选择至少一个 API 模型；当前路由与配置已保留")?
                    .public_id
                    .clone();
                restore_connections(&mut self.route_session, data_dir, &target_home).await?;
                self.prepared = None;
                self.route_session.discard_preview();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                });
                chatgpt::deselect_models(data_dir)?;
                let summary = self
                    .route_session
                    .prepare(data_dir, &target_home, port, &model, &helper)
                    .await;
                let snapshot = load_snapshot(data_dir, false).map_err(|error| error.message())?;
                let ready = summary.is_ok();
                let preview = summary.as_ref().ok().cloned();
                let _ = self.weak.upgrade_in_event_loop(move |app| {
                    app.set_route_preview_model(model.as_str().into());
                    show_result(&app, Ok(snapshot));
                    app.set_default_model(model.into());
                    app.set_route_preview(
                        preview.as_ref().map(route_preview_data).unwrap_or_default(),
                    );
                    app.set_route_preview_ready(ready);
                    app.set_active_page(0);
                    app.set_publish_drawer_open(true);
                });
                summary?;
                Ok("已准备 API 路由预览；点击“确认启用”，无需订阅登录".to_owned())
            }
        }
    }

    pub(super) async fn check_login(&self, target: String) {
        let result = match codex_home(&target) {
            Ok(path) => client::login_status(&path).await,
            Err(error) => Err(error),
        };
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_initial_uses_the_first_visible_character() {
        assert_eq!(account_initial("y.defect@gmail.com"), "Y");
        assert_eq!(account_initial("_ éclair@example.invalid"), "É");
        assert_eq!(account_initial("账号@example.invalid"), "账");
        assert_eq!(account_initial("--"), "-");
        assert_eq!(account_initial(""), "");
    }

    #[test]
    fn account_removal_checks_saved_fixed_and_default_provider_references() {
        use base64::Engine;
        let root =
            std::env::temp_dir().join(format!("switchx-account-removal-{}", new_id().unwrap()));
        let data = root.join("data");
        let home = root.join("codex");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir(&home).unwrap();
        let auth = home.join("auth.json");
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/synthetic-chatgpt-auth.json"
        ))
        .unwrap();
        let mut parts = fixture["tokens"]["id_token"]
            .as_str()
            .unwrap()
            .split('.')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        parts[0] = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        fixture["tokens"]["id_token"] = serde_json::json!(parts.join("."));
        fixture["tokens"]["access_token"] = fixture["tokens"]["id_token"].clone();
        std::fs::write(&auth, serde_json::to_vec(&fixture).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let manager = AccountManager::open(&data).unwrap();
        let account = manager.import_current(&home).unwrap();
        manager.set_default(&account.id).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        let fixed = ProviderRecord {
            id: "fixed-provider".into(),
            name: "固定上游".into(),
            base_url: chatgpt::BASE_URL.into(),
            model_id: "synthetic-model".into(),
            credential_ref: None,
            icon_id: None,
            kind: ProviderKind::Chatgpt,
            account_binding: Some(AccountBinding::Fixed(account.id.clone())),
        };
        let default = ProviderRecord {
            id: "default-provider".into(),
            name: "默认上游".into(),
            account_binding: Some(AccountBinding::Default),
            ..fixed.clone()
        };
        store.put_provider(&fixed).unwrap();
        store.put_provider(&default).unwrap();
        let account_file = data.join("codex_oauth_auth.json");
        let before = std::fs::read(&account_file).unwrap();
        let error = ensure_account_unbound(&data, &account.id).unwrap_err();
        assert!(error.contains("固定上游") && error.contains("默认上游"));
        // Inspect current database state again instead of trusting an old dialog snapshot.
        store.delete_provider(&fixed.id).unwrap();
        let error = ensure_account_unbound(&data, &account.id).unwrap_err();
        assert!(!error.contains("固定上游") && error.contains("默认上游"));
        store.delete_provider(&default.id).unwrap();
        ensure_account_unbound(&data, &account.id).unwrap();
        assert_eq!(std::fs::read(&account_file).unwrap(), before);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
