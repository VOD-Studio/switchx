//! Native model route lifecycle. Provider keys never enter the generated Codex config.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::net::TcpListener;

use crate::{
    app,
    catalog::{self, Publication},
    chatgpt, client,
    config_transaction::{self, PreparedSwitch},
    credentials::Secret,
    direct,
    routing::{RouterState, RunningRouter, Upstream},
    storage::{AccountBinding, ModelRecord, ProviderKind, ProviderRecord, Store},
};

struct PreparedRoute {
    switch: PreparedSwitch,
    listener: TcpListener,
    address: SocketAddr,
    target: PathBuf,
    state_dir: PathBuf,
    requested_port: u16,
    default_model: String,
    client_version: String,
    cli_path: PathBuf,
    token_reference: String,
    local_token: Secret,
    accounts: HashMap<String, ResolvedAccount>,
    xai_accounts: HashMap<String, String>,
    publication: Publication,
    models: Vec<ModelRecord>,
    providers: Vec<ProviderRecord>,
    config_provider_id: String,
    config_state: app::ProviderConfigState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedAccount {
    id: Option<String>,
    workspace: Option<chatgpt::Workspace>,
}

struct LocalProbe {
    discovery: SocketAddr,
    responses: HashMap<String, SocketAddr>,
}

struct ActiveRoute {
    server: RunningRouter,
    address: SocketAddr,
    target: PathBuf,
    state_dir: PathBuf,
    catalog_path: PathBuf,
    public_models: HashSet<String>,
    cli_path: PathBuf,
    provider_config: Secret,
    token_reference: String,
    uses_chatgpt: bool,
}

#[derive(Default)]
pub struct RouteSession {
    prepared: Option<PreparedRoute>,
    active: Option<ActiveRoute>,
    account_manager: Option<(PathBuf, crate::accounts::AccountManager)>,
    local_probe: Option<LocalProbe>,
    xai_manager: Option<(PathBuf, crate::xai::AccountManager)>,
    xai_probe: Option<SocketAddr>,
}

impl RouteSession {
    /// Run the complete publishing path against synthetic loopback endpoints.
    #[doc(hidden)]
    pub fn for_local_probe(
        data_dir: &Path,
        oauth_origin: &str,
        discovery: SocketAddr,
        responses: HashMap<String, SocketAddr>,
    ) -> Result<Self, String> {
        if discovery.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            || responses
                .values()
                .any(|address| address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        {
            return Err("测试端点必须是 IPv4 回环地址".into());
        }
        let manager = crate::accounts::AccountManager::open_mock(data_dir, oauth_origin)?;
        Ok(Self {
            account_manager: Some((data_dir.into(), manager)),
            local_probe: Some(LocalProbe {
                discovery,
                responses,
            }),
            ..Self::default()
        })
    }

    fn manager(&mut self, data_dir: &Path) -> Result<crate::accounts::AccountManager, String> {
        if let Some((directory, manager)) = &self.account_manager {
            if directory != data_dir {
                return Err("账号数据目录已变化，请重新打开路由".into());
            }
            return Ok(manager.clone());
        }
        let manager = crate::accounts::AccountManager::open(data_dir)?;
        self.account_manager = Some((data_dir.into(), manager.clone()));
        Ok(manager)
    }

    /// Synthetic probes only; managed OAuth and route publication use production paths.
    pub fn with_xai_probe(
        data_dir: &Path,
        oauth_origin: &str,
        responses: SocketAddr,
    ) -> Result<Self, String> {
        if responses.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
            return Err("Grok 测试上游必须是 IPv4 回环地址".into());
        }
        Ok(Self {
            xai_manager: Some((
                data_dir.into(),
                crate::xai::AccountManager::open_mock(data_dir, oauth_origin)?,
            )),
            xai_probe: Some(responses),
            ..Self::default()
        })
    }

    fn xai_manager(&mut self, data_dir: &Path) -> Result<crate::xai::AccountManager, String> {
        if let Some((directory, manager)) = &self.xai_manager {
            if directory != data_dir {
                return Err("Grok 数据目录已变化，请重新打开路由".into());
            }
            return Ok(manager.clone());
        }
        let manager = crate::xai::AccountManager::open(data_dir)?;
        self.xai_manager = Some((data_dir.into(), manager.clone()));
        Ok(manager)
    }

    fn resolve_xai_accounts(
        &mut self,
        data_dir: &Path,
        providers: &[ProviderRecord],
    ) -> Result<HashMap<String, String>, String> {
        let mut accounts = HashMap::new();
        for provider in providers
            .iter()
            .filter(|provider| provider.kind == ProviderKind::XaiOAuth)
        {
            crate::xai::validate_provider(provider)?;
            let account = self
                .xai_manager(data_dir)?
                .resolve_binding(provider.account_binding.as_ref().unwrap())?;
            accounts.insert(provider.id.clone(), account.id);
        }
        Ok(accounts)
    }

    pub async fn wait_for_accounts(&self) -> Result<(), String> {
        if let Some((_, manager)) = &self.account_manager {
            manager.wait_for_idle().await?;
        }
        if let Some((_, manager)) = &self.xai_manager {
            manager.wait_for_idle().await;
        }
        Ok(())
    }

    /// Close admission even if an external edit removed the recovery journal.
    pub async fn drain_for_exit(&self) -> Result<(), String> {
        if let Some(active) = &self.active {
            active.server.pause();
        }
        let result = self.wait_for_accounts().await;
        if result.is_err()
            && let Some(active) = &self.active
        {
            active.server.resume();
        }
        result
    }

    async fn resolve_accounts(
        &mut self,
        state_dir: &Path,
        home: &Path,
        cli: &Path,
        providers: &[ProviderRecord],
    ) -> Result<HashMap<String, ResolvedAccount>, String> {
        let subscriptions: Vec<_> = providers
            .iter()
            .filter(|provider| provider.kind == ProviderKind::Chatgpt)
            .collect();
        if subscriptions.is_empty() {
            return Ok(HashMap::new());
        }
        if subscriptions.len() > 1
            && subscriptions
                .iter()
                .any(|provider| provider.account_binding == Some(AccountBinding::Native))
        {
            return Err("同时发布多个订阅连接时，请为每个连接绑定保存的账号".into());
        }
        let manager = self.manager(state_dir)?;
        let mut accounts = HashMap::new();
        // Resolve saved bindings first; the target login is only the Codex entry identity.
        for provider in &subscriptions {
            chatgpt::validate_provider(provider)?;
            let account = manager
                .resolve_binding(provider.account_binding.as_ref().unwrap())
                .await?;
            let (id, workspace) = if let Some(account) = account {
                let workspace = if let Some(probe) = &self.local_probe {
                    manager
                        .workspace_for_route_mock(&account.id, home, cli, probe.discovery)
                        .await?
                } else {
                    manager.workspace_for_route(&account.id, home, cli).await?
                };
                (Some(account.id), Some(workspace))
            } else {
                (None, None)
            };
            accounts.insert(provider.id.clone(), ResolvedAccount { id, workspace });
        }
        let entry_workspace = if let Some(probe) = &self.local_probe {
            chatgpt::workspace_using_mock(home, cli, probe.discovery).await?
        } else {
            chatgpt::workspace_using(home, cli).await?
        };
        for resolved in accounts
            .values_mut()
            .filter(|resolved| resolved.id.is_none())
        {
            resolved.workspace = entry_workspace.clone();
        }
        Ok(accounts)
    }

    pub fn is_running(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.server.is_running())
    }

    pub fn address(&self) -> Option<SocketAddr> {
        self.active.as_ref().map(|active| active.address)
    }

    pub fn recording_failed(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.server.recording_failed())
    }

    pub fn chatgpt_error(&self) -> Option<&'static str> {
        self.active
            .as_ref()
            .and_then(|active| active.server.chatgpt_error())
    }

    pub fn chatgpt_error_for(&self, provider_id: &str) -> Option<&'static str> {
        self.active
            .as_ref()
            .and_then(|active| active.server.chatgpt_error_for(provider_id))
    }

    pub fn discard_preview(&mut self) {
        self.prepared = None;
    }

    pub async fn prepare(
        &mut self,
        state_dir: &Path,
        config_home: &Path,
        port: u16,
        default_model: &str,
        helper: &Path,
    ) -> Result<String, String> {
        self.discard_preview();
        if self.active.is_some() {
            return Err("请先恢复原配置并停止当前路由".into());
        }
        let target = client::config_path(config_home)?;
        if !config_home.is_dir() {
            return Err("Codex 配置目录不存在".into());
        }
        let (models, providers) = selected_inputs(state_dir)?;
        let config_provider_id = models
            .iter()
            .find(|model| model.enabled && model.public_id == default_model)
            .ok_or("默认模型未发布")?
            .provider_id
            .clone();
        let config_state = app::load_provider_config(state_dir, &config_provider_id)?;
        let publication = catalog::publish_saved(&models)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| "本地端口无法使用；请检查端口占用或选择其他端口")?;
        let address = listener.local_addr().map_err(|_| "无法读取本地路由地址")?;
        let token_reference = format!("router-{}", app::new_id()?);
        let local_token = Secret::new(format!("{}{}", app::new_id()?, app::new_id()?));
        let uses_chatgpt = providers
            .iter()
            .any(|provider| provider.kind == ProviderKind::Chatgpt);
        let cli_path = client::cli_path()?;
        let accounts = self
            .resolve_accounts(state_dir, config_home, &cli_path, &providers)
            .await?;
        let xai_accounts = self.resolve_xai_accounts(state_dir, &providers)?;
        let switch =
            PreparedSwitch::inspect(&target, state_dir, &publication, address, default_model)?;
        let remote_direct_only = providers
            .iter()
            .find(|provider| provider.id == config_provider_id)
            .is_some_and(|provider| provider.kind == ProviderKind::ApiKey)
            && config_state
                .options
                .as_ref()
                .is_some_and(|options| options.remote_compaction);
        let switch = if let Some(mut options) = config_state.options.clone() {
            // API routing cannot forward server-state compaction continuations.
            if remote_direct_only {
                options.remote_compaction = false;
            }
            switch.with_codex_options(&options, &config_state.common)?
        } else if !config_state.common.is_empty() {
            switch.with_common_config(&config_state.common)?
        } else {
            switch
        };
        let switch = if providers.iter().any(|provider| {
            provider.id == config_provider_id && provider.kind == ProviderKind::XaiOAuth
        }) {
            let model = publication.catalog["models"]
                .as_array()
                .and_then(|models| models.iter().find(|model| model["slug"] == default_model))
                .ok_or("Grok 默认模型资料缺失")?;
            let proposed: toml_edit::DocumentMut = switch
                .preview
                .proposed
                .parse()
                .map_err(|_| "Grok 路由配置无效")?;
            let effort = proposed
                .get("model_reasoning_effort")
                .and_then(toml_edit::Item::as_str);
            let supported = model["supported_reasoning_levels"].as_array();
            if effort.is_some_and(|effort| {
                supported
                    .is_some_and(|levels| !levels.iter().any(|level| level["effort"] == effort))
            }) {
                let default = model["default_reasoning_level"]
                    .as_str()
                    .ok_or("Grok 模型资料缺少可用默认思考档位")?;
                switch.with_common_config(&format!("model_reasoning_effort = \"{default}\"\n"))?
            } else {
                switch
            }
        } else {
            switch
        };
        let switch = if uses_chatgpt {
            switch.with_chatgpt_auth(local_token.expose(), &token_reference)?
        } else {
            switch.with_credential_helper(helper, &token_reference)?
        };
        let client_version = client::check_catalog_using(&publication.catalog, &cli_path).await?;
        let mappings = models
            .iter()
            .filter(|model| model.enabled)
            .map(|model| {
                let provider = providers
                    .iter()
                    .find(|provider| provider.id == model.provider_id)
                    .unwrap();
                let destination = |provider: &ProviderRecord| {
                    format!(
                        "{} ({})",
                        provider.name,
                        reqwest::Url::parse(&provider.base_url)
                            .unwrap()
                            .origin()
                            .ascii_serialization()
                    )
                };
                let fallback = model
                    .fallback_provider_id
                    .as_ref()
                    .map(|id| {
                        let provider = providers
                            .iter()
                            .find(|provider| &provider.id == id)
                            .unwrap();
                        format!(
                            "\n  备用 → {} / {}；仅主连接建立失败时尝试，费用与数据接收方可能变化",
                            destination(provider),
                            model.upstream_model
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "{} → {} / {}{fallback}",
                    model.public_id,
                    destination(provider),
                    model.upstream_model
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let summary = format!(
            "{client_version} 已核对 {} 个可选模型 · http://{address}/v1\n目标：{}\n{mappings}\n受管变更：{}{}",
            publication.routes.len(),
            target.display(),
            switch.preview.changed_fields.join("、"),
            if uses_chatgpt {
                if accounts.values().any(|account| account.id.is_some()) {
                    "\n订阅请求固定使用所选保存账号；仅官方模型传递认证。独立本地令牌会写入仅当前用户可读的配置及恢复记录。切换账号请先恢复；切到第三方时请新建会话。"
                } else {
                    "\n订阅认证由目标 Codex 管理；仅官方模型传递认证。独立本地令牌会写入仅当前用户可读的配置及恢复记录。切换工作区请先恢复；切到第三方时请新建会话。"
                }
            } else {
                ""
            }
        );
        let destinations = accounts
            .iter()
            .map(|(provider_id, resolved)| {
                let provider = providers
                    .iter()
                    .find(|provider| &provider.id == provider_id)
                    .unwrap();
                let account = resolved.id.as_deref().unwrap_or("原生登录");
                match &resolved.workspace {
                    Some(workspace) => format!(
                        "{} · 账号 {} · 工作区 {} · {} · {}",
                        provider.name,
                        account,
                        workspace.account_id,
                        workspace.backend_origin,
                        workspace.routing_override
                    ),
                    None => format!("{} · 账号 {}", provider.name, account),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let summary = if destinations.is_empty() {
            summary
        } else {
            format!("{summary}\n{destinations}")
        };
        let summary = if xai_accounts.is_empty() {
            summary
        } else {
            let mut bindings = xai_accounts
                .iter()
                .map(|(provider, account)| format!("Grok 上游 {provider} · 保存账号 {account}"))
                .collect::<Vec<_>>();
            bindings.sort();
            format!(
                "{summary}\n{}\nGrok 账号在发布时固定；更换账号或模型请启动新会话。",
                bindings.join("\n")
            )
        };
        let summary = if config_state.options.is_some() || !config_state.common.is_empty() {
            format!(
                "{summary}\nCodex 窗口与通用配置使用默认模型的供应商设置{}",
                if remote_direct_only {
                    "；API 上游的远程压缩选项仅在直连时生效。"
                } else {
                    "。"
                }
            )
        } else {
            summary
        };
        self.prepared = Some(PreparedRoute {
            switch,
            listener,
            address,
            target,
            state_dir: state_dir.into(),
            requested_port: port,
            default_model: default_model.into(),
            client_version,
            cli_path,
            token_reference,
            local_token,
            accounts,
            xai_accounts,
            publication,
            models,
            providers,
            config_provider_id,
            config_state,
        });
        Ok(summary)
    }

    pub async fn apply(
        &mut self,
        state_dir: &Path,
        config_home: &Path,
        port: u16,
        default_model: &str,
    ) -> Result<(), String> {
        let prepared = self.prepared.take().ok_or("请先预览路由发布")?;
        if prepared.target != client::config_path(config_home)?
            || prepared.state_dir != state_dir
            || prepared.requested_port != port
            || prepared.default_model != default_model
        {
            return Err("路由目标、端口或默认模型已变化，请重新预览".into());
        }
        let (models, providers) = selected_inputs(state_dir)?;
        if models != prepared.models
            || providers != prepared.providers
            || app::load_provider_config(state_dir, &prepared.config_provider_id)?
                != prepared.config_state
        {
            return Err("上游或模型资料已变化，请重新预览".into());
        }
        if client::cli_path()? != prepared.cli_path
            || client::check_catalog_using(&prepared.publication.catalog, &prepared.cli_path)
                .await?
                != prepared.client_version
        {
            return Err("Codex CLI 版本已变化，请重新预览".into());
        }
        let resolved = self
            .resolve_accounts(state_dir, config_home, &prepared.cli_path, &providers)
            .await?;
        if self.resolve_xai_accounts(state_dir, &providers)? != prepared.xai_accounts {
            return Err("Grok 绑定账号已变化，请重新预览路由".into());
        }
        if resolved != prepared.accounts {
            return Err("订阅账号、工作区或区域路由已变化，请重新预览".into());
        }
        let uses_chatgpt = !resolved.is_empty();
        let manager = if uses_chatgpt {
            Some(self.manager(state_dir)?)
        } else {
            None
        };
        let mut upstreams = HashMap::new();
        for provider in &providers {
            if provider.kind == ProviderKind::XaiOAuth {
                let manager = self.xai_manager(state_dir)?;
                let id = &prepared.xai_accounts[&provider.id];
                let token = manager.credential(id).await?;
                let base_url = self
                    .xai_probe
                    .map(|address| format!("http://{address}"))
                    .unwrap_or(crate::xai::BASE_URL.into());
                let available = direct::fetch_models(&base_url, token.expose()).await?;
                for model in models
                    .iter()
                    .filter(|model| model.provider_id == provider.id)
                {
                    if !available.contains(&model.upstream_model) {
                        return Err(format!(
                            "{} 的 Grok 账号目录中没有 {}",
                            provider.name, model.upstream_model
                        ));
                    }
                }
                let upstream = match self.xai_probe {
                    Some(address) => Upstream::xai_mock(address, manager, id.clone())?,
                    None => Upstream::xai(manager, id.clone())?,
                };
                upstreams.insert(provider.id.clone(), upstream);
                continue;
            }
            if provider.kind == ProviderKind::Chatgpt {
                let account = &resolved[&provider.id];
                let catalog = chatgpt::catalog().await?;
                for model in models
                    .iter()
                    .filter(|model| model.provider_id == provider.id)
                {
                    if !catalog
                        .iter()
                        .any(|entry| entry["slug"] == model.upstream_model)
                    {
                        return Err(format!(
                            "目标 Codex 的内置官方目录没有 {}；请更新官方模型资料",
                            model.upstream_model
                        ));
                    }
                }
                let upstream = match (&account.id, &account.workspace) {
                    (Some(id), Some(workspace)) => {
                        if let Some(probe) = &self.local_probe {
                            let address = *probe
                                .responses
                                .get(&provider.id)
                                .ok_or("缺少订阅测试响应端点")?;
                            Upstream::managed_chatgpt_mock(
                                address,
                                manager.as_ref().unwrap().clone(),
                                id.clone(),
                                config_home.into(),
                                workspace,
                            )?
                        } else {
                            Upstream::managed_chatgpt(
                                manager.as_ref().unwrap().clone(),
                                id.clone(),
                                config_home.into(),
                                workspace,
                            )?
                        }
                    }
                    (None, Some(workspace)) => Upstream::chatgpt_for_workspace(workspace)?,
                    (None, None) => Upstream::chatgpt(),
                    _ => return Err("保存账号的工作区资料缺失，请重新预览".into()),
                };
                upstreams.insert(provider.id.clone(), upstream);
                continue;
            }
            let token = app::provider_credential(state_dir, provider)?;
            let available = direct::fetch_models(&provider.base_url, token.expose())
                .await
                .map_err(|error| format!("{}：{error}", provider.name))?;
            for model in models
                .iter()
                .filter(|model| model.provider_id == provider.id)
            {
                if !available.contains(&model.upstream_model) {
                    return Err(format!(
                        "{} 的目录中没有映射模型 {}",
                        provider.name, model.upstream_model
                    ));
                }
            }
            upstreams.insert(
                provider.id.clone(),
                Upstream::with_secret(&provider.base_url, token)?,
            );
        }
        if selected_inputs(state_dir)? != (models.clone(), providers.clone())
            || app::load_provider_config(state_dir, &prepared.config_provider_id)?
                != prepared.config_state
        {
            return Err("检查期间上游或模型资料已变化，请重新预览".into());
        }
        // Recheck default/fixed identity immediately before publishing. Refreshes are not identity changes.
        if uses_chatgpt {
            for provider in providers
                .iter()
                .filter(|provider| provider.kind == ProviderKind::Chatgpt)
            {
                let id = manager
                    .as_ref()
                    .unwrap()
                    .resolve_binding(provider.account_binding.as_ref().unwrap())
                    .await?
                    .map(|account| account.id);
                if id != prepared.accounts[&provider.id].id {
                    return Err("检查期间订阅账号选择已变化，请重新预览".into());
                }
            }
        }
        let local_token = prepared.local_token;
        let request_store = app::open_store(state_dir).map_err(|error| error.message())?;
        let generation = prepared
            .switch
            .catalog_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let catalog_path = prepared.switch.catalog_path().to_path_buf();
        let public_models = prepared.publication.routes.keys().cloned().collect();
        let document: toml_edit::DocumentMut = prepared
            .switch
            .preview
            .proposed
            .parse()
            .map_err(|_| "待发布的 Codex 配置无效")?;
        let provider_config =
            Secret::new(document["model_providers"][crate::config::PROVIDER_ID].to_string());
        let state = RouterState::new(
            prepared.address,
            local_token.expose().into(),
            prepared.publication,
            upstreams,
        )?
        .with_request_log(request_store, generation);
        // Keep the guard on API-only publications too: an old subscription
        // session must not escape its saved context after restoring the route.
        let state =
            state.with_session_store(app::open_store(state_dir).map_err(|error| error.message())?);
        let server = RunningRouter::start(prepared.listener, state)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| "无法创建本地检查客户端")?;
        let health = client
            .get(format!("http://{}/v1/models", prepared.address))
            .header(crate::routing::LOCAL_TOKEN_HEADER, local_token.expose())
            .send()
            .await
            .map_err(|_| "本地路由验证失败")?;
        if !health.status().is_success() {
            return Err("本地路由鉴权验证失败".into());
        }
        let saved_token = app::open_store(state_dir)
            .map_err(|_| "无法保存本地路由访问令牌")
            .and_then(|store| {
                store
                    .put_local_token(&prepared.token_reference, &local_token)
                    .map_err(|_| "无法保存本地路由访问令牌")
            });
        if let Err(error) = saved_token {
            server.stop().await;
            return Err(error.into());
        }
        self.active = Some(ActiveRoute {
            server,
            address: prepared.address,
            target: prepared.target,
            state_dir: prepared.state_dir,
            catalog_path,
            public_models,
            cli_path: prepared.cli_path,
            provider_config,
            token_reference: prepared.token_reference,
            uses_chatgpt,
        });
        if let Err(error) = prepared.switch.apply() {
            // An error after journal publication may follow a successful rename.
            // Keep the route usable until its journal has been recovered.
            if !state_dir.join("switch-journal.json").exists() {
                self.stop().await?;
            }
            return Err(format!(
                "路由配置未确认生效：{error}；如有恢复记录，请先恢复原配置"
            ));
        }
        Ok(())
    }

    pub async fn codex_launcher(
        &self,
        state_dir: &Path,
        config_home: &Path,
    ) -> Result<PathBuf, String> {
        let active = self
            .active
            .as_ref()
            .filter(|active| active.server.is_running())
            .ok_or("请先开启模型路由，再启动 Codex")?;
        if active.state_dir != state_dir {
            return Err("路由数据目录已变化，请使用当前路由的数据目录启动 Codex".into());
        }
        let target = client::config_path(config_home)?;
        let recovery = config_transaction::recovery(state_dir)?
            .ok_or("路由恢复记录已变化，请检查配置后重新发布")?;
        if target != active.target || recovery.config_path != active.target {
            return Err("目标配置目录已变化，请使用当前路由的配置目录启动 Codex".into());
        }
        let bytes = config_transaction::read_config(&target)?.ok_or("路由配置文件已不存在")?;
        let document: toml_edit::DocumentMut = std::str::from_utf8(&bytes)
            .map_err(|_| "Codex 配置不是 UTF-8")?
            .parse()
            .map_err(|_| "Codex 配置 TOML 无效")?;
        let field = |name: &str| document.get(name).and_then(toml_edit::Item::as_str);
        let provider = document
            .get("model_providers")
            .and_then(|providers| providers.get(crate::config::PROVIDER_ID));
        if field("model_provider") != Some(crate::config::PROVIDER_ID)
            || field("model_catalog_json") != active.catalog_path.to_str()
            || !provider
                .is_some_and(|provider| provider.to_string() == active.provider_config.expose())
            || !field("model").is_some_and(|model| active.public_models.contains(model))
            || !active.catalog_path.is_file()
        {
            return Err("路由配置已被外部修改，请检查配置并重新发布后再启动 Codex".into());
        }
        if active.uses_chatgpt {
            if let Some(probe) = &self.local_probe {
                chatgpt::workspace_using_mock(config_home, &active.cli_path, probe.discovery)
                    .await?;
            } else {
                chatgpt::workspace_using(config_home, &active.cli_path).await?;
            }
        }
        client::write_codex_launcher(state_dir, config_home, &active.cli_path).await
    }

    pub async fn restore(&mut self, state_dir: &Path, config_home: &Path) -> Result<(), String> {
        self.discard_preview();
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.state_dir != state_dir)
        {
            return Err("路由恢复数据目录不匹配，请使用当前路由的数据目录".into());
        }
        let recovery = config_transaction::recovery(state_dir)?.ok_or("没有待恢复的路由配置")?;
        if self.active.as_ref().is_some_and(|active| {
            recovery.local_token_reference.as_deref() != Some(active.token_reference.as_str())
        }) {
            return Err("路由恢复记录与当前路由不匹配，请检查配置后恢复".into());
        }
        let target = client::config_path(config_home)?;
        if target != recovery.config_path
            || self
                .active
                .as_ref()
                .is_some_and(|active| active.target != target)
        {
            return Err(format!(
                "路由恢复目标不匹配；请使用 {}",
                recovery.config_path.parent().unwrap().display()
            ));
        }
        if let Some(active) = &self.active {
            active.server.pause();
        }
        let without_conflicts = |result: config_transaction::RestoreResult| {
            if result.conflicts.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "已保留外部改动，以下字段有冲突：{}；恢复记录保留",
                    result.conflicts.join("、")
                ))
            }
        };
        // Keep the recovery reference until SQLite cleanup succeeds, so a
        // locked or unavailable database can be retried after the route stops.
        let result = config_transaction::restore_preserving_journal(&target, state_dir)
            .and_then(without_conflicts);
        if let Err(error) = result {
            if let Some(active) = &self.active {
                active.server.resume();
            }
            return Err(error);
        }
        self.stop().await?;
        if let Some(reference) = recovery.local_token_reference {
            delete_local_token(state_dir, &reference)?;
        }
        config_transaction::restore(&target, state_dir).and_then(without_conflicts)
    }

    async fn stop(&mut self) -> Result<(), String> {
        if let Some(active) = self.active.take() {
            active.server.stop().await;
            delete_local_token(&active.state_dir, &active.token_reference)?;
        }
        Ok(())
    }
}

fn delete_local_token(state_dir: &Path, reference: &str) -> Result<(), String> {
    Store::open_recovery(&state_dir.join("switchx.sqlite"))
        .map_err(|_| "路由已停止，但数据库中的本地令牌清理失败")?
        .delete_local_token(reference)
        .map_err(|_| "路由已停止，但数据库中的本地令牌清理失败".into())
}

fn selected_inputs(state_dir: &Path) -> Result<(Vec<ModelRecord>, Vec<ProviderRecord>), String> {
    let store = app::open_store(state_dir).map_err(|error| error.message())?;
    let mut models = store.models().map_err(|_| "无法读取模型资料")?;
    let required: HashSet<String> = models
        .iter()
        .filter(|model| model.enabled)
        .flat_map(|model| {
            std::iter::once(model.provider_id.clone()).chain(model.fallback_provider_id.clone())
        })
        .collect();
    let selected: Vec<_> = models
        .iter()
        .filter(|model| model.enabled)
        .cloned()
        .collect();
    models.retain(|model| {
        model.enabled
            || selected.iter().any(|primary| {
                primary.fallback_provider_id.as_deref() == Some(&model.provider_id)
                    && primary.upstream_model == model.upstream_model
            })
    });
    let providers: Vec<_> = store
        .providers()
        .map_err(|_| "无法读取上游资料")?
        .into_iter()
        .filter(|provider| required.contains(&provider.id))
        .collect();
    for model in &models {
        let provider = providers
            .iter()
            .find(|provider| provider.id == model.provider_id)
            .ok_or("所选模型的上游已不存在")?;
        direct::validate_provider(&provider.name, &provider.base_url, &model.upstream_model)?;
        if provider.kind != ProviderKind::ApiKey {
            if provider.kind == ProviderKind::Chatgpt {
                chatgpt::validate_provider(provider)?;
            } else {
                crate::xai::validate_provider(provider)?;
            }
            if model.fallback_provider_id.is_some()
                || models.iter().any(|model| {
                    model.fallback_provider_id.as_deref() == Some(provider.id.as_str())
                })
            {
                return Err("订阅账号不参与自动备用切换".into());
            }
        }
    }
    for provider in &providers {
        if provider.kind == ProviderKind::ApiKey {
            app::provider_credential(state_dir, provider)?;
        }
    }
    Ok((models, providers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_bound_account_preserves_native_files() {
        let root = std::env::temp_dir().join(format!(
            "switchx-account-preview-{}",
            app::new_id().unwrap()
        ));
        let home = root.join("codex");
        let data = root.join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir(&data).unwrap();
        let config = b"# unmodified native config\nmodel = \"user-model\"\n";
        let auth = b"synthetic unowned auth bytes";
        std::fs::write(home.join("config.toml"), config).unwrap();
        std::fs::write(home.join("auth.json"), auth).unwrap();
        let templates: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        chatgpt::save_connection(&data, templates["models"].as_array().unwrap()).unwrap();
        let model = app::open_store(&data)
            .unwrap()
            .models()
            .unwrap()
            .into_iter()
            .find(|model| model.enabled)
            .unwrap()
            .public_id;
        chatgpt::bind_managed_account(&data, Some("0123456789abcdef0123456789abcdef")).unwrap();
        let mut session = RouteSession::default();
        let error = session
            .prepare(&data, &home, 0, &model, &root.join("must-not-run"))
            .await
            .unwrap_err();
        assert!(error.contains("账号"));
        assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), config);
        assert_eq!(std::fs::read(home.join("auth.json")).unwrap(), auth);
        assert!(!home.join(".switchx-account.json").exists());
        assert!(!data.join("codex_oauth_auth.json").exists());
        assert!(!data.join("switch-journal.json").exists());
        assert!(session.prepared.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn launcher_requires_the_running_routes_unchanged_target_and_catalog() {
        let root =
            std::env::temp_dir().join(format!("switchx-launch-guard-{}", app::new_id().unwrap()));
        let home = root.join("codex");
        let data = root.join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir(&data).unwrap();
        let mut session = RouteSession::default();
        assert!(
            session
                .codex_launcher(&data, &home)
                .await
                .unwrap_err()
                .contains("开启模型路由")
        );

        let templates =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let publication = catalog::publish(
            &templates,
            &[catalog::Selection {
                public_id: "sx-test",
                display_name: "Synthetic test",
                provider_id: "mock",
                upstream_model: "deepseek-flash",
            }],
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let switch = PreparedSwitch::inspect(
            &home.join("config.toml"),
            &data,
            &publication,
            address,
            "sx-test",
        )
        .unwrap();
        let catalog_path = switch.catalog_path().to_path_buf();
        switch.apply().unwrap();
        let mut upstreams = HashMap::new();
        upstreams.insert(
            "mock".into(),
            Upstream::with_secret(
                "http://127.0.0.1:1/v1",
                Secret::new("synthetic-only".into()),
            )
            .unwrap(),
        );
        let server = RunningRouter::start(
            listener,
            RouterState::new(address, "s".repeat(32), publication, upstreams).unwrap(),
        )
        .unwrap();
        session.active = Some(ActiveRoute {
            server,
            address,
            target: home.join("config.toml"),
            state_dir: data.clone(),
            catalog_path,
            public_models: HashSet::from(["sx-test".into()]),
            cli_path: root.join("must-not-be-executed"),
            provider_config: Secret::new(
                std::fs::read_to_string(home.join("config.toml"))
                    .unwrap()
                    .parse::<toml_edit::DocumentMut>()
                    .unwrap()["model_providers"][crate::config::PROVIDER_ID]
                    .to_string(),
            ),
            token_reference: String::new(),
            uses_chatgpt: false,
        });
        assert!(
            session
                .codex_launcher(&root.join("another-data"), &home)
                .await
                .unwrap_err()
                .contains("数据目录")
        );
        assert!(
            session
                .restore(&root.join("another-data"), &home)
                .await
                .unwrap_err()
                .contains("数据目录")
        );
        assert!(session.is_running());
        assert!(config_transaction::recovery(&data).unwrap().is_some());
        assert!(
            session
                .codex_launcher(&data, &root.join("another-home"))
                .await
                .unwrap_err()
                .contains("目标配置目录")
        );
        let original = std::fs::read_to_string(home.join("config.toml")).unwrap();
        for changed in [
            original.replace("switchx_router", "external-provider"),
            original.replace("sx-test", "unpublished-model"),
            original.replace("catalog-", "another-catalog-"),
            original.replace(&address.to_string(), "127.0.0.1:1"),
            original.replace("SWITCHX_LOCAL_TOKEN", "EXTERNAL_KEY"),
            original.replace(
                "requires_openai_auth = false",
                "requires_openai_auth = true",
            ),
            original.replace("wire_api = \"responses\"", "wire_api = \"chat\""),
        ] {
            std::fs::write(home.join("config.toml"), changed).unwrap();
            assert!(
                session
                    .codex_launcher(&data, &home)
                    .await
                    .unwrap_err()
                    .contains("外部修改")
            );
            assert!(!data.join("launch-codex.command").exists());
        }
        std::fs::write(home.join("config.toml"), original).unwrap();
        // Exit must close admission even when the journal disappeared externally.
        let journal_path = data.join("switch-journal.json");
        let journal = std::fs::read(&journal_path).unwrap();
        std::fs::remove_file(&journal_path).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let endpoint = format!("http://{address}/v1/models");
        assert!(
            client
                .get(&endpoint)
                .header(crate::routing::LOCAL_TOKEN_HEADER, "s".repeat(32))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        session.drain_for_exit().await.unwrap();
        assert_eq!(
            client
                .get(&endpoint)
                .header(crate::routing::LOCAL_TOKEN_HEADER, "s".repeat(32))
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(!journal_path.exists());
        std::fs::write(journal_path, journal).unwrap();
        session.active.take().unwrap().server.stop().await;
        config_transaction::restore(&home.join("config.toml"), &data).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn recovery_retains_local_token_until_conflicts_and_cleanup_failures_are_resolved() {
        let root =
            std::env::temp_dir().join(format!("switchx-token-recovery-{}", app::new_id().unwrap()));
        let home = root.join("codex");
        let data = root.join("data");
        std::fs::create_dir_all(&home).unwrap();
        let original = "model = \"original\"\n";
        let config_path = home.join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let reference = format!("router-{}", app::new_id().unwrap());
        let other_reference = format!("router-{}", app::new_id().unwrap());
        let token = Secret::new("a".repeat(64));
        let store = app::open_store(&data).unwrap();
        store.put_local_token(&reference, &token).unwrap();
        store
            .put_local_token(&other_reference, &Secret::new("b".repeat(64)))
            .unwrap();
        let templates =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let publication = catalog::publish(
            &templates,
            &[catalog::Selection {
                public_id: "sx-test",
                display_name: "Synthetic test",
                provider_id: "mock",
                upstream_model: "deepseek-flash",
            }],
        )
        .unwrap();
        PreparedSwitch::inspect(
            &config_path,
            &data,
            &publication,
            "127.0.0.1:18731".parse().unwrap(),
            "sx-test",
        )
        .unwrap()
        .with_credential_helper(&std::env::current_exe().unwrap(), &reference)
        .unwrap()
        .apply()
        .unwrap();
        let applied = std::fs::read_to_string(&config_path).unwrap();
        assert!(!applied.contains(token.expose()));
        assert!(
            !std::fs::read_to_string(data.join("switch-journal.json"))
                .unwrap()
                .contains(token.expose())
        );
        let legacy = rusqlite::Connection::open(data.join("switchx.sqlite")).unwrap();
        legacy
            .execute_batch(
                "ALTER TABLE providers DROP COLUMN icon_id;
             ALTER TABLE providers DROP COLUMN account_binding;
             ALTER TABLE providers DROP COLUMN kind;
             DROP TABLE session_bindings;
             PRAGMA user_version = 8;",
            )
            .unwrap();
        assert!(Store::needs_recovery_before_migration(&data.join("switchx.sqlite")).unwrap());
        assert!(app::open_store(&data).is_err());
        let legacy_bytes = std::fs::read(data.join("switchx.sqlite")).unwrap();
        assert_eq!(
            Store::open_credentials_read_only(&data.join("switchx.sqlite"))
                .unwrap()
                .local_token(&reference)
                .unwrap()
                .unwrap()
                .expose(),
            token.expose()
        );
        assert_eq!(
            std::fs::read(data.join("switchx.sqlite")).unwrap(),
            legacy_bytes
        );
        std::fs::write(
            &config_path,
            applied.replace("model = \"sx-test\"", "model = \"external\""),
        )
        .unwrap();
        let mut session = RouteSession::default();
        assert!(session.restore(&data, &home).await.is_err());
        assert!(config_transaction::recovery(&data).unwrap().is_some());
        assert_eq!(
            legacy
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
        assert_eq!(
            store.local_token(&reference).unwrap().unwrap().expose(),
            token.expose()
        );
        let conflicted = std::fs::read_to_string(&config_path).unwrap();
        std::fs::write(
            &config_path,
            conflicted.replace("model = \"external\"", "model = \"original\""),
        )
        .unwrap();
        let connection = rusqlite::Connection::open(data.join("switchx.sqlite")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER refuse_synthetic_token_delete BEFORE DELETE ON app_settings
                 WHEN OLD.key LIKE 'local_token:%'
                 BEGIN SELECT RAISE(ABORT, 'synthetic token delete failure'); END;",
            )
            .unwrap();
        let error = session.restore(&data, &home).await.unwrap_err();
        assert!(error.contains("令牌清理失败"));
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        assert!(config_transaction::recovery(&data).unwrap().is_some());
        assert!(store.local_token(&reference).unwrap().is_some());
        connection
            .execute_batch("DROP TRIGGER refuse_synthetic_token_delete")
            .unwrap();
        drop(connection);
        session.restore(&data, &home).await.unwrap();
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        assert!(config_transaction::recovery(&data).unwrap().is_none());
        assert!(store.local_token(&reference).unwrap().is_none());
        assert_eq!(
            legacy
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
        drop(legacy);
        assert!(app::open_store(&data).is_ok());
        assert_eq!(
            store
                .local_token(&other_reference)
                .unwrap()
                .unwrap()
                .expose(),
            "b".repeat(64)
        );
        delete_local_token(&data, &reference).unwrap();
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_each_provider_once_and_only_the_matching_backup_model() {
        let path = std::env::temp_dir().join(format!(
            "switchx-selected-models-{}",
            app::new_id().unwrap()
        ));
        let store = app::open_store(&path).unwrap();
        for id in ["primary", "backup"] {
            store
                .put_provider_with_models_options_and_key(
                    &ProviderRecord {
                        kind: crate::storage::ProviderKind::ApiKey,
                        account_binding: None,
                        icon_id: None,
                        id: id.into(),
                        name: id.into(),
                        base_url: "https://example.invalid/v1".into(),
                        model_id: "unrelated-default".into(),
                        credential_ref: None,
                    },
                    &[],
                    None,
                    &Secret::new(format!("synthetic-{id}-key")),
                )
                .unwrap();
        }
        let metadata = catalog::mapping_metadata(
            "model-a",
            "Model A",
            &catalog::MappingSettings {
                context_window: "128000",
                reasoning_levels: Some(""),
                default_reasoning: Some(""),
            },
            None,
        )
        .unwrap();
        let first = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-first".into(),
            display_name: "First".into(),
            upstream_model: "model-a".into(),
            metadata: metadata.to_string(),
            enabled: true,
            fallback_provider_id: Some("backup".into()),
        };
        store.put_model(&first).unwrap();
        let mut second = first.clone();
        second.public_id = "sx-second".into();
        second.upstream_model = "model-b".into();
        second.fallback_provider_id = None;
        let mut metadata = metadata;
        metadata["slug"] = "model-b".into();
        second.metadata = metadata.to_string();
        store.put_model(&second).unwrap();
        store
            .put_model(&ModelRecord {
                provider_id: "backup".into(),
                public_id: "sx-backup".into(),
                enabled: false,
                fallback_provider_id: None,
                ..first
            })
            .unwrap();
        store
            .put_model(&ModelRecord {
                provider_id: "backup".into(),
                public_id: "sx-unused".into(),
                upstream_model: "unused-model".into(),
                metadata: "invalid but unpublished".into(),
                enabled: false,
                ..second
            })
            .unwrap();
        let (models, providers) = selected_inputs(&path).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(models.len(), 3);
        assert!(models.iter().all(|model| model.public_id != "sx-unused"));
        assert_eq!(catalog::publish_saved(&models).unwrap().routes.len(), 2);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
