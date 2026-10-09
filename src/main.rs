use switchx::ui::{
    AccountRow, AppWindow, CodeSpan, ModelRow, ProviderIconRow, ProviderPresetRow, ProviderRow,
    RequestRow, SwitchXTray, SyntaxHighlighting, Theme,
};

#[cfg(target_os = "macos")]
mod macos;

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    accounts::AccountManager,
    app::{self, AppError, Snapshot, data_directory, load_snapshot},
    catalog, chatgpt, client, code_highlight, config_transaction,
    credentials::Secret,
    direct,
    direct_config::{self, PreparedDirectSwitch},
    provider_config::{self, CodexOptions},
    provider_icons,
    routed::RouteSession,
    storage::{AccountBinding, ProviderKind, ProviderRecord, Store},
};
use tokio::sync::{
    mpsc::{self, error::TrySendError},
    watch,
};

enum Command {
    Refresh(bool),
    RefreshRequests,
    PollRouteStatus,
    Save {
        id: String,
        name: String,
        url: String,
        model: String,
        key: String,
        options: CodexOptions,
        icon_id: String,
    },
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
    BeginXaiEditor(String),
    SaveXai {
        id: String,
        name: String,
        account_id: String,
    },
    XaiAccount {
        action: i32,
        id: String,
    },
    SaveSubscription {
        id: String,
        name: String,
        account_id: String,
        auth: Secret,
        options: CodexOptions,
        icon_id: String,
    },
    OpenCommonConfig(String),
    ExtractCommonConfig(String),
    SaveCommonConfig(String),
    Delete(String),
    Check {
        id: String,
        home: String,
        check_id: String,
    },
    FetchModels {
        scope: i32,
        generation: i32,
        provider: String,
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
        action: i32,
        home: String,
        port: String,
    },
    Account {
        action: i32,
        id: String,
        home: String,
    },
    SaveModel {
        provider: String,
        original_id: String,
        public_id: String,
        name: String,
        upstream_model: String,
        path: String,
        context: String,
        levels: String,
        default_reasoning: String,
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
    LaunchCodex {
        home: String,
    },
    CancelRoutePreview,
    Quit,
}

struct SubscriptionAuthDraft {
    provider_id: String,
    binding: AccountBinding,
    account_id: Option<String>,
    contents: Secret,
    home: PathBuf,
}

fn subscription_binding(account_id: &str) -> AccountBinding {
    match account_id {
        "" => AccountBinding::Native,
        "@default" => AccountBinding::Default,
        id => AccountBinding::Fixed(id.into()),
    }
}

fn subscription_auth_draft(
    data_dir: &Path,
    provider_id: &str,
    binding: AccountBinding,
    home: &Path,
) -> Result<SubscriptionAuthDraft, String> {
    let manager = AccountManager::open(data_dir)?;
    let account_id = match &binding {
        AccountBinding::Native => None,
        AccountBinding::Default => manager.default_id()?,
        AccountBinding::Fixed(id) => Some(id.clone()),
    };
    let contents = manager.editor_auth(&binding, home)?;
    Ok(SubscriptionAuthDraft {
        provider_id: provider_id.into(),
        binding,
        account_id,
        contents,
        home: home.into(),
    })
}

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
    app.set_busy(false);
    match result {
        Ok(snapshot) => {
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
                    })
                    .collect::<Vec<_>>(),
            )));
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
                    let brand = provider_icon_row(
                        app,
                        resolved_provider_icon_id(
                            provider.kind,
                            &provider.base_url,
                            &provider.icon_id,
                        ),
                    );
                    ProviderRow {
                        check_id: checking
                            .iter()
                            .find(|row| row.id == provider.id)
                            .map(|row| row.check_id)
                            .unwrap_or_default(),
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
                    }
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
            app.set_error_code(error.code().into());
            app.set_error_message(error.message().into());
            app.set_error_action(error.action().into());
            app.set_status_text("上次成功读取的记录仍保留在窗口中".into());
        }
    }
}

fn show_action(app: &AppWindow, result: Result<String, String>) {
    app.set_busy(false);
    show_action_feedback(app, result);
}

fn show_action_feedback(app: &AppWindow, result: Result<String, String>) {
    match result {
        Ok(message) => {
            app.set_action_message(message.into());
            app.set_error_code("".into());
            app.set_error_message("".into());
            app.set_error_action("".into());
        }
        Err(message) => {
            app.set_error_code("operation_failed".into());
            app.set_error_message(message.into());
            app.set_error_action("检查输入或刷新后重试；发生配置冲突时先检查目标文件。".into());
        }
    }
}

struct AccountView {
    rows: Vec<AccountRow>,
    selected: String,
    status: String,
}

fn provider_account_id(provider: &ProviderRecord, default_id: Option<&str>) -> Option<String> {
    if provider.kind != ProviderKind::Chatgpt {
        return None;
    }
    match &provider.account_binding {
        Some(AccountBinding::Fixed(id)) => Some(id.clone()),
        Some(AccountBinding::Default) => default_id.map(str::to_owned),
        _ => None,
    }
}

fn account_provider_names(
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

fn account_view(data_dir: &Path, home: &Path) -> Result<AccountView, String> {
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
                    id: account.id.into(),
                    label: account.label.into(),
                    workspace: account.workspace_id.into(),
                    is_default: account.is_default,
                    requires_reauth: false,
                    bound_provider_count: names.len() as i32,
                    bound_provider_names: names.join("、").into(),
                }
            })
            .collect(),
        selected,
        status,
    })
}

fn show_accounts(app: &AppWindow, view: Result<AccountView, String>) {
    match view {
        Ok(view) => {
            app.set_accounts(ModelRc::new(VecModel::from(view.rows)));
            app.set_selected_account_id(view.selected.into());
            app.set_account_status(view.status.into());
        }
        Err(error) => app.set_account_status(error.into()),
    }
}

fn xai_account_view(data_dir: &Path) -> Result<Vec<AccountRow>, String> {
    let accounts = switchx::xai::AccountManager::open(data_dir)?.list()?;
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
            }
        })
        .collect())
}

fn show_xai_accounts(app: &AppWindow, view: Result<Vec<AccountRow>, String>) {
    match view {
        Ok(rows) => {
            app.set_xai_status(format!("已保存 {} 个 Grok 账号", rows.len()).into());
            app.set_xai_accounts(ModelRc::new(VecModel::from(rows)));
        }
        Err(error) => app.set_xai_status(error.into()),
    }
}

fn show_xai_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    accounts: Vec<switchx::xai::AccountInfo>,
) {
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

fn discard_account_previews(app: &AppWindow) {
    app.set_direct_preview_ready(false);
    app.set_route_preview_ready(false);
}

fn editor_codex_options(app: &AppWindow) -> Result<CodexOptions, String> {
    let options = CodexOptions {
        remote_compaction: app.get_edit_remote_compaction(),
        use_common_config: app.get_edit_use_common_config(),
        context_1m: app.get_edit_context_1m(),
        compact_limit: if app.get_edit_context_1m() {
            app.get_edit_compact_limit()
                .trim()
                .parse()
                .map_err(|_| "压缩阈值须为小于 1000000 的正整数")?
        } else {
            900_000
        },
        config_toml: app
            .get_subscription_editor_open()
            .then(|| app.get_edit_config_preview().to_string()),
    };
    options.validate()?;
    Ok(options)
}

fn set_subscription_config(app: &AppWindow, config: Result<String, String>) {
    match config {
        Ok(config) => {
            let base = CodexOptions {
                use_common_config: app.get_edit_use_common_config(),
                ..CodexOptions::default()
            };
            match provider_config::subscription_options_from_config(&base, &config) {
                Ok(options) => {
                    app.set_config_editor_updating(true);
                    app.set_edit_config_preview(config.into());
                    app.set_edit_context_1m(options.context_1m);
                    app.set_edit_compact_limit(options.compact_limit.to_string().into());
                    app.set_edit_config_error("".into());
                    app.set_config_editor_updating(false);
                }
                Err(error) => app.set_edit_config_error(error.into()),
            }
        }
        Err(error) => app.set_edit_config_error(error.into()),
    }
}

fn update_subscription_common(app: &AppWindow) {
    if app.get_config_editor_updating() || !app.get_subscription_editor_open() {
        return;
    }
    set_subscription_config(
        app,
        provider_config::set_subscription_common(
            app.get_edit_config_preview().as_str(),
            app.get_common_config_saved().as_str(),
            app.get_edit_use_common_config(),
        ),
    );
}

fn update_subscription_context(app: &AppWindow) {
    if app.get_config_editor_updating() || !app.get_subscription_editor_open() {
        return;
    }
    let result = (|| {
        let limit = if app.get_edit_context_1m() {
            app.get_edit_compact_limit()
                .trim()
                .parse()
                .map_err(|_| "压缩阈值须为小于 1000000 的正整数")?
        } else {
            900_000
        };
        provider_config::set_subscription_context(
            app.get_edit_config_preview().as_str(),
            app.get_edit_context_1m(),
            limit,
        )
    })();
    set_subscription_config(app, result);
}

fn update_provider_config_preview(app: &AppWindow) {
    if app.get_subscription_editor_open() {
        set_subscription_config(app, Ok(app.get_edit_config_preview().to_string()));
        return;
    }
    if !app.get_editor_open() {
        return;
    }
    let result = (|| {
        let options = editor_codex_options(app)?;
        let helper = std::env::current_exe().map_err(|_| "无法定位 SwitchX 凭据程序")?;
        provider_config::preview(
            app.get_edit_name().as_str(),
            app.get_edit_url().as_str(),
            app.get_edit_model().as_str(),
            app.get_edit_id().as_str(),
            &helper,
            &options,
            app.get_common_config_saved().as_str(),
        )
    })();
    match result {
        Ok(preview) => {
            app.set_edit_config_preview(preview.into());
            app.set_edit_config_error("".into());
        }
        Err(error) => {
            app.set_edit_config_error(error.into());
            app.set_edit_config_preview("".into());
        }
    }
}

fn replace_common_config(app: &AppWindow, common: String) {
    let previous = app.get_common_config_saved();
    let draft = if app.get_subscription_editor_open()
        && app.get_edit_use_common_config()
        && previous.as_str() != common
    {
        Some(
            provider_config::set_subscription_common(
                app.get_edit_config_preview().as_str(),
                previous.as_str(),
                false,
            )
            .and_then(|config| {
                provider_config::subscription_editor_config(
                    "",
                    &CodexOptions {
                        config_toml: Some(config),
                        ..CodexOptions::default()
                    },
                    &common,
                )
            }),
        )
    } else {
        None
    };
    app.set_common_config_saved(common.into());
    if let Some(draft) = draft {
        set_subscription_config(app, draft);
    } else {
        update_provider_config_preview(app);
    }
}

fn resolved_provider_icon_id(kind: ProviderKind, base_url: &str, icon_id: &str) -> &'static str {
    if let Some(icon) = provider_icons::icon(icon_id) {
        icon.id
    } else {
        provider_icons::default_icon_id(kind, base_url)
    }
}

fn provider_icon_row(app: &AppWindow, id: &str) -> ProviderIconRow {
    app.get_provider_icons()
        .iter()
        .find(|row| row.id == id)
        .unwrap_or_default()
}

fn set_editor_provider_icon(app: &AppWindow, icon_id: &str, kind: ProviderKind, base_url: &str) {
    let override_id = provider_icons::icon(icon_id).map_or("", |icon| icon.id);
    let row = provider_icon_row(app, resolved_provider_icon_id(kind, base_url, override_id));
    app.set_edit_icon_id(override_id.into());
    app.set_edit_icon_name(row.name);
    app.set_edit_icon(row.icon);
    app.set_edit_icon_monochrome(row.monochrome);
}

fn update_editor_provider_icon(app: &AppWindow) {
    set_editor_provider_icon(
        app,
        &app.get_edit_icon_id(),
        if app.get_subscription_editor_open() {
            ProviderKind::Chatgpt
        } else {
            ProviderKind::ApiKey
        },
        &app.get_edit_url(),
    );
}

fn filter_provider_icons(app: &AppWindow, query: &str) {
    let matches = provider_icons::search(query);
    let rows = app
        .get_provider_icons()
        .iter()
        .filter(|row| matches.iter().any(|icon| row.id == icon.id))
        .collect::<Vec<_>>();
    app.set_filtered_provider_icons(ModelRc::new(VecModel::from(rows)));
}

fn initialize_provider_icons(app: &AppWindow) -> Result<(), slint::LoadImageError> {
    let icons = provider_icons::PROVIDER_ICONS
        .iter()
        .map(|icon| {
            provider_icons::load_image(icon).map(|image| ProviderIconRow {
                id: icon.id.into(),
                name: icon.name.into(),
                icon: image,
                monochrome: icon.monochrome,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let icons = ModelRc::new(VecModel::from(icons));
    app.set_provider_icons(icons.clone());
    app.set_filtered_provider_icons(icons);
    Ok(())
}

fn connect_provider_icon_editor(app: &AppWindow) {
    let weak = app.as_weak();
    app.on_update_provider_icon(move || {
        if let Some(app) = weak.upgrade() {
            update_editor_provider_icon(&app);
        }
    });
    let weak = app.as_weak();
    app.on_open_provider_icon_picker(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_config_managed()
            || (!app.get_editor_open() && !app.get_subscription_editor_open())
        {
            return;
        }
        app.set_provider_icon_query("".into());
        filter_provider_icons(&app, "");
        app.set_icon_picker_open(true);
    });
    let weak = app.as_weak();
    app.on_filter_provider_icons(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_provider_icons(&app, &query);
        }
    });
    let weak = app.as_weak();
    app.on_choose_provider_icon(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_config_managed()
            || !app.get_icon_picker_open()
            || (!id.is_empty() && provider_icons::icon(&id).is_none())
        {
            return;
        }
        app.set_edit_icon_id(id);
        update_editor_provider_icon(&app);
    });
    let weak = app.as_weak();
    app.on_close_provider_icon_picker(move || {
        if let Some(app) = weak.upgrade() {
            app.set_icon_picker_open(false);
            app.set_provider_icon_query("".into());
        }
    });
}

fn apply_provider_preset(app: &AppWindow, id: &str) {
    if app.get_busy() || app.get_config_managed() || !app.get_edit_id().is_empty() {
        return;
    }
    let preset = app::PROVIDER_PRESETS.iter().find(|preset| preset.id == id);
    if preset.is_none() && !id.is_empty() {
        return;
    }
    let brand = app
        .get_provider_presets()
        .iter()
        .find(|preset| preset.id == id)
        .unwrap_or_default();
    app.set_edit_key("".into());
    app.set_edit_preset_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_name(preset.map_or("", |preset| preset.name).into());
    app.set_edit_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_model(preset.map_or("", |preset| preset.model_id).into());
    app.set_edit_preset_id(brand.id);
    app.set_edit_preset_icon(brand.icon);
    app.set_edit_preset_monochrome(brand.monochrome);
    set_editor_provider_icon(app, "", ProviderKind::ApiKey, &app.get_edit_url());
}

fn show_provider_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    options: CodexOptions,
    common: String,
) {
    let selected_preset = (provider.is_none() && app.get_connection_picker_open())
        .then(|| app.get_connection_preset_id());
    app.set_editor_open(false);
    app.set_subscription_editor_open(false);
    app.set_xai_editor_open(false);
    app.set_common_config_editor_open(false);
    app.set_icon_picker_open(false);
    app.set_common_config_saved(common.into());
    let preset = provider
        .as_ref()
        .and_then(|provider| app::provider_preset(&provider.base_url));
    let brand = app
        .get_provider_presets()
        .iter()
        .find(|row| preset.is_some_and(|preset| row.id == preset.id))
        .unwrap_or_default();
    app.set_edit_id(
        provider
            .as_ref()
            .map_or("", |provider| provider.id.as_str())
            .into(),
    );
    app.set_edit_name(
        provider
            .as_ref()
            .map_or("", |provider| provider.name.as_str())
            .into(),
    );
    app.set_edit_preset_url(preset.map_or("", |preset| preset.base_url).into());
    app.set_edit_url(
        provider
            .as_ref()
            .map_or("", |provider| provider.base_url.as_str())
            .into(),
    );
    app.set_edit_model(
        provider
            .as_ref()
            .map_or("", |provider| provider.model_id.as_str())
            .into(),
    );
    app.set_edit_key("".into());
    app.set_edit_preset_id(brand.id);
    app.set_edit_preset_icon(brand.icon);
    app.set_edit_preset_monochrome(brand.monochrome);
    set_editor_provider_icon(
        app,
        provider
            .as_ref()
            .and_then(|provider| provider.icon_id.as_deref())
            .unwrap_or(""),
        ProviderKind::ApiKey,
        provider
            .as_ref()
            .map_or("", |provider| provider.base_url.as_str()),
    );
    app.set_edit_remote_compaction(options.remote_compaction);
    app.set_edit_use_common_config(options.use_common_config);
    app.set_edit_context_1m(options.context_1m);
    app.set_edit_compact_limit(options.compact_limit.to_string().into());
    app.set_edit_config_error("".into());
    app.set_delete_confirm(false);
    app.set_busy(false);
    app.set_editor_open(true);
    app.set_connection_picker_open(false);
    if let Some(preset) = selected_preset {
        apply_provider_preset(app, &preset);
    }
    update_provider_config_preview(app);
}

fn close_subscription_editor(app: &AppWindow) {
    app.set_subscription_editor_generation(
        app.get_subscription_editor_generation().wrapping_add(1),
    );
    app.set_subscription_editor_pending(false);
    app.set_subscription_editor_open(false);
    app.set_subscription_auth_json("".into());
    app.set_subscription_auth_error("".into());
    app.set_common_config_editor_open(false);
    app.set_icon_picker_open(false);
}

fn show_subscription_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    accounts: Vec<switchx::accounts::AccountInfo>,
    config: String,
    options: CodexOptions,
    common: String,
    auth: Secret,
) {
    app.set_subscription_editor_open(false);
    app.set_icon_picker_open(false);
    app.set_config_editor_updating(true);
    let mut ids = vec![slint::SharedString::default()];
    let mut account_options = vec![slint::SharedString::from("跟随 Codex 登录")];
    let selected = provider
        .as_ref()
        .and_then(|provider| match &provider.account_binding {
            Some(AccountBinding::Fixed(id)) => Some(id.as_str()),
            _ => None,
        });
    let mut choice = 0;
    if provider
        .as_ref()
        .is_some_and(|provider| provider.account_binding == Some(AccountBinding::Default))
    {
        ids.push("@default".into());
        account_options.push("跟随默认保存账号".into());
        choice = 1;
    }
    for account in accounts {
        if selected == Some(account.id.as_str()) {
            choice = ids.len() as i32;
        }
        ids.push(account.id.into());
        account_options.push(format!("{} · {}", account.label, account.workspace_id).into());
    }
    app.set_subscription_id(
        provider
            .as_ref()
            .map_or("", |provider| provider.id.as_str())
            .into(),
    );
    app.set_subscription_name(
        provider
            .as_ref()
            .map_or("ChatGPT 订阅", |provider| provider.name.as_str())
            .into(),
    );
    set_editor_provider_icon(
        app,
        provider
            .as_ref()
            .and_then(|provider| provider.icon_id.as_deref())
            .unwrap_or(""),
        ProviderKind::Chatgpt,
        chatgpt::BASE_URL,
    );
    app.set_subscription_binding_label(
        match provider
            .as_ref()
            .and_then(|provider| provider.account_binding.as_ref())
        {
            Some(AccountBinding::Fixed(_)) if choice == 0 => {
                "原绑定账号已移除；请明确选择其他账号".into()
            }
            Some(AccountBinding::Native) | None if provider.is_some() => {
                "跟随所选 Codex 目录的登录；也可选择保存账号或粘贴完整 auth.json。".into()
            }
            Some(AccountBinding::Default) => {
                "现有绑定：跟随默认账号。选择已保存账号后才会改为固定绑定。".into()
            }
            _ => "选择保存账号时显示其登录 JSON；修改身份请添加其他账号后再改绑。".into(),
        },
    );
    app.set_subscription_account_ids(ModelRc::new(VecModel::from(ids)));
    app.set_subscription_account_options(ModelRc::new(VecModel::from(account_options)));
    app.set_subscription_account_choice(choice);
    app.set_editor_open(false);
    app.set_xai_editor_open(false);
    app.set_edit_key("".into());
    app.set_common_config_editor_open(false);
    app.set_model_editor_open(false);
    app.set_delete_confirm(false);
    app.set_common_config_saved(common.into());
    app.set_edit_remote_compaction(false);
    app.set_edit_use_common_config(options.use_common_config);
    app.set_edit_context_1m(options.context_1m);
    app.set_edit_compact_limit(options.compact_limit.to_string().into());
    app.set_edit_config_preview(config.into());
    app.set_edit_config_error("".into());
    app.set_subscription_auth_json(auth.expose().into());
    app.set_subscription_auth_error("".into());
    app.set_config_editor_updating(false);
    app.set_subscription_editor_open(true);
    app.set_connection_picker_open(false);
    app.set_active_page(1);
    app.set_busy(false);
}

fn open_provider_link(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法打开系统浏览器，请检查默认浏览器设置".into())
}

fn queue(app: &AppWindow, sender: &mpsc::Sender<Command>, command: Command) {
    app.set_busy(true);
    if let Err(error) = sender.try_send(command) {
        app.set_busy(false);
        let message = match error {
            TrySendError::Full(_) => "操作仍在执行，请稍后重试",
            TrySendError::Closed(_) => "后台状态通道已停止",
        };
        show_action(app, Err(message.into()));
    }
}

fn connect_provider_checks(app: &AppWindow, sender: &mpsc::Sender<Command>) {
    let sender = sender.clone();
    let weak = app.as_weak();
    app.on_check_provider(move |id| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy()
            || app.get_recovery_only()
            || !app
                .get_providers()
                .iter()
                .any(|row| row.id == id && row.check_id.is_empty())
        {
            return;
        }
        let check_id = match app::new_id() {
            Ok(id) => id,
            Err(error) => {
                show_action_feedback(&app, Err(error));
                return;
            }
        };
        set_provider_check_id(&app, &id, &check_id);
        if let Err(error) = sender.try_send(Command::Check {
            id: id.to_string(),
            home: app.get_config_home().into(),
            check_id: check_id.clone(),
        }) {
            let message = match error {
                TrySendError::Full(_) => "操作仍在执行，请稍后重试",
                TrySendError::Closed(_) => "后台状态通道已停止",
            };
            finish_provider_check(&app, &id, &check_id, Err(message.into()));
        }
    });
}

fn set_provider_check_id(app: &AppWindow, id: &str, check_id: &str) {
    let rows = app
        .get_providers()
        .iter()
        .map(|mut row| {
            if row.id == id {
                row.check_id = check_id.into();
            }
            row
        })
        .collect::<Vec<_>>();
    app.set_providers(ModelRc::new(VecModel::from(rows)));
    filter_providers(app, &app.get_provider_query());
}

fn finish_provider_check(
    app: &AppWindow,
    id: &str,
    check_id: &str,
    result: Result<String, String>,
) {
    if app
        .get_providers()
        .iter()
        .any(|row| row.id == id && row.check_id == check_id)
    {
        set_provider_check_id(app, id, "");
        show_action_feedback(app, result);
    }
}

fn home(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    client::config_path(&path)?;
    Ok(path)
}

fn sync_owned_account(data_dir: &Path, home: &Path) -> Result<(), String> {
    let manager = AccountManager::open(data_dir)?;
    // An external/native login has no matching ownership marker. Following it
    // must not adopt that credential into another saved account.
    if manager.active_id(home)?.is_some() {
        manager.sync_current(home)?;
    }
    Ok(())
}

fn ensure_accounts_editable(data_dir: &Path) -> Result<(), String> {
    if data_dir.join("direct-journal.json").exists()
        || data_dir.join("switch-journal.json").exists()
    {
        return Err("请先恢复并停止路由，再修改默认账号或移除账号".into());
    }
    Ok(())
}

fn ensure_account_unbound(data_dir: &Path, account_id: &str) -> Result<(), String> {
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

fn common_config_for_home(data_dir: &Path, target: &str) -> Result<String, String> {
    match home(target).and_then(|target| app::initialize_common_config(data_dir, &target)) {
        Ok(common) => Ok(common),
        Err(error) => {
            eprintln!("未自动提取 Codex 通用配置：{error}");
            app::load_common_config(data_dir)
        }
    }
}

async fn restore_connections(
    session: &mut RouteSession,
    data_dir: &Path,
    target: &Path,
) -> Result<(), String> {
    if config_transaction::recovery(data_dir)?.is_some() {
        session.restore(data_dir, target).await?;
    } else if direct_config::active_target(data_dir)?.is_some() {
        let result = direct_config::restore(&client::config_path(target)?, data_dir)?;
        if !result.conflicts.is_empty() {
            return Err(format!(
                "配置恢复有冲突：{}；登录操作未开始",
                result.conflicts.join("、")
            ));
        }
    }
    Ok(())
}

fn route_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| "本地端口必须在 1–65535 之间".into())
}

fn show_config_status(app: &AppWindow, status: client::ConfigStatus) {
    app.set_direct_active(status.direct_active);
    app.set_route_managed(status.route_managed);
    app.set_config_status(
        format!("{} · {} · {}", status.mode, status.provider, status.model).into(),
    );
}

fn update_model_display_name(app: &AppWindow) {
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

fn open_model_editor(app: &AppWindow, provider_id: &str, original_id: &str) -> Result<(), String> {
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
        None => format!("sx-{}", app::new_id()?).into(),
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
    app.set_model_editor_open(true);
    app.set_fallback_editor_open(false);
    app.set_editor_open(false);
    app.set_edit_key("".into());
    app.set_active_page(0);
    update_reasoning_options(app);
    Ok(())
}

fn update_reasoning_options(app: &AppWindow) {
    let text = app.get_model_reasoning_levels();
    let levels = catalog::reasoning_levels(&text).unwrap_or_default();
    let mut options = vec![slint::SharedString::from("未设置")];
    options.extend(levels.into_iter().map(slint::SharedString::from));
    if !options.contains(&app.get_model_default_reasoning()) {
        app.set_model_default_reasoning("未设置".into());
    }
    app.set_model_reasoning_options(ModelRc::new(VecModel::from(options)));
}

async fn check_provider(
    data: Result<(PathBuf, ProviderRecord), String>,
    target: &str,
    xai_manager: Option<switchx::xai::AccountManager>,
) -> Result<String, String> {
    let (data_dir, provider) = data?;
    match provider.kind {
        ProviderKind::XaiOAuth => {
            let manager = xai_manager.ok_or("Grok 账号状态不可用")?;
            let account = manager.resolve_binding(
                provider
                    .account_binding
                    .as_ref()
                    .ok_or("Grok 账号绑定缺失")?,
            )?;
            let token = manager.credential(&account.id).await?;
            direct::check_models(&provider, token.expose()).await?;
            Ok(format!(
                "{}：Grok 账号目录已连通；实际推理权限尚未验证",
                provider.name
            ))
        }
        ProviderKind::Chatgpt => {
            let target = home(target)?;
            let manager = AccountManager::open(&data_dir)?;
            let account = manager
                .resolve_binding(
                    provider
                        .account_binding
                        .as_ref()
                        .ok_or("订阅账号绑定缺失")?,
                )
                .await?;
            if let Some(account) = account {
                manager.credential(&account.id, &target).await?;
                Ok(format!(
                    "{}：绑定账号凭据可读取；实际官方请求权限尚未验证",
                    provider.name
                ))
            } else {
                let status = chatgpt::account(&target, false).await?;
                status.require_chatgpt()?;
                Ok(format!("{}：{}", provider.name, status.label()))
            }
        }
        ProviderKind::ApiKey => {
            let token = app::provider_credential(&data_dir, &provider)?;
            direct::check_models(&provider, token.expose()).await?;
            Ok(format!(
                "{}：/models 已连通，目录包含 {}；Responses 工具调用尚未验证",
                provider.name, provider.model_id
            ))
        }
    }
}

async fn worker(
    mut receiver: mpsc::Receiver<Command>,
    weak: slint::Weak<AppWindow>,
    directory: Result<PathBuf, AppError>,
) -> Result<(), String> {
    let mut prepared: Option<(
        PreparedDirectSwitch,
        ProviderRecord,
        app::ProviderConfigState,
    )> = None;
    let mut route_session = RouteSession::default();
    let mut pending_login: Option<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> = None;
    let mut pending_xai_login: Option<(watch::Sender<bool>, tokio::task::JoinHandle<()>)> = None;
    let mut xai_manager: Option<switchx::xai::AccountManager> = None;
    let mut account_home = client::default_home().ok();
    let mut subscription_auth: Option<SubscriptionAuthDraft> = None;
    let mut provider_checks = tokio::task::JoinSet::new();
    while let Some(command) = receiver.recv().await {
        while provider_checks.try_join_next().is_some() {}
        let recovery_only = directory
            .as_ref()
            .ok()
            .map(|path| Store::needs_recovery_before_migration(&path.join("switchx.sqlite")))
            .transpose()
            .map_err(|_| "无法检查数据库升级前的恢复状态")?
            .unwrap_or(false);
        if recovery_only
            && !matches!(
                command,
                Command::PollRouteStatus
                    | Command::InspectConfig(_)
                    | Command::RestoreConfig(_)
                    | Command::Quit
            )
        {
            let check = match &command {
                Command::Check { id, check_id, .. } => Some((id.clone(), check_id.clone())),
                _ => None,
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                if let Some((id, check_id)) = check {
                    finish_provider_check(&app, &id, &check_id, Err("请先恢复原配置".into()));
                }
                app.set_recovery_only(true);
                app.set_loading(false);
                app.set_config_managed(true);
                app.set_active_page(5);
                show_action(
                    &app,
                    Err("旧版资料有配置待恢复；请先恢复原配置，成功后再升级并读取资料。".into()),
                );
            });
            continue;
        }
        let target = match &command {
            Command::Account { home, .. }
            | Command::Subscription { home, .. }
            | Command::InspectRoute { home, .. }
            | Command::ApplyRoute { home, .. }
            | Command::LaunchCodex { home }
            | Command::InspectDirect { home, .. }
            | Command::BeginProviderEditor { home, .. }
            | Command::FetchModels { home, .. }
            | Command::InspectConfig(home)
            | Command::CheckLogin(home)
            | Command::ImportCurrent(home)
            | Command::ApplyDirect(home)
            | Command::RestoreConfig(home) => Some(home),
            _ => None,
        };
        if let Some(target) = target
            && let Ok(home) = home(target)
        {
            account_home = Some(home);
        }
        match command {
            Command::PollRouteStatus => {}
            Command::RefreshRequests => {
                let result = directory
                    .as_ref()
                    .map_err(|error| *error)
                    .and_then(|path| app::load_requests(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(records) => {
                            app.set_requests(ModelRc::new(VecModel::from(
                                records
                                    .into_iter()
                                    .map(|record| RequestRow {
                                        time: record.time.into(),
                                        route: record.route.into(),
                                        timing: record.timing.into(),
                                        duration: record.duration.into(),
                                        detail: record.detail.into(),
                                        status: record.status.label().into(),
                                        completed: record.status
                                            == switchx::storage::RequestStatus::Completed,
                                        cancelled: record.status
                                            == switchx::storage::RequestStatus::Cancelled,
                                        error: record.error.into(),
                                        fallback: record.fallback.into(),
                                    })
                                    .collect::<Vec<_>>(),
                            )));
                            app.set_request_error("".into());
                        }
                        Err(error) => app.set_request_error(
                            format!("请求记录读取失败：{}；保留上次列表", error.message()).into(),
                        ),
                    }
                });
            }
            Command::Refresh(check_credentials) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| *error)
                    .and_then(|path| load_snapshot(path, check_credentials));
                let accounts = directory.as_ref().ok().map(|path| xai_account_view(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_result(&app, result);
                    if let Some(view) = accounts {
                        show_xai_accounts(&app, view);
                    }
                });
            }
            Command::BeginXaiEditor(id) => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|e| e.message())?;
                    app::ensure_editable(data_dir)?;
                    let provider = if id.is_empty() {
                        None
                    } else {
                        let provider = app::load_provider(data_dir, &id)?;
                        switchx::xai::validate_provider(&provider)?;
                        Some(provider)
                    };
                    Ok::<_, String>((
                        provider,
                        switchx::xai::AccountManager::open(data_dir)?.list()?,
                    ))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((provider, accounts)) => show_xai_editor(&app, provider, accounts),
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::SaveXai {
                id,
                name,
                account_id,
            } => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|e| e.message())?;
                    let binding = if account_id == "@default" {
                        AccountBinding::Default
                    } else {
                        AccountBinding::Fixed(account_id)
                    };
                    switchx::xai::save_provider(
                        data_dir,
                        (!id.is_empty()).then_some(id.as_str()),
                        &name,
                        binding,
                    )
                })();
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let accounts = directory.as_ref().ok().map(|path| xai_account_view(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(view) = accounts {
                        show_xai_accounts(&app, view);
                    }
                    if let Some(snapshot) = snapshot {
                        set_provider_check_id(&app, &id, "");
                        show_result(&app, snapshot);
                        app.set_xai_editor_open(false);
                        discard_account_previews(&app);
                    }
                    show_action(
                        &app,
                        result.map(|()| "Grok 连接与账号绑定已保存；在工作台预览并启用路由".into()),
                    );
                });
            }
            Command::XaiAccount { action, id } => {
                let result = async {
                    let data_dir = directory.as_ref().map_err(|e| e.message())?;
                    if action == 3 {
                        if let Some((cancel, task)) = pending_xai_login.take() {
                            cancel.send_replace(true);
                            let _ = task.await;
                        }
                        let _ = weak.upgrade_in_event_loop(|app| {
                            app.set_xai_pending(false);
                            app.set_xai_user_code("".into());
                            app.set_xai_login_url("".into());
                            app.set_xai_login_id("".into());
                        });
                        return Ok("Grok 登录已取消".into());
                    }
                    if pending_xai_login
                        .as_ref()
                        .is_some_and(|(_, task)| !task.is_finished())
                    {
                        return Err("请先完成或取消 Grok 登录".into());
                    }
                    let manager = match &xai_manager {
                        Some(manager) => manager.clone(),
                        None => {
                            let manager = switchx::xai::AccountManager::open(data_dir)?;
                            xai_manager = Some(manager.clone());
                            manager
                        }
                    };
                    match action {
                        0 => Ok("已刷新 Grok 账号列表".into()),
                        1 => {
                            app::ensure_editable(data_dir)?;
                            let login = manager.start_login().await?;
                            open_provider_link(&login.verification_url)?;
                            let login_id = app::new_id()?;
                            let ui_id = login_id.clone();
                            let url = login.verification_url.clone();
                            let code = login.user_code.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_xai_login_id(ui_id.into());
                                app.set_xai_pending(true);
                                app.set_xai_login_url(url.into());
                                app.set_xai_user_code(code.into());
                            });
                            let (cancel, receiver) = watch::channel(false);
                            let window = weak.clone();
                            let data_dir = data_dir.clone();
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
                                    show_xai_accounts(&app, view);
                                    match result {
                                        Ok(account) => app.set_action_message(
                                            format!(
                                                "Grok 账号 {} 已保存，可绑定到 Grok 上游",
                                                account.label
                                            )
                                            .into(),
                                        ),
                                        Err(error) => {
                                            app.set_xai_status(error.clone().into());
                                            app.set_action_message(error.into());
                                        }
                                    }
                                    discard_account_previews(&app);
                                });
                            });
                            pending_xai_login = Some((cancel, task));
                            Ok("已打开 xAI 官方页面，等待验证码授权".into())
                        }
                        2 => {
                            app::ensure_editable(data_dir)?;
                            manager.set_default(&id).await?;
                            prepared = None;
                            route_session.discard_preview();
                            Ok("默认 Grok 账号已更新，请重新预览路由".into())
                        }
                        4 => {
                            app::ensure_editable(data_dir)?;
                            let rows = xai_account_view(data_dir)?;
                            if rows
                                .iter()
                                .any(|a| a.id == id.as_str() && a.bound_provider_count > 0)
                            {
                                return Err("Grok 账号仍被上游引用，请先重新绑定或删除连接".into());
                            }
                            manager.remove(&id).await?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ = weak.upgrade_in_event_loop(|app| {
                                app.set_delete_account_confirm(false);
                                app.set_delete_account_id("".into());
                            });
                            Ok("Grok 账号已移除".into())
                        }
                        _ => Err("未知 Grok 账号操作".into()),
                    }
                }
                .await;
                let accounts = directory.as_ref().ok().map(|path| xai_account_view(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(view) = accounts {
                        show_xai_accounts(&app, view);
                    }
                    discard_account_previews(&app);
                    show_action(&app, result);
                });
            }
            Command::BeginProviderEditor { id, home: target } => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let common = common_config_for_home(data_dir, &target)?;
                    let (provider, options) = if id.is_empty() {
                        (None, CodexOptions::for_common(&common)?)
                    } else {
                        let provider = app::load_provider(data_dir, &id)?;
                        if provider.kind != ProviderKind::ApiKey {
                            return Err("请使用订阅连接编辑器修改名称和账号绑定".into());
                        }
                        let state = app::load_provider_config(data_dir, &id)?;
                        let options = state
                            .options
                            .map(Ok)
                            .unwrap_or_else(|| CodexOptions::for_common(&common))?;
                        (Some(provider), options)
                    };
                    Ok::<_, String>((provider, options, common))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((provider, options, common)) => {
                        show_provider_editor(&app, provider, options, common)
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::BeginSubscriptionEditor {
                id,
                home: target,
                generation,
            } => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    if data_dir.join("direct-journal.json").exists()
                        || data_dir.join("switch-journal.json").exists()
                    {
                        return Err("请先恢复原配置再编辑订阅连接".into());
                    }
                    let provider = if id.is_empty() {
                        None
                    } else {
                        let provider = app::load_provider(data_dir, &id)?;
                        if provider.kind != ProviderKind::Chatgpt {
                            return Err("此上游不是 ChatGPT 订阅连接".into());
                        }
                        Some(provider)
                    };
                    let common = common_config_for_home(data_dir, &target)?;
                    let options = match &provider {
                        Some(provider) => {
                            app::load_provider_config(data_dir, &provider.id)?.options
                        }
                        None => None,
                    }
                    .map(Ok)
                    .unwrap_or_else(|| CodexOptions::for_common(&common))?;
                    let model = provider
                        .as_ref()
                        .map_or("", |provider| provider.model_id.as_str());
                    let config =
                        provider_config::subscription_editor_config(model, &options, &common)?;
                    let options =
                        provider_config::subscription_options_from_config(&options, &config)?;
                    let binding = provider
                        .as_ref()
                        .and_then(|provider| provider.account_binding.clone())
                        .unwrap_or(AccountBinding::Native);
                    let draft = subscription_auth_draft(data_dir, &id, binding, &home(&target)?)?;
                    let auth = Secret::new(draft.contents.expose().to_owned());
                    subscription_auth = Some(draft);
                    let accounts = AccountManager::open(data_dir)?.list()?;
                    Ok::<_, String>((provider, accounts, config, options, common, auth))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if app.get_subscription_editor_generation() != generation {
                        return;
                    }
                    app.set_subscription_editor_pending(false);
                    match result {
                        Ok((provider, accounts, config, options, common, auth)) => {
                            show_subscription_editor(
                                &app, provider, accounts, config, options, common, auth,
                            )
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::LoadSubscriptionAuth {
                provider_id,
                account_id,
                home: target,
                generation,
            } => {
                subscription_auth = None;
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    app::ensure_editable(data_dir)?;
                    let draft = subscription_auth_draft(
                        data_dir,
                        &provider_id,
                        subscription_binding(&account_id),
                        &home(&target)?,
                    )?;
                    let auth = Secret::new(draft.contents.expose().to_owned());
                    subscription_auth = Some(draft);
                    Ok::<_, String>(auth)
                })();
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if app.get_subscription_editor_generation() != generation
                        || !app.get_subscription_editor_open()
                    {
                        return;
                    }
                    app.set_subscription_editor_pending(false);
                    app.set_busy(false);
                    match result {
                        Ok(auth) => {
                            app.set_subscription_auth_json(auth.expose().into());
                            app.set_subscription_auth_error("".into());
                        }
                        Err(error) => {
                            app.set_subscription_auth_json("".into());
                            app.set_subscription_auth_error(error.into());
                        }
                    }
                });
            }
            Command::CancelSubscriptionEditor => {
                subscription_auth = None;
                let _ = weak.upgrade_in_event_loop(|app| app.set_busy(false));
            }
            Command::SaveSubscription {
                id,
                name,
                account_id,
                auth,
                options,
                icon_id,
            } => {
                let result = async {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    app::ensure_editable(data_dir)?;
                    chatgpt::validate_subscription_name(&name)?;
                    let mut binding = subscription_binding(&account_id);
                    let draft = subscription_auth
                        .as_ref()
                        .filter(|draft| draft.provider_id == id && draft.binding == binding)
                        .ok_or("账号选择已变化，请重新打开订阅编辑器")?;
                    let models = if id.is_empty() {
                        chatgpt::catalog().await?
                    } else {
                        Vec::new()
                    };
                    let common = app::load_common_config(data_dir)?;
                    chatgpt::validate_subscription_options(
                        data_dir,
                        (!id.is_empty()).then_some(id.as_str()),
                        &models,
                        &options,
                        &common,
                    )?;
                    let changed = serde_json::from_str::<serde_json::Value>(auth.expose()).ok()
                        != serde_json::from_str::<serde_json::Value>(draft.contents.expose()).ok();
                    if changed {
                        AccountManager::validate_editor_auth(auth.expose())?;
                        let manager = AccountManager::open(data_dir)?;
                        if binding == AccountBinding::Default
                            && manager.default_id()? != draft.account_id
                        {
                            return Err("默认账号在编辑期间已变化，请重新打开编辑器".into());
                        }
                        let account = manager.save_editor_auth_if_unchanged(
                            auth.expose(),
                            draft.account_id.as_deref(),
                            draft.contents.expose(),
                            (binding == AccountBinding::Native).then_some(draft.home.as_path()),
                        )?;
                        binding = AccountBinding::Fixed(account.id);
                    }
                    let saved = chatgpt::save_subscription_with_codex_options_and_icon(
                        data_dir,
                        (!id.is_empty()).then_some(id.as_str()),
                        &name,
                        binding,
                        &models,
                        &options,
                        &common,
                        Some(&icon_id),
                    );
                    if changed {
                        saved.map_err(|error| format!("账号凭据已保存，订阅连接尚未保存：{error}"))
                    } else {
                        saved
                    }
                }
                .await;
                if result.is_ok() {
                    subscription_auth = None;
                    prepared = None;
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let accounts = directory
                    .as_ref()
                    .ok()
                    .zip(account_home.as_ref())
                    .map(|(path, home)| account_view(path, home));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        set_provider_check_id(&app, &id, "");
                        show_result(&app, snapshot);
                        app.set_subscription_editor_open(false);
                        discard_account_previews(&app);
                    }
                    if let Some(accounts) = accounts {
                        show_accounts(&app, accounts);
                    }
                    show_action(
                        &app,
                        result.map(|_| {
                            "订阅连接、登录资料与 Codex 配置已保存；下次开启路由时应用".into()
                        }),
                    );
                });
            }
            Command::OpenCommonConfig(target) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| common_config_for_home(path, &target));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(common) => {
                            replace_common_config(&app, common.clone());
                            app.set_common_config_draft(common.into());
                            app.set_common_config_error("".into());
                            app.set_common_config_message("".into());
                            app.set_common_config_current_source(
                                "供应商表单当前 config.toml".into(),
                            );
                            app.set_common_config_editor_open(true);
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::ExtractCommonConfig(form_toml) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::extract_and_save_common_config(path, &form_toml));
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(snippet) => {
                            app.set_common_config_current_source(
                                "供应商表单当前 config.toml".into(),
                            );
                            replace_common_config(&app, snippet.clone());
                            app.set_common_config_draft(snippet.into());
                            app.set_common_config_error("".into());
                            app.set_common_config_message(
                                "已从供应商表单提取并保存通用配置；取消不会撤销这次提取。".into(),
                            );
                            app.set_direct_preview_ready(false);
                            app.set_route_preview_ready(false);
                            show_action(&app, Ok("已从供应商表单提取并保存 Codex 通用配置".into()));
                        }
                        Err(error) => {
                            app.set_common_config_error(error.clone().into());
                            show_action(&app, Err(error));
                        }
                    }
                });
            }
            Command::SaveCommonConfig(snippet) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::save_common_config(path, &snippet));
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    match result {
                        Ok(()) => {
                            replace_common_config(&app, snippet);
                            app.set_common_config_error("".into());
                            app.set_common_config_editor_open(false);
                            app.set_direct_preview_ready(false);
                            app.set_route_preview_ready(false);
                            show_action(&app, Ok("Codex 通用配置已保存；勾选应用通用配置的供应商将在下次切换时使用。".into()));
                        }
                        Err(error) => app.set_common_config_error(error.into()),
                    }
                });
            }
            Command::Save {
                id,
                name,
                url,
                model,
                key,
                options,
                icon_id,
            } => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| {
                        app::save_provider_with_codex_options_and_icon(
                            path,
                            (!id.is_empty()).then_some(id.as_str()),
                            &name,
                            &url,
                            &model,
                            key,
                            &options,
                            Some(&icon_id),
                        )
                    });
                if result.is_ok() {
                    prepared = None;
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    show_action(&app, result.map(|()| "上游与 API Key 已保存到本机".into()));
                    if let Some(snapshot) = snapshot {
                        set_provider_check_id(&app, &id, "");
                        show_result(&app, snapshot);
                        app.set_editor_open(false);
                        app.set_subscription_editor_open(false);
                        app.set_direct_preview_ready(false);
                        app.set_route_preview_ready(false);
                    }
                });
            }
            Command::Delete(id) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| app::delete_provider(path, &id));
                let was_deleted =
                    result.is_ok() || result.as_ref().is_err_and(|error| error.contains("已删除"));
                if was_deleted {
                    prepared = None;
                    route_session.discard_preview();
                }
                let snapshot = directory
                    .as_ref()
                    .ok()
                    .map(|path| load_snapshot(path, false));
                let accounts = directory.as_ref().ok().map(|path| xai_account_view(path));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(view) = accounts {
                        show_xai_accounts(&app, view);
                    }
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        if was_deleted {
                            app.set_editor_open(false);
                            app.set_xai_editor_open(false);
                            app.set_subscription_editor_open(false);
                            app.set_direct_preview_ready(false);
                            app.set_route_preview_ready(false);
                        }
                    }
                    show_action(&app, result.map(|()| "上游已删除".into()));
                });
            }
            Command::Check {
                id,
                home: target,
                check_id,
            } => {
                let data = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|path| {
                        let provider = app::load_provider(path, &id)?;
                        if provider.kind == ProviderKind::XaiOAuth && xai_manager.is_none() {
                            xai_manager = Some(switchx::xai::AccountManager::open(path)?);
                        }
                        Ok((path.clone(), provider))
                    });
                let manager = xai_manager.clone();
                let weak = weak.clone();
                provider_checks.spawn(async move {
                    let result = check_provider(data, &target, manager).await;
                    let _ = weak.upgrade_in_event_loop(move |app| {
                        finish_provider_check(&app, &id, &check_id, result);
                    });
                });
            }
            Command::FetchModels {
                scope,
                generation,
                provider,
                url,
                key,
                home: _,
            } => {
                let result = async {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    if !provider.is_empty() {
                        let selected = app::load_provider(data_dir, &provider)?;
                        if selected.kind == ProviderKind::XaiOAuth {
                            switchx::xai::validate_provider(&selected)?;
                            let manager = switchx::xai::AccountManager::open(data_dir)?;
                            let account = manager
                                .resolve_binding(selected.account_binding.as_ref().unwrap())?;
                            let token = manager.credential(&account.id).await?;
                            return direct::fetch_models(switchx::xai::BASE_URL, token.expose())
                                .await
                                .map(|models| (models, false));
                        }
                    }
                    let is_subscription = !provider.is_empty()
                        && app::load_provider(data_dir, &provider)?.kind == ProviderKind::Chatgpt;
                    if is_subscription {
                        return Ok((
                            chatgpt::catalog()
                                .await?
                                .iter()
                                .filter_map(|model| model["slug"].as_str().map(str::to_owned))
                                .collect::<Vec<_>>(),
                            true,
                        ));
                    }
                    let token = if key.expose().is_empty() {
                        let data_dir = directory.as_ref().map_err(|error| error.message())?;
                        if provider.is_empty() {
                            return Err("请先填写 API 地址和 API Key".into());
                        }
                        app::provider_credential(
                            data_dir,
                            &app::load_provider(data_dir, &provider)?,
                        )?
                    } else {
                        key
                    };
                    direct::fetch_models(&url, token.expose())
                        .await
                        .map(|models| (models, false))
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_busy(false);
                    if generation != app.get_discovery_generation()
                        || scope != app.get_discovery_scope()
                        || (scope == 0 && !app.get_editor_open())
                        || (scope == 1 && !app.get_model_editor_open())
                    {
                        return;
                    }
                    app.set_fetching_models(false);
                    match result {
                        Ok((models, is_subscription)) => {
                            app.set_discovery_message(if models.is_empty() {
                                "上游返回空列表，可手动填写模型 ID".into()
                            } else {
                                if is_subscription {
                                    format!(
                                        "已读取 CLI 内置的 {} 个官方模型；账号权限以实际请求为准",
                                        models.len()
                                    )
                                    .into()
                                } else {
                                    format!("已获取 {} 个模型，可从列表选择", models.len()).into()
                                }
                            });
                            app.set_fetched_models(ModelRc::new(VecModel::from(
                                models
                                    .into_iter()
                                    .map(slint::SharedString::from)
                                    .collect::<Vec<_>>(),
                            )));
                        }
                        Err(error) => app.set_discovery_message(error.into()),
                    }
                });
            }
            Command::InspectDirect { id, home: target } => {
                route_session.discard_preview();
                let _ = weak.upgrade_in_event_loop(|app| app.set_route_preview_ready(false));
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let provider = app::load_provider(data_dir, &id)?;
                    app::provider_credential(data_dir, &provider)?;
                    let provider = app::load_provider(data_dir, &id)?;
                    let target = client::config_path(&home(&target)?)?;
                    let helper = std::env::current_exe()
                        .map_err(|_| "无法定位 SwitchX credential helper")?;
                    let state = app::load_provider_config(data_dir, &id)?;
                    let prepared =
                        PreparedDirectSwitch::inspect(&target, data_dir, &provider, &helper)?;
                    let prepared = if let Some(options) = &state.options {
                        prepared.with_codex_options(options, &state.common)?
                    } else if !state.common.is_empty() {
                        prepared.with_common_config(&state.common)?
                    } else {
                        prepared
                    };
                    Ok::<_, String>((prepared, provider, state))
                })();
                match result {
                    Ok((switch, provider, state)) => {
                        let summary = format!(
                            "目标：{} · 模型：{} · 受管变更：{}",
                            provider.name,
                            provider.model_id,
                            switch.changes.join("、")
                        );
                        prepared = Some((switch, provider, state));
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            app.set_busy(false);
                            app.set_direct_preview(summary.into());
                            app.set_direct_preview_ready(true);
                            app.set_action_message("差异已准备；应用前将再次检查上游目录".into());
                        });
                    }
                    Err(error) => {
                        prepared = None;
                        let _ = weak.upgrade_in_event_loop(move |app| {
                            app.set_direct_preview_ready(false);
                            show_action(&app, Err(error));
                        });
                    }
                }
            }
            Command::ApplyDirect(target) => {
                let result = match prepared.take() {
                    Some((switch, selected, selected_config)) => {
                        let checked = (|| {
                            let target_path = client::config_path(&home(&target)?)?;
                            if target_path != switch.target() {
                                return Err("配置目录已变化，请重新预览".into());
                            }
                            let data_dir = directory.as_ref().map_err(|error| error.message())?;
                            let latest = app::load_provider(data_dir, &selected.id)?;
                            if latest != selected
                                || app::load_provider_config(data_dir, &selected.id)?
                                    != selected_config
                            {
                                return Err("上游资料已变化，请重新预览".into());
                            }
                            let token = app::provider_credential(data_dir, &latest)?;
                            Ok::<_, String>((data_dir.clone(), latest, token))
                        })();
                        match checked {
                            Ok((data_dir, provider, token)) => {
                                match direct::check_models(&provider, token.expose()).await {
                                    Ok(()) => switch.apply().map(|()| {
                                        let status =
                                            client::inspect(Path::new(&target), &data_dir).ok();
                                        (
                                            "直连配置已写入；请重启目标 Codex 客户端后验证实际请求"
                                                .to_owned(),
                                            status,
                                        )
                                    }),
                                    Err(error) => Err(error),
                                }
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => Err("请先预览直连变更".into()),
                };
                let _ = weak.upgrade_in_event_loop(move |app| {
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
            Command::RestoreConfig(target) => {
                prepared = None;
                let result = async {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let target_home = home(&target)?;
                    if config_transaction::recovery(data_dir)?.is_some() {
                        route_session.restore(data_dir, &target_home).await?;
                    } else {
                        route_session.discard_preview();
                        let result =
                            direct_config::restore(&client::config_path(&target_home)?, data_dir)?;
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
                let fallback_status = directory
                    .as_ref()
                    .ok()
                    .and_then(|path| client::inspect(Path::new(&target), path).ok());
                let restored_data = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|data_dir| {
                        let _ = common_config_for_home(data_dir, &target);
                        (
                            load_snapshot(data_dir, false),
                            home(&target).map(|target| account_view(data_dir, &target)),
                        )
                    });
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_ready(false);
                    if let Some(status) = fallback_status { show_config_status(&app, status); }
                    match result {
                        Ok(status) => {
                            app.set_recovery_only(false);
                            show_config_status(&app, status);
                            show_action(&app, Ok("原配置的受管字段已恢复，本地路由已停止；请重启目标 Codex 客户端".into()));
                            if let Some((snapshot, accounts)) = restored_data {
                                show_result(&app, snapshot);
                                if let Ok(accounts) = accounts { show_accounts(&app, accounts); }
                            }
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::InspectConfig(target) => {
                let result = (|| {
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let managed = direct_config::active_target(data_dir)?
                        .or(config_transaction::recovery(data_dir)?.map(|route| route.config_path));
                    let target = managed
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                        .unwrap_or(home(&target)?);
                    if !recovery_only {
                        common_config_for_home(data_dir, &target.to_string_lossy())?;
                    }
                    client::inspect(&target, data_dir).map(|status| (status, target))
                })();
                let accounts =
                    result
                        .as_ref()
                        .ok()
                        .filter(|_| !recovery_only)
                        .and_then(|(_, target)| {
                            account_home = Some(target.clone());
                            directory
                                .as_ref()
                                .ok()
                                .map(|data_dir| account_view(data_dir, target))
                        });
                let _ = weak.upgrade_in_event_loop(move |app| match result {
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
            Command::ImportCurrent(target) => {
                let result = (|| {
                    let candidate = client::import_candidate(&home(&target)?)?;
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let common = common_config_for_home(data_dir, &target)?;
                    let options = CodexOptions::for_common(&common)?;
                    Ok::<_, String>((candidate, options, common))
                })();
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok((candidate, options, common)) => {
                        let provider = ProviderRecord {
                            id: String::new(),
                            name: candidate.name,
                            base_url: candidate.base_url,
                            model_id: candidate.model_id,
                            credential_ref: None,
                            icon_id: None,
                            kind: ProviderKind::ApiKey,
                            account_binding: None,
                        };
                        show_provider_editor(&app, Some(provider), options, common);
                        app.set_active_page(1);
                        show_action(
                            &app,
                            Ok(
                                "已填入当前上游资料；请输入 API Key 后保存。原配置与凭据未改动"
                                    .into(),
                            ),
                        );
                    }
                    Err(error) => show_action(&app, Err(error)),
                });
            }
            Command::CheckLogin(target) => {
                let result = match home(&target) {
                    Ok(path) => client::login_status(&path).await,
                    Err(error) => Err(error),
                };
                let _ = weak.upgrade_in_event_loop(move |app| match result {
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
            Command::Account {
                action,
                id,
                home: target,
            } => {
                let result: Result<String, String> = async {
                    let target_home = home(&target)?;
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    let manager = AccountManager::open(data_dir)?;
                    if pending_login
                        .as_ref()
                        .is_some_and(|(_, task)| !task.is_finished())
                    {
                        return Err("登录仍在进行；请先完成或取消登录".into());
                    }
                    match action {
                        0 => Ok("已刷新保存的账号列表".into()),
                        1 => {
                            let login_id = app::new_id()?;
                            let login = manager.start_login().await?;
                            open_provider_link(&login.verification_url)?;
                            let code = login.user_code.clone();
                            let url = login.verification_url.clone();
                            let ui_id = login_id.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_chatgpt_login_id(ui_id.into());
                                app.set_chatgpt_login_pending(true);
                                app.set_login_user_code(code.into());
                                app.set_login_url(url.into());
                                app.set_account_status("正在等待官方设备验证码登录…".into());
                            });
                            let (cancel, receiver) = watch::channel(false);
                            let window = weak.clone();
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
                                            app.set_action_message(error.into());
                                        }
                                    }
                                });
                            });
                            pending_login = Some((cancel, task));
                            Ok("已打开官方登录页；完成验证码登录后保存账号".into())
                        }
                        2 => {
                            ensure_accounts_editable(data_dir)?;
                            manager.set_default(&id)?;
                            route_session.discard_preview();
                            let _ = weak
                                .upgrade_in_event_loop(|app| app.set_route_preview_ready(false));
                            Ok("已设为默认账号；固定绑定保持不变，旧的跟随默认账号连接会使用此默认选择".into())
                        }
                        3 => {
                            let use_default = id.is_empty();
                            let id = if use_default {
                                manager.default_id()?.ok_or("请先添加或设置默认账号")?
                            } else {
                                id.clone()
                            };
                            restore_connections(&mut route_session, data_dir, &target_home).await?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ =
                                weak.upgrade_in_event_loop(|app| discard_account_previews(&app));
                            manager.activate(&id, &target_home).await?;
                            let _ = weak.upgrade_in_event_loop(|app| {
                                app.set_auth_status("已设置 Codex 入口登录；各上游绑定独立管理".into());
                            });
                            Ok("账号已写入 Codex 入口登录；各上游绑定保持不变，请重新发布并启动新会话".into())
                        }
                        4 => {
                            ensure_accounts_editable(data_dir)?;
                            ensure_account_unbound(data_dir, &id)?;
                            prepared = None;
                            route_session.discard_preview();
                            manager.remove(&id, &target_home).await?;
                            let removed_id = id.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_route_preview_ready(false);
                                app.set_delete_account_confirm(false);
                                app.set_delete_account_id("".into());
                                if app.get_selected_account_id() == removed_id.as_str() {
                                    app.set_auth_status("请重新选择账号并检查登录".into());
                                }
                            });
                            Ok("账号已移除；固定绑定此账号的请求不会自动改用其他账号".into())
                        }
                        5 => {
                            let account = manager.import_current(&target_home)?;
                            Ok(format!(
                                "已保存此 Codex 目录的账号 {}；当前登录保持不变",
                                account.label
                            ))
                        }
                        _ => Err("未知账号操作".into()),
                    }
                }
                .await;
                let accounts = directory.as_ref().ok().and_then(|data_dir| {
                    home(&target).ok().map(|home| account_view(data_dir, &home))
                });
                let status = directory.as_ref().ok().and_then(|data_dir| {
                    home(&target)
                        .ok()
                        .and_then(|home| client::inspect(&home, data_dir).ok())
                });
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                    }
                    if let Some(accounts) = accounts {
                        show_accounts(&app, accounts);
                    }
                    if let Some(status) = status {
                        show_config_status(&app, status);
                    }
                    show_action(&app, result);
                });
            }
            Command::Subscription {
                action,
                home: target,
                port,
            } => {
                let result: Result<String, String> = async {
                    let target_home = home(&target)?;
                    let data_dir = directory.as_ref().map_err(|error| error.message())?;
                    if pending_login
                        .as_ref()
                        .is_some_and(|(_, task)| !task.is_finished())
                        && action != 3
                    {
                        return Err("登录仍在进行；可先完成或取消登录".into());
                    }
                    match action {
                        0 => {
                            let models = chatgpt::catalog().await?;
                            chatgpt::save_connection(data_dir, &models)?;
                            route_session.discard_preview();
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                show_result(&app, Ok(snapshot));
                                app.set_route_preview_ready(false);
                                app.set_connections_tab(1);
                                app.set_active_page(1);
                            });
                            Ok("已添加订阅连接与 CLI 内置模型资料；请登录后预览发布".to_owned())
                        }
                        1 => {
                            // Reauthentication is an explicit restore-then-login operation.
                            restore_connections(&mut route_session, data_dir, &target_home).await?;
                            sync_owned_account(data_dir, &target_home)?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            let login =
                                chatgpt::Session::start(&target_home).await?.login().await?;
                            open_provider_link(&login.url)?;
                            let (cancel, receiver) = watch::channel(false);
                            let id = app::new_id()?;
                            let login_id = id.clone();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_chatgpt_login_id(id.into());
                                app.set_chatgpt_login_pending(true);
                                app.set_login_user_code("".into());
                                app.set_login_url("".into());
                                app.set_auth_status("正在等待官方浏览器登录…".into());
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            let window = weak.clone();
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
                            pending_login = Some((cancel, task));
                            Ok("已打开官方登录页；原配置已恢复，可在此取消登录".to_owned())
                        }
                        2 => {
                            let manager = AccountManager::open(data_dir)?;
                            if let Some(id) = manager.active_id(&target_home)? {
                                manager.refresh(&id, &target_home).await?;
                            }
                            let status = chatgpt::account(&target_home, true).await?;
                            sync_owned_account(data_dir, &target_home)?;
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_auth_status(status.label().into())
                            });
                            status.require_chatgpt()?;
                            Ok(
                                "已检查订阅登录并同步续期凭据；活跃会话也可由 Codex 自动续期"
                                    .to_owned(),
                            )
                        }
                        3 => {
                            if let Some((cancel, _)) = &pending_login {
                                cancel.send_replace(true);
                            }
                            Ok("已请求取消官方登录".to_owned())
                        }
                        4 => {
                            let port = route_port(&port)?;
                            let helper =
                                std::env::current_exe().map_err(|_| "无法定位 SwitchX 程序")?;
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
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
                            restore_connections(&mut route_session, data_dir, &target_home).await?;
                            prepared = None;
                            route_session.discard_preview();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_direct_preview_ready(false);
                                app.set_route_preview_ready(false);
                            });
                            chatgpt::deselect_models(data_dir)?;
                            let summary = route_session
                                .prepare(data_dir, &target_home, port, &model, &helper)
                                .await;
                            let snapshot =
                                load_snapshot(data_dir, false).map_err(|error| error.message())?;
                            let ready = summary.is_ok();
                            let preview = summary.as_ref().ok().cloned().unwrap_or_default();
                            let _ = weak.upgrade_in_event_loop(move |app| {
                                app.set_route_preview_model(model.as_str().into());
                                show_result(&app, Ok(snapshot));
                                app.set_default_model(model.into());
                                app.set_route_preview(preview.into());
                                app.set_route_preview_ready(ready);
                                app.set_active_page(0);
                                app.set_publish_drawer_open(true);
                            });
                            summary?;
                            Ok("已准备 API 路由预览；点击“确认启用”，无需订阅登录".to_owned())
                        }
                        _ => Err("未知订阅操作".into()),
                    }
                }
                .await;
                let status = home(&target).ok().and_then(|path| {
                    directory
                        .as_ref()
                        .ok()
                        .and_then(|data_dir| client::inspect(&path, data_dir).ok())
                });
                let accounts = directory.as_ref().ok().and_then(|data_dir| {
                    home(&target).ok().map(|home| account_view(data_dir, &home))
                });
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(accounts) = accounts {
                        show_accounts(&app, accounts);
                    }
                    if let Some(status) = status {
                        show_config_status(&app, status);
                    }
                    if matches!(action, 1 | 2)
                        && let Err(error) = &result
                    {
                        app.set_auth_status(error.as_str().into());
                    }
                    show_action(&app, result);
                });
            }
            Command::SaveModel {
                provider,
                original_id,
                public_id,
                name,
                upstream_model,
                path,
                context,
                levels,
                default_reasoning,
            } => {
                let result = async {
                    let directory = directory
                        .as_ref()
                        .map_err(|error| error.message().to_owned())?;
                    let keep_imported_reasoning =
                        !path.trim().is_empty() && levels.trim().is_empty();
                    let input = app::ModelInput {
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
                    if app::load_provider(directory, &provider)?.kind == ProviderKind::Chatgpt {
                        chatgpt::save_mapping(directory, input).await
                    } else {
                        app::save_mapping(directory, input)
                    }
                }
                .await;
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        app.set_model_editor_open(false);
                        app.set_route_preview_ready(false);
                    }
                    show_action(
                        &app,
                        result.map(|()| {
                            "模型映射已保存；预览发布后可开启路由并刷新 Codex 模型菜单".into()
                        }),
                    );
                });
            }
            Command::DeleteModel(public_id) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| app::delete_model(directory, &public_id));
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
                    if let Some(snapshot) = snapshot {
                        show_result(&app, snapshot);
                        app.set_model_editor_open(false);
                        app.set_route_preview_ready(false);
                    }
                    show_action(&app, result.map(|()| "模型映射已删除".into()));
                });
            }
            Command::SaveFallback(provider, fallback) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| {
                        app::save_fallback(directory, &provider, fallback.as_deref())
                    });
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
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
            Command::SelectModels(public_ids, enabled) => {
                let result = directory
                    .as_ref()
                    .map_err(|error| error.message().to_owned())
                    .and_then(|directory| app::select_models(directory, &public_ids, enabled));
                if result.is_ok() {
                    route_session.discard_preview();
                }
                let snapshot = result
                    .as_ref()
                    .ok()
                    .and_then(|_| directory.as_ref().ok())
                    .map(|path| load_snapshot(path, false));
                let _ = weak.upgrade_in_event_loop(move |app| {
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
            Command::InspectRoute {
                home: target,
                port,
                model,
            } => {
                prepared = None;
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    let target = home(&target)?;
                    let helper = std::env::current_exe().map_err(|_| "无法定位 SwitchX 程序")?;
                    route_session
                        .prepare(directory, &target, route_port(&port)?, &model, &helper)
                        .await
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_direct_preview_ready(false);
                    app.set_route_preview_model(model.as_str().into());
                    app.set_route_preview_ready(result.is_ok());
                    match result {
                        Ok(summary) => {
                            app.set_route_preview(summary.into());
                            show_action(
                                &app,
                                Ok(
                                    "发布预览已准备；确认启用后点击“启动 Codex”，在新会话选择模型"
                                        .into(),
                                ),
                            );
                        }
                        Err(error) => show_action(&app, Err(error)),
                    }
                });
            }
            Command::ApplyRoute {
                home: target,
                port,
                model,
            } => {
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    route_session
                        .apply(directory, &home(&target)?, route_port(&port)?, &model)
                        .await
                }
                .await;
                let status = directory
                    .as_ref()
                    .ok()
                    .and_then(|path| client::inspect(Path::new(&target), path).ok());
                let _ = weak.upgrade_in_event_loop(move |app| {
                    app.set_route_preview_ready(false);
                    if let Some(status) = status {
                        show_config_status(&app, status);
                    }
                    show_action(
                        &app,
                        result.map(|()| {
                            "模型路由已开启，目录和配置已发布；点击“启动 Codex”后在新会话选择模型".into()
                        }),
                    );
                });
            }
            Command::LaunchCodex { home: target } => {
                let result = async {
                    let directory = directory.as_ref().map_err(|error| error.message())?;
                    let path = route_session
                        .codex_launcher(directory, &home(&target)?)
                        .await?;
                    client::open_codex_launcher(&path)?;
                    Ok("已打开新的 Codex 会话；输入 /model 选择已发布模型".into())
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| show_action(&app, result));
            }
            Command::CancelRoutePreview => {
                route_session.discard_preview();
                let _ = weak.upgrade_in_event_loop(|app| {
                    app.set_route_preview_ready(false);
                    app.set_route_preview("".into());
                    show_action(&app, Ok("已取消发布预览".into()));
                });
            }
            Command::Quit => {
                while provider_checks.join_next().await.is_some() {}
                if let Some((cancel, task)) = pending_xai_login.take() {
                    cancel.send_replace(true);
                    let _ = task.await;
                }
                if let Some(manager) = &xai_manager {
                    manager.wait_for_idle().await;
                }
                if let Some((cancel, task)) = pending_login.take() {
                    cancel.send_replace(true);
                    let _ = task.await;
                }
                let result = async {
                    if let Ok(directory) = &directory
                        && let Some(recovery) = config_transaction::recovery(directory)?
                    {
                        route_session
                            .restore(directory, recovery.config_path.parent().unwrap())
                            .await?;
                    }
                    // Recovery pauses and stops new requests before waiting for
                    // any detached refresh to save its rotated credentials.
                    route_session.wait_for_accounts().await?;
                    if !recovery_only
                        && let (Ok(directory), Some(home)) = (&directory, &account_home)
                    {
                        AccountManager::open(directory)?.wait_for_idle().await?;
                        sync_owned_account(directory, home)?;
                    }
                    route_session.drain_for_exit().await?;
                    Ok::<_, String>(())
                }
                .await;
                let _ = weak.upgrade_in_event_loop(move |app| match result {
                    Ok(()) => {
                        let _ = slint::quit_event_loop();
                    }
                    Err(error) => {
                        let _ = app.show();
                        app.set_active_page(5);
                        show_action(&app, Err(format!("退出前恢复未完成：{error}")));
                    }
                });
            }
        }
        let running = route_session.is_running();
        let recording_failed = route_session.recording_failed();
        let chatgpt_error = route_session.chatgpt_error();
        let provider_errors = directory
            .as_ref()
            .ok()
            .filter(|_| !recovery_only)
            .and_then(|path| Store::open_read_only(&path.join("switchx.sqlite")).ok())
            .and_then(|store| store.providers().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|provider| provider.kind == ProviderKind::Chatgpt)
            .map(|provider| {
                let error = route_session
                    .chatgpt_error_for(&provider.id)
                    .unwrap_or("")
                    .to_owned();
                (provider.id, error)
            })
            .collect::<Vec<_>>();
        let address = route_session
            .address()
            .map(|address| format!("http://{address}/v1"))
            .unwrap_or_default();
        let route_managed = directory
            .as_ref()
            .is_ok_and(|path| path.join("switch-journal.json").exists());
        let managed = route_managed
            || directory
                .as_ref()
                .is_ok_and(|path| path.join("direct-journal.json").exists());
        let _ = weak.upgrade_in_event_loop(move |app| {
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
            if recording_failed {
                app.set_request_error("部分请求记录写入失败；请检查数据库权限和磁盘空间".into());
            }
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
    while provider_checks.join_next().await.is_some() {}
    if let Some((cancel, task)) = pending_xai_login {
        cancel.send_replace(true);
        let _ = task.await;
    }
    if let Some(manager) = xai_manager {
        manager.wait_for_idle().await;
    }
    if let Some((cancel, task)) = pending_login {
        cancel.send_replace(true);
        let _ = task.await;
    }
    let recovery_only = directory
        .as_ref()
        .ok()
        .map(|path| Store::needs_recovery_before_migration(&path.join("switchx.sqlite")))
        .transpose()
        .map_err(|_| "无法检查退出前的恢复状态")?
        .unwrap_or(false);
    // Also recover when the platform exits the event loop without using our tray.
    if let Ok(directory) = &directory
        && let Some(recovery) = config_transaction::recovery(directory)?
    {
        route_session
            .restore(directory, recovery.config_path.parent().unwrap())
            .await?;
    }
    route_session.wait_for_accounts().await?;
    if !recovery_only && let (Ok(directory), Some(home)) = (&directory, &account_home) {
        AccountManager::open(directory)?.wait_for_idle().await?;
        sync_owned_account(directory, home)?;
    }
    route_session.drain_for_exit().await?;
    Ok(())
}

fn credential_command() -> Result<bool, Box<dyn std::error::Error>> {
    let Some(secret) = credential_from_args(std::env::args_os().skip(1))? else {
        return Ok(false);
    };
    println!("{}", secret.expose());
    Ok(true)
}

fn credential_from_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Option<Secret>, Box<dyn std::error::Error>> {
    let Some(command) = args.next() else {
        return Ok(None);
    };
    if command != "credential" && command != "local-token" {
        return Err("unknown SwitchX command".into());
    }
    let reference = args.next().ok_or("credential reference is missing")?;
    let data_dir = PathBuf::from(
        args.next()
            .ok_or("credential data directory is missing; regenerate the SwitchX configuration")?,
    );
    if args.next().is_some() {
        return Err("unexpected credential command argument".into());
    }
    let reference = reference
        .to_str()
        .ok_or("credential reference is invalid")?;
    if !data_dir.is_absolute() {
        return Err("credential data directory must be absolute".into());
    }
    if command == "local-token"
        && !reference
            .strip_prefix("router-")
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err("local token reference is invalid".into());
    }
    let database = data_dir.join("switchx.sqlite");
    let directory_metadata = std::fs::symlink_metadata(&data_dir)
        .map_err(|_| "local token data directory is unavailable")?;
    let database_metadata =
        std::fs::symlink_metadata(&database).map_err(|_| "local token database is unavailable")?;
    if !directory_metadata.is_dir()
        || directory_metadata.file_type().is_symlink()
        || !database_metadata.is_file()
        || database_metadata.file_type().is_symlink()
    {
        return Err("credential storage must be a regular directory and database".into());
    }
    let store = Store::open_credentials_read_only(&database)?;
    let secret = if command == "credential" {
        store
            .provider_api_key(reference)?
            .ok_or("upstream API key is missing")?
    } else {
        store
            .local_token(reference)?
            .ok_or("local token is missing")?
    };
    Ok(Some(secret))
}

fn connect_syntax_highlighting(app: &AppWindow) {
    app.global::<SyntaxHighlighting>()
        .on_spans(|source, language| {
            let spans = code_highlight::spans(source.as_str(), language.as_str())
                .into_iter()
                .map(|span| {
                    use code_highlight::TokenKind;
                    let kind = match span.kind {
                        TokenKind::Plain => 0,
                        TokenKind::Key => 1,
                        TokenKind::String => 2,
                        TokenKind::Number => 3,
                        TokenKind::Literal => 4,
                        TokenKind::Comment => 5,
                        TokenKind::Section => 6,
                    };
                    CodeSpan {
                        text: span.text.into(),
                        prefix: span.prefix.into(),
                        line_text: span.line_text.into(),
                        line: span.line,
                        kind,
                    }
                })
                .collect::<Vec<_>>();
            ModelRc::new(VecModel::from(spans))
        });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if credential_command()? {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    macos::configure_window()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let app = AppWindow::new()?;
    #[cfg(target_os = "macos")]
    {
        app.set_native_titlebar_overlay(true);
        app.global::<Theme>()
            .set_system_reduced_motion(macos::prefers_reduced_motion());
        let weak = app.as_weak();
        app.on_refresh_system_appearance(move || {
            if let Some(app) = weak.upgrade() {
                app.global::<Theme>()
                    .set_system_reduced_motion(macos::prefers_reduced_motion());
            }
        });
    }
    connect_syntax_highlighting(&app);
    let presets = app::PROVIDER_PRESETS
        .iter()
        .map(|preset| {
            slint::Image::load_from_svg_data(preset.icon).map(|icon| ProviderPresetRow {
                id: preset.id.into(),
                name: preset.name.into(),
                icon,
                monochrome: preset.monochrome,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    app.set_provider_presets(ModelRc::new(VecModel::from(presets)));
    initialize_provider_icons(&app)?;
    let tray = SwitchXTray::new()?;
    #[cfg(target_os = "macos")]
    {
        // Slint defers native tray creation until its change handlers run.
        slint::platform::update_timers_and_animations();
        if let Err(error) = macos::use_template_tray_icon("switchx") {
            eprintln!("SwitchX menu bar appearance: {error}");
        }
    }
    let window = app.as_weak();
    tray.on_show_app(move || {
        if let Some(app) = window.upgrade() {
            let _ = app.show();
        }
    });
    let directory = data_directory();
    app.set_data_path(
        directory
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "不可用".into())
            .into(),
    );
    app.set_config_home(
        client::default_home()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into(),
    );
    let (sender, receiver) = mpsc::channel::<Command>(8);
    let background = runtime.spawn(worker(receiver, app.as_weak(), directory));
    let window = app.as_weak();
    let restore_sender = sender.clone();
    tray.on_restore_config(move || {
        if let Some(app) = window.upgrade() {
            let _ = app.show();
            queue(
                &app,
                &restore_sender,
                Command::RestoreConfig(app.get_config_home().to_string()),
            );
        }
    });
    let quit_sender = sender.clone();
    let weak = app.as_weak();
    tray.on_quit_app(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &quit_sender, Command::Quit);
        }
    });
    #[cfg(target_os = "macos")]
    {
        let quit_sender = sender.clone();
        let weak = app.as_weak();
        macos::install_quit_handler(move || {
            if let Some(app) = weak.upgrade() {
                queue(&app, &quit_sender, Command::Quit);
            }
        })?;
    }
    let weak = app.as_weak();
    let refresh_sender = sender.clone();
    app.on_refresh(move |check_credentials| {
        if let Some(app) = weak.upgrade() {
            app.set_loading(true);
            queue(&app, &refresh_sender, Command::Refresh(check_credentials));
        }
    });
    let weak = app.as_weak();
    let request_sender = sender.clone();
    app.on_refresh_requests(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &request_sender, Command::RefreshRequests);
        }
    });
    let weak = app.as_weak();
    app.on_filter_providers(move |query| {
        if let Some(app) = weak.upgrade() {
            filter_providers(&app, &query);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_provider_editor(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::BeginProviderEditor {
                    id: id.into(),
                    home: app.get_config_home().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_xai_editor(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::BeginXaiEditor(id.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_xai(move || {
        if let Some(app) = weak.upgrade() {
            let account_id = app
                .get_xai_account_ids()
                .row_data(app.get_xai_account_choice().max(0) as usize)
                .map(|v| v.to_string())
                .unwrap_or_default();
            queue(
                &app,
                &callback_sender,
                Command::SaveXai {
                    id: app.get_xai_provider_id().into(),
                    name: app.get_xai_provider_name().into(),
                    account_id,
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_xai_account_action(move |action, id| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::XaiAccount {
                    action,
                    id: id.into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_begin_subscription_editor(move |id| {
        if let Some(app) = weak.upgrade() {
            let generation = app.get_subscription_editor_generation().wrapping_add(1);
            app.set_subscription_editor_generation(generation);
            app.set_subscription_editor_pending(true);
            queue(
                &app,
                &callback_sender,
                Command::BeginSubscriptionEditor {
                    id: id.into(),
                    home: app.get_config_home().into(),
                    generation,
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_subscription(move |id, name, account_id| {
        if let Some(app) = weak.upgrade() {
            let options = match editor_codex_options(&app) {
                Ok(options) => options,
                Err(error) => {
                    app.set_edit_config_error(error.into());
                    return;
                }
            };
            queue(
                &app,
                &callback_sender,
                Command::SaveSubscription {
                    id: id.into(),
                    name: name.into(),
                    account_id: account_id.into(),
                    auth: Secret::new(app.get_subscription_auth_json().to_string()),
                    options,
                    icon_id: app.get_edit_icon_id().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_cancel_subscription_editor(move || {
        if let Some(app) = weak.upgrade() {
            close_subscription_editor(&app);
            let _ = callback_sender.try_send(Command::CancelSubscriptionEditor);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_load_subscription_auth(move || {
        if let Some(app) = weak.upgrade() {
            let generation = app.get_subscription_editor_generation().wrapping_add(1);
            app.set_subscription_editor_generation(generation);
            app.set_subscription_editor_pending(true);
            let account_id = app
                .get_subscription_account_ids()
                .row_data(app.get_subscription_account_choice() as usize)
                .unwrap_or_default();
            app.set_subscription_binding_label(
                match account_id.as_str() {
                    "" => "跟随所选 Codex 目录的登录；修改 JSON 后将保存为独立绑定账号。",
                    "@default" => "跟随默认保存账号；修改 JSON 后固定绑定本次编辑的账号。",
                    _ => "选择保存账号时显示其登录 JSON；修改身份请添加其他账号后再改绑。",
                }
                .into(),
            );
            app.set_subscription_auth_json("".into());
            queue(
                &app,
                &callback_sender,
                Command::LoadSubscriptionAuth {
                    provider_id: app.get_subscription_id().into(),
                    account_id: account_id.into(),
                    home: app.get_config_home().into(),
                    generation,
                },
            );
        }
    });
    let weak = app.as_weak();
    app.on_format_subscription_auth(move || {
        if let Some(app) = weak.upgrade() {
            match AccountManager::format_editor_auth(app.get_subscription_auth_json().as_str()) {
                Ok(auth) => {
                    app.set_subscription_auth_json(auth.expose().into());
                    app.set_subscription_auth_error("".into());
                }
                Err(error) => app.set_subscription_auth_error(error.into()),
            }
        }
    });
    let weak = app.as_weak();
    app.on_subscription_auth_edited(move || {
        if let Some(app) = weak.upgrade() {
            app.set_subscription_auth_error(
                AccountManager::validate_editor_auth(app.get_subscription_auth_json().as_str())
                    .err()
                    .unwrap_or_default()
                    .into(),
            );
        }
    });
    let weak = app.as_weak();
    app.on_subscription_config_edited(move || {
        if let Some(app) = weak.upgrade()
            && !app.get_config_editor_updating()
        {
            set_subscription_config(&app, Ok(app.get_edit_config_preview().to_string()));
        }
    });
    let weak = app.as_weak();
    app.on_update_subscription_context(move || {
        if let Some(app) = weak.upgrade() {
            update_subscription_context(&app);
        }
    });
    let weak = app.as_weak();
    app.on_update_subscription_common(move || {
        if let Some(app) = weak.upgrade() {
            update_subscription_common(&app);
        }
    });
    let weak = app.as_weak();
    app.on_update_provider_config_preview(move || {
        if let Some(app) = weak.upgrade() {
            update_provider_config_preview(&app);
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_open_common_config(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::OpenCommonConfig(app.get_config_home().into()),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_extract_common_config(move || {
        if let Some(app) = weak.upgrade() {
            update_provider_config_preview(&app);
            if !app.get_edit_config_error().is_empty() {
                let error = app.get_edit_config_error().to_string();
                app.set_common_config_error(error.clone().into());
                show_action(&app, Err(error));
                return;
            }
            if app.get_edit_config_preview().trim().is_empty() {
                let error = "供应商配置预览为空，请先检查表单内容".to_owned();
                app.set_common_config_error(error.clone().into());
                show_action(&app, Err(error));
                return;
            }
            queue(
                &app,
                &callback_sender,
                Command::ExtractCommonConfig(app.get_edit_config_preview().into()),
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_common_config(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SaveCommonConfig(app.get_common_config_draft().into()),
            );
        }
    });
    let weak = app.as_weak();
    app.on_cancel_common_config(move || {
        if let Some(app) = weak.upgrade() {
            if app.get_busy() {
                return;
            }
            app.set_common_config_draft(app.get_common_config_saved());
            app.set_common_config_error("".into());
            app.set_common_config_message("".into());
            app.set_common_config_editor_open(false);
        }
    });
    connect_provider_icon_editor(&app);
    let weak = app.as_weak();
    app.on_open_provider_preset_link(move |api_key| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.get_busy() {
            return;
        }
        let Some(preset) = app::PROVIDER_PRESETS
            .iter()
            .find(|preset| preset.id == app.get_edit_preset_id().as_str())
        else {
            return;
        };
        let url = if api_key {
            preset.api_key_url
        } else {
            preset.website_url
        };
        show_action(
            &app,
            open_provider_link(url).map(|()| "已在浏览器打开供应商页面".into()),
        );
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_provider(move |id, name, url, model, key| {
        if let Some(app) = weak.upgrade() {
            let options = match editor_codex_options(&app) {
                Ok(options) => options,
                Err(error) => {
                    app.set_edit_config_error(error.into());
                    return;
                }
            };
            queue(
                &app,
                &callback_sender,
                Command::Save {
                    id: id.into(),
                    name: name.into(),
                    url: url.into(),
                    model: model.into(),
                    key: key.into(),
                    options,
                    icon_id: app.get_edit_icon_id().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_delete_provider(move |id| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::Delete(id.into()));
        }
    });
    connect_provider_checks(&app, &sender);
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_fetch_models(move |scope| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let (provider, url, key) = if scope == 0 {
            (
                app.get_edit_id().to_string(),
                app.get_edit_url().to_string(),
                Secret::new(app.get_edit_key().into()),
            )
        } else {
            let Some(provider) = app
                .get_providers()
                .iter()
                .find(|provider| provider.id == app.get_model_provider_id())
            else {
                show_action(&app, Err("请先保存上游连接".into()));
                return;
            };
            (
                provider.id.to_string(),
                provider.base_url.to_string(),
                Secret::new(String::new()),
            )
        };
        app.set_discovery_scope(scope);
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_apply_direct(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::ApplyDirect(app.get_config_home().into()),
            );
        }
    });
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
    app.on_import_current(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::ImportCurrent(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_check_login(move |home| {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::CheckLogin(home.into()));
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_subscription_action(move |action| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::Subscription {
                    action,
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_account_action(move |action, id| {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::Account {
                    action,
                    id: id.into(),
                    home: app.get_config_home().into(),
                },
            );
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_save_model(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::SaveModel {
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
                },
            );
        }
    });
    let weak = app.as_weak();
    app.on_begin_model_editor(move |provider, original_id| {
        if let Some(app) = weak.upgrade()
            && let Err(error) = open_model_editor(&app, &provider, &original_id)
        {
            show_action(&app, Err(error));
        }
    });
    let weak = app.as_weak();
    app.on_add_selected_model(move || {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(provider) = app
            .get_model_provider_ids()
            .row_data(app.get_model_provider_choice() as usize)
            && let Err(error) = open_model_editor(&app, &provider, "")
        {
            show_action(&app, Err(error));
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_delete_model(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::DeleteModel(app.get_model_original_id().into()),
            );
        }
    });
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
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_inspect_route(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::InspectRoute {
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                    model: app.get_default_model().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_apply_route(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::ApplyRoute {
                    home: app.get_config_home().into(),
                    port: app.get_route_port().into(),
                    model: app.get_default_model().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_launch_codex(move || {
        if let Some(app) = weak.upgrade() {
            queue(
                &app,
                &callback_sender,
                Command::LaunchCodex {
                    home: app.get_config_home().into(),
                },
            );
        }
    });
    let callback_sender = sender.clone();
    let weak = app.as_weak();
    app.on_cancel_route_preview(move || {
        if let Some(app) = weak.upgrade() {
            queue(&app, &callback_sender, Command::CancelRoutePreview);
        }
    });
    queue(&app, &sender, Command::Refresh(false));
    queue(
        &app,
        &sender,
        Command::InspectConfig(app.get_config_home().to_string()),
    );
    let weak = app.as_weak();
    app.window().on_close_requested(move || {
        if let Some(app) = weak.upgrade() {
            app.set_icon_picker_open(false);
            if app.get_subscription_editor_open() || app.get_subscription_editor_pending() {
                app.invoke_cancel_subscription_editor();
            }
        }
        slint::CloseRequestResponse::HideWindow
    });
    let result = app.run();
    #[cfg(target_os = "macos")]
    macos::clear_quit_handler();
    drop(app);
    drop(tray);
    drop(sender);
    runtime.block_on(background)??;
    drop(runtime);
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checking_one_provider_keeps_other_connections_interactive() {
        use slint::platform::{
            Platform, PointerEventButton, WindowAdapter, WindowEvent,
            software_renderer::MinimalSoftwareWindow,
        };
        use std::rc::Rc;

        struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for PreviewPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(Default::default());
        slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        app.set_active_page(1);
        let providers = ModelRc::new(VecModel::from(vec![
            ProviderRow {
                id: "synthetic-chatgpt".into(),
                name: "ChatGPT".into(),
                is_subscription: true,
                ..Default::default()
            },
            ProviderRow {
                id: "synthetic-grok".into(),
                name: "Grok".into(),
                is_subscription: true,
                is_grok: true,
                ..Default::default()
            },
        ]));
        app.set_providers(providers.clone());
        app.set_filtered_providers(providers);
        let (sender, mut receiver) = mpsc::channel(8);
        connect_provider_checks(&app, &sender);
        app.on_begin_xai_editor({
            let sender = sender.clone();
            let weak = app.as_weak();
            move |id| {
                queue(
                    &weak.upgrade().unwrap(),
                    &sender,
                    Command::BeginXaiEditor(id.into()),
                )
            }
        });
        app.show().unwrap();
        let draw = || {
            slint::platform::update_timers_and_animations();
            let size = WindowAdapter::size(window.as_ref());
            let mut pixels =
                slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(pixels.make_mut_slice(), size.width as usize);
            });
            if let Some(output) = std::env::var_os("SWITCHX_PROVIDER_CHECK_SNAPSHOTS") {
                use std::io::Write;
                let output = PathBuf::from(output);
                assert!(output.is_absolute());
                std::fs::create_dir_all(&output).unwrap();
                let theme = if app.global::<Theme>().get_dark() {
                    "dark"
                } else {
                    "light"
                };
                let checking = app
                    .get_providers()
                    .iter()
                    .filter(|row| !row.check_id.is_empty())
                    .count();
                let name = format!(
                    "{theme}-{}x{}-{checking}checking-busy{}.ppm",
                    size.width,
                    size.height,
                    app.get_busy()
                );
                let mut file = std::fs::File::create(output.join(name)).unwrap();
                write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
                file.write_all(pixels.as_bytes()).unwrap();
            }
        };
        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
            draw();
        };
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                app.set_error_code("".into());
                app.set_error_message("".into());
                app.set_error_action("".into());
                app.set_action_message("".into());
                draw();
                // Use the real callback to start one check, then click the other row's controls.
                click(width as f32 - 220.0, 374.0);
                assert!(
                    !app.get_busy(),
                    "one provider check must not lock the whole app"
                );
                let Ok(Command::Check { id, check_id, .. }) = receiver.try_recv() else {
                    panic!("the first check must be queued");
                };
                assert_eq!(id, "synthetic-chatgpt");
                let chatgpt_check = check_id;
                app.invoke_check_provider("synthetic-chatgpt".into());
                assert!(
                    receiver.try_recv().is_err(),
                    "duplicate checks must be ignored"
                );
                click(width as f32 - 220.0, 490.0);
                let Ok(Command::Check { id, check_id, .. }) = receiver.try_recv() else {
                    panic!("the other connection must still be checkable");
                };
                assert_eq!(id, "synthetic-grok");
                let grok_check = check_id;
                click(width as f32 - 114.0, 490.0);
                assert!(
                    matches!(receiver.try_recv(), Ok(Command::BeginXaiEditor(id)) if id == "synthetic-grok")
                );
                assert!(app.get_busy());
                finish_provider_check(
                    &app,
                    "synthetic-chatgpt",
                    &chatgpt_check,
                    Ok("合成检查结果".into()),
                );
                assert!(app.get_busy(), "a check must not unlock another operation");
                assert!(!app.get_providers().row_data(1).unwrap().check_id.is_empty());
                app.set_busy(false);
                // Saving and rechecking a connection invalidates its old completion.
                set_provider_check_id(&app, "synthetic-grok", "");
                app.invoke_check_provider("synthetic-grok".into());
                let Ok(Command::Check { check_id, .. }) = receiver.try_recv() else {
                    panic!("a saved connection can be checked again");
                };
                finish_provider_check(&app, "synthetic-grok", &grok_check, Err("过时结果".into()));
                assert_eq!(app.get_providers().row_data(1).unwrap().check_id, check_id);
                finish_provider_check(&app, "synthetic-grok", &check_id, Err("合成失败".into()));
                assert!(app.get_providers().row_data(1).unwrap().check_id.is_empty());
                assert_eq!(app.get_error_message(), "合成失败");
            }
        }
        drop(receiver);
        app.invoke_check_provider("synthetic-grok".into());
        assert!(app.get_providers().row_data(1).unwrap().check_id.is_empty());
        assert_eq!(app.get_error_message(), "后台状态通道已停止");
    }

    #[tokio::test]
    async fn provider_checks_overlap_without_blocking_other_commands() {
        use axum::{Json, Router, routing::get};
        use std::{sync::Arc, time::Duration};
        use tokio::{net::TcpListener, sync::Notify, time::timeout};

        let slow_started = Arc::new(Notify::new());
        let release_slow = Arc::new(Notify::new());
        let fast_started = Arc::new(Notify::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/slow/models",
                        get({
                            let started = slow_started.clone();
                            let release = release_slow.clone();
                            move || {
                                let started = started.clone();
                                let release = release.clone();
                                async move {
                                    started.notify_one();
                                    release.notified().await;
                                    Json(serde_json::json!({"data": [{"id": "synthetic-model"}]}))
                                }
                            }
                        }),
                    )
                    .route(
                        "/fast/models",
                        get({
                            let started = fast_started.clone();
                            move || {
                                let started = started.clone();
                                async move {
                                    started.notify_one();
                                    Json(serde_json::json!({"data": [{"id": "synthetic-model"}]}))
                                }
                            }
                        }),
                    ),
            )
            .into_future(),
        );
        let data = std::env::temp_dir().join(format!("switchx-checks-{}", app::new_id().unwrap()));
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(&data.join("switchx.sqlite")).unwrap();
        for id in ["slow", "fast"] {
            store
                .put_provider_with_models_options_and_key(
                    &ProviderRecord {
                        id: id.into(),
                        name: id.into(),
                        base_url: format!("http://{address}/{id}"),
                        model_id: "synthetic-model".into(),
                        credential_ref: None,
                        kind: ProviderKind::ApiKey,
                        account_binding: None,
                        icon_id: None,
                    },
                    &[],
                    None,
                    &Secret::new("synthetic-key".into()),
                )
                .unwrap();
        }
        drop(store);
        let (sender, receiver) = mpsc::channel(8);
        let background = tokio::spawn(worker(receiver, slint::Weak::default(), Ok(data.clone())));
        sender
            .send(Command::Check {
                id: "slow".into(),
                home: String::new(),
                check_id: "slow-check".into(),
            })
            .await
            .unwrap();
        timeout(Duration::from_secs(2), slow_started.notified())
            .await
            .unwrap();
        sender
            .send(Command::Check {
                id: "fast".into(),
                home: String::new(),
                check_id: "fast-check".into(),
            })
            .await
            .unwrap();
        timeout(Duration::from_secs(2), fast_started.notified())
            .await
            .expect("the second check must start while the first is waiting");
        sender.send(Command::Delete("fast".into())).await.unwrap();
        timeout(Duration::from_secs(2), async {
            while app::load_provider(&data, "fast").is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("other commands must proceed while a check is waiting");
        release_slow.notify_one();
        drop(sender);
        timeout(Duration::from_secs(2), background)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.abort();
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn account_removal_checks_saved_fixed_and_default_provider_references() {
        use base64::Engine;
        let root = std::env::temp_dir().join(format!(
            "switchx-account-removal-{}",
            app::new_id().unwrap()
        ));
        let data = root.join("data");
        let home = root.join("codex");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir(&home).unwrap();
        let auth = home.join("auth.json");
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/synthetic-chatgpt-auth.json"
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

    #[test]
    fn credential_helper_reads_api_key_without_migrating_v8_v9_or_v10() {
        let root =
            std::env::temp_dir().join(format!("switchx-key-helper-{}", app::new_id().unwrap()));
        for version in [8, 9, 10] {
            let data = root.join(format!("v{version}"));
            std::fs::create_dir_all(&data).unwrap();
            let database = data.join("switchx.sqlite");
            let provider = ProviderRecord {
                id: "synthetic-api".into(),
                name: "Synthetic API".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "synthetic-model".into(),
                credential_ref: None,
                icon_id: None,
                kind: ProviderKind::ApiKey,
                account_binding: None,
            };
            {
                let store = Store::open(&database).unwrap();
                store
                    .put_provider_with_models_options_and_key(
                        &provider,
                        &[],
                        None,
                        &Secret::new("synthetic-key".into()),
                    )
                    .unwrap();
            }
            {
                let connection = rusqlite::Connection::open(&database).unwrap();
                connection
                    .pragma_update(None, "user_version", version)
                    .unwrap();
            }
            let before = std::fs::read(&database).unwrap();
            let value = credential_from_args(
                [
                    std::ffi::OsString::from("credential"),
                    provider.id.into(),
                    data.as_os_str().to_owned(),
                ]
                .into_iter(),
            )
            .unwrap()
            .unwrap();
            assert_eq!(value.expose(), "synthetic-key");
            assert_eq!(std::fs::read(&database).unwrap(), before);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn local_token_helper_args(reference: &str, data_dir: &Path) -> [std::ffi::OsString; 3] {
        [
            "local-token".into(),
            reference.into(),
            data_dir.as_os_str().to_owned(),
        ]
    }

    #[test]
    fn local_token_helper_reads_only_its_explicit_database() {
        let root =
            std::env::temp_dir().join(format!("switchx-local-helper-{}", app::new_id().unwrap()));
        let reference = format!("router-{}", "a".repeat(32));
        for (directory, value) in [(root.join("first"), "a"), (root.join("second"), "b")] {
            std::fs::create_dir_all(&directory).unwrap();
            let database = directory.join("switchx.sqlite");
            let token = Secret::new(value.repeat(64));
            {
                let store = Store::open(&database).unwrap();
                store.put_local_token(&reference, &token).unwrap();
            }
            let before = std::fs::read(&database).unwrap();
            let result =
                credential_from_args(local_token_helper_args(&reference, &directory).into_iter())
                    .unwrap()
                    .unwrap();
            assert_eq!(result.expose(), token.expose());
            assert_eq!(std::fs::read(&database).unwrap(), before);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_token_helper_rejects_bad_args_before_opening_a_database() {
        let missing = std::env::temp_dir().join(format!(
            "switchx-local-helper-missing-{}",
            app::new_id().unwrap()
        ));
        let reference = format!("router-{}", "a".repeat(32));
        let valid = local_token_helper_args(&reference, &missing);
        let cases = [
            (valid[..2].to_vec(), "data directory is missing"),
            (
                local_token_helper_args(&reference, Path::new("relative")).to_vec(),
                "data directory must be absolute",
            ),
            (
                valid.clone().into_iter().chain(["extra".into()]).collect(),
                "unexpected credential command argument",
            ),
            (
                local_token_helper_args("../outside", &missing).to_vec(),
                "local token reference is invalid",
            ),
        ];
        for (args, expected) in cases {
            let error = credential_from_args(args.into_iter())
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
            assert!(!missing.exists());
        }
        assert!(credential_from_args(std::iter::empty()).unwrap().is_none());
    }

    #[test]
    fn local_token_helper_does_not_create_or_upgrade_databases() {
        let directory = std::env::temp_dir().join(format!(
            "switchx-local-helper-read-only-{}",
            app::new_id().unwrap()
        ));
        std::fs::create_dir(&directory).unwrap();
        let database = directory.join("switchx.sqlite");
        let reference = format!("router-{}", "a".repeat(32));
        let args = local_token_helper_args(&reference, &directory);
        assert!(credential_from_args(args.clone().into_iter()).is_err());
        assert!(!database.exists());
        drop(Store::open(&database).unwrap());
        let before = std::fs::read(&database).unwrap();
        let error = credential_from_args(args.clone().into_iter())
            .unwrap_err()
            .to_string();
        assert_eq!(error, "local token is missing");
        assert_eq!(std::fs::read(&database).unwrap(), before);
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch("PRAGMA user_version = 7;")
                .unwrap();
        }
        let before = std::fs::read(&database).unwrap();
        assert!(credential_from_args(args.into_iter()).is_err());
        assert_eq!(std::fs::read(&database).unwrap(), before);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_token_helper_rejects_symlinked_storage() {
        let root = std::env::temp_dir().join(format!(
            "switchx-local-helper-symlink-{}",
            app::new_id().unwrap()
        ));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let database = data.join("switchx.sqlite");
        let reference = format!("router-{}", "a".repeat(32));
        {
            let store = Store::open(&database).unwrap();
            store
                .put_local_token(&reference, &Secret::new("a".repeat(64)))
                .unwrap();
        }
        let before = std::fs::read(&database).unwrap();
        let linked_directory = root.join("linked");
        std::os::unix::fs::symlink(&data, &linked_directory).unwrap();
        assert!(
            credential_from_args(
                local_token_helper_args(&reference, &linked_directory).into_iter()
            )
            .is_err()
        );
        let linked_database_directory = root.join("linked-database");
        std::fs::create_dir(&linked_database_directory).unwrap();
        std::os::unix::fs::symlink(&database, linked_database_directory.join("switchx.sqlite"))
            .unwrap();
        assert!(
            credential_from_args(
                local_token_helper_args(&reference, &linked_database_directory).into_iter()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&database).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn embedded_provider_logos_decode() {
        for preset in app::PROVIDER_PRESETS {
            let image = slint::Image::load_from_svg_data(preset.icon).unwrap();
            assert!(image.size().width > 0, "{}", preset.id);
            assert!(image.size().height > 0, "{}", preset.id);
        }
        for icon in provider_icons::PROVIDER_ICONS {
            let image = provider_icons::load_image(icon)
                .unwrap_or_else(|error| panic!("{}: {error}", icon.id));
            assert!(image.size().width > 0, "{}", icon.id);
            assert!(image.size().height > 0, "{}", icon.id);
            assert!(
                image
                    .to_rgba8_premultiplied()
                    .unwrap()
                    .as_slice()
                    .iter()
                    .all(|pixel| {
                        pixel.r <= pixel.a && pixel.g <= pixel.a && pixel.b <= pixel.a
                    }),
                "{}",
                icon.id
            );
        }
    }

    #[test]
    fn unified_connection_picker_routes_choices_and_clears_credentials() {
        use slint::platform::{
            Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
            software_renderer::MinimalSoftwareWindow,
        };
        use std::{cell::Cell, fs, io::Write, rc::Rc};

        struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for PreviewPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(Default::default());
        slint::platform::set_platform(Box::new(PreviewPlatform(window.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        app.global::<Theme>().set_animations_enabled(false);
        app.set_loading(false);
        app.set_active_page(1);
        initialize_provider_icons(&app).unwrap();
        app.set_provider_presets(ModelRc::new(VecModel::from(
            app::PROVIDER_PRESETS
                .iter()
                .map(|preset| ProviderPresetRow {
                    id: preset.id.into(),
                    name: preset.name.into(),
                    icon: slint::Image::load_from_svg_data(preset.icon).unwrap(),
                    monochrome: preset.monochrome,
                })
                .collect::<Vec<_>>(),
        )));
        let calls = Rc::new(Cell::new(0));
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_provider_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_provider_editor(
                &weak.upgrade().unwrap(),
                None,
                CodexOptions::default(),
                String::new(),
            );
        });
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_subscription_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_subscription_editor(
                &weak.upgrade().unwrap(),
                None,
                Vec::new(),
                String::new(),
                CodexOptions::default(),
                String::new(),
                Secret::new(String::new()),
            );
        });
        let weak = app.as_weak();
        let count = calls.clone();
        app.on_begin_xai_editor(move |id| {
            assert!(id.is_empty());
            count.set(count.get() + 1);
            show_xai_editor(&weak.upgrade().unwrap(), None, Vec::new());
        });
        let weak = app.as_weak();
        app.on_cancel_subscription_editor(move || {
            close_subscription_editor(&weak.upgrade().unwrap())
        });
        let click = |x, y| {
            let position = slint::LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
            slint::platform::update_timers_and_animations();
        };
        let escape = || {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: Key::Escape.into(),
            });
            window.dispatch_event(WindowEvent::KeyReleased {
                text: Key::Escape.into(),
            });
            slint::platform::update_timers_and_animations();
        };
        let draw = |name: &str| {
            slint::platform::update_timers_and_animations();
            let size = WindowAdapter::size(window.as_ref());
            let mut pixels =
                slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
            window.request_redraw();
            window.draw_if_needed(|renderer| {
                renderer.render(pixels.make_mut_slice(), size.width as usize);
            });
            if let Some(output) = std::env::var_os("SWITCHX_CONNECTION_SNAPSHOTS") {
                let output = PathBuf::from(output);
                assert!(output.is_absolute());
                fs::create_dir_all(&output).unwrap();
                let mut file = fs::File::create(output.join(format!("{name}.ppm"))).unwrap();
                write!(file, "P6\n{} {}\n255\n", size.width, size.height).unwrap();
                file.write_all(pixels.as_bytes()).unwrap();
            }
        };
        app.show().unwrap();
        for (width, height) in [(1200, 820), (1000, 680)] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            let left = width as f32 - 734.0;
            for dark in [false, true] {
                app.invoke_set_appearance(dark);
                let suffix = format!("{}-{width}x{height}", if dark { "dark" } else { "light" });
                draw(&format!("connections-{suffix}"));
                click(width as f32 - 100.0, 282.0);
                assert!(app.get_connection_picker_open());
                draw(&format!("picker-{suffix}"));
                click(left + 90.0, 200.0);
                assert!(app.get_subscription_editor_open());
                assert!(!app.get_connection_picker_open());
                draw(&format!("chatgpt-{suffix}"));
                app.set_subscription_auth_json("synthetic-unsaved-credential".into());
                click(left + 50.0, 148.0);
                assert!(app.get_connection_picker_open());
                assert!(app.get_subscription_auth_json().is_empty());
                draw("back-from-chatgpt");
                click(left + 440.0, 200.0);
                assert!(app.get_xai_editor_open());
                draw(&format!("grok-{suffix}"));
                click(left + 50.0, 148.0);
                assert!(app.get_connection_picker_open());
                assert!(!app.get_xai_editor_open());
                draw("back-from-grok");
                click(left + 90.0, 320.0);
                assert!(app.get_editor_open());
                assert!(app.get_edit_url().is_empty());
                draw(&format!("custom-api-{suffix}"));
                app.set_edit_key("synthetic-unsaved-key".into());
                escape();
                assert!(!app.get_editor_open());
                assert!(app.get_edit_key().is_empty());
                for (index, preset) in app::PROVIDER_PRESETS.iter().enumerate() {
                    app.invoke_open_connection_picker();
                    draw("before-api-preset");
                    click(
                        left + 90.0 + (index % 2) as f32 * 350.0,
                        410.0 + (index / 2) as f32 * 90.0,
                    );
                    assert!(app.get_editor_open(), "{}", preset.id);
                    assert_eq!(app.get_edit_preset_id(), preset.id);
                    assert_eq!(app.get_edit_name(), preset.name);
                    assert_eq!(app.get_edit_url(), preset.base_url);
                    assert_eq!(app.get_edit_model(), preset.model_id);
                    if index == 0 {
                        draw(&format!("preset-api-{suffix}"));
                    }
                    app.set_edit_key("synthetic-unsaved-key".into());
                    draw("before-back-from-api");
                    click(left + 50.0, 148.0);
                    assert!(app.get_connection_picker_open());
                    assert!(app.get_edit_key().is_empty());
                }
                escape();
                assert!(!app.get_connection_picker_open());
            }
        }
        app.invoke_open_connection_picker();
        draw("before-disabled-choices");
        let before = calls.get();
        for (busy, managed, pending) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            app.set_busy(busy);
            app.set_config_managed(managed);
            app.set_chatgpt_login_pending(pending);
            draw("disabled-choice");
            click(356.0, 200.0);
            assert_eq!(calls.get(), before);
        }
        app.set_busy(false);
        app.set_config_managed(false);
        app.set_chatgpt_login_pending(false);
        draw("before-scrim-close");
        click(80.0, 400.0);
        assert!(!app.get_connection_picker_open());
        app.invoke_open_connection_picker();
        draw("before-chatgpt-close");
        click(356.0, 200.0);
        app.set_subscription_auth_json("synthetic-unsaved-credential".into());
        escape();
        assert!(!app.get_subscription_editor_open());
        assert!(app.get_subscription_auth_json().is_empty());
        app.invoke_open_connection_picker();
        draw("before-grok-close");
        click(706.0, 200.0);
        draw("grok-before-close");
        click(964.0, 84.0);
        assert!(!app.get_xai_editor_open());
        for (busy, managed) in [(true, false), (false, true)] {
            app.set_busy(busy);
            app.set_config_managed(managed);
            app.invoke_open_connection_picker();
            assert!(!app.get_connection_picker_open());
        }
        app.set_busy(false);
        app.set_config_managed(false);
        app.invoke_open_connection_picker();
        app.set_active_page(5);
        draw("after-navigation");
        assert!(!app.get_connection_picker_open());
    }

    #[test]
    fn provider_avatar_picker_preserves_form_drafts_and_defaults() {
        use slint::platform::{Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow};
        use std::rc::Rc;

        struct PreviewPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for PreviewPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        slint::platform::set_platform(Box::new(PreviewPlatform(MinimalSoftwareWindow::new(
            Default::default(),
        ))))
        .unwrap();
        let app = AppWindow::new().unwrap();
        initialize_provider_icons(&app).unwrap();
        connect_provider_icon_editor(&app);
        app.set_active_page(1);
        // Commit the initial page change to Slint's change trackers, as the
        // native event loop does before the next user interaction.
        slint::platform::update_timers_and_animations();
        show_provider_editor(&app, None, CodexOptions::default(), String::new());
        app.set_edit_name("Custom draft".into());
        app.set_edit_key("synthetic-draft-key".into());
        app.invoke_open_provider_icon_picker();
        assert!(app.get_icon_picker_open());
        assert_eq!(app.get_filtered_provider_icons().row_count(), 110);
        app.invoke_filter_provider_icons("CHATGPT".into());
        assert!(
            app.get_filtered_provider_icons()
                .iter()
                .any(|icon| icon.id == "openai")
        );
        app.invoke_choose_provider_icon("deepseek".into());
        app.invoke_close_provider_icon_picker();
        assert_eq!(app.get_edit_icon_id(), "deepseek");
        assert_eq!(app.get_edit_name(), "Custom draft");
        assert_eq!(app.get_edit_key(), "synthetic-draft-key");
        assert!(!app.get_icon_picker_open());
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("https://example.invalid/icon.svg".into());
        assert_eq!(app.get_edit_icon_id(), "deepseek");
        app.invoke_choose_provider_icon("".into());
        assert!(app.get_edit_icon_id().is_empty());
        assert_eq!(app.get_edit_icon().size().width, 0);
        app.invoke_filter_provider_icons("no-such-provider-icon".into());
        assert_eq!(app.get_filtered_provider_icons().row_count(), 0);

        show_subscription_editor(
            &app,
            None,
            Vec::new(),
            String::new(),
            CodexOptions::default(),
            String::new(),
            Secret::new("{}".into()),
        );
        assert!(app.get_edit_icon_id().is_empty());
        assert_eq!(app.get_edit_icon_name(), "OpenAI");
        assert!(app.get_edit_icon().size().width > 0);
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("kimi".into());
        app.invoke_close_provider_icon_picker();
        assert_eq!(app.get_edit_icon_id(), "kimi");
        assert_eq!(app.get_subscription_auth_json(), "{}");
        app.invoke_open_provider_icon_picker();
        app.invoke_choose_provider_icon("".into());
        assert_eq!(app.get_edit_icon_name(), "OpenAI");

        let weak = app.as_weak();
        app.on_cancel_subscription_editor(move || {
            if let Some(app) = weak.upgrade() {
                close_subscription_editor(&app);
            }
        });
        app.set_active_page(0);
        slint::platform::update_timers_and_animations();
        assert!(!app.get_subscription_editor_open());
        assert!(!app.get_icon_picker_open());
        assert!(app.get_subscription_auth_json().is_empty());

        app.set_active_page(1);
        show_provider_editor(&app, None, CodexOptions::default(), String::new());
        app.set_edit_key("synthetic-unsaved-key".into());
        app.set_active_page(5);
        slint::platform::update_timers_and_animations();
        assert!(!app.get_editor_open());
        assert!(app.get_edit_key().is_empty());

        app.set_config_managed(true);
        app.set_direct_active(true);
        assert_eq!(app.get_connection_label(), "直连已启用");
        app.set_direct_active(false);
        assert_eq!(app.get_connection_label(), "配置待恢复");
        app.set_route_running(true);
        assert_eq!(app.get_connection_label(), "路由已启用");
        app.set_route_running(false);
        app.set_config_managed(false);
        assert_eq!(app.get_connection_label(), "待启用");

        let theme = app.global::<Theme>();
        theme.set_animations_enabled(true);
        assert_eq!(theme.get_drawer_motion(), 340);
        theme.set_system_reduced_motion(true);
        assert_eq!(theme.get_fast(), 0);
        assert_eq!(theme.get_page_motion(), 0);
        assert_eq!(theme.get_drawer_motion(), 0);
        theme.set_system_reduced_motion(false);
        theme.set_animations_enabled(false);
        assert_eq!(theme.get_drawer_motion(), 0);
    }

    #[test]
    fn explicit_provider_avatar_overrides_default_branding() {
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::Chatgpt, chatgpt::BASE_URL, ""),
            "openai"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::Chatgpt, chatgpt::BASE_URL, "deepseek"),
            "deepseek"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::ApiKey, "https://api.deepseek.com", "openai"),
            "openai"
        );
        assert_eq!(
            resolved_provider_icon_id(ProviderKind::ApiKey, "https://api.deepseek.com", ""),
            "deepseek"
        );
        assert_eq!(
            resolved_provider_icon_id(
                ProviderKind::ApiKey,
                "https://example.invalid/v1",
                "unknown"
            ),
            ""
        );
    }
}
