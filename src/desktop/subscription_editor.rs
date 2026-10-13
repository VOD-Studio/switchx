//! The ChatGPT subscription editor, including the authorized auth.json editor.

use std::path::{Path, PathBuf};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use switchx::{
    app,
    chatgpt::{self, accounts::AccountManager},
    codex::{
        self,
        provider_config::{self, CodexOptions},
    },
    credentials::Secret,
    storage::{AccountBinding, ProviderKind, ProviderRecord},
    ui::AppWindow,
};
use tokio::sync::mpsc;

use super::{
    accounts::{account_view, show_accounts},
    codex_config::{clear_previews, codex_home},
    command::{Command, SubscriptionForm, queue},
    common_config::common_config_for_home,
    connection_models::directory_draft,
    feedback::show_action,
    icon_picker::set_editor_provider_icon,
    provider_checks::set_provider_check_id,
    provider_editor::editor_codex_options,
    workbench::{created_connection_id, show_result},
    worker::Worker,
};

pub(super) struct SubscriptionAuthDraft {
    provider_id: String,
    binding: AccountBinding,
    account_id: Option<String>,
    contents: Secret,
    home: PathBuf,
    edited: Option<Secret>,
}

pub(super) fn subscription_binding(account_id: &str) -> AccountBinding {
    match account_id {
        "" => AccountBinding::Native,
        "@default" => AccountBinding::Default,
        id => AccountBinding::Fixed(id.into()),
    }
}

pub(super) fn subscription_auth_draft(
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
        edited: None,
    })
}

pub(super) fn set_subscription_config(app: &AppWindow, config: Result<String, String>) {
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

pub(super) fn update_subscription_common(app: &AppWindow) {
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

pub(super) fn update_subscription_context(app: &AppWindow) {
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

pub(super) fn close_subscription_editor(app: &AppWindow) {
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

pub(super) fn show_subscription_editor(
    app: &AppWindow,
    provider: Option<ProviderRecord>,
    accounts: Vec<chatgpt::accounts::AccountInfo>,
    config: String,
    options: CodexOptions,
    common: String,
    auth: Secret,
) {
    app.invoke_reset_connection_draft();
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

pub(super) fn connect(app: &AppWindow, sender: &mpsc::Sender<Command>) {
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
                    app.set_edit_config_error(error.clone().into());
                    show_action(&app, Err(error));
                    return;
                }
            };
            queue(
                &app,
                &callback_sender,
                Command::SaveSubscription(SubscriptionForm {
                    id: id.into(),
                    name: name.into(),
                    account_id: account_id.into(),
                    auth: (!app.get_connection_models_open())
                        .then(|| Secret::new(app.get_subscription_auth_json().to_string())),
                    options,
                    icon_id: app.get_edit_icon_id().into(),
                    directory: directory_draft(&app),
                }),
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
}

impl Worker {
    pub(super) fn begin_subscription_editor(
        &mut self,
        id: String,
        target: String,
        generation: i32,
    ) {
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            if codex::config_managed(data_dir) {
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
                Some(provider) => app::load_provider_config(data_dir, &provider.id)?.options,
                None => None,
            }
            .map(Ok)
            .unwrap_or_else(|| CodexOptions::for_common(&common))?;
            let model = provider
                .as_ref()
                .map_or("", |provider| provider.model_id.as_str());
            let config = provider_config::subscription_editor_config(model, &options, &common)?;
            let options = provider_config::subscription_options_from_config(&options, &config)?;
            let binding = provider
                .as_ref()
                .and_then(|provider| provider.account_binding.clone())
                .unwrap_or(AccountBinding::Native);
            let draft = subscription_auth_draft(data_dir, &id, binding, &codex_home(&target)?)?;
            let auth = Secret::new(draft.contents.expose().to_owned());
            self.subscription_auth = Some(draft);
            let accounts = AccountManager::open(data_dir)?.list()?;
            Ok::<_, String>((provider, accounts, config, options, common, auth))
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| {
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

    pub(super) fn load_subscription_auth(
        &mut self,
        provider_id: String,
        account_id: String,
        target: String,
        generation: i32,
    ) {
        self.subscription_auth = None;
        let result = (|| {
            let data_dir = self.data_dir.path()?;
            app::ensure_editable(data_dir)?;
            let draft = subscription_auth_draft(
                data_dir,
                &provider_id,
                subscription_binding(&account_id),
                &codex_home(&target)?,
            )?;
            let auth = Secret::new(draft.contents.expose().to_owned());
            self.subscription_auth = Some(draft);
            Ok::<_, String>(auth)
        })();
        let _ = self.weak.upgrade_in_event_loop(move |app| {
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

    pub(super) fn cancel_subscription_editor(&mut self) {
        self.subscription_auth = None;
        let _ = self.weak.upgrade_in_event_loop(|app| app.set_busy(false));
    }

    pub(super) fn subscription_directory_auth(&mut self, auth: Option<Secret>, generation: i32) {
        let contents = self.subscription_auth.as_mut().map(|draft| {
            if let Some(auth) = auth {
                draft.edited = Some(auth);
            }
            Secret::new(
                draft
                    .edited
                    .as_ref()
                    .unwrap_or(&draft.contents)
                    .expose()
                    .to_owned(),
            )
        });
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            app.set_busy(false);
            if generation != app.get_subscription_editor_generation()
                || !app.get_subscription_editor_open()
                || app.get_connection_models_open()
            {
                return;
            }
            if let Some(contents) = contents {
                app.set_subscription_auth_json(contents.expose().into());
            }
        });
    }

    pub(super) async fn save_subscription(&mut self, form: SubscriptionForm) {
        let SubscriptionForm {
            id,
            name,
            account_id,
            auth,
            options,
            icon_id,
            directory,
        } = form;
        let result = async {
            let data_dir = self.data_dir.path()?;
            app::ensure_editable(data_dir)?;
            chatgpt::validate_subscription_name(&name)?;
            let mut binding = subscription_binding(&account_id);
            let draft = self
                .subscription_auth
                .as_ref()
                .filter(|draft| draft.provider_id == id && draft.binding == binding)
                .ok_or("账号选择已变化，请重新打开订阅编辑器")?;
            let models = if id.is_empty() {
                chatgpt::catalog().await?
            } else {
                Vec::new()
            };
            if let Some(directory) = &directory {
                let provider = ProviderRecord {
                    id: "draft".into(),
                    name: name.clone(),
                    base_url: chatgpt::BASE_URL.into(),
                    model_id: String::new(),
                    kind: ProviderKind::Chatgpt,
                    account_binding: Some(binding.clone()),
                    credential_ref: None,
                    icon_id: None,
                };
                app::validate_connection_draft(data_dir, &provider, directory, &models)?;
            }
            let common = app::load_common_config(data_dir)?;
            chatgpt::validate_subscription_options(
                data_dir,
                (!id.is_empty()).then_some(id.as_str()),
                &models,
                &options,
                &common,
            )?;
            let auth = auth
                .as_ref()
                .or(draft.edited.as_ref())
                .unwrap_or(&draft.contents);
            let changed = serde_json::from_str::<serde_json::Value>(auth.expose()).ok()
                != serde_json::from_str::<serde_json::Value>(draft.contents.expose()).ok();
            if changed {
                AccountManager::validate_editor_auth(auth.expose())?;
                let manager = AccountManager::open(data_dir)?;
                if binding == AccountBinding::Default && manager.default_id()? != draft.account_id {
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
            let saved = chatgpt::save_subscription_with_directory(
                data_dir,
                (!id.is_empty()).then_some(id.as_str()),
                &name,
                binding,
                &models,
                &options,
                &common,
                Some(&icon_id),
                directory.as_deref(),
            );
            if changed {
                saved.map_err(|error| format!("账号凭据已保存，订阅连接尚未保存：{error}"))
            } else {
                saved
            }
        }
        .await;
        if result.is_ok() {
            self.subscription_auth = None;
            self.discard_previews();
        }
        let snapshot = self.snapshot_after(&result);
        let accounts = self
            .data_dir
            .ok()
            .zip(self.account_home.as_ref())
            .map(|(path, home)| account_view(path, home));
        let _ = self.weak.upgrade_in_event_loop(move |app| {
            let created_connection = snapshot
                .as_ref()
                .and_then(|snapshot| created_connection_id(&app, snapshot));
            if let Some(snapshot) = snapshot {
                set_provider_check_id(&app, &id, "");
                show_result(&app, snapshot);
                app.set_subscription_editor_open(false);
                app.invoke_reset_connection_draft();
                clear_previews(&app);
            }
            if let Some(accounts) = accounts {
                show_accounts(&app, accounts);
            }
            show_action(
                &app,
                result.map(|_| "订阅连接、登录资料与 Codex 配置已保存；下次开启路由时应用".into()),
            );
            if let Some(id) = created_connection {
                app.invoke_begin_connection_models(id);
            }
        });
    }
}
