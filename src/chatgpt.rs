//! Codex owns ChatGPT OAuth credentials, refresh and logout. SwitchX never reads them.

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
    storage::{ModelRecord, ProviderRecord},
};

// The explicit provider identity selects authentication, never the model name or URL.
pub const PROVIDER_ID: &str = "switchx-chatgpt";
pub const BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const ACCOUNT_HEADER: &str = "chatgpt-account-id";
const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

/// Native account metadata only; no access, refresh or ID token is read by SwitchX.
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

    fn from_reply(reply: &Value) -> Result<Option<Self>, String> {
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
    if provider.id != PROVIDER_ID
        || provider.base_url != BASE_URL
        || provider.credential_ref.is_some()
    {
        return Err("订阅连接的身份或固定官方地址无效；不能使用第三方地址或 API Key".into());
    }
    Ok(())
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(client::cli_executable());
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
    app::ensure_editable(data_dir)?;
    let first = models.first().ok_or("官方目录没有模型")?;
    let store = app::open_store(data_dir).map_err(|error| error.message())?;
    let provider = if let Some(provider) = store
        .provider(PROVIDER_ID)
        .map_err(|_| "无法读取订阅连接")?
    {
        validate_provider(&provider)?;
        provider
    } else {
        ProviderRecord {
            id: PROVIDER_ID.into(),
            name: "ChatGPT 订阅".into(),
            base_url: BASE_URL.into(),
            model_id: first["slug"].as_str().ok_or("官方模型 ID 无效")?.into(),
            credential_ref: None,
        }
    };
    let saved = store.models().map_err(|_| "无法读取模型资料")?;
    let mut additions: Vec<ModelRecord> = Vec::new();
    for metadata in models {
        catalog::validate_metadata(metadata)?;
        let upstream_model = metadata["slug"].as_str().unwrap();
        if saved
            .iter()
            .any(|model| model.provider_id == PROVIDER_ID && model.upstream_model == upstream_model)
        {
            continue;
        }
        let public_id = format!(
            "sx-chatgpt-{}",
            upstream_model
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '-' {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        );
        if saved
            .iter()
            .chain(&additions)
            .any(|model| model.public_id == public_id)
        {
            return Err(format!(
                "官方模型 {upstream_model} 的公开 ID {public_id} 与其他映射冲突；本批未保存，请修改公开 ID 后重试"
            ));
        }
        let model = ModelRecord {
            provider_id: PROVIDER_ID.into(),
            public_id,
            display_name: format!("{upstream_model}/ChatGPT"),
            upstream_model: upstream_model.into(),
            metadata: metadata.to_string(),
            enabled: saved.iter().all(|model| model.provider_id != PROVIDER_ID)
                && upstream_model == provider.model_id,
            fallback_provider_id: None,
        };
        catalog::publish_saved(&[ModelRecord {
            enabled: true,
            ..model.clone()
        }])?;
        additions.push(model);
    }
    store
        .put_provider_with_models(&provider, &additions)
        .map_err(|_| "无法保存订阅连接和官方模型资料".into())
}

pub async fn require_login(home: &Path) -> Result<(), String> {
    // Honor existing auth storage; only Codex reads or refreshes its credentials.
    workspace(home).await.map(|_| ())
}

pub async fn workspace(home: &Path) -> Result<Option<Workspace>, String> {
    let (status, workspace) = Session::start(home).await?.read_account(false).await?;
    status.require_chatgpt()?;
    Ok(workspace)
}

pub async fn save_mapping(data_dir: &Path, input: app::ModelInput<'_>) -> Result<(), String> {
    if input.provider_id != PROVIDER_ID {
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
    fn official_identity_and_login_links_are_locked() {
        let mut provider = ProviderRecord {
            id: PROVIDER_ID.into(),
            name: "ChatGPT".into(),
            base_url: BASE_URL.into(),
            model_id: "fixture-model".into(),
            credential_ref: None,
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
}
