//! The background worker owns route, login, and preview state. It runs one
//! window command at a time; checks, quota queries, and logins continue as
//! tracked tasks so they never block other commands.

use std::path::{Path, PathBuf};

use switchx::{
    app::{self, AppError, Snapshot, load_snapshot},
    chatgpt::accounts::AccountManager,
    codex::{client, direct_switch::PreparedDirectSwitch, route_switch},
    route_session::RouteSession,
    storage::{ModelRecord, ProviderRecord, Store},
    ui::AppWindow,
    xai,
};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};

use super::{
    accounts::sync_owned_account,
    codex_config::codex_home,
    command::Command,
    control::{ControlPreviews, ControlServer},
    feedback::show_action,
    provider_checks::finish_provider_check,
    subscription_editor::SubscriptionAuthDraft,
};

/// The SwitchX data directory, or why it is unavailable.
pub(super) struct DataDir(Result<PathBuf, AppError>);

impl DataDir {
    /// For reads that show the typed error with its recovery action.
    pub(super) fn get(&self) -> Result<&Path, AppError> {
        self.0.as_deref().map_err(|error| *error)
    }

    /// For operations that report a message.
    pub(super) fn path(&self) -> Result<&Path, String> {
        self.get().map_err(|error| error.message().to_owned())
    }

    pub(super) fn ok(&self) -> Option<&Path> {
        self.0.as_deref().ok()
    }

    /// Old journals must be restored before the database may migrate.
    fn needs_recovery(&self, error: &str) -> Result<bool, String> {
        self.ok()
            .map(|path| Store::needs_recovery_before_migration(&path.join("switchx.sqlite")))
            .transpose()
            .map_err(|_| error.to_owned())
            .map(Option::unwrap_or_default)
    }
}

/// A device or browser login waiting in the background.
pub(super) struct PendingLogin {
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl PendingLogin {
    pub(super) fn new(cancel: watch::Sender<bool>, task: JoinHandle<()>) -> Self {
        Self { cancel, task }
    }

    pub(super) fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub(super) fn request_cancel(&self) {
        self.cancel.send_replace(true);
    }

    pub(super) async fn cancel(self) {
        self.request_cancel();
        let _ = self.task.await;
    }
}

/// An inspected direct switch, with the provider and configuration it was built from.
pub(super) type PreparedDirect = (
    PreparedDirectSwitch,
    ProviderRecord,
    app::ProviderConfigState,
);

/// Codex owns its prepared switch, live router, and account synchronization home.
/// Selecting another client's view never replaces this running context.
pub(super) struct CodexRuntime {
    pub(super) prepared: Option<PreparedDirect>,
    pub(super) route_session: RouteSession,
    pub(super) account_home: Option<PathBuf>,
}

impl Default for CodexRuntime {
    fn default() -> Self {
        Self {
            prepared: None,
            route_session: RouteSession::default(),
            account_home: client::default_home().ok(),
        }
    }
}

pub(super) struct Worker {
    pub(super) weak: slint::Weak<AppWindow>,
    pub(super) data_dir: DataDir,
    pub(super) codex: CodexRuntime,
    pub(super) pending_login: Option<PendingLogin>,
    pub(super) pending_xai_login: Option<PendingLogin>,
    pub(super) xai_manager: Option<xai::AccountManager>,
    pub(super) quota_account_manager: Option<AccountManager>,
    pub(super) subscription_auth: Option<SubscriptionAuthDraft>,
    /// The directory the connection editor opened, compared again on save.
    pub(super) connection_models_snapshot: Option<(String, Vec<ModelRecord>)>,
    pub(super) provider_checks: JoinSet<()>,
    pub(super) xai_quota_queries: JoinSet<()>,
    pub(super) codex_quota_queries: JoinSet<()>,
    pub(super) recovery_only: bool,
    pub(super) control_previews: ControlPreviews,
    /// The agent control listener while the user allows it.
    pub(super) control_server: Option<ControlServer>,
    pub(super) control_sender: mpsc::WeakSender<Command>,
}

/// Runs window commands until every sender is dropped, then restores
/// managed configuration for exit.
pub(super) async fn run(
    receiver: mpsc::Receiver<Command>,
    control_sender: mpsc::WeakSender<Command>,
    weak: slint::Weak<AppWindow>,
    directory: Result<PathBuf, AppError>,
) -> Result<(), String> {
    Worker {
        weak,
        data_dir: DataDir(directory),
        codex: CodexRuntime::default(),
        pending_login: None,
        pending_xai_login: None,
        xai_manager: None,
        quota_account_manager: None,
        subscription_auth: None,
        connection_models_snapshot: None,
        provider_checks: JoinSet::new(),
        xai_quota_queries: JoinSet::new(),
        codex_quota_queries: JoinSet::new(),
        recovery_only: false,
        control_previews: ControlPreviews::default(),
        control_server: None,
        control_sender,
    }
    .run(receiver)
    .await
}

impl Worker {
    async fn run(mut self, mut receiver: mpsc::Receiver<Command>) -> Result<(), String> {
        let control = self.data_dir.ok().is_some_and(switchx::control::enabled);
        self.sync_agent_control(control).await;
        while let Some(command) = receiver.recv().await {
            while self.provider_checks.try_join_next().is_some() {}
            while self.xai_quota_queries.try_join_next().is_some() {}
            while self.codex_quota_queries.try_join_next().is_some() {}
            self.recovery_only = self
                .data_dir
                .needs_recovery("无法检查数据库升级前的恢复状态")?;
            if self.recovery_only && !command.allowed_during_recovery() {
                self.refuse_until_restored(&command);
                continue;
            }
            if let Some(home) = command.codex_home().and_then(|text| codex_home(text).ok()) {
                self.codex.account_home = Some(home);
            }
            self.handle(command).await?;
            self.publish_route_status();
        }
        // Agents cannot reach the app once its window has gone.
        self.control_server = None;
        self.stop_background_tasks().await?;
        let recovery_only = self.data_dir.needs_recovery("无法检查退出前的恢复状态")?;
        // Also recover when the platform exits the event loop without using our tray.
        self.restore_for_exit(recovery_only).await
    }

    async fn handle(&mut self, command: Command) -> Result<(), String> {
        // Anything the window does may change what an agent previewed.
        if !matches!(
            command,
            Command::Control { .. }
                | Command::PollRouteStatus
                | Command::RefreshRequests
                | Command::Refresh(_)
        ) {
            self.control_previews.clear();
        }
        match command {
            Command::InitializeClients => self.initialize_client_workspaces(),
            Command::ClientNavigation { workspace_id, page } => {
                self.client_navigation(workspace_id, page)
            }
            Command::InspectClaude(home) => self.inspect_claude(home),
            Command::PollRouteStatus => {}
            Command::RefreshRequests => self.refresh_requests(),
            Command::ClearRequests => self.clear_requests(),
            Command::Refresh(check_credentials) => self.refresh(check_credentials),
            Command::BeginXaiEditor(id) => self.begin_xai_editor(id),
            Command::SaveXai {
                id,
                name,
                account_id,
                directory,
            } => self.save_xai(id, name, account_id, directory),
            Command::CodexQuota { requests, home } => self.query_codex_quota(requests, home),
            Command::XaiQuota(requests) => self.query_xai_quota(requests),
            Command::XaiAccount { action, id } => self.xai_account(action, id).await,
            Command::BeginProviderEditor { id, home } => self.begin_provider_editor(id, home),
            Command::BeginSubscriptionEditor {
                id,
                home,
                generation,
            } => self.begin_subscription_editor(id, home, generation),
            Command::LoadSubscriptionAuth {
                provider_id,
                account_id,
                home,
                generation,
            } => self.load_subscription_auth(provider_id, account_id, home, generation),
            Command::CancelSubscriptionEditor => self.cancel_subscription_editor(),
            Command::SubscriptionDirectoryAuth { auth, generation } => {
                self.subscription_directory_auth(auth, generation)
            }
            Command::SaveSubscription(form) => self.save_subscription(form).await,
            Command::OpenCommonConfig(home) => self.open_common_config(home),
            Command::ExtractCommonConfig(form_toml) => self.extract_common_config(form_toml),
            Command::SaveCommonConfig(snippet) => self.save_common_config(snippet),
            Command::Save(form) => self.save_provider(form),
            Command::Delete(id) => self.delete_provider(id),
            Command::Check {
                requests,
                home,
                started,
            } => {
                for (id, check_id) in requests {
                    self.check_provider(id, home.clone(), check_id, started);
                }
            }
            Command::FetchModels {
                scope,
                generation,
                provider,
                draft,
                url,
                key,
                home: _,
            } => {
                self.fetch_models(scope, generation, provider, draft, url, key)
                    .await
            }
            Command::InspectDirect { id, home } => {
                let _ = self.inspect_direct(id, home);
            }
            Command::ApplyDirect(home) => {
                let _ = self.apply_direct(home).await;
            }
            Command::RestoreConfig(home) => {
                let _ = self.restore_config(home).await;
            }
            Command::InspectConfig(home) => self.inspect_config(home),
            Command::ImportCurrent(home) => self.import_current(home),
            Command::CheckLogin(home) => self.check_login(home).await,
            Command::Subscription { action, home, port } => {
                self.subscription(action, home, port).await
            }
            Command::Account { action, id, home } => self.account(action, id, home).await,
            Command::BeginModelEditor {
                provider,
                original_id,
            } => self.begin_model_editor(provider, original_id),
            Command::SaveModel(form) => self.save_model(form).await,
            Command::SaveModels(form) => self.save_models(form).await,
            Command::BeginConnectionModels {
                provider,
                discovered,
            } => self.begin_connection_models(provider, discovered),
            Command::SaveConnectionModels { provider, models } => {
                self.save_connection_models(provider, models).await
            }
            Command::DeleteModel(public_id) => self.delete_model(public_id),
            Command::SelectModels(public_ids, enabled) => self.select_models(public_ids, enabled),
            Command::SaveFallback(public_id, fallback) => self.save_fallback(public_id, fallback),
            Command::InspectRoute { home, port, model } => {
                let _ = self.inspect_route(home, port, model).await;
            }
            Command::ApplyRoute { home, port, model } => {
                let _ = self.apply_route(home, port, model).await;
            }
            Command::CancelRoutePreview => self.cancel_route_preview(),
            Command::Control { request, reply } => {
                let _ = reply.send(self.control(request).await);
            }
            Command::SetAgentControl(enabled) => self.set_agent_control(enabled).await,
            Command::Quit => return self.quit().await,
        }
        Ok(())
    }

    fn refuse_until_restored(&self, command: &Command) {
        let checks = match command {
            Command::Check { requests, .. } => requests.clone(),
            _ => Vec::new(),
        };
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            for (id, check_id) in checks {
                finish_provider_check(&app, &id, &check_id, 0, Err("请先恢复原配置".into()));
            }
            app.set_recovery_only(true);
            app.set_loading(false);
            app.set_request_loading(false);
            app.set_request_clearing(false);
            app.set_request_clear_open(false);
            app.set_config_managed(true);
            app.set_active_page(5);
            show_action(
                &app,
                Err("旧版资料有配置待恢复；请先恢复原配置，成功后再升级并读取资料。".into()),
            );
        });
    }

    /// Previews depend on saved connections, so any change invalidates them.
    pub(super) fn discard_previews(&mut self) {
        self.codex.prepared = None;
        self.codex.route_session.discard_preview();
    }

    /// The snapshot to show after a change; failed changes keep the window as is.
    pub(super) fn snapshot_after<T, E>(
        &self,
        result: &Result<T, E>,
    ) -> Option<Result<Snapshot, AppError>> {
        result
            .as_ref()
            .ok()
            .and(self.data_dir.ok())
            .map(|path| load_snapshot(path, false))
    }

    async fn quit(&mut self) -> Result<(), String> {
        self.stop_background_tasks().await?;
        let result = self.restore_for_exit(self.recovery_only).await;
        let _ = self.weak.upgrade_in_event_loop(move |app| match result {
            Ok(()) => {
                let _ = slint::quit_event_loop();
            }
            Err(error) => {
                app.set_window_visible(true);
                let _ = slint::ComponentHandle::show(&app);
                app.set_active_page(5);
                show_action(&app, Err(format!("退出前恢复未完成：{error}")));
            }
        });
        Ok(())
    }

    async fn stop_background_tasks(&mut self) -> Result<(), String> {
        self.codex_quota_queries.abort_all();
        while self.codex_quota_queries.join_next().await.is_some() {}
        if let Some(manager) = &self.quota_account_manager {
            manager.wait_for_idle().await?;
        }
        self.xai_quota_queries.abort_all();
        while self.xai_quota_queries.join_next().await.is_some() {}
        while self.provider_checks.join_next().await.is_some() {}
        if let Some(login) = self.pending_xai_login.take() {
            login.cancel().await;
        }
        if let Some(manager) = &self.xai_manager {
            manager.wait_for_idle().await;
        }
        if let Some(login) = self.pending_login.take() {
            login.cancel().await;
        }
        Ok(())
    }

    async fn restore_for_exit(&mut self, recovery_only: bool) -> Result<(), String> {
        if let Some(data_dir) = self.data_dir.ok()
            && let Some(recovery) = route_switch::recovery(data_dir)?
        {
            self.codex
                .route_session
                .restore(data_dir, recovery.config_path.parent().unwrap())
                .await?;
        }
        // Recovery pauses and stops new requests before waiting for
        // any detached refresh to save its rotated credentials.
        self.codex.route_session.wait_for_accounts().await?;
        if !recovery_only
            && let (Some(data_dir), Some(home)) = (self.data_dir.ok(), &self.codex.account_home)
        {
            AccountManager::open(data_dir)?.wait_for_idle().await?;
            sync_owned_account(data_dir, home)?;
        }
        self.codex.route_session.drain_for_exit().await
    }
}
