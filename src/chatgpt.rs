//! Native Codex login and workspace discovery. Managed accounts live in `accounts`.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::watch,
    time::timeout,
};

use crate::{
    app, catalog, client,
    storage::{AccountBinding, ModelRecord, ProviderKind, ProviderRecord},
};

// The explicit provider identity selects authentication, never the model name or URL.
pub const PROVIDER_ID: &str = "switchx-chatgpt";
pub const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const ACCOUNT_HEADER: &str = "chatgpt-account-id";
const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

pub fn managed_account(data_dir: &Path) -> Result<Option<String>, String> {
    match account_binding(data_dir)? {
        Some(id) if id == "default" => crate::accounts::AccountManager::open(data_dir)?
            .default_id()?
            .map(Some)
            .ok_or_else(|| "尚未保存默认 ChatGPT 账号，请添加或选择账号".into()),
        binding => Ok(binding),
    }
}

pub fn account_binding(data_dir: &Path) -> Result<Option<String>, String> {
    app::open_store(data_dir)
        .map_err(|_| "无法读取订阅账号绑定")?
        .chatgpt_account_binding()
        .map_err(|_| "无法读取订阅账号绑定".into())
}

pub fn bind_managed_account(data_dir: &Path, id: Option<&str>) -> Result<(), String> {
    app::ensure_editable(data_dir)?;
    app::open_store(data_dir)
        .map_err(|_| "无法读取订阅账号绑定")?
        .bind_chatgpt_account(id)
        .map_err(|_| "无法保存订阅账号绑定".into())
}

/// Workspace routing metadata from the native CLI; this type contains no OAuth tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workspace {
    pub account_id: String,
    pub backend_origin: String,
    pub routing_override: String,
}

impl Workspace {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let valid_account = !self.account_id.is_empty()
            && self.account_id.len() <= 256
            && self
                .account_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte));
        let valid_origin = reqwest::Url::parse(&self.backend_origin).is_ok_and(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url
                    .host_str()
                    .is_some_and(|host| host == "chatgpt.com" || host.ends_with(".chatgpt.com"))
                && url.origin().ascii_serialization() == self.backend_origin
        });
        if !valid_account
            || !valid_origin
            || !matches!(
                self.routing_override.as_str(),
                "NO_CONSTRAINT" | "us" | "us_cr"
            )
        {
            return Err("官方工作区路由资料无效；只允许已选工作区的 ChatGPT 官方地址".into());
        }
        Ok(())
    }

    pub(crate) fn from_reply(reply: &Value) -> Result<Option<Self>, String> {
        let value = &reply["workspaceRouting"];
        // Older CLIs omit this field; accept the snake_case form used by some releases.
        let value = if value.is_null() {
            &reply["workspace_routing"]
        } else {
            value
        };
        if value.is_null() {
            return Ok(None);
        }
        let field = |camel: &str, snake: &str| {
            value[camel]
                .as_str()
                .or_else(|| value[snake].as_str())
                .unwrap_or("")
                .to_owned()
        };
        let routing_override = field("accountRoutingOverride", "account_routing_override");
        let workspace = Self {
            account_id: field("chatgptAccountId", "chatgpt_account_id"),
            backend_origin: field("backendOrigin", "backend_origin"),
            routing_override: match routing_override.as_str() {
                "noConstraint" | "no_constraint" => "NO_CONSTRAINT".into(),
                "usCr" => "us_cr".into(),
                _ => routing_override,
            },
        };
        workspace.validate()?;
        Ok(Some(workspace))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Chatgpt,
    ApiKey,
    SignedOut,
    Unknown,
}

impl AccountStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT 已登录；会话续期由 Codex 管理",
            Self::ApiKey => "当前是 API Key 登录，请使用 ChatGPT 登录后开启订阅路由",
            Self::SignedOut => "ChatGPT 未登录或登录已失效，请重新登录",
            Self::Unknown => "无法确认订阅登录类型，请使用 ChatGPT 登录",
        }
    }
    pub fn require_chatgpt(self) -> Result<(), String> {
        if self == Self::Chatgpt {
            Ok(())
        } else {
            Err(self.label().into())
        }
    }
}

pub fn validate_provider(provider: &ProviderRecord) -> Result<(), String> {
    if provider.kind != crate::storage::ProviderKind::Chatgpt
        || provider.account_binding.is_none()
        || provider.base_url != BASE_URL
        || provider.credential_ref.is_some()
    {
        return Err("订阅连接的身份或固定官方地址无效；不能使用第三方地址或 API Key".into());
    }
    Ok(())
}

fn command(home: &Path) -> Command {
    command_using(home, &client::cli_executable())
}

fn command_using(home: &Path, cli_path: &Path) -> Command {
    let mut command = Command::new(cli_path);
    command
        .current_dir(home)
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .kill_on_drop(true);
    command
}

pub struct Session {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    pending_completion: Option<Value>,
}

impl Session {
    pub async fn start(home: &Path) -> Result<Self, String> {
        client::config_path(home)?;
        if !home.is_dir() {
            return Err("Codex 配置目录不存在".into());
        }
        Self::spawn(command(home)).await
    }

    pub async fn start_using(home: &Path, cli_path: &Path) -> Result<Self, String> {
        client::config_path(home)?;
        if !home.is_dir() || !cli_path.is_absolute() {
            return Err("Codex 目录或 CLI 路径无效".into());
        }
        Self::spawn(command_using(home, cli_path)).await
    }

    async fn spawn(mut command: Command) -> Result<Self, String> {
        let mut child = command
            .args([
                "-c",
                "model_provider=\"openai\"",
                "app-server",
                "--listen",
                "stdio://",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "无法启动 Codex 登录服务；可设置 SWITCHX_CODEX_CLI")?;
        let input = child.stdin.take().ok_or("Codex 登录服务输入不可用")?;
        let output = BufReader::new(child.stdout.take().ok_or("Codex 登录服务输出不可用")?);
        let mut session = Self {
            child,
            input,
            output,
            next_id: 1,
            pending_completion: None,
        };
        session.request("initialize", json!({"clientInfo":{"name":"switchx", "title":"SwitchX", "version":env!("CARGO_PKG_VERSION")}})).await?;
        session
            .send(&json!({"method":"initialized", "params":{}}))
            .await?;
        Ok(session)
    }

    async fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut bytes = serde_json::to_vec(message).map_err(|_| "无法生成 Codex 登录请求")?;
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .map_err(|_| "Codex 登录服务已断开".into())
    }

    async fn next(&mut self) -> Result<Value, String> {
        let mut line = Vec::new();
        loop {
            let buffer = self
                .output
                .fill_buf()
                .await
                .map_err(|_| "无法读取 Codex 登录服务")?;
            if buffer.is_empty() {
                return Err("Codex 登录服务已退出".into());
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            if line.len() + count > MAX_REPLY_BYTES {
                return Err("Codex 登录服务响应超过读取上限".into());
            }
            line.extend_from_slice(&buffer[..count]);
            self.output.consume(count);
            if line.last() == Some(&b'\n') {
                return serde_json::from_slice(&line)
                    .map_err(|_| "Codex 登录服务响应格式无效".into());
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        timeout(RPC_TIMEOUT, async {
            self.send(&json!({"id":id,"method":method,"params":params}))
                .await?;
            loop {
                let message = self.next().await?;
                if message["id"] == id {
                    if message.get("error").is_some() {
                        let code = message["error"]["code"].as_i64().unwrap_or(0);
                        return Err(format!(
                            "Codex 登录操作失败（{method}，错误 {code}）；请检查账号状态或重新登录"
                        ));
                    }
                    return message
                        .get("result")
                        .cloned()
                        .ok_or("Codex 登录操作没有返回结果".into());
                }
                if message["method"] == "account/login/completed" {
                    self.pending_completion = Some(message);
                }
            }
        })
        .await
        .map_err(|_| "Codex 登录操作超时，请稍后重试")?
    }

    pub async fn account(&mut self, refresh: bool) -> Result<AccountStatus, String> {
        self.read_account(refresh).await.map(|(status, _)| status)
    }

    async fn read_account(
        &mut self,
        refresh: bool,
    ) -> Result<(AccountStatus, Option<Workspace>), String> {
        let result = self
            .request("account/read", json!({"refreshToken":refresh}))
            .await?;
        let status = match result["account"]["type"].as_str() {
            Some("chatgpt") => AccountStatus::Chatgpt,
            Some("apiKey") => AccountStatus::ApiKey,
            None if result["account"].is_null() => AccountStatus::SignedOut,
            _ => AccountStatus::Unknown,
        };
        let workspace = Workspace::from_reply(&result)?;
        if workspace.is_some() && status != AccountStatus::Chatgpt {
            return Err("Codex 返回了不匹配的账号与工作区资料".into());
        }
        Ok((status, workspace))
    }

    pub async fn login(mut self) -> Result<Login, String> {
        let result = self
            .request("account/login/start", json!({"type":"chatgpt"}))
            .await?;
        let id = result["loginId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or("Codex 未返回登录编号")?
            .to_owned();
        let url = result["authUrl"]
            .as_str()
            .filter(|url| valid_login_url(url))
            .ok_or("Codex 未返回有效的官方登录地址")?
            .to_owned();
        Ok(Login {
            session: self,
            id,
            url,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn valid_login_url(value: &str) -> bool {
    value.len() <= 8192
        && reqwest::Url::parse(value).is_ok_and(|url| {
            url.scheme() == "https"
                && matches!(url.host_str(), Some("auth.openai.com" | "chatgpt.com"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
}

pub struct Login {
    session: Session,
    id: String,
    pub url: String,
}

impl Login {
    pub async fn finish(self, cancel: watch::Receiver<bool>) -> Result<AccountStatus, String> {
        self.finish_with_timeout(cancel, Duration::from_secs(300))
            .await
    }

    async fn finish_with_timeout(
        mut self,
        mut cancel: watch::Receiver<bool>,
        wait: Duration,
    ) -> Result<AccountStatus, String> {
        let result = tokio::select! {
            biased;
            _ = cancel.wait_for(|cancel| *cancel) => Err("登录已取消；可重新检查原有登录".into()),
            result = timeout(wait, async {
                loop {
                    let message = match self.session.pending_completion.take() {
                        Some(message) => message,
                        None => self.session.next().await?,
                    };
                    if message["method"] == "account/login/completed" && message["params"]["loginId"] == self.id {
                        if message["params"]["success"] != true { return Err("ChatGPT 登录未完成，请重试".into()); }
                        let status = self.session.account(false).await?;
                        status.require_chatgpt()?;
                        return Ok(status);
                    }
                }
            }) => result.unwrap_or_else(|_| Err("登录等待超时，请重新发起登录".into())),
        };
        if result.is_err() {
            let _ = self
                .session
                .request("account/login/cancel", json!({"loginId":self.id}))
                .await;
        }
        result
    }
}

pub async fn account(home: &Path, refresh: bool) -> Result<AccountStatus, String> {
    Session::start(home).await?.account(refresh).await
}

// Load the selected CLI's complete built-in instructions and capabilities without an account.
// Standard Responses/SSE is explicit; the separate Responses Lite transport is not enabled.
pub async fn catalog() -> Result<Vec<Value>, String> {
    struct CatalogHome(PathBuf);
    impl Drop for CatalogHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!("switchx-chatgpt-catalog-{}", app::new_id()?));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .map_err(|_| "无法创建官方目录检查临时目录")?;
    let home = CatalogHome(path);
    let output = timeout(
        RPC_TIMEOUT,
        command(&home.0).args(["debug", "models"]).output(),
    )
    .await
    .map_err(|_| "读取 Codex 内置模型目录超时")?
    .map_err(|_| "无法读取 Codex 内置模型目录")?;
    if !output.status.success() || output.stdout.len() > MAX_REPLY_BYTES {
        return Err("读取 Codex 内置模型目录失败".into());
    }
    let directory: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Codex 内置模型目录格式无效")?;
    let mut models = directory["models"]
        .as_array()
        .ok_or("Codex 内置目录缺少模型列表")?
        .iter()
        .filter(|model| model["supported_in_api"] == true && model["visibility"] == "list")
        .cloned()
        .collect::<Vec<_>>();
    for model in &mut models {
        model["use_responses_lite"] = false.into();
        catalog::validate_metadata(model)?;
    }
    models.sort_by_key(|model| model["priority"].as_i64().unwrap_or(i64::MAX));
    if models.is_empty() {
        return Err("目标 Codex CLI 没有可发布的官方模型资料".into());
    }
    Ok(models)
}

pub fn save_connection(data_dir: &Path, models: &[Value]) -> Result<(), String> {
    let store = app::open_store(data_dir).map_err(|error| error.message())?;
    let binding = store
        .provider(PROVIDER_ID)
        .map_err(|_| "无法读取订阅连接")?
        .and_then(|provider| provider.account_binding)
        .unwrap_or(AccountBinding::Native);
    drop(store);
    save_subscription(data_dir, Some(PROVIDER_ID), "ChatGPT 订阅", binding, models).map(|_| ())
}

/// Save a separate subscription connection without changing the target Codex login.
pub fn save_subscription(
    data_dir: &Path,
    provider_id: Option<&str>,
    name: &str,
    binding: AccountBinding,
    models: &[Value],
) -> Result<String, String> {
    save_subscription_inner(data_dir, provider_id, name, binding, models, None)
}

pub fn save_subscription_with_codex_options(
    data_dir: &Path,
    provider_id: Option<&str>,
    name: &str,
    binding: AccountBinding,
    models: &[Value],
    options: &crate::provider_config::CodexOptions,
    common: &str,
) -> Result<String, String> {
    let mut options = match &options.config_toml {
        Some(config) => crate::provider_config::subscription_options_from_config(options, config)?,
        None => options.clone(),
    };
    options.validate()?;
    if options.use_common_config {
        let mut shared = crate::provider_config::validate_common(common)?;
        // Keep context choices local while other exact shared values remain inherited.
        shared.remove("model_context_window");
        shared.remove("model_auto_compact_token_limit");
        if let Some(config) = &options.config_toml {
            options.config_toml = Some(crate::provider_config::set_subscription_common(
                config,
                &shared.to_string(),
                false,
            )?);
        }
    }
    save_subscription_inner(data_dir, provider_id, name, binding, models, Some(&options))
}

pub fn validate_subscription_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.trim().len() > 120 || name.chars().any(char::is_control) {
        return Err("请输入有效的订阅连接名称".into());
    }
    Ok(())
}

/// Check editor contents before saving private auth files, without creating or migrating storage.
pub fn validate_subscription_options(
    data_dir: &Path,
    provider_id: Option<&str>,
    models: &[Value],
    options: &crate::provider_config::CodexOptions,
    common: &str,
) -> Result<(), String> {
    options.validate()?;
    if options.use_common_config {
        crate::provider_config::validate_common(common)?;
    }
    app::ensure_editable(data_dir)?;
    if !data_dir.is_absolute() {
        return Err("订阅配置的数据目录须为绝对路径".into());
    }
    for metadata in models {
        catalog::validate_metadata(metadata)?;
    }
    let path = data_dir.join("switchx.sqlite");
    let (old, saved) = if path.exists() {
        let store = crate::storage::Store::open_read_only(&path)
            .map_err(|_| "无法只读检查订阅连接，请刷新后重试")?;
        let old = provider_id
            .map(|id| store.provider(id))
            .transpose()
            .map_err(|_| "无法读取订阅连接")?
            .flatten();
        let saved = store.models().map_err(|_| "无法读取模型资料")?;
        (old, saved)
    } else {
        (None, Vec::new())
    };
    if let Some(provider) = &old {
        validate_provider(provider)?;
    }
    if provider_id.is_some_and(|id| id != PROVIDER_ID) && old.is_none() {
        return Err("订阅连接不存在，请刷新后重试".into());
    }
    if old.is_none() && models.is_empty() {
        return Err("官方目录没有模型".into());
    }
    subscription_model_for_options(provider_id, models, Some(options), old.as_ref(), &saved)?;
    Ok(())
}

fn subscription_model_for_options(
    provider_id: Option<&str>,
    models: &[Value],
    options: Option<&crate::provider_config::CodexOptions>,
    old: Option<&ProviderRecord>,
    saved: &[ModelRecord],
) -> Result<Option<String>, String> {
    let selected = options
        .and_then(|options| options.config_toml.as_deref())
        .map(crate::provider_config::subscription_model)
        .transpose()?
        .flatten();
    if let Some(model) = &selected
        && !models
            .iter()
            .any(|metadata| metadata["slug"].as_str() == Some(model.as_str()))
        && !saved.iter().any(|mapping| {
            Some(mapping.provider_id.as_str()) == provider_id && mapping.upstream_model == *model
        })
        && old.is_none_or(|provider| provider.model_id != *model)
    {
        return Err("订阅配置的模型不在官方目录或当前连接映射中，请刷新官方目录后重试".into());
    }
    Ok(selected)
}

fn save_subscription_inner(
    data_dir: &Path,
    provider_id: Option<&str>,
    name: &str,
    binding: AccountBinding,
    models: &[Value],
    options: Option<&crate::provider_config::CodexOptions>,
) -> Result<String, String> {
    app::ensure_editable(data_dir)?;
    validate_subscription_name(name)?;
    binding.encode().map_err(|_| "订阅账号绑定无效")?;
    if let AccountBinding::Fixed(id) = &binding {
        let manager = crate::accounts::AccountManager::open(data_dir)?;
        if !manager.list()?.iter().any(|account| &account.id == id) {
            return Err("绑定账号已不存在，请重新选择".into());
        }
    }
    let store = app::open_store(data_dir).map_err(|error| error.message())?;
    let old = provider_id
        .map(|id| store.provider(id))
        .transpose()
        .map_err(|_| "无法读取订阅连接")?
        .flatten();
    if let Some(provider) = &old {
        validate_provider(provider)?;
    }
    if provider_id.is_some_and(|id| id != PROVIDER_ID) && old.is_none() {
        return Err("订阅连接不存在，请刷新后重试".into());
    }
    for metadata in models {
        catalog::validate_metadata(metadata)?;
    }
    let saved = store.models().map_err(|_| "无法读取模型资料")?;
    let first = models.first();
    if old.is_none() && first.is_none() {
        return Err("官方目录没有模型".into());
    }
    let id = match provider_id {
        Some(id) => id.to_owned(),
        None => app::new_id()?,
    };
    let selected_model =
        subscription_model_for_options(provider_id, models, options, old.as_ref(), &saved)?;
    let provider = ProviderRecord {
        id: id.clone(),
        name: name.trim().into(),
        base_url: BASE_URL.into(),
        model_id: selected_model.unwrap_or_else(|| {
            old.as_ref()
                .map(|provider| provider.model_id.clone())
                .unwrap_or_else(|| first.unwrap()["slug"].as_str().unwrap_or("").into())
        }),
        credential_ref: None,
        kind: ProviderKind::Chatgpt,
        account_binding: Some(binding),
    };
    let mut additions: Vec<ModelRecord> = Vec::new();
    for metadata in models {
        let upstream_model = metadata["slug"].as_str().unwrap();
        if saved
            .iter()
            .any(|model| model.provider_id == id && model.upstream_model == upstream_model)
        {
            continue;
        }
        let slug: String = upstream_model
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let public_id = if id == PROVIDER_ID {
            format!("sx-chatgpt-{slug}")
        } else {
            let prefix = format!("sx-chatgpt-{id}-");
            let remaining = 64_usize
                .checked_sub(prefix.len())
                .filter(|length| *length >= 18)
                .ok_or("订阅连接 ID 过长，无法生成公开模型 ID")?;
            if slug.len() <= remaining {
                format!("{prefix}{slug}")
            } else {
                use std::hash::{DefaultHasher, Hash, Hasher};
                let mut hash = DefaultHasher::new();
                upstream_model.hash(&mut hash);
                // IDs are initialized once and retained on subsequent imports.
                // The suffix distinguishes long slugs while honoring the 64-byte catalog limit.
                format!(
                    "{prefix}{}-{:016x}",
                    slug[..remaining - 17].trim_end_matches('-'),
                    hash.finish()
                )
            }
        };
        if saved
            .iter()
            .chain(&additions)
            .any(|model| model.public_id == public_id)
        {
            return Err(format!(
                "官方模型 {upstream_model} 的公开 ID {public_id} 与其他映射冲突；本批未保存"
            ));
        }
        let model = ModelRecord {
            provider_id: id.clone(),
            public_id,
            display_name: format!("{upstream_model}/{}", provider.name),
            upstream_model: upstream_model.into(),
            metadata: metadata.to_string(),
            enabled: saved.iter().all(|model| model.provider_id != id)
                && upstream_model == provider.model_id,
            fallback_provider_id: None,
        };
        catalog::publish_saved(&[ModelRecord {
            enabled: true,
            ..model.clone()
        }])?;
        additions.push(model);
    }
    match options {
        Some(options) => {
            let options = serde_json::to_string(options).map_err(|_| "无法编码订阅配置")?;
            store.put_provider_with_models_and_options(&provider, &additions, &options)
        }
        None => store.put_provider_with_models(&provider, &additions),
    }
    .map_err(|_| "无法保存订阅连接、配置和官方模型资料")?;
    Ok(id)
}

pub async fn require_login(home: &Path) -> Result<(), String> {
    // Honor existing auth storage; only Codex reads or refreshes its credentials.
    workspace(home).await.map(|_| ())
}

pub fn deselect_models(data_dir: &Path) -> Result<(), String> {
    app::ensure_editable(data_dir)?;
    app::open_store(data_dir)
        .map_err(|error| error.message())?
        .deselect_subscription_models()
        .map_err(|_| "无法取消订阅模型选择；本批未保存".into())
}

pub async fn workspace(home: &Path) -> Result<Option<Workspace>, String> {
    let (status, workspace) = Session::start(home).await?.read_account(false).await?;
    status.require_chatgpt()?;
    Ok(workspace)
}

pub async fn workspace_using(home: &Path, cli_path: &Path) -> Result<Option<Workspace>, String> {
    let (status, workspace) = Session::start_using(home, cli_path)
        .await?
        .read_account(false)
        .await?;
    status.require_chatgpt()?;
    Ok(workspace)
}

/// Synthetic probes only; discovery overrides cannot point outside IPv4 loopback.
#[doc(hidden)]
pub async fn workspace_using_mock(
    home: &Path,
    cli_path: &Path,
    address: std::net::SocketAddr,
) -> Result<Option<Workspace>, String> {
    if address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) {
        return Err("账号测试发现地址必须是 IPv4 回环地址".into());
    }
    let mut command = command_using(home, cli_path);
    command.args(["-c", &format!("chatgpt_base_url=\"http://{address}\"")]);
    let (status, workspace) = Session::spawn(command).await?.read_account(false).await?;
    status.require_chatgpt()?;
    Ok(workspace)
}

pub async fn save_mapping(data_dir: &Path, input: app::ModelInput<'_>) -> Result<(), String> {
    if app::load_provider(data_dir, input.provider_id)?.kind != ProviderKind::Chatgpt {
        return Err("官方模型资料只能用于订阅连接".into());
    }
    let source = catalog()
        .await?
        .into_iter()
        .find(|model| model["slug"] == input.upstream_model)
        .ok_or("目标 CLI 内置官方目录没有此模型，请获取列表后选择")?;
    // Keep the CLI's complete tool and instruction template, even when adding a new mapping.
    app::save_mapping_from(data_dir, input, Some(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_subscription_connections_keep_accounts_and_model_ids_when_renamed_or_rebound() {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let root = std::env::temp_dir().join(format!(
            "switchx-separate-subscriptions-{}",
            app::new_id().unwrap()
        ));
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let manager = crate::accounts::AccountManager::open(&data).unwrap();
        let mut ids = Vec::new();
        for suffix in ["a", "b"] {
            let home = root.join(suffix);
            std::fs::create_dir(&home).unwrap();
            let mut auth: Value = serde_json::from_str(include_str!(
                "../tests/fixtures/synthetic-chatgpt-auth.json"
            ))
            .unwrap();
            let claims = json!({"sub":format!("synthetic-{suffix}"),"email":format!("{suffix}@example.invalid"),
                "https://api.openai.com/auth":{"chatgpt_account_id":format!("workspace-{suffix}"),"chatgpt_plan_type":"plus","user_id":format!("synthetic-{suffix}")},"exp":4102444800_i64});
            let jwt = format!(
                "{}.{}.synthetic",
                URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#),
                URL_SAFE_NO_PAD.encode(claims.to_string())
            );
            auth["tokens"]["id_token"] = jwt.clone().into();
            auth["tokens"]["access_token"] = jwt.into();
            auth["tokens"]["account_id"] = format!("workspace-{suffix}").into();
            std::fs::write(home.join("auth.json"), auth.to_string()).unwrap();
            ids.push(manager.import_current(&home).unwrap().id);
        }
        let templates: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let mut models = templates["models"].as_array().unwrap().clone();
        for slug in [
            "synthetic-shared-chatgpt-model-long-a",
            "synthetic-shared-chatgpt-model-long-b",
        ] {
            let mut model = models[0].clone();
            model["slug"] = slug.into();
            models.push(model);
        }
        let a = save_subscription(
            &data,
            None,
            "Team A",
            AccountBinding::Fixed(ids[0].clone()),
            &models,
        )
        .unwrap();
        let b = save_subscription(
            &data,
            None,
            "Team B",
            AccountBinding::Fixed(ids[1].clone()),
            &models,
        )
        .unwrap();
        assert_ne!(a, b);
        let store = app::open_store(&data).unwrap();
        let before = store.models().unwrap();
        assert_eq!(before.iter().filter(|model| model.enabled).count(), 2);
        assert!(before.iter().all(|model| model.public_id.len() <= 64));
        assert_eq!(
            before
                .iter()
                .map(|model| &model.public_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            before.len()
        );
        for id in [&a, &b] {
            assert!(
                before
                    .iter()
                    .filter(|model| &model.provider_id == id)
                    .all(|model| model.public_id.starts_with(&format!("sx-chatgpt-{id}-")))
            );
        }
        drop(store);
        save_subscription(
            &data,
            Some(&a),
            "Renamed A",
            AccountBinding::Fixed(ids[1].clone()),
            &[],
        )
        .unwrap();
        save_subscription(
            &data,
            Some(&a),
            "Renamed A",
            AccountBinding::Fixed(ids[1].clone()),
            &models,
        )
        .unwrap();
        let store = app::open_store(&data).unwrap();
        assert_eq!(store.models().unwrap(), before);
        assert_eq!(
            store.provider(&a).unwrap().unwrap().account_binding,
            Some(AccountBinding::Fixed(ids[1].clone()))
        );
        manager.set_default(&ids[0]).unwrap();
        assert_eq!(
            store.provider(&a).unwrap().unwrap().account_binding,
            Some(AccountBinding::Fixed(ids[1].clone()))
        );
        assert!(
            app::save_provider(
                &data,
                Some(&a),
                "Wrong editor",
                BASE_URL,
                "same-model",
                "synthetic-key".into()
            )
            .is_err()
        );
        assert!(save_subscription(&data, None, "Native", AccountBinding::Native, &models).is_ok());
        assert!(
            save_subscription(
                &data,
                None,
                "Deleted",
                AccountBinding::Fixed("missing".into()),
                &models
            )
            .is_err()
        );
        let a_model = before.iter().find(|model| model.provider_id == a).unwrap();
        assert!(app::save_fallback(&data, &a_model.public_id, Some(&b)).is_err());
        app::delete_provider(&data, &b).unwrap();
        assert!(store.provider(&b).unwrap().is_none());
        assert!(store.provider(&a).unwrap().is_some());
        assert!(
            store
                .models()
                .unwrap()
                .iter()
                .all(|model| model.provider_id != b)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn official_identity_and_login_links_are_locked() {
        let mut provider = ProviderRecord {
            id: PROVIDER_ID.into(),
            name: "ChatGPT".into(),
            base_url: BASE_URL.into(),
            model_id: "fixture-model".into(),
            credential_ref: None,
            kind: ProviderKind::Chatgpt,
            account_binding: Some(AccountBinding::Native),
        };
        assert!(validate_provider(&provider).is_ok());
        provider.base_url = "https://example.invalid/v1".into();
        assert!(validate_provider(&provider).is_err());
        provider.base_url = BASE_URL.into();
        provider.credential_ref = Some("secret-reference".into());
        assert!(validate_provider(&provider).is_err());
        assert!(valid_login_url(
            "https://auth.openai.com/oauth/authorize?state=synthetic"
        ));
        for url in [
            "http://auth.openai.com/oauth",
            "https://auth.openai.com.evil.test/oauth",
            "https://user@auth.openai.com/oauth",
            "https://example.invalid/oauth",
        ] {
            assert!(!valid_login_url(url));
        }
        assert!(AccountStatus::Chatgpt.require_chatgpt().is_ok());
        assert!(AccountStatus::ApiKey.require_chatgpt().is_err());
    }

    #[test]
    fn native_workspace_metadata_cannot_select_a_third_party_or_drop_region_constraints() {
        let mut reply = json!({"workspaceRouting":{"chatgptAccountId":"fixture-workspace","backendOrigin":"https://us.chatgpt.com","accountRoutingOverride":"us"}});
        let workspace = Workspace::from_reply(&reply).unwrap().unwrap();
        assert_eq!(workspace.routing_override, "us");
        for origin in [
            "https://example.invalid",
            "http://chatgpt.com",
            "https://chatgpt.com.evil.test",
            "https://user@chatgpt.com",
            "https://chatgpt.com/path",
            "https://chatgpt.com?secret=fixture",
        ] {
            reply["workspaceRouting"]["backendOrigin"] = origin.into();
            assert!(Workspace::from_reply(&reply).is_err());
        }
        reply["workspaceRouting"]["backendOrigin"] = "https://chatgpt.com".into();
        reply["workspaceRouting"]["accountRoutingOverride"] = "unknown".into();
        assert!(Workspace::from_reply(&reply).is_err());
        assert!(
            Workspace::from_reply(&json!({"account":{"type":"chatgpt"}}))
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    struct MockHome(PathBuf);

    #[cfg(unix)]
    impl MockHome {
        async fn session(mode: &str) -> (Self, Session) {
            let home = Self(
                std::env::temp_dir().join(format!("switchx-login-test-{}", app::new_id().unwrap())),
            );
            std::fs::create_dir(&home.0).unwrap();
            let script = r#"
while IFS= read -r message; do
    printf '%s\n' "$message" >> "$1"
    id="${message#*\"id\":}"
    id="${id%%,*}"
    case "$message" in
        *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
        *'"method":"account/read"'*)
            case "$2" in
                api) account='{"type":"apiKey"}' ;;
                signedout) account='null' ;;
                error) printf '{"id":%s,"error":{"message":"secret-not-for-ui"}}\n' "$id"; continue ;;
                *) account='{"type":"chatgpt","email":"synthetic@example.invalid","planType":"plus"}' ;;
            esac
            printf '{"id":%s,"result":{"account":%s}}\n' "$id" "$account" ;;
        *'"method":"account/login/start"'*)
            if [ "$2" = early ]; then
                printf '%s\n' '{"method":"account/login/completed","params":{"loginId":"fixture-login","success":true}}'
            fi
            printf '{"id":%s,"result":{"type":"chatgpt","loginId":"fixture-login","authUrl":"https://auth.openai.com/oauth/authorize?state=synthetic"}}\n' "$id" ;;
        *'"method":"account/login/cancel"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
    esac
done
"#;
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", script, "switchx-login-test"])
                .arg(home.0.join("rpc.jsonl"))
                .arg(mode)
                .kill_on_drop(true);
            let session = Session::spawn(command).await.unwrap();
            (home, session)
        }

        fn requests(&self) -> Vec<Value> {
            std::fs::read_to_string(self.0.join("rpc.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    #[cfg(unix)]
    impl Drop for MockHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn managed_account_refresh_and_rpc_errors_never_expose_credentials() {
        let (home, mut session) = MockHome::session("chatgpt").await;
        assert_eq!(
            session.account(false).await.unwrap(),
            AccountStatus::Chatgpt
        );
        assert_eq!(session.account(true).await.unwrap(), AccountStatus::Chatgpt);
        let requests = home.requests();
        let reads = requests
            .iter()
            .filter(|message| message["method"] == "account/read")
            .collect::<Vec<_>>();
        assert_eq!(reads[0]["params"]["refreshToken"], false);
        assert_eq!(reads[1]["params"]["refreshToken"], true);
        for (mode, status) in [
            ("api", AccountStatus::ApiKey),
            ("signedout", AccountStatus::SignedOut),
        ] {
            let (_home, mut session) = MockHome::session(mode).await;
            assert_eq!(session.account(false).await.unwrap(), status);
            assert!(status.require_chatgpt().is_err());
        }
        let (_home, mut session) = MockHome::session("error").await;
        let error = session.account(true).await.unwrap_err();
        assert!(!error.contains("secret-not-for-ui"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn login_completion_cancel_and_timeout_share_the_managed_lifecycle() {
        let (home, session) = MockHome::session("early").await;
        let login = session.login().await.unwrap();
        let (_cancel, receiver) = watch::channel(false);
        assert_eq!(
            login.finish(receiver).await.unwrap(),
            AccountStatus::Chatgpt
        );
        assert!(
            home.requests()
                .iter()
                .any(|request| request["method"] == "account/read")
        );

        for cancel_now in [true, false] {
            let (home, session) = MockHome::session("waiting").await;
            let login = session.login().await.unwrap();
            let (_cancel, receiver) = watch::channel(cancel_now);
            let error = login
                .finish_with_timeout(receiver, Duration::from_millis(25))
                .await
                .unwrap_err();
            assert!(error.contains(if cancel_now { "取消" } else { "超时" }));
            let requests = home.requests();
            let cancellation = requests
                .iter()
                .find(|request| request["method"] == "account/login/cancel")
                .unwrap();
            assert_eq!(cancellation["params"]["loginId"], "fixture-login");
        }
    }

    #[test]
    fn adding_subscription_preserves_existing_mappings_and_stores_no_oauth_secrets() {
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let models = fixture["models"].as_array().unwrap();
        save_connection(&path, models).unwrap();
        let store = app::open_store(&path).unwrap();
        let provider = store.provider(PROVIDER_ID).unwrap().unwrap();
        assert!(provider.credential_ref.is_none());
        let saved = store.models().unwrap();
        assert_eq!(saved.len(), models.len());
        assert_eq!(saved.iter().filter(|model| model.enabled).count(), 1);
        app::select_model(&path, &saved[0].public_id, false).unwrap();
        save_connection(&path, models).unwrap();
        assert!(store.models().unwrap().iter().all(|model| !model.enabled));
        assert_eq!(store.models().unwrap().len(), models.len());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn subscription_raw_toml_and_model_metadata_save_atomically_without_changing_selections() {
        use crate::provider_config::{CodexOptions, subscription_options_from_config};
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-config-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let models = fixture["models"].as_array().unwrap();
        let raw = "# authored subscription\nmodel = 'gpt-5.5'\nmodel_reasoning_effort = 'high'\nmodel_context_window = 128000\nmodel_auto_compact_token_limit = 96000\n[mcp_servers.local]\ncommand = 'synthetic-command'\n";
        let options = subscription_options_from_config(&CodexOptions::default(), raw).unwrap();
        validate_subscription_options(&path, None, models, &options, "").unwrap();
        assert!(!path.exists());
        let id = save_subscription_with_codex_options(
            &path,
            None,
            "Native subscription",
            AccountBinding::Native,
            models,
            &options,
            "",
        )
        .unwrap();
        let store = app::open_store(&path).unwrap();
        let provider = store.provider(&id).unwrap().unwrap();
        assert_eq!(provider.model_id, "gpt-5.5");
        assert_eq!(provider.account_binding, Some(AccountBinding::Native));
        let saved_options = CodexOptions::from_saved(&store.provider_codex_options(&id).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(saved_options.config_toml.as_deref(), Some(raw));
        assert!(!saved_options.context_1m);
        assert_eq!(saved_options.compact_limit, 96000);
        let mappings = store.models().unwrap();
        assert_eq!(mappings.iter().filter(|model| model.enabled).count(), 1);
        assert_eq!(
            mappings
                .iter()
                .find(|model| model.enabled)
                .unwrap()
                .upstream_model,
            "gpt-5.5"
        );
        let updated = subscription_options_from_config(
            &options,
            "model = 'deepseek-flash'\nmodel_reasoning_effort = 'max'\n",
        )
        .unwrap();
        save_subscription_with_codex_options(
            &path,
            Some(&id),
            "Renamed",
            AccountBinding::Native,
            &[],
            &updated,
            "",
        )
        .unwrap();
        assert_eq!(
            store.provider(&id).unwrap().unwrap().model_id,
            "deepseek-flash"
        );
        assert_eq!(store.models().unwrap(), mappings);
        let last_options = store.provider_codex_options(&id).unwrap();
        save_subscription(
            &path,
            Some(&id),
            "Renamed again",
            AccountBinding::Native,
            &[],
        )
        .unwrap();
        assert_eq!(store.provider_codex_options(&id).unwrap(), last_options);
        let last_provider = store.provider(&id).unwrap().unwrap();
        for config in [
            "model = 'unknown-official'",
            "[env]\nOPENAI_API_KEY = 'synthetic-secret'",
            "[broken",
        ] {
            let invalid = CodexOptions {
                config_toml: Some(config.into()),
                ..Default::default()
            };
            assert!(validate_subscription_options(&path, Some(&id), &[], &invalid, "").is_err());
            assert!(
                save_subscription_with_codex_options(
                    &path,
                    Some(&id),
                    "Should not save",
                    AccountBinding::Native,
                    &[],
                    &invalid,
                    "",
                )
                .is_err()
            );
            assert_eq!(store.provider(&id).unwrap().unwrap(), last_provider);
            assert_eq!(store.provider_codex_options(&id).unwrap(), last_options);
            assert_eq!(store.models().unwrap(), mappings);
        }
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn subscription_inherits_updated_common_settings_while_retaining_authored_overrides_and_context()
     {
        use crate::provider_config::{
            CodexOptions, subscription_editor_config, subscription_options_from_config,
        };
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-common-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let models = fixture["models"].as_array().unwrap();
        let common = "approval_policy = 'on-request'\nmodel_context_window = 1000000\nmodel_auto_compact_token_limit = 900000\n[features]\nhooks = false\nmemories = true\n";
        let effective = "# authored override\nmodel = 'gpt-5.5'\napproval_policy = 'never' # own policy\nmodel_context_window = 1000000\nmodel_auto_compact_token_limit = 900000\n[features]\nhooks = false\nmemories = true\n";
        let options =
            subscription_options_from_config(&CodexOptions::default(), effective).unwrap();
        let id = save_subscription_with_codex_options(
            &path,
            None,
            "Inherited",
            AccountBinding::Native,
            models,
            &options,
            common,
        )
        .unwrap();
        let store = app::open_store(&path).unwrap();
        let options = CodexOptions::from_saved(&store.provider_codex_options(&id).unwrap())
            .unwrap()
            .unwrap();
        let authored: toml_edit::DocumentMut =
            options.config_toml.as_deref().unwrap().parse().unwrap();
        assert!(authored.get("features").is_none());
        assert_eq!(authored["approval_policy"].as_str(), Some("never"));
        assert_eq!(authored["model_context_window"].as_integer(), Some(1000000));
        let updated_common = "approval_policy = 'on-request'\nmodel_context_window = 256000\nmodel_auto_compact_token_limit = 128000\n[features]\nhooks = true\nmemories = false\nchronicle = true\n";
        store.put_common_codex_config(updated_common).unwrap();
        let rendered = subscription_editor_config("gpt-5.5", &options, updated_common).unwrap();
        assert!(rendered.contains("# authored override"));
        assert!(rendered.contains("# own policy"));
        let document: toml_edit::DocumentMut = rendered.parse().unwrap();
        assert_eq!(document["features"]["hooks"].as_bool(), Some(true));
        assert_eq!(document["features"]["memories"].as_bool(), Some(false));
        assert_eq!(document["approval_policy"].as_str(), Some("never"));
        assert_eq!(document["model_context_window"].as_integer(), Some(1000000));
        let mut applied: toml_edit::DocumentMut =
            "model = 'sx-managed'\n[model_providers.router]\nname = 'Router'\n"
                .parse()
                .unwrap();
        crate::provider_config::apply_to_document(&mut applied, &options, updated_common, "router")
            .unwrap();
        assert_eq!(applied["features"]["hooks"].as_bool(), Some(true));
        assert_eq!(applied["features"]["chronicle"].as_bool(), Some(true));
        assert_eq!(applied["approval_policy"].as_str(), Some("never"));
        assert_eq!(applied["model"].as_str(), Some("sx-managed"));
        assert_eq!(applied["model_context_window"].as_integer(), Some(1000000));
        let off = subscription_options_from_config(&options, "model = 'gpt-5.5'\n").unwrap();
        let reopened = subscription_editor_config("gpt-5.5", &off, common).unwrap();
        assert!(
            reopened
                .parse::<toml_edit::DocumentMut>()
                .unwrap()
                .get("model_context_window")
                .is_none()
        );
        crate::provider_config::apply_to_document(&mut applied, &off, common, "router").unwrap();
        assert!(applied.get("model_context_window").is_none());
        assert!(applied.get("model_auto_compact_token_limit").is_none());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn adding_official_models_cannot_overwrite_a_user_renamed_mapping() {
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-collision-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let models = fixture["models"].as_array().unwrap();
        save_connection(&path, &models[..1]).unwrap();
        let store = app::open_store(&path).unwrap();
        let existing = store.models().unwrap().remove(0);
        let collision = "sx-chatgpt-gpt-5-5";
        app::save_mapping(
            &path,
            app::ModelInput {
                provider_id: PROVIDER_ID,
                original_id: &existing.public_id,
                public_id: collision,
                display_name: &existing.display_name,
                upstream_model: &existing.upstream_model,
                catalog_path: "",
                settings: None,
            },
        )
        .unwrap();
        let before = store.models().unwrap();
        assert!(before[0].enabled);
        let provider = store.provider(PROVIDER_ID).unwrap();
        let mut new_model = models[0].clone();
        new_model["slug"] = "fixture-new".into();
        let mut additions = vec![new_model];
        additions.extend(models.clone());
        let error = save_connection(&path, &additions).unwrap_err();
        assert!(error.contains(collision));
        assert!(error.contains("本批未保存"));
        assert_eq!(store.models().unwrap(), before);
        assert_eq!(store.provider(PROVIDER_ID).unwrap(), provider);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn normalized_official_model_ids_cannot_collide_within_one_addition() {
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-normalization-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let mut dotted = fixture["models"][0].clone();
        dotted["slug"] = "fixture.model".into();
        let mut hyphenated = dotted.clone();
        hyphenated["slug"] = "fixture-model".into();
        let error = save_connection(&path, &[dotted, hyphenated]).unwrap_err();
        assert!(error.contains("sx-chatgpt-fixture-model"));
        let store = app::open_store(&path).unwrap();
        assert!(store.models().unwrap().is_empty());
        assert!(store.provider(PROVIDER_ID).unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn api_return_deselects_all_subscription_models_and_preserves_api_mappings() {
        let path = std::env::temp_dir().join(format!(
            "switchx-subscription-selection-test-{}",
            app::new_id().unwrap()
        ));
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        save_connection(&path, fixture["models"].as_array().unwrap()).unwrap();
        let store = app::open_store(&path).unwrap();
        let provider = store.provider(PROVIDER_ID).unwrap().unwrap();
        let official = store.models().unwrap();
        for model in &official {
            app::select_model(&path, &model.public_id, true).unwrap();
        }
        let api_provider = ProviderRecord {
            id: "fixture-api".into(),
            name: "Fixture API".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: official[0].upstream_model.clone(),
            credential_ref: None,
            kind: ProviderKind::ApiKey,
            account_binding: None,
        };
        let api = ModelRecord {
            provider_id: api_provider.id.clone(),
            public_id: "sx-fixture-api".into(),
            enabled: true,
            ..official[0].clone()
        };
        store
            .put_provider_with_models(&api_provider, std::slice::from_ref(&api))
            .unwrap();
        deselect_models(&path).unwrap();
        let saved = store.models().unwrap();
        assert_eq!(saved.len(), official.len() + 1);
        assert_eq!(
            saved
                .iter()
                .find(|model| model.provider_id == api_provider.id),
            Some(&api)
        );
        for model in official {
            assert!(saved.contains(&ModelRecord {
                enabled: false,
                ..model
            }));
        }
        assert_eq!(store.provider(PROVIDER_ID).unwrap(), Some(provider));
        deselect_models(&path).unwrap();
        assert_eq!(store.models().unwrap(), saved);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
