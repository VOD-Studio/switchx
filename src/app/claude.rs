//! Claude operations used by both the window and agent controls.

use super::open_store;
#[cfg(test)]
mod tests;
use crate::{
    claude::{
        self, AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy, Roles,
        UpstreamProtocol,
    },
    clients::{ClientKind, ClientWorkspace, WorkspaceKey},
    credentials::Secret,
    storage::Store,
};
use serde_json::Value;
use std::path::Path;

pub struct ClaudeSnapshot {
    pub workspace: ClientWorkspace,
    pub connections: Vec<Connection>,
    pub status: claude::config::Status,
    pub configuration_error: Option<String>,
    pub cli_path: String,
    pub route_options: claude::publication::Options,
    pub route_models: Vec<claude::publication::PublishedModel>,
    pub route_error: String,
    pub recent_requests: Vec<String>,
}

pub fn claude_workspace(data_dir: &Path, home: &Path) -> Result<ClientWorkspace, String> {
    let key = WorkspaceKey::new(ClientKind::ClaudeCode, home)?;
    if Store::needs_recovery_before_migration(&data_dir.join("switchx.sqlite"))
        .map_err(|_| "无法检查恢复状态")?
    {
        return Store::open_recovery(&data_dir.join("switchx.sqlite"))
            .map_err(|_| "无法打开恢复资料")?
            .client_workspaces()
            .map_err(|_| "请先恢复旧版配置")?
            .into_iter()
            .find(|w| w.key == key)
            .ok_or_else(|| "请先恢复旧版配置".into());
    }
    super::remember_client_workspace(data_dir, ClientKind::ClaudeCode, &key.home)
}

pub fn load_claude(data_dir: &Path, home: &Path) -> Result<ClaudeSnapshot, String> {
    let workspace = claude_workspace(data_dir, home)?;
    // Recovery remains readable before any future schema upgrade is allowed.
    let store =
        Store::open_recovery(&data_dir.join("switchx.sqlite")).map_err(|_| "无法读取客户端资料")?;
    let status = claude::config::status(data_dir, &workspace)?;
    let connections = store
        .claude_connections(&workspace.id)
        .map_err(|_| "无法读取 Claude 连接")?;
    let configuration_error = claude::config::read_settings(&workspace).err();
    let cli_path = store
        .client_cli(&workspace.id)
        .map_err(|_| "无法读取 CLI 路径")?
        .unwrap_or_default();
    let route_options = store
        .claude_route_options(&workspace.id)
        .unwrap_or_default();
    let plan = claude::publication::build(&store, &workspace.id, route_options.clone());
    let (route_options, route_models, route_error) = match plan {
        Ok(plan) => (plan.options, plan.models, String::new()),
        Err(error) => (route_options, Vec::new(), error),
    };
    let recent_requests = store
        .claude_requests(&workspace.id, 20)
        .unwrap_or_default()
        .into_iter()
        .take(20)
        .map(|r| {
            let provider = connections
                .iter()
                .find(|c| Some(c.id.as_str()) == r.provider_id.as_deref())
                .map(|c| c.name.as_str())
                .unwrap_or("未知连接");
            format!(
                "{} · {} · {} · {}",
                r.status.label(),
                provider,
                r.upstream_model.as_deref().unwrap_or("未选模型"),
                super::format_duration(r.duration_ms)
            )
        })
        .collect();
    Ok(ClaudeSnapshot {
        route_options,
        route_models,
        route_error,
        recent_requests,
        workspace,
        connections,
        status,
        configuration_error,
        cli_path,
    })
}

pub fn validate_claude_endpoint(base: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(base.trim()).map_err(|_| "API 地址无效")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || !(url.scheme() == "https"
            || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1")))
    {
        return Err(
            "API 地址须使用 HTTPS 或本机 127.0.0.1 HTTP，且不能包含凭据、查询参数或片段".into(),
        );
    }
    if url.path().ends_with("/messages") || url.path().ends_with("/models") {
        return Err("请填写 API 根地址，不要包含 messages 或 models 接口路径".into());
    }
    Ok(url.as_str().trim_end_matches('/').into())
}

pub fn save_claude_connection(
    data_dir: &Path,
    home: &Path,
    mut connection: Connection,
    key: Option<Secret>,
    expected: Option<i64>,
) -> Result<Connection, String> {
    let workspace = claude_workspace(data_dir, home)?;
    if connection.workspace_id != workspace.id {
        return Err("编辑器的工作空间已变化，请重新打开".into());
    }
    connection.name = connection.name.trim().to_owned();
    if connection.name.is_empty()
        || connection.name.len() > 240
        || connection.name.chars().any(char::is_control)
    {
        return Err("连接名称不能为空、过长或包含控制字符".into());
    }
    connection.settings.validate()?;
    connection.base_url = if connection.settings.auth == AuthMode::Native {
        String::new()
    } else {
        validate_claude_endpoint(&connection.base_url)?
    };
    if let Some(key) = &key {
        if key.expose().is_empty()
            || key.expose().len() > 16384
            || key.expose().chars().any(char::is_control)
        {
            return Err("API Key 或 Token 无效".into());
        }
        if connection.settings.auth == AuthMode::Native {
            return Err("官方登录不需要填写 API Key".into());
        }
    }
    let store = open_store(data_dir).map_err(|e| e.message())?;
    store.client_edit(|| {
        if expected.is_some() && claude::config::status(data_dir, &workspace)?.managed {
            return Err("此 Claude 工作空间正在托管配置，请先恢复再修改已有连接".into());
        }
        store.write_claude_connection(&connection, key.as_ref(), expected)
    })
}

pub fn delete_claude_connection(
    data_dir: &Path,
    home: &Path,
    id: &str,
    revision: i64,
) -> Result<(), String> {
    let workspace = claude_workspace(data_dir, home)?;
    let store = open_store(data_dir).map_err(|e| e.message())?;
    store.client_edit(|| {
        if claude::config::status(data_dir, &workspace)?.managed {
            return Err("请先恢复此 Claude 工作空间的配置，再删除连接".into());
        }
        store.remove_claude_connection(&workspace.id, id, revision)
    })
}

pub fn save_claude_cli(data_dir: &Path, home: &Path, executable: &str) -> Result<(), String> {
    let workspace = claude_workspace(data_dir, home)?;
    if !executable.trim().is_empty() {
        claude::client::executable(Some(executable.trim()))?;
    }
    open_store(data_dir)
        .map_err(|e| e.message())?
        .save_client_cli(&workspace.id, executable.trim())
        .map_err(|_| "无法保存 CLI 路径".into())
}

pub async fn inspect_claude_cli(
    data_dir: &Path,
    home: &Path,
) -> Result<claude::client::CliInfo, String> {
    let workspace = claude_workspace(data_dir, home)?;
    let path = open_store(data_dir)
        .map_err(|e| e.message())?
        .client_cli(&workspace.id)
        .map_err(|_| "无法读取 CLI 路径")?;
    claude::client::inspect(path.as_deref(), &workspace.key.home).await
}

pub async fn preview_claude_direct(
    data_dir: &Path,
    home: &Path,
    id: &str,
    helper: &Path,
) -> Result<claude::config::Prepared, String> {
    let workspace = claude_workspace(data_dir, home)?;
    let cli = inspect_claude_cli(data_dir, home).await?;
    prepare_claude_direct(data_dir, &workspace, id, helper, cli.capabilities)
}

/// The same preparation used after a CLI check, also usable by isolated probes.
pub fn prepare_claude_direct(
    data_dir: &Path,
    workspace: &ClientWorkspace,
    id: &str,
    helper: &Path,
    capabilities: crate::clients::ClaudeCapabilities,
) -> Result<claude::config::Prepared, String> {
    let store = open_store(data_dir).map_err(|e| e.message())?;
    let connection = store
        .claude_connection(&workspace.id, id)
        .map_err(|_| "无法读取连接")?
        .ok_or("连接不属于此 Claude 工作空间")?;
    let key = if connection.settings.auth == AuthMode::Native {
        None
    } else {
        store.provider_api_key(id).map_err(|_| "无法读取连接凭据")?
    };
    claude::config::prepare(data_dir, workspace, connection, key, helper, capabilities)
}

pub fn preview_claude_restore(
    data_dir: &Path,
    home: &Path,
    keep_conflicts: bool,
) -> Result<claude::config::Prepared, String> {
    let workspace = claude_workspace(data_dir, home)?;
    claude::config::prepare_restore(data_dir, &workspace, keep_conflicts)
}

pub fn apply_claude_change(
    data_dir: &Path,
    home: &Path,
    prepared: claude::config::Prepared,
) -> Result<Vec<String>, String> {
    if prepared.is_route_publication() {
        return Err("路由发布必须通过运行状态服务完成".into());
    }
    let workspace = claude_workspace(data_dir, home)?;
    if prepared.workspace_id() != workspace.id {
        return Err("预览与当前工作空间不匹配，请重新预览".into());
    }
    let store =
        Store::open_recovery(&data_dir.join("switchx.sqlite")).map_err(|_| "无法打开客户端资料")?;
    store.client_edit(|| {
        if let Some(expected) = prepared.provider() {
            let current = store
                .claude_connection(&workspace.id, &expected.id)
                .map_err(|_| "无法读取连接")?
                .ok_or("预览中的连接已被删除")?;
            if &current != expected {
                return Err("连接在预览后发生变化，请重新预览".into());
            }
            let key = if current.settings.auth == AuthMode::Native {
                None
            } else {
                store
                    .provider_api_key(&current.id)
                    .map_err(|_| "无法核对连接凭据")?
            };
            if !prepared.key_matches(key.as_ref()) {
                return Err("连接凭据在预览后发生变化，请重新预览".into());
            }
        }
        prepared.apply(data_dir)
    })
}

pub fn import_claude_current(data_dir: &Path, home: &Path) -> Result<Connection, String> {
    let workspace = claude_workspace(data_dir, home)?;
    if claude::config::status(data_dir, &workspace)?.managed {
        return Err("当前配置已由 SwitchX 管理，请使用已保存的连接".into());
    }
    let value = claude::config::read_settings(&workspace)?;
    if value.get("apiKeyHelper").is_some() {
        return Err("当前配置使用凭据 helper；不会执行导入的命令，请手动添加连接并填写凭据".into());
    }
    let env = value.get("env").and_then(Value::as_object);
    let string = |key: &str| {
        env.and_then(|env| env.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    if [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_USE_GATEWAY",
        "CLAUDE_CODE_USE_MANTLE",
        "CLAUDE_CODE_USE_ANTHROPIC_AWS",
        "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    ]
    .iter()
    .any(|key| matches!(string(key).as_str(), "1" | "true" | "yes"))
    {
        return Err("当前配置使用云平台协议；这一阶段支持 Anthropic 兼容直连".into());
    }
    let bearer = string("ANTHROPIC_AUTH_TOKEN");
    let api_key = string("ANTHROPIC_API_KEY");
    let (auth, key) = if !bearer.is_empty() {
        (AuthMode::Bearer, Some(Secret::new(bearer)))
    } else if !api_key.is_empty() {
        (AuthMode::ApiKey, Some(Secret::new(api_key)))
    } else {
        (AuthMode::Native, None)
    };
    let base = string("ANTHROPIC_BASE_URL");
    if auth == AuthMode::Native
        && !base.is_empty()
        && base.trim_end_matches('/') != "https://api.anthropic.com"
    {
        return Err("当前网关配置没有可导入的 API 凭据，请手动添加连接".into());
    }
    let mut models = Vec::new();
    let picker = if let Some(picker) = value.get("modelPicker") {
        for row in picker
            .get("options")
            .and_then(Value::as_array)
            .ok_or("modelPicker.options 无效")?
        {
            let mut model = ModelOption::new(
                row.get("model")
                    .and_then(Value::as_str)
                    .ok_or("模型条目缺少 model")?,
            );
            model.label = row
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or(&model.model)
                .to_owned();
            model.description = row
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            model.behaves_as = row
                .get("behavesAs")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into();
            models.push(model);
        }
        if picker.get("replaceBuiltInOptions").and_then(Value::as_bool) == Some(true) {
            PickerPolicy::Replace
        } else {
            PickerPolicy::Append
        }
    } else {
        PickerPolicy::Preserve
    };
    let mut default_model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if default_model.is_empty() {
        default_model = string("ANTHROPIC_MODEL");
    }
    if default_model.is_empty() && auth != AuthMode::Native {
        default_model = string("ANTHROPIC_DEFAULT_SONNET_MODEL");
        if default_model.is_empty() {
            default_model = "sonnet".into();
        }
    }
    if value.get("modelPicker").is_none()
        && !default_model.is_empty()
        && !models.iter().any(|m| m.model == default_model)
    {
        models.push(ModelOption::new(&default_model));
    }
    let settings = ConnectionSettings {
        auth,
        protocol: crate::claude::UpstreamProtocol::Messages,
        picker,
        models,
        default_model,
        roles: Roles {
            sonnet: string("ANTHROPIC_DEFAULT_SONNET_MODEL"),
            opus: string("ANTHROPIC_DEFAULT_OPUS_MODEL"),
            haiku: string("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            fable: string("ANTHROPIC_DEFAULT_FABLE_MODEL"),
            subagent: string("CLAUDE_CODE_SUBAGENT_MODEL"),
        },
    };
    let connection = Connection {
        id: crate::id::new_id()?,
        workspace_id: workspace.id,
        name: if auth == AuthMode::Native {
            "本机官方登录"
        } else {
            "导入的 Claude 连接"
        }
        .into(),
        base_url: if base.is_empty() {
            "https://api.anthropic.com".into()
        } else {
            base
        },
        settings,
        revision: 0,
        has_key: false,
    };
    save_claude_connection(data_dir, home, connection, key, None)
}

pub async fn discover_claude_models(
    base: &str,
    protocol: UpstreamProtocol,
    auth: AuthMode,
    key: &Secret,
) -> Result<Vec<ModelOption>, String> {
    if auth == AuthMode::Native {
        return Err("官方登录的模型由 Claude Code 提供，也可以手动添加".into());
    }
    if protocol == UpstreamProtocol::Responses && auth != AuthMode::Bearer {
        return Err("Responses 模型目录需要 Bearer Token".into());
    }
    let base = validate_claude_endpoint(base)?;
    // The saved base is a root; append the resource path without guessing from its name.
    let url = format!("{base}/v1/models");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| "无法初始化模型查询")?;
    let mut request = client.get(url);
    if protocol == UpstreamProtocol::Messages {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if auth != AuthMode::ApiKey {
        request = request.bearer_auth(key.expose());
    }
    if auth != AuthMode::Bearer {
        request = request.header("x-api-key", key.expose());
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "模型查询失败，请检查地址和网络")?;
    if !response.status().is_success() {
        return Err(format!(
            "模型目录返回 HTTP {}；可检查凭据或手动添加模型",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "读取模型目录失败")? {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err("模型目录超过 1 MiB 限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "上游没有返回有效的模型 JSON")?;
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("上游没有提供模型目录，可手动添加")?;
    let mut models = Vec::new();
    for row in rows {
        let Some(id) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty()
            || id.len() > 512
            || id.chars().any(char::is_whitespace)
            || models.iter().any(|m: &ModelOption| m.model == id)
        {
            continue;
        }
        let mut model = ModelOption::new(id);
        if let Some(label) = row
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|s| s.len() <= 240 && !s.chars().any(char::is_control))
        {
            model.label = label.into();
        }
        models.push(model);
        if models.len() == 200 {
            break;
        }
    }
    if models.is_empty() {
        return Err("上游目录没有可用的模型条目，可手动添加".into());
    }
    Ok(models)
}

pub fn claude_workspace_by_id(data_dir: &Path, id: &str) -> Result<ClientWorkspace, String> {
    let store = Store::open_credentials_read_only(&data_dir.join("switchx.sqlite"))
        .map_err(|_| "无法读取工作空间")?;
    let workspace = store
        .client_workspace(id)
        .map_err(|_| "无法读取工作空间")?
        .ok_or("工作空间不存在")?;
    if workspace.key.client != ClientKind::ClaudeCode {
        return Err("目标不是 Claude 工作空间".into());
    }
    Ok(workspace)
}

pub fn claude_connection_key(data_dir: &Path, home: &Path, id: &str) -> Result<Secret, String> {
    let workspace = claude_workspace(data_dir, home)?;
    let store = open_store(data_dir).map_err(|e| e.message())?;
    store
        .claude_connection(&workspace.id, id)
        .map_err(|_| "无法读取连接")?
        .ok_or("连接不属于当前工作空间")?;
    store
        .provider_api_key(id)
        .map_err(|_| "无法读取凭据")?
        .ok_or_else(|| "连接尚未配置凭据".into())
}

/// Read the routing form through the same workspace services as apply.
pub fn route_plan(
    data: &Path,
    home: &Path,
    options: claude::publication::Options,
) -> Result<claude::publication::Plan, String> {
    let workspace = claude_workspace(data, home)?;
    let store = open_store(data).map_err(|e| e.message())?;
    claude::publication::build(&store, &workspace.id, options)
}
