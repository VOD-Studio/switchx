//! Claude listener, immutable publications and durable recovery ownership.
use super::{
    config::{self, Prepared, RouteMetadata},
    publication::{self, Options, Plan},
    router::{self, Destination, Snapshot},
};
use crate::{
    app,
    clients::{ClaudeCapabilities, ClientWorkspace},
    credentials::Secret,
    storage::Store,
};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio::net::TcpListener;

pub struct PreparedRoute {
    pub preview: config::Preview,
    workspace: ClientWorkspace,
    plan: Plan,
    snapshot: Arc<Snapshot>,
    configuration: Prepared,
    metadata: RouteMetadata,
    token: Secret,
    listener: Option<TcpListener>,
    old_metadata: Option<RouteMetadata>,
}
struct Active {
    _owner: config::Lock,
    server: router::Running,
    metadata: RouteMetadata,
    workspace: ClientWorkspace,
}
#[derive(Default)]
pub struct Sessions {
    active: BTreeMap<String, Active>,
}
impl Sessions {
    pub fn statuses(&self) -> Vec<(String, bool, bool)> {
        self.active
            .iter()
            .map(|(id, a)| {
                (
                    id.clone(),
                    a.server.is_running(),
                    a.server.recording_failed(),
                )
            })
            .collect()
    }
    pub fn is_running(&self, workspace: &str) -> bool {
        self.active
            .get(workspace)
            .is_some_and(|a| a.server.is_running())
    }
    pub fn recording_failed(&self, workspace: &str) -> bool {
        self.active
            .get(workspace)
            .is_some_and(|a| a.server.recording_failed())
    }
    pub fn mode(&self, workspace: &str) -> Option<publication::Mode> {
        self.active.get(workspace).map(|a| a.metadata.mode)
    }
    pub fn address(&self, workspace: &str) -> Option<std::net::SocketAddr> {
        self.active.get(workspace).map(|a| a.server.address())
    }
    pub async fn prepare(
        &self,
        data: &Path,
        home: &Path,
        options: Options,
        helper: &Path,
        capabilities: ClaudeCapabilities,
    ) -> Result<PreparedRoute, String> {
        let workspace = app::claude::claude_workspace(data, home)?;
        let store = app::open_store(data).map_err(|e| e.message())?;
        let plan = publication::build(&store, &workspace.id, options)?;
        plan.settings.validate_client(capabilities)?;
        let old_metadata = config::route_metadata(data, &workspace)?;
        let (listener, address, reference, token) = if let Some(active) =
            self.active.get(&workspace.id)
        {
            if !active.server.is_running()
                || old_metadata
                    .as_ref()
                    .is_none_or(|m| m.token_ref != active.metadata.token_ref)
            {
                return Err("路由状态已变化，请先恢复配置".into());
            }
            if plan.options.port != 0 && plan.options.port != active.server.address().port() {
                return Err("修改端口前请先恢复并停止当前路由".into());
            }
            let token = store
                .local_token(&active.metadata.token_ref)
                .map_err(|_| "无法读取本地路由凭据")?
                .ok_or("本地路由凭据丢失，请先恢复")?;
            (
                None,
                active.server.address(),
                active.metadata.token_ref.clone(),
                token,
            )
        } else {
            if old_metadata.is_some() {
                return Err(
                    "此 Claude 工作空间有未恢复的路由配置，请先恢复；不会自动启动旧路由".into(),
                );
            }
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, plan.options.port))
                .await
                .map_err(|_| "无法绑定 Claude 路由端口，请检查占用")?;
            let address = listener
                .local_addr()
                .map_err(|_| "无法读取 Claude 路由地址")?;
            let reference = format!("router-{}", workspace.id);
            let token = Secret::new(format!("{}{}", crate::id::new_id()?, crate::id::new_id()?));
            (Some(listener), address, reference, token)
        };
        let generation = crate::id::new_id()?;
        let mut destinations = Vec::new();
        for model in &plan.models {
            let connection = plan
                .connections
                .iter()
                .find(|c| c.id == model.provider_id)
                .ok_or("模型连接不存在")?;
            let key = store
                .provider_api_key(&connection.id)
                .map_err(|_| "无法读取上游凭据")?
                .ok_or("连接凭据缺失")?;
            destinations.push(Destination {
                model: model.clone(),
                base_url: connection.base_url.clone(),
                auth: connection.settings.auth,
                key,
            });
        }
        let snapshot = Arc::new(Snapshot::new(
            generation.clone(),
            destinations,
            plan.aliases.clone(),
        )?);
        let metadata = RouteMetadata {
            address,
            token_ref: reference,
            mode: plan.options.mode,
            generation,
            providers: plan.connections.iter().map(|c| c.id.clone()).collect(),
        };
        let configuration = config::prepare_route(
            data,
            &workspace,
            &plan,
            metadata.clone(),
            helper,
            capabilities,
        )?;
        let mut preview = configuration.preview.clone();
        preview
            .notes
            .push("首次启用或更换端点后请新建 CLI 会话；运行中更新菜单后可用 /model 选择。".into());
        preview.notes.insert(
            0,
            format!(
                "本地端点：http://{address}；{} 个连接，{} 个显式模型绑定。",
                plan.connections.len(),
                plan.models.len()
            ),
        );
        preview.notes.push(
            "SwitchX 需保持运行；恢复冲突时保留监听和令牌。未知模型不会发送到默认连接。".into(),
        );
        preview
            .notes
            .push("本次不启用故障转移或协议转换；上游必须原生支持 Anthropic Messages。".into());
        for model in &plan.models {
            preview.notes.push(format!(
                "{} → {} / {}{}",
                model.public_id,
                model.provider_name,
                model.upstream_model,
                if model.menu {
                    ""
                } else {
                    "（默认或角色绑定）"
                }
            ));
        }
        Ok(PreparedRoute {
            preview,
            workspace,
            plan,
            snapshot,
            configuration,
            metadata,
            token,
            listener,
            old_metadata,
        })
    }
    pub async fn apply(
        &mut self,
        data: &Path,
        prepared: PreparedRoute,
        capabilities: ClaudeCapabilities,
    ) -> Result<(), String> {
        let PreparedRoute {
            workspace,
            plan,
            snapshot,
            configuration,
            metadata,
            token,
            listener,
            old_metadata,
            ..
        } = prepared;
        plan.settings.validate_client(capabilities)?;
        let store = app::open_store(data).map_err(|e| e.message())?;
        // Validate before allocating persistent resources, and again under the commit lock.
        validate(&store, &workspace, &plan, &snapshot)?;
        if config::route_metadata(data, &workspace)? != old_metadata {
            return Err("路由状态在预览后发生变化，请重新预览".into());
        }
        let creating = listener.is_some();
        if creating {
            // Lease first: a crash before or after token persistence always has a cleanup path.
            let owner = config::route_owner(&workspace)?;
            store.client_edit(|| {
                validate(&store, &workspace, &plan, &snapshot)?;
                configuration.validate_current(data)?;
                if config::route_metadata(data, &workspace)? != old_metadata {
                    return Err("路由已由另一个操作接管，请重新预览".into());
                }
                config::write_route_lease(data, &workspace, &metadata)?;
                store
                    .put_local_token(&metadata.token_ref, &token)
                    .map_err(|_| "无法保存本地路由凭据，请恢复以清理租约".into())
            })?;
            let server = router::Running::start(
                listener.unwrap(),
                token,
                snapshot.clone(),
                workspace.id.clone(),
                Some(app::open_store(data).map_err(|e| e.message())?),
            )?;
            self.active.insert(
                workspace.id.clone(),
                Active {
                    _owner: owner,
                    server,
                    metadata: metadata.clone(),
                    workspace: workspace.clone(),
                },
            );
        }
        let active = self
            .active
            .get_mut(&workspace.id)
            .ok_or("路由监听已消失，请重新预览")?;
        active.server.pause();
        let result = store.client_edit(|| {
            validate(&store, &workspace, &plan, &snapshot)?;
            if !creating && config::route_metadata(data, &workspace)? != old_metadata {
                return Err("路由状态在预览后发生变化，请重新预览".into());
            }
            store
                .save_claude_route_options(&workspace.id, &plan.options)
                .map_err(|_| "无法保存路由选项")?;
            configuration.apply(data).map(|_| ())
        });
        // Even an interrupted final journal update may have written the new configuration.
        let published = config::route_metadata(data, &workspace)?
            .is_some_and(|m| m.generation == metadata.generation);
        if result.is_ok() || published {
            active.server.publish(snapshot);
            active.metadata = metadata.clone();
        }
        active.server.resume();
        if result.is_ok() {
            config::write_route_lease(data, &workspace, &metadata)?;
        }
        result
    }
    pub async fn restore(
        &mut self,
        data: &Path,
        home: &Path,
        prepared: Prepared,
    ) -> Result<Vec<String>, String> {
        let workspace = app::claude::claude_workspace(data, home)?;
        let metadata = config::route_metadata(data, &workspace)?;
        let _owner = if metadata.is_some() && !self.active.contains_key(&workspace.id) {
            Some(config::route_owner(&workspace)?)
        } else {
            None
        };
        if let Some(active) = self.active.get(&workspace.id) {
            active.server.pause();
        }
        let result = app::claude::apply_claude_change(data, home, prepared);
        match &result {
            Ok(conflicts) if conflicts.is_empty() => {
                if let Some(metadata) = metadata {
                    let store = Store::open_recovery(&data.join("switchx.sqlite"))
                        .map_err(|_| "配置已恢复，但无法清理本地路由凭据")?;
                    let token = store
                        .local_token(&metadata.token_ref)
                        .map_err(|_| "无法核对待清理路由令牌")?;
                    if store.delete_local_token(&metadata.token_ref).is_err() {
                        if let Some(active) = self.active.get(&workspace.id) {
                            active.server.resume();
                        }
                        return Err(
                            "配置已恢复，但令牌清理失败；监听和恢复租约已保留，请重试恢复".into(),
                        );
                    }
                    if let Err(error) = config::clear_route_lease(data, &workspace) {
                        if let Some(token) = &token {
                            store
                                .put_local_token(&metadata.token_ref, token)
                                .map_err(|_| "恢复租约和令牌清理失败，请保留资料目录")?;
                        }
                        if let Some(active) = self.active.get(&workspace.id) {
                            active.server.resume();
                        }
                        return Err(error);
                    }
                }
                if let Some(active) = self.active.remove(&workspace.id) {
                    active.server.stop().await;
                }
            }
            _ => {
                if let Some(active) = self.active.get(&workspace.id) {
                    active.server.resume();
                }
            }
        }
        result
    }
    /// Restore every live Claude workspace before stopping any of its listeners.
    /// Direct-only configurations remain untouched on app exit.
    pub async fn restore_for_exit(&mut self, data: &Path) -> Result<(), String> {
        let workspaces: Vec<_> = self.active.values().map(|a| a.workspace.clone()).collect();
        for workspace in workspaces {
            let prepared = config::prepare_restore(data, &workspace, false)?;
            let conflicts = self.restore(data, &workspace.key.home, prepared).await?;
            if !conflicts.is_empty() {
                return Err(format!(
                    "Claude 配置有外部修改，路由继续运行：{}",
                    conflicts.join("、")
                ));
            }
        }
        Ok(())
    }
}
impl PreparedRoute {
    pub fn home(&self) -> &Path {
        &self.workspace.key.home
    }
    pub fn workspace_id(&self) -> &str {
        &self.workspace.id
    }
}
fn validate(
    store: &Store,
    workspace: &ClientWorkspace,
    plan: &Plan,
    snapshot: &Snapshot,
) -> Result<(), String> {
    // Also catches a new/removed aggregate connection after preview.
    let current = publication::build(store, &workspace.id, plan.options.clone())?;
    if current.connections != plan.connections
        || current
            .models
            .iter()
            .map(|m| &m.public_id)
            .collect::<Vec<_>>()
            != plan.models.iter().map(|m| &m.public_id).collect::<Vec<_>>()
    {
        return Err("连接或模型在预览后发生变化，请重新预览".into());
    }
    for connection in &plan.connections {
        let key = store
            .provider_api_key(&connection.id)
            .map_err(|_| "无法核对连接凭据")?
            .ok_or("连接凭据已移除")?;
        if snapshot
            .destinations
            .values()
            .filter(|d| d.model.provider_id == connection.id)
            .any(|d| !crate::control::verify(d.key.expose(), key.expose()))
        {
            return Err("连接凭据在预览后发生变化，请重新预览".into());
        }
    }
    Ok(())
}
