//! User settings transactions. Public previews and journals contain no secrets;
//! complete configuration images live only in private backup files.

use super::{AuthMode, Connection, PickerPolicy, client, json_edit};
use crate::{
    clients::{ClaudeCapabilities, ClientKind, ClientWorkspace, WorkspaceKey},
    credentials::Secret,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const LIMIT: usize = 2 * 1024 * 1024;
const AUTH_ENV: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_API_KEY_HELPER_TTL_MS",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
];
const OAUTH_ENV: &[&str] = &[
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_OAUTH_SCOPES",
];
const MODEL_ENV: &[&str] = &[
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL_FORCE",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Journal {
    version: u32,
    workspace_id: String,
    target: PathBuf,
    transaction: String,
    provider_id: String,
    provider_name: String,
    protected_providers: Vec<String>,
    baseline_exists: bool,
    observed_exists: bool,
    paths: Vec<Vec<String>>,
    pending: bool,
    #[serde(default)]
    route: Option<RouteMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteMetadata {
    pub address: std::net::SocketAddr,
    pub token_ref: String,
    pub mode: super::publication::Mode,
    pub generation: String,
    pub providers: Vec<String>,
}
impl RouteMetadata {
    fn validate(&self) -> Result<(), String> {
        if self.address.ip() != std::net::Ipv4Addr::LOCALHOST
            || self.address.port() == 0
            || !self.token_ref.strip_prefix("router-").is_some_and(valid_id)
            || !valid_id(&self.generation)
        {
            return Err("Claude 路由恢复记录无效".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub target: PathBuf,
    pub provider: String,
    pub operation: &'static str,
    pub fields: Vec<String>,
    pub models: Vec<Value>,
    pub notes: Vec<String>,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub managed: bool,
    pub pending: bool,
    pub provider_id: String,
    pub provider_name: String,
    pub mode: String,
    pub endpoint: Option<String>,
}

struct Image(Option<Secret>);
impl Image {
    fn text(&self) -> &str {
        self.0.as_ref().map_or("{}", Secret::expose)
    }
    fn exists(&self) -> bool {
        self.0.is_some()
    }
    fn bytes(&self) -> Option<&[u8]> {
        self.0.as_ref().map(|value| value.expose().as_bytes())
    }
    fn value(&self) -> Result<Value, String> {
        json_edit::value(self.text())
    }
    fn copy(&self) -> Self {
        Self(
            self.0
                .as_ref()
                .map(|value| Secret::new(value.expose().into())),
        )
    }
    fn matches(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}

pub struct Prepared {
    pub preview: Preview,
    workspace: ClientWorkspace,
    observed: Image,
    baseline: Image,
    desired: Image,
    journal: Option<Journal>,
    paths: Vec<Vec<String>>,
    restore: bool,
    keep_conflicts: bool,
    provider: Option<Connection>,
    key: Option<Secret>,
    route: Option<RouteMetadata>,
}

pub fn has_recovery(data_dir: &Path) -> bool {
    match fs::read_dir(data_dir.join("claude")) {
        Ok(mut entries) => entries.any(|entry| {
            entry.map_or(true, |entry| {
                entry.path().join("active.json").exists()
                    || entry.path().join("route-lease.json").exists()
            })
        }),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn directory(data_dir: &Path, workspace_id: &str) -> Result<PathBuf, String> {
    if !data_dir.is_absolute() || !valid_id(workspace_id) {
        return Err("工作空间身份无效".into());
    }
    let root = data_dir.join("claude");
    let dir = root.join(workspace_id);
    for path in [&root, &dir] {
        if fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            return Err("Claude 状态目录必须是普通目录".into());
        }
    }
    Ok(dir)
}

fn read(path: &Path) -> Result<Image, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("配置或备份必须是普通文件，不能是符号链接".into())
        }
        Ok(metadata) if metadata.len() > LIMIT as u64 => Err("配置文件超过 2 MiB 限制".into()),
        Ok(_) => {
            let mut bytes = Vec::new();
            File::open(path)
                .and_then(|file| file.take(LIMIT as u64 + 1).read_to_end(&mut bytes))
                .map_err(|_| "无法读取配置或备份")?;
            if bytes.len() > LIMIT {
                return Err("配置文件过大".into());
            }
            let text = String::from_utf8(bytes).map_err(|_| "配置必须使用 UTF-8 编码")?;
            Ok(Image(Some(Secret::new(text))))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Image(None)),
        Err(_) => Err("无法读取配置或备份".into()),
    }
}

fn journal(data_dir: &Path, workspace: &ClientWorkspace) -> Result<Option<Journal>, String> {
    let image = read(&directory(data_dir, &workspace.id)?.join("active.json"))?;
    if !image.exists() {
        return Ok(None);
    }
    let journal: Journal = serde_json::from_str(image.text()).map_err(|_| "Claude 恢复记录无效")?;
    if journal.version != 1
        || journal.workspace_id != workspace.id
        || journal.target != workspace.key.config_path()
        || !valid_id(&journal.transaction)
        || journal.paths.iter().any(|path| !owned_path(path))
    {
        return Err("Claude 恢复记录与目标不匹配".into());
    }
    if let Some(route) = &journal.route {
        route.validate()?;
        if route.token_ref != format!("router-{}", workspace.id) {
            return Err("路由令牌引用不属于当前工作空间".into());
        }
    }
    Ok(Some(journal))
}

fn owned_path(path: &[String]) -> bool {
    match path {
        [field] => ["apiKeyHelper", "model", "modelPicker"].contains(&field.as_str()),
        [env, field] if env == "env" => {
            AUTH_ENV.contains(&field.as_str())
                || OAUTH_ENV.contains(&field.as_str())
                || MODEL_ENV.contains(&field.as_str())
        }
        _ => false,
    }
}

fn snapshot(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    journal: &Journal,
    kind: &str,
) -> Result<Image, String> {
    let image = read(
        &directory(data_dir, &workspace.id)?.join(format!("{}-{kind}.json", journal.transaction)),
    )?;
    if !image.exists() {
        return Err("Claude 恢复备份缺失，请保留现有配置并检查备份目录".into());
    }
    image.value()?;
    if (kind == "baseline" && !journal.baseline_exists)
        || (kind == "observed" && !journal.observed_exists)
    {
        Ok(Image(None))
    } else {
        Ok(image)
    }
}

pub fn status(data_dir: &Path, workspace: &ClientWorkspace) -> Result<Status, String> {
    let journal = journal(data_dir, workspace)?;
    let lease = route_lease(data_dir, workspace)?;
    let route = journal
        .as_ref()
        .and_then(|j| j.route.as_ref())
        .or(lease.as_ref());
    Ok(Status {
        managed: journal.is_some() || lease.is_some(),
        pending: journal.as_ref().is_some_and(|j| j.pending)
            || (lease.is_some() && journal.as_ref().is_none_or(|j| j.route.is_none())),
        provider_id: journal
            .as_ref()
            .map(|j| j.provider_id.clone())
            .unwrap_or_default(),
        provider_name: journal
            .as_ref()
            .map(|j| j.provider_name.clone())
            .unwrap_or_else(|| "待清理的路由".into()),
        mode: route
            .map(|r| r.mode.as_str())
            .unwrap_or(if journal.is_some() { "direct" } else { "none" })
            .into(),
        endpoint: route.map(|r| format!("http://{}", r.address)),
    })
}
pub fn route_metadata(
    data_dir: &Path,
    workspace: &ClientWorkspace,
) -> Result<Option<RouteMetadata>, String> {
    Ok(journal(data_dir, workspace)?
        .and_then(|j| j.route)
        .or(route_lease(data_dir, workspace)?))
}
fn route_lease(
    data_dir: &Path,
    workspace: &ClientWorkspace,
) -> Result<Option<RouteMetadata>, String> {
    let image = read(&directory(data_dir, &workspace.id)?.join("route-lease.json"))?;
    if !image.exists() {
        return Ok(None);
    }
    let route: RouteMetadata =
        serde_json::from_str(image.text()).map_err(|_| "Claude 路由租约无效")?;
    route.validate()?;
    if route.token_ref != format!("router-{}", workspace.id) {
        return Err("路由租约不属于当前工作空间".into());
    }
    Ok(Some(route))
}
pub(crate) fn write_route_lease(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    route: &RouteMetadata,
) -> Result<(), String> {
    route.validate()?;
    private_dir(&data_dir.join("claude"))?;
    let dir = directory(data_dir, &workspace.id)?;
    private_dir(&dir)?;
    let target = dir.join("route-lease.json");
    let text = serde_json::to_string(route).map_err(|_| "无法保存路由租约")?;
    replace(&target, &Image(Some(Secret::new(text))), &read(&target)?)
}
pub(crate) fn clear_route_lease(
    data_dir: &Path,
    workspace: &ClientWorkspace,
) -> Result<(), String> {
    let target = directory(data_dir, &workspace.id)?.join("route-lease.json");
    if target.exists() {
        fs::remove_file(&target).map_err(|_| "无法清理路由租约")?;
        crate::codex::route_switch::sync_parent(&target)?;
    }
    Ok(())
}

pub fn protects(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    provider_id: &str,
) -> Result<bool, String> {
    Ok(journal(data_dir, workspace)?
        .is_some_and(|j| j.protected_providers.iter().any(|id| id == provider_id)))
}

pub fn read_settings(workspace: &ClientWorkspace) -> Result<Value, String> {
    read(&workspace.key.config_path())?.value()
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).into()).collect()
}
fn restore_fields(
    current: &Image,
    baseline: &Image,
    applied: &Image,
    observed: Option<&Image>,
    paths: &[Vec<String>],
) -> Result<(Image, Vec<String>), String> {
    let current_value = current.value()?;
    let base = baseline.value()?;
    let applied = applied.value()?;
    let observed = observed.map(Image::value).transpose()?;
    let mut text = current.text().to_owned();
    let mut conflicts = Vec::new();
    for field in paths {
        let now = json_edit::get(&current_value, field);
        let original = json_edit::get(&base, field);
        if now == original {
            continue;
        }
        if now == json_edit::get(&applied, field)
            || observed
                .as_ref()
                .is_some_and(|old| now == json_edit::get(old, field))
        {
            text = json_edit::set(&text, field, original)?;
        } else {
            conflicts.push(field.join("."));
        }
    }
    let value = json_edit::value(&text)?;
    if base.get("env").is_none()
        && value
            .get("env")
            .and_then(Value::as_object)
            .is_some_and(|env| env.is_empty())
    {
        text = json_edit::set(&text, &path(&["env"]), None)?;
    }
    Ok((Image(Some(Secret::new(text))), conflicts))
}

pub fn prepare(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    connection: Connection,
    key: Option<Secret>,
    helper: &Path,
    capabilities: ClaudeCapabilities,
) -> Result<Prepared, String> {
    prepare_inner(
        data_dir,
        workspace,
        connection,
        key,
        helper,
        capabilities,
        None,
    )
}

pub fn prepare_route(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    plan: &super::publication::Plan,
    route: RouteMetadata,
    helper: &Path,
    capabilities: ClaudeCapabilities,
) -> Result<Prepared, String> {
    route.validate()?;
    let connection = Connection {
        id: workspace.id.clone(),
        workspace_id: workspace.id.clone(),
        name: if route.mode == super::publication::Mode::Aggregate {
            "Claude 模型聚合"
        } else {
            "Claude 模型路由"
        }
        .into(),
        base_url: format!("http://{}", route.address),
        settings: plan.settings.clone(),
        revision: 0,
        has_key: true,
    };
    prepare_inner(
        data_dir,
        workspace,
        connection,
        None,
        helper,
        capabilities,
        Some(route),
    )
}
#[allow(clippy::too_many_arguments)]
fn prepare_inner(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    connection: Connection,
    key: Option<Secret>,
    helper: &Path,
    capabilities: ClaudeCapabilities,
    route: Option<RouteMetadata>,
) -> Result<Prepared, String> {
    if workspace.key.client != ClientKind::ClaudeCode {
        return Err("目标不是 Claude Code 工作空间".into());
    }
    connection.settings.validate_client(capabilities)?;
    if route.is_none() && connection.settings.protocol == super::UpstreamProtocol::Responses {
        return Err("Responses 连接需要 SwitchX 路由转换，不能作为 Claude 直连启用".into());
    }
    if connection.workspace_id != workspace.id {
        return Err("连接不属于当前工作空间".into());
    }
    let observed = read(&workspace.key.config_path())?;
    observed.value()?;
    let previous = journal(data_dir, workspace)?;
    if route.is_none() && route_metadata(data_dir, workspace)?.is_some() {
        return Err("请先恢复并停止此 Claude 路由，再启用直连".into());
    }
    let (baseline, mut paths) = if let Some(previous) = &previous {
        if previous.pending {
            return Err("上次配置写入尚未完成，请先恢复".into());
        }
        let old_base = snapshot(data_dir, workspace, previous, "baseline")?;
        let old_applied = snapshot(data_dir, workspace, previous, "applied")?;
        let (restored, conflicts) =
            restore_fields(&observed, &old_base, &old_applied, None, &previous.paths)?;
        if !conflicts.is_empty() {
            return Err(format!(
                "受管字段已被外部修改，请先恢复或保留外部值后再切换：{}",
                conflicts.join("、")
            ));
        }
        let base = if observed.matches(&old_applied) {
            old_base
        } else {
            restored
        };
        (
            base,
            previous.paths.iter().cloned().collect::<BTreeSet<_>>(),
        )
    } else {
        (observed.copy(), BTreeSet::new())
    };
    let mut desired = baseline.text().to_owned();
    let mut changes: Vec<(Vec<String>, Option<Value>)> = AUTH_ENV
        .iter()
        .map(|key| (path(&["env", key]), None))
        .collect();
    changes.push((path(&["apiKeyHelper"]), None));
    let settings = &connection.settings;
    if settings.auth != AuthMode::Native {
        changes.extend(OAUTH_ENV.iter().map(|key| (path(&["env", key]), None)));
        changes.push((
            path(&["env", "ANTHROPIC_BASE_URL"]),
            Some(connection.base_url.clone().into()),
        ));
        let credential = key.as_ref();
        if route.is_none() && credential.is_none() {
            return Err("连接凭据不存在，请重新输入".into());
        }
        match settings.auth {
            AuthMode::Helper => changes.push((
                path(&["apiKeyHelper"]),
                Some(
                    if let Some(route) = &route {
                        client::local_token_command(helper, &route.token_ref, data_dir)?
                    } else {
                        client::helper_command(helper, &connection.id, data_dir)?
                    }
                    .into(),
                ),
            )),
            AuthMode::ApiKey => changes.push((
                path(&["env", "ANTHROPIC_API_KEY"]),
                Some(credential.ok_or("连接凭据不存在")?.expose().into()),
            )),
            AuthMode::Bearer => changes.push((
                path(&["env", "ANTHROPIC_AUTH_TOKEN"]),
                Some(credential.ok_or("连接凭据不存在")?.expose().into()),
            )),
            AuthMode::Native => unreachable!(),
        }
    }
    changes.extend(MODEL_ENV.iter().map(|key| (path(&["env", key]), None)));
    changes.push((
        path(&["model"]),
        (!settings.default_model.is_empty()).then(|| settings.default_model.clone().into()),
    ));
    for (role, model) in settings.roles.fields() {
        if !model.is_empty() {
            changes.push((path(&["env", role]), Some(model.into())));
        }
    }
    if settings.picker != PickerPolicy::Preserve {
        let options: Vec<_> = settings
            .models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.picker_value())
            .collect();
        changes.push((path(&["modelPicker"]),Some(json!({"replaceBuiltInOptions": settings.picker == PickerPolicy::Replace, "options": options}))));
    }
    for (field, value) in changes {
        desired = json_edit::set(&desired, &field, value.as_ref())?;
        paths.insert(field);
    }
    if desired.len() > LIMIT {
        return Err("生成的配置超过 2 MiB 限制，请减少模型或配置内容".into());
    }
    let after = json_edit::value(&desired)?;
    let before = observed.value()?;
    let fields: Vec<_> = paths
        .iter()
        .filter(|p| json_edit::get(&before, p) != json_edit::get(&after, p))
        .map(|p| p.join("."))
        .collect();
    let mut notes = vec![
        "仅更新用户级 settings.json；权限、Hooks、MCP 和其他用户字段保留。".into(),
        "模型菜单可更新；当前会话的模型请用 /model 选择，并用 /status 核对实际来源。".into(),
        "项目设置、启动参数、环境变量或组织设置可能影响最终生效值。".into(),
    ];
    if settings.picker == PickerPolicy::Preserve {
        notes.push("保留接管前的模型菜单；本连接保存的列表不会发布。".into());
    }
    if settings.auth == AuthMode::Helper {
        notes.push("通过 SwitchX helper 读取凭据；移动或删除 SwitchX 前请恢复配置。".into());
    } else if settings.auth != AuthMode::Native {
        notes.push("凭据将写入权限受限的 Claude 用户配置，预览不显示其内容。".into());
    }
    if before.get("availableModels").is_some() {
        notes.push("检测到 availableModels 限制，CLI 可能过滤菜单中的条目。".into());
    }
    let preview = Preview {
        target: workspace.key.config_path(),
        provider: connection.name.clone(),
        operation: route.as_ref().map_or("apply", |r| r.mode.as_str()),
        fields,
        models: if settings.picker == PickerPolicy::Preserve {
            Vec::new()
        } else {
            settings
                .models
                .iter()
                .filter(|m| m.enabled)
                .map(|m| m.picker_value())
                .collect()
        },
        notes,
        conflicts: Vec::new(),
    };
    Ok(Prepared {
        preview,
        workspace: workspace.clone(),
        observed,
        baseline,
        desired: Image(Some(Secret::new(desired))),
        journal: previous,
        paths: paths.into_iter().collect(),
        restore: false,
        keep_conflicts: false,
        provider: route.is_none().then_some(connection),
        key,
        route,
    })
}

pub fn prepare_restore(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    keep_conflicts: bool,
) -> Result<Prepared, String> {
    let saved = journal(data_dir, workspace)?;
    if saved.is_none()
        && let Some(route) = route_lease(data_dir, workspace)?
    {
        let observed = read(&workspace.key.config_path())?;
        return Ok(Prepared {
            preview: Preview {
                target: workspace.key.config_path(),
                provider: "未完成的 Claude 路由".into(),
                operation: "restore",
                fields: Vec::new(),
                models: Vec::new(),
                notes: vec!["清理未完成的路由监听与本地令牌；客户端文件保持原状。".into()],
                conflicts: Vec::new(),
            },
            workspace: workspace.clone(),
            baseline: observed.copy(),
            desired: observed.copy(),
            observed,
            journal: None,
            paths: Vec::new(),
            restore: true,
            keep_conflicts,
            provider: None,
            key: None,
            route: Some(route),
        });
    }
    let journal = saved.ok_or("这个 Claude 工作空间没有待恢复配置")?;
    let observed = read(&workspace.key.config_path())?;
    let baseline = snapshot(data_dir, workspace, &journal, "baseline")?;
    let applied = snapshot(data_dir, workspace, &journal, "applied")?;
    let before_apply = snapshot(data_dir, workspace, &journal, "observed")?;
    let (mut desired, conflicts) = restore_fields(
        &observed,
        &baseline,
        &applied,
        journal.pending.then_some(&before_apply),
        &journal.paths,
    )?;
    if observed.matches(&applied) || (journal.pending && observed.matches(&before_apply)) {
        desired = baseline.copy();
    }
    let preview = Preview {
        target: workspace.key.config_path(),
        provider: journal.provider_name.clone(),
        operation: "restore",
        fields: journal.paths.iter().map(|p| p.join(".")).collect(),
        models: Vec::new(),
        notes: vec![if keep_conflicts {
            "保留当前外部修改，恢复其他受管字段并结束管理。".into()
        } else {
            "恢复接入前的受管字段；外部修改将保留并报告。".into()
        }],
        conflicts,
    };
    Ok(Prepared {
        preview,
        workspace: workspace.clone(),
        observed,
        baseline,
        desired,
        journal: Some(journal.clone()),
        paths: journal.paths.clone(),
        route: journal.route.clone(),
        restore: true,
        keep_conflicts,
        provider: None,
        key: None,
    })
}

impl Prepared {
    pub fn is_route_publication(&self) -> bool {
        self.route.is_some() && !self.restore
    }
    pub fn route_generation(&self) -> Option<&str> {
        self.route.as_ref().map(|r| r.generation.as_str())
    }
    pub fn validate_capabilities(&self, capabilities: ClaudeCapabilities) -> Result<(), String> {
        self.provider.as_ref().map_or(Ok(()), |provider| {
            provider.settings.validate_client(capabilities)
        })
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace.id
    }
    pub fn provider(&self) -> Option<&Connection> {
        self.provider.as_ref()
    }
    pub fn key_matches(&self, key: Option<&Secret>) -> bool {
        self.key.as_ref().map(Secret::expose) == key.map(Secret::expose)
    }

    pub(crate) fn validate_current(&self, data_dir: &Path) -> Result<(), String> {
        if WorkspaceKey::new(ClientKind::ClaudeCode, &self.workspace.key.home)?
            != self.workspace.key
            || !read(&self.workspace.key.config_path())?.matches(&self.observed)
            || journal(data_dir, &self.workspace)? != self.journal
        {
            return Err("配置、目录或恢复记录在预览后发生变化，请重新预览".into());
        }
        if !self.restore
            && self.route.is_none()
            && route_metadata(data_dir, &self.workspace)?.is_some()
        {
            return Err("请先恢复并停止 Claude 路由".into());
        }
        Ok(())
    }

    pub fn apply(self, data_dir: &Path) -> Result<Vec<String>, String> {
        if WorkspaceKey::new(ClientKind::ClaudeCode, &self.workspace.key.home)?
            != self.workspace.key
        {
            return Err("配置目录的实际位置已变化，请重新预览".into());
        }
        let target = self.workspace.key.config_path();
        private_dir(&self.workspace.key.home)?;
        let _lock = lock(&target)?;
        self.validate_current(data_dir)?;
        if !read(&target)?.matches(&self.observed)
            || journal(data_dir, &self.workspace)? != self.journal
        {
            return Err("配置或恢复记录在预览后发生变化，请重新预览".into());
        }
        let directory = directory(data_dir, &self.workspace.id)?;
        if self.restore {
            replace(&target, &self.desired, &self.observed)?;
            if self.journal.is_some() && (self.preview.conflicts.is_empty() || self.keep_conflicts)
            {
                fs::remove_file(directory.join("active.json"))
                    .map_err(|_| "配置已恢复，但恢复记录清理失败，请重试")?;
                crate::codex::route_switch::sync_parent(&directory.join("active.json"))?;
            }
            return Ok(if self.keep_conflicts {
                Vec::new()
            } else {
                self.preview.conflicts
            });
        }
        private_dir(&data_dir.join("claude"))?;
        private_dir(&directory)?;
        let transaction = crate::id::new_id()?;
        for (kind, image) in [
            ("baseline", &self.baseline),
            ("observed", &self.observed),
            ("applied", &self.desired),
        ] {
            write_new(
                &directory.join(format!("{transaction}-{kind}.json")),
                image.text().as_bytes(),
            )?;
        }
        let connection = self.provider;
        let provider_id = connection
            .as_ref()
            .map(|c| c.id.clone())
            .unwrap_or_default();
        let provider_name = connection
            .as_ref()
            .map(|c| c.name.clone())
            .unwrap_or_else(|| self.preview.provider.clone());
        let mut protected = self
            .journal
            .as_ref()
            .map(|j| j.protected_providers.clone())
            .unwrap_or_default();
        if let Some(route) = &self.route {
            protected.extend(route.providers.iter().cloned());
        } else if !protected.contains(&provider_id) {
            protected.push(provider_id.clone());
        }
        let mut journal = Journal {
            version: 1,
            workspace_id: self.workspace.id.clone(),
            target: target.clone(),
            transaction,
            provider_id: provider_id.clone(),
            provider_name,
            protected_providers: protected,
            baseline_exists: self.baseline.exists(),
            observed_exists: self.observed.exists(),
            paths: self.paths,
            pending: true,
            route: self.route.clone(),
        };
        write_journal(&directory, &journal)?;
        replace(&target, &self.desired, &self.observed)?;
        journal.pending = false;
        journal.protected_providers = self
            .route
            .map(|r| r.providers)
            .unwrap_or_else(|| vec![provider_id]);
        write_journal(&directory, &journal)?;
        Ok(Vec::new())
    }
}

pub(crate) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock(target: &Path) -> Result<Lock, String> {
    lock_file(target.with_file_name(".switchx-claude.lock"))
}
pub(crate) fn route_owner(workspace: &ClientWorkspace) -> Result<Lock, String> {
    private_dir(&workspace.key.home)?;
    lock_file(workspace.key.home.join(".switchx-claude-route.lock"))
}
fn lock_file(path: PathBuf) -> Result<Lock, String> {
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err("配置锁文件无效".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|_| "无法打开 Claude 配置锁")?;
    file.try_lock()
        .map_err(|_| "其他 SwitchX 实例正在修改此 Claude 配置")?;
    Ok(Lock(file))
}

fn private_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("配置或备份目录无效".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(path)
                .map_err(|_| "无法创建私有配置目录".into())
        }
        Err(_) => Err("无法访问配置目录".into()),
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| "无法创建配置备份")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "无法保存配置备份")?;
    crate::codex::route_switch::sync_parent(path)
}
fn replace(path: &Path, desired: &Image, expected: &Image) -> Result<(), String> {
    if !read(path)?.matches(expected) {
        return Err("配置在写入前发生变化，请重新预览".into());
    }
    if let Some(bytes) = desired.bytes() {
        let temporary = path.with_file_name(format!(".switchx-{}.tmp", crate::id::new_id()?));
        write_new(&temporary, bytes)?;
        let result = (|| {
            if !read(path)?.matches(expected) {
                return Err("配置在写入前发生变化，请重新预览".into());
            }
            fs::rename(&temporary, path).map_err(|_| "无法原子替换配置文件")?;
            crate::codex::route_switch::sync_parent(path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    } else {
        if expected.exists() {
            fs::remove_file(path).map_err(|_| "无法恢复原先不存在的配置文件")?;
        }
        crate::codex::route_switch::sync_parent(path)
    }
}
fn write_journal(directory: &Path, journal: &Journal) -> Result<(), String> {
    let path = directory.join("active.json");
    let text = serde_json::to_string_pretty(journal).map_err(|_| "无法生成恢复记录")?;
    replace(&path, &Image(Some(Secret::new(text))), &read(&path)?)
}
