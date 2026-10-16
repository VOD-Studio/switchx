//! Requests from the window to the background worker.

use slint::ComponentHandle;
use switchx::{app, codex::provider_config::CodexOptions, credentials::Secret, ui::AppWindow};
use tokio::sync::{
    mpsc::{self, error::TrySendError},
    oneshot,
};

use super::{control::ControlRequest, feedback::show_action};

pub(super) enum Command {
    InitializeClients,
    ClientNavigation {
        workspace_id: String,
        page: i32,
    },
    InspectClaude(String),
    Refresh(bool),
    RefreshRequests,
    ClearRequests,
    PollRouteStatus,
    Save(ProviderForm),
    BeginProviderEditor {
        id: String,
        home: String,
    },
    BeginSubscriptionEditor {
        id: String,
        home: String,
        generation: i32,
    },
    LoadSubscriptionAuth {
        provider_id: String,
        account_id: String,
        home: String,
        generation: i32,
    },
    CancelSubscriptionEditor,
    SubscriptionDirectoryAuth {
        auth: Option<Secret>,
        generation: i32,
    },
    BeginXaiEditor(String),
    SaveXai {
        id: String,
        name: String,
        account_id: String,
        directory: Option<Vec<app::ConnectionModel>>,
    },
    XaiAccount {
        action: XaiAccountAction,
        id: String,
    },
    XaiQuota(Vec<(String, String)>),
    CodexQuota {
        requests: Vec<(String, String)>,
        home: String,
    },
    SaveSubscription(SubscriptionForm),
    OpenCommonConfig(String),
    ExtractCommonConfig(String),
    SaveCommonConfig(String),
    Delete(String),
    Check {
        /// Provider IDs and their corresponding per-card check IDs.
        requests: Vec<(String, String)>,
        home: String,
        started: std::time::Instant,
    },
    FetchModels {
        scope: i32,
        generation: i32,
        provider: String,
        draft: Option<switchx::storage::ProviderRecord>,
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
        action: SubscriptionAction,
        home: String,
        port: String,
    },
    Account {
        action: AccountAction,
        id: String,
        home: String,
    },
    BeginModelEditor {
        provider: String,
        original_id: String,
    },
    SaveModel(ModelForm),
    SaveModels(BatchForm),
    BeginConnectionModels {
        provider: String,
        discovered: Vec<app::ConnectionModel>,
    },
    SaveConnectionModels {
        provider: String,
        models: Vec<app::ConnectionModel>,
    },
    DeleteModel(String),
    SelectModels(Vec<String>, bool),
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
    CancelRoutePreview,
    /// A request from `switchx mcp --allow-control`, answered on `reply`.
    Control {
        request: ControlRequest,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// Turns the agent control channel on or off and saves the choice.
    SetAgentControl(bool),
    Quit,
}

impl Command {
    /// The Codex home this command targets, as typed in the window.
    pub(super) fn codex_home(&self) -> Option<&str> {
        match self {
            Self::Account { home, .. }
            | Self::Subscription { home, .. }
            | Self::InspectRoute { home, .. }
            | Self::ApplyRoute { home, .. }
            | Self::InspectDirect { home, .. }
            | Self::BeginProviderEditor { home, .. }
            | Self::FetchModels { home, .. }
            | Self::InspectConfig(home)
            | Self::CheckLogin(home)
            | Self::ImportCurrent(home)
            | Self::ApplyDirect(home)
            | Self::RestoreConfig(home) => Some(home),
            Self::Control { request, .. } => request.codex_home(),
            _ => None,
        }
    }

    /// Commands that stay available while an old journal blocks the database upgrade.
    pub(super) fn allowed_during_recovery(&self) -> bool {
        matches!(
            self,
            Self::PollRouteStatus
                | Self::InitializeClients
                | Self::ClientNavigation { .. }
                | Self::InspectClaude(_)
                | Self::InspectConfig(_)
                | Self::RestoreConfig(_)
                | Self::Control { .. }
                | Self::Quit
        )
    }
}

/// The API upstream form. Discovered models picked for a new upstream are
/// saved with it.
pub(super) struct ProviderForm {
    pub id: String,
    pub name: String,
    pub url: String,
    pub model: String,
    pub key: String,
    pub options: CodexOptions,
    pub icon_id: String,
    pub picked: Vec<String>,
    pub directory: Option<Vec<app::ConnectionModel>>,
    /// Connection-wide User-Agent, header JSON and body JSON drafts.
    pub request_overrides: (String, String, String),
}

pub(super) struct SubscriptionForm {
    pub id: String,
    pub name: String,
    pub account_id: String,
    pub auth: Option<Secret>,
    pub options: CodexOptions,
    pub icon_id: String,
    pub directory: Option<Vec<app::ConnectionModel>>,
}

/// The single model mapping editor.
pub(super) struct ModelForm {
    pub input_modalities: Option<switchx::catalog::InputModalities>,
    pub request_overrides: (String, String, String),
    pub provider: String,
    pub original_id: String,
    pub public_id: String,
    pub name: String,
    pub upstream_model: String,
    pub path: String,
    pub context: String,
    pub levels: String,
    pub default_reasoning: String,
}

/// A batch addition from the model picker.
pub(super) struct BatchForm {
    pub input_modalities: Option<switchx::catalog::InputModalities>,
    pub request_overrides: (String, String, String),
    pub provider: String,
    /// Upstream model, row context window, row reasoning levels, row input capabilities.
    pub models: Vec<(
        String,
        String,
        String,
        Option<switchx::catalog::InputModalities>,
    )>,
    pub context: String,
    pub levels: String,
    pub default_reasoning: String,
}

/// `account-action` codes for saved ChatGPT accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AccountAction {
    Refresh,
    Login,
    SetDefault,
    /// Writes the account into the Codex entry login.
    Activate,
    Remove,
    ImportCurrent,
}

impl AccountAction {
    /// Handled on the UI thread without the worker.
    pub(super) const REFRESH_QUOTA: i32 = 6;
    pub(super) const TICK_QUOTA: i32 = 7;

    pub(super) fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Refresh,
            1 => Self::Login,
            2 => Self::SetDefault,
            3 => Self::Activate,
            4 => Self::Remove,
            5 => Self::ImportCurrent,
            _ => return None,
        })
    }
}

/// `xai-account-action` codes for saved Grok accounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum XaiAccountAction {
    Refresh,
    Login,
    SetDefault,
    CancelLogin,
    Remove,
}

impl XaiAccountAction {
    /// Handled on the UI thread without the worker.
    pub(super) const REFRESH_QUOTA: i32 = 5;
    pub(super) const TICK_QUOTA: i32 = 6;

    pub(super) fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::Refresh,
            1 => Self::Login,
            2 => Self::SetDefault,
            3 => Self::CancelLogin,
            4 => Self::Remove,
            _ => return None,
        })
    }
}

/// `subscription-action` codes for the native Codex login and routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubscriptionAction {
    AddConnection,
    /// Restores managed configuration, then starts the official browser login.
    Login,
    CheckLogin,
    CancelLogin,
    /// Prepares an API-only route preview after leaving subscription routing.
    SwitchToApi,
}

impl SubscriptionAction {
    pub(super) fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::AddConnection,
            1 => Self::Login,
            2 => Self::CheckLogin,
            3 => Self::CancelLogin,
            4 => Self::SwitchToApi,
            _ => return None,
        })
    }
}

pub(super) fn send_error(error: &TrySendError<Command>) -> &'static str {
    match error {
        TrySendError::Full(_) => "操作仍在执行，请稍后重试",
        TrySendError::Closed(_) => "后台状态通道已停止",
    }
}

/// Marks the window busy and hands the command to the worker.
pub(super) fn queue(app: &AppWindow, sender: &mpsc::Sender<Command>, command: Command) {
    let account_removal = matches!(
        &command,
        Command::Account {
            action: AccountAction::Remove,
            ..
        } | Command::XaiAccount {
            action: XaiAccountAction::Remove,
            ..
        }
    );
    app.set_connection_models_saving(matches!(&command, Command::SaveConnectionModels { .. }));
    app.set_busy(true);
    if let Err(error) = sender.try_send(command) {
        app.set_connection_models_saving(false);
        app.set_busy(false);
        if account_removal {
            app.set_account_removing_id("".into());
        }
        show_action(app, Err(send_error(&error).into()));
    }
}

/// Wires a window callback that queues one command built from the window state.
pub(super) fn queue_on(
    app: &AppWindow,
    sender: &mpsc::Sender<Command>,
    build: impl Fn(&AppWindow) -> Command + 'static,
) -> impl Fn() + 'static {
    let sender = sender.clone();
    let weak = app.as_weak();
    move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &sender, build(&app));
        }
    }
}
