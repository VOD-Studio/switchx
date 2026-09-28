//! Native API route lifecycle. Provider keys never enter the generated Codex config.

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
    client,
    config_transaction::{self, PreparedSwitch},
    credentials::{CredentialError, CredentialStore, ROUTER_TOKEN_SERVICE, Secret},
    direct,
    routing::{RouterState, RunningRouter, Upstream},
    storage::{ModelRecord, ProviderRecord},
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
    token_reference: String,
    publication: Publication,
    models: Vec<ModelRecord>,
    providers: Vec<ProviderRecord>,
}

struct ActiveRoute {
    server: RunningRouter,
    address: SocketAddr,
    target: PathBuf,
    token_reference: String,
}

#[derive(Default)]
pub struct RouteSession {
    prepared: Option<PreparedRoute>,
    active: Option<ActiveRoute>,
}

impl RouteSession {
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
        let publication = catalog::publish_saved(&models)?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| "本地端口无法使用；请检查端口占用或选择其他端口")?;
        let address = listener.local_addr().map_err(|_| "无法读取本地路由地址")?;
        let token_reference = format!("router-{}", app::new_id()?);
        let switch =
            PreparedSwitch::inspect(&target, state_dir, &publication, address, default_model)?
                .with_credential_helper(helper, &token_reference)?;
        let client_version = client::check_catalog(&publication.catalog).await?;
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
                            provider.model_id
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
            "{client_version} 已解析 {} 个模型 · http://{address}/v1\n目标：{}\n{mappings}\n受管变更：{}",
            publication.routes.len(),
            target.display(),
            switch.preview.changed_fields.join("、")
        );
        self.prepared = Some(PreparedRoute {
            switch,
            listener,
            address,
            target,
            state_dir: state_dir.into(),
            requested_port: port,
            default_model: default_model.into(),
            client_version,
            token_reference,
            publication,
            models,
            providers,
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
        if models != prepared.models || providers != prepared.providers {
            return Err("上游或模型资料已变化，请重新预览".into());
        }
        if client::check_catalog(&prepared.publication.catalog).await? != prepared.client_version {
            return Err("Codex CLI 版本已变化，请重新预览".into());
        }
        let mut upstreams = HashMap::new();
        for provider in &providers {
            let token = app::provider_credential(provider)?;
            direct::check_models(provider, token.expose())
                .await
                .map_err(|error| format!("{}：{error}", provider.name))?;
            upstreams.insert(
                provider.id.clone(),
                Upstream::with_secret(&provider.base_url, token)?,
            );
        }
        if selected_inputs(state_dir)? != (models, providers) {
            return Err("检查期间上游或模型资料已变化，请重新预览".into());
        }
        let local_token = Secret::new(format!("{}{}", app::new_id()?, app::new_id()?));
        let request_store = app::open_store(state_dir).map_err(|error| error.message())?;
        let generation = prepared
            .switch
            .catalog_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let state = RouterState::new(
            prepared.address,
            local_token.expose().into(),
            prepared.publication,
            upstreams,
        )?
        .with_request_log(request_store, generation);
        let server = RunningRouter::start(prepared.listener, state)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| "无法创建本地检查客户端")?;
        let health = client
            .get(format!("http://{}/v1/models", prepared.address))
            .bearer_auth(local_token.expose())
            .send()
            .await
            .map_err(|_| "本地路由验证失败")?;
        if !health.status().is_success() {
            return Err("本地路由鉴权验证失败".into());
        }
        CredentialStore::new(ROUTER_TOKEN_SERVICE)
            .and_then(|store| store.put(&prepared.token_reference, &local_token))
            .map_err(|_| "无法保存本地路由访问令牌")?;
        self.active = Some(ActiveRoute {
            server,
            address: prepared.address,
            target: prepared.target,
            token_reference: prepared.token_reference,
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

    pub async fn restore(&mut self, state_dir: &Path, config_home: &Path) -> Result<(), String> {
        self.discard_preview();
        let recovery = config_transaction::recovery(state_dir)?.ok_or("没有待恢复的路由配置")?;
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
        let result = config_transaction::restore(&target, state_dir).and_then(|result| {
            if result.conflicts.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "已保留外部改动，以下字段有冲突：{}；路由与恢复记录保留",
                    result.conflicts.join("、")
                ))
            }
        });
        if let Err(error) = result {
            if let Some(active) = &self.active {
                active.server.resume();
            }
            return Err(error);
        }
        self.stop().await?;
        if let Some(reference) = recovery.local_token_reference {
            delete_local_token(&reference)?;
        }
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), String> {
        if let Some(active) = self.active.take() {
            active.server.stop().await;
            delete_local_token(&active.token_reference)?;
        }
        Ok(())
    }
}

fn delete_local_token(reference: &str) -> Result<(), String> {
    match CredentialStore::new(ROUTER_TOKEN_SERVICE).and_then(|store| store.delete(reference)) {
        Ok(()) | Err(CredentialError::Missing) => Ok(()),
        Err(_) => Err("路由已停止，但系统凭据中的本地令牌清理失败".into()),
    }
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
    models.retain(|model| required.contains(&model.provider_id));
    let mut providers = Vec::with_capacity(models.len());
    for model in &models {
        let provider = store
            .provider(&model.provider_id)
            .map_err(|_| "无法读取上游资料")?
            .ok_or("所选模型的上游已不存在")?;
        if provider.model_id != model.upstream_model {
            return Err(format!(
                "{} 的上游模型已变化，请重新导入模型资料",
                model.public_id
            ));
        }
        direct::validate_provider(&provider.name, &provider.base_url, &provider.model_id)?;
        if provider.credential_ref.as_deref() != Some(provider.id.as_str()) {
            return Err(format!("{} 的凭据引用无效，请重新保存上游", provider.name));
        }
        providers.push(provider);
    }
    Ok((models, providers))
}
