//! Grok Build-compatible device OAuth. Credentials never enter SQLite or Codex auth.json.
use crate::{
    accounts::{ensure_directory, private_replace, read_private},
    app,
    credentials::Secret,
    storage::{AccountBinding, ModelRecord, ProviderKind, ProviderRecord},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use zeroize::{Zeroize, Zeroizing};

pub const BASE_URL: &str = "https://api.x.ai/v1";
const ISSUER: &str = "https://auth.x.ai";
// Public Grok CLI client, as in CC Switch's xai_oauth_auth.rs; not a SwitchX-owned client.
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub requires_reauth: bool,
}
#[derive(Clone)]
pub struct AccountManager(Arc<Inner>);
struct Inner {
    path: PathBuf,
    issuer: String,
    client: reqwest::Client,
    operation: Arc<AsyncMutex<()>>,
    cache: Mutex<HashMap<String, CachedToken>>,
}
struct CachedToken {
    secret: Secret,
    expires_at: i64,
    refresh_token: Secret,
}
type StoreSnapshot = (AccountStore, Zeroizing<Option<Vec<u8>>>);

#[derive(Default, Serialize, Deserialize)]
struct AccountStore {
    version: u8,
    accounts: BTreeMap<String, StoredAccount>,
    default_account_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct StoredAccount {
    id: String,
    subject: String,
    label: String,
    refresh_token: String,
    requires_reauth: bool,
}
impl Drop for StoredAccount {
    fn drop(&mut self) {
        self.refresh_token.zeroize();
    }
}
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    token_endpoint: String,
    device_authorization_endpoint: String,
}
#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
}
impl Drop for TokenReply {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(v) = &mut self.refresh_token {
            v.zeroize();
        }
        if let Some(v) = &mut self.id_token {
            v.zeroize();
        }
    }
}
pub struct DeviceLogin {
    pub verification_url: String,
    pub user_code: String,
    manager: AccountManager,
    token_endpoint: String,
    device_code: Secret,
    interval: Duration,
    expires_at: tokio::time::Instant,
}
impl AccountManager {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        Self::open_using(data_dir, ISSUER)
    }
    /// Synthetic loopback auth servers only; inference destinations stay independently pinned.
    pub fn open_mock(data_dir: &Path, origin: &str) -> Result<Self, String> {
        let url = reqwest::Url::parse(origin).map_err(|_| "Grok 测试认证地址无效")?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.origin().ascii_serialization() != origin
        {
            return Err("Grok 测试认证只允许 IPv4 回环地址".into());
        }
        Self::open_using(data_dir, origin)
    }
    fn open_using(data_dir: &Path, issuer: &str) -> Result<Self, String> {
        ensure_directory(data_dir)?;
        let manager = Self(Arc::new(Inner {
            path: data_dir.join("xai_oauth_auth.json"),
            issuer: issuer.into(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(30))
                .user_agent("switchx-xai-oauth")
                .build()
                .map_err(|_| "无法创建 Grok 登录连接")?,
            operation: Arc::new(AsyncMutex::new(())),
            cache: Mutex::new(HashMap::new()),
        }));
        manager.read()?;
        Ok(manager)
    }
    fn read(&self) -> Result<StoreSnapshot, String> {
        let bytes = Zeroizing::new(read_private(&self.0.path)?);
        let store = match &*bytes {
            Some(bytes) => {
                serde_json::from_slice::<AccountStore>(bytes).map_err(|_| "Grok 账号文件无效")?
            }
            None => AccountStore {
                version: 1,
                ..Default::default()
            },
        };
        if store.version != 1
            || store.accounts.len() > 256
            || store
                .default_account_id
                .as_ref()
                .is_some_and(|id| !store.accounts.contains_key(id))
            || store.accounts.iter().any(|(id, a)| {
                id != &a.id
                    || !valid_id(id)
                    || a.subject.is_empty()
                    || a.subject.len() > 256
                    || !valid_secret(&a.refresh_token)
                    || a.label.len() > 256
                    || a.label.chars().any(char::is_control)
            })
        {
            return Err("Grok 账号文件资料无效".into());
        }
        Ok((store, bytes))
    }
    fn save(&self, store: &AccountStore, expected: &Option<Vec<u8>>) -> Result<(), String> {
        let bytes =
            Zeroizing::new(serde_json::to_vec_pretty(store).map_err(|_| "无法序列化 Grok 账号")?);
        private_replace(&self.0.path, &bytes, expected)
    }
    fn lock(&self) -> Result<File, String> {
        let path = self.0.path.with_extension("lock");
        if let Ok(meta) = std::fs::symlink_metadata(&path)
            && (!meta.is_file() || meta.file_type().is_symlink())
        {
            return Err("Grok 账号锁必须是普通文件".into());
        }
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(path).map_err(|_| "无法锁定 Grok 账号文件")?;
        file.try_lock()
            .map_err(|_| "Grok 账号正在其他实例中更新，请稍后重试")?;
        Ok(file)
    }
    pub fn list(&self) -> Result<Vec<AccountInfo>, String> {
        let (store, _) = self.read()?;
        Ok(store.accounts.values().map(|a| info(a, &store)).collect())
    }
    pub fn resolve_binding(&self, binding: &AccountBinding) -> Result<AccountInfo, String> {
        let (store, _) = self.read()?;
        let id = match binding {
            AccountBinding::Fixed(id) => id,
            AccountBinding::Default => store
                .default_account_id
                .as_ref()
                .ok_or("请先设置默认 Grok 账号")?,
            AccountBinding::Native => return Err("Grok 上游必须绑定保存的 Grok 账号".into()),
        };
        let account = store
            .accounts
            .get(id)
            .ok_or("绑定的 Grok 账号已删除，请重新选择")?;
        if account.requires_reauth {
            return Err("Grok 账号凭据失效，请重新登录".into());
        }
        Ok(info(account, &store))
    }
    pub async fn set_default(&self, id: &str) -> Result<(), String> {
        let _operation = self.0.operation.lock().await;
        let _lock = self.lock()?;
        let (mut store, bytes) = self.read()?;
        let account = store.accounts.get(id).ok_or("Grok 账号不存在")?;
        if account.requires_reauth {
            return Err("Grok 账号凭据失效，请先重新登录".into());
        }
        store.default_account_id = Some(id.into());
        self.save(&store, &bytes)
    }
    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let _operation = self.0.operation.lock().await;
        let _lock = self.lock()?;
        let (mut store, bytes) = self.read()?;
        store.accounts.remove(id).ok_or("Grok 账号不存在")?;
        if store.default_account_id.as_deref() == Some(id) {
            store.default_account_id = None;
        }
        let result = self.save(&store, &bytes);
        if result.is_ok() {
            self.0
                .cache
                .lock()
                .map_err(|_| "Grok 账号状态不可用")?
                .remove(id);
        }
        result
    }
    /// A rejected access token is refreshed on the next request, never by replaying inference.
    pub fn invalidate_access(&self, id: &str, rejected: &str) {
        if let Ok(mut cache) = self.0.cache.lock()
            && cache
                .get(id)
                .is_some_and(|token| token.secret.expose() == rejected)
        {
            cache.remove(id);
        }
    }

    pub async fn wait_for_idle(&self) {
        let _guard = self.0.operation.lock().await;
    }
    async fn discover(&self) -> Result<Discovery, String> {
        let response = self
            .0
            .client
            .get(format!(
                "{}/.well-known/openid-configuration",
                self.0.issuer
            ))
            .send()
            .await
            .map_err(|_| "无法连接 Grok 认证服务")?;
        if !response.status().is_success() {
            return Err("无法发现 Grok 认证端点".into());
        }
        let discovery: Discovery = serde_json::from_value(read_json(response).await?)
            .map_err(|_| "Grok 认证端点格式无效")?;
        if discovery.issuer != self.0.issuer {
            return Err("Grok 认证签发方不匹配".into());
        }
        for endpoint in [
            &discovery.token_endpoint,
            &discovery.device_authorization_endpoint,
        ] {
            validate_endpoint(endpoint, &self.0.issuer)?;
        }
        Ok(discovery)
    }
    pub async fn start_login(&self) -> Result<DeviceLogin, String> {
        let discovery = self.discover().await?;
        let response = self
            .form(
                &discovery.device_authorization_endpoint,
                &[("client_id", CLIENT_ID), ("scope", SCOPE)],
            )
            .await?;
        if !response.status().is_success() {
            return Err(format!(
                "Grok 设备码请求失败（HTTP {}）",
                response.status().as_u16()
            ));
        }
        let mut value = read_json(response).await?;
        let code = value["device_code"]
            .as_str()
            .filter(|v| valid_secret(v))
            .ok_or("Grok 设备码响应无效")?
            .to_owned();
        let user_code = value["user_code"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
            .ok_or("Grok 用户验证码无效")?
            .to_owned();
        let url = value["verification_uri_complete"]
            .as_str()
            .or_else(|| value["verification_uri"].as_str())
            .ok_or("Grok 登录地址缺失")?
            .to_owned();
        validate_verification(&url)?;
        let expires = value["expires_in"]
            .as_u64()
            .filter(|v| (1..=86400).contains(v))
            .ok_or("Grok 验证码有效期无效")?;
        let interval = value["interval"].as_u64().unwrap_or(5).clamp(1, 60);
        zeroize_value(&mut value);
        Ok(DeviceLogin {
            manager: self.clone(),
            verification_url: url,
            user_code,
            token_endpoint: discovery.token_endpoint,
            device_code: Secret::new(code),
            interval: Duration::from_secs(interval),
            expires_at: tokio::time::Instant::now() + Duration::from_secs(expires),
        })
    }
    async fn form(&self, url: &str, pairs: &[(&str, &str)]) -> Result<reqwest::Response, String> {
        let body = Zeroizing::new(
            reqwest::Url::parse_with_params("https://form.invalid", pairs)
                .map_err(|_| "Grok 授权参数无效")?
                .query()
                .unwrap_or_default()
                .to_owned(),
        );
        self.0
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| "Grok 认证连接失败，请重试".into())
    }
    pub async fn credential(&self, id: &str) -> Result<Secret, String> {
        let operation = self.0.operation.clone().lock_owned().await;
        let manager = self.clone();
        let id = id.to_owned();
        // Once refresh starts, finish persisting any rotated token even if its caller cancels.
        tokio::spawn(async move {
            let _operation = operation;
            manager.refresh(&id).await
        })
        .await
        .map_err(|_| "Grok 凭据刷新任务失败")?
    }
    async fn refresh(&self, id: &str) -> Result<Secret, String> {
        let _lock = self.lock()?;
        let (mut store, bytes) = self.read()?;

        let account = store.accounts.get_mut(id).ok_or("Grok 账号已删除")?;
        if account.requires_reauth {
            return Err("Grok 账号需要重新登录".into());
        }
        if let Some(cached) = self
            .0
            .cache
            .lock()
            .map_err(|_| "Grok 账号状态不可用")?
            .get(id)
            && cached.refresh_token.expose() == account.refresh_token
            && cached.expires_at > chrono::Utc::now().timestamp_millis() + 60_000
        {
            return Ok(Secret::new(cached.secret.expose().into()));
        }
        let discovery = self.discover().await?;
        let response = self
            .form(
                &discovery.token_endpoint,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", CLIENT_ID),
                    ("refresh_token", &account.refresh_token),
                    ("scope", SCOPE),
                ],
            )
            .await?;
        let status = response.status();
        let value = read_json(response).await;
        if matches!(status.as_u16(), 401 | 403)
            || value.as_ref().is_ok_and(|v| {
                matches!(v["error"].as_str(), Some("invalid_grant" | "invalid_token"))
            })
        {
            account.requires_reauth = true;
            self.save(&store, &bytes)?;
            self.0
                .cache
                .lock()
                .map_err(|_| "Grok 账号状态不可用")?
                .remove(id);
            return Err("Grok 账号凭据失效，请重新登录".into());
        }
        if !status.is_success() {
            return Err(format!("Grok 凭据刷新失败（HTTP {}）", status.as_u16()));
        }
        let tokens = token_reply(value?)?;
        if let Some(subject) = token_identity(&tokens).map(|v| v.0)
            && subject != account.subject
        {
            return Err("Grok 刷新返回其他账号，请重新登录".into());
        }
        if let Some(refresh) = &tokens.refresh_token {
            account.refresh_token.clone_from(refresh);
        }
        self.save(&store, &bytes)?;
        let key = Secret::new(tokens.access_token.clone());
        self.0
            .cache
            .lock()
            .map_err(|_| "Grok 账号状态不可用")?
            .insert(
                id.into(),
                CachedToken {
                    secret: Secret::new(tokens.access_token.clone()),
                    expires_at: chrono::Utc::now().timestamp_millis()
                        + tokens.expires_in.unwrap_or(3600).clamp(1, 86400) * 1000,
                    refresh_token: Secret::new(store.accounts[id].refresh_token.clone()),
                },
            );
        Ok(key)
    }
    fn accept_login(
        &self,
        tokens: &TokenReply,
        canceled: &watch::Receiver<bool>,
    ) -> Result<AccountInfo, String> {
        let _lock = self.lock()?;
        if *canceled.borrow() {
            return Err("Grok 登录已取消".into());
        }
        let (mut store, bytes) = self.read()?;

        let (subject, label) = token_identity(tokens).ok_or("Grok 登录缺少账号身份")?;
        let refresh = tokens
            .refresh_token
            .as_ref()
            .ok_or("Grok 登录缺少刷新凭据")?;
        let id = store
            .accounts
            .values()
            .find(|a| a.subject == subject)
            .map(|a| a.id.clone())
            .unwrap_or(app::new_id()?);
        if store.accounts.len() >= 256 && !store.accounts.contains_key(&id) {
            return Err("Grok 保存账号已达上限".into());
        }
        store.accounts.insert(
            id.clone(),
            StoredAccount {
                id: id.clone(),
                subject,
                label,
                refresh_token: refresh.clone(),
                requires_reauth: false,
            },
        );
        if store.default_account_id.is_none() {
            store.default_account_id = Some(id.clone());
        }
        self.save(&store, &bytes)?;
        self.0
            .cache
            .lock()
            .map_err(|_| "Grok 账号状态不可用")?
            .insert(
                id.clone(),
                CachedToken {
                    secret: Secret::new(tokens.access_token.clone()),
                    expires_at: chrono::Utc::now().timestamp_millis()
                        + tokens.expires_in.unwrap_or(3600).clamp(1, 86400) * 1000,
                    refresh_token: Secret::new(refresh.clone()),
                },
            );
        Ok(info(&store.accounts[&id], &store))
    }
}
impl DeviceLogin {
    pub async fn finish(self, mut cancel: watch::Receiver<bool>) -> Result<AccountInfo, String> {
        let mut interval = self.interval;
        loop {
            tokio::select! { biased; _ = cancel.wait_for(|v| *v) => return Err("Grok 登录已取消".into()), _ = tokio::time::sleep_until(self.expires_at) => return Err("Grok 验证码已过期，请重新登录".into()), _ = tokio::time::sleep(interval) => {} }
            let pairs = [
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", CLIENT_ID),
                ("device_code", self.device_code.expose()),
            ];
            let response = tokio::select! { biased; _ = cancel.wait_for(|v| *v) => return Err("Grok 登录已取消".into()), _ = tokio::time::sleep_until(self.expires_at) => return Err("Grok 验证码已过期，请重新登录".into()), result = self.manager.form(&self.token_endpoint, &pairs) => result? };
            let status = response.status();
            let value = tokio::select! {
                biased;
                _ = cancel.wait_for(|v| *v) => return Err("Grok 登录已取消".into()),
                _ = tokio::time::sleep_until(self.expires_at) => return Err("Grok 验证码已过期，请重新登录".into()),
                result = read_json(response) => result?,
            };
            match value["error"].as_str() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval = (interval + Duration::from_secs(5)).min(Duration::from_secs(65));
                    continue;
                }
                Some("access_denied") => return Err("Grok 登录授权被拒绝".into()),
                Some("expired_token") => return Err("Grok 验证码已过期，请重新登录".into()),
                Some(_) => return Err("Grok 登录授权失败，请重试".into()),
                None => {}
            }
            if !status.is_success() {
                return Err(format!("Grok 登录失败（HTTP {}）", status.as_u16()));
            }
            let tokens = token_reply(value)?;
            let _operation = self.manager.0.operation.lock().await;
            return self.manager.accept_login(&tokens, &cancel);
        }
    }
}
fn info(account: &StoredAccount, store: &AccountStore) -> AccountInfo {
    AccountInfo {
        id: account.id.clone(),
        label: account.label.clone(),
        requires_reauth: account.requires_reauth,
        is_default: store.default_account_id.as_deref() == Some(&account.id),
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn valid_secret(v: &str) -> bool {
    !v.is_empty() && v.len() <= 16384 && !v.chars().any(char::is_whitespace)
}
fn token_reply(mut value: Value) -> Result<TokenReply, String> {
    let result =
        serde_json::from_value::<TokenReply>(value.clone()).map_err(|_| "Grok 登录响应格式无效");
    zeroize_value(&mut value);
    let tokens = result?;
    if !valid_secret(&tokens.access_token)
        || tokens
            .refresh_token
            .as_ref()
            .is_some_and(|v| !valid_secret(v))
    {
        return Err("Grok 登录凭据格式无效".into());
    }
    Ok(tokens)
}
fn token_identity(tokens: &TokenReply) -> Option<(String, String)> {
    let claims = tokens
        .id_token
        .as_deref()
        .and_then(claims)
        .or_else(|| claims(&tokens.access_token))?;
    let subject = claims["sub"].as_str()?.to_owned();
    if subject.is_empty() || subject.len() > 256 || subject.chars().any(char::is_control) {
        return None;
    }
    let label = claims["email"]
        .as_str()
        .or_else(|| claims["name"].as_str())
        .unwrap_or("Grok 账号");
    Some((
        subject,
        label.chars().filter(|c| !c.is_control()).take(64).collect(),
    ))
}
fn claims(token: &str) -> Option<Value> {
    let bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?);
    serde_json::from_slice(&bytes).ok()
}
fn zeroize_value(value: &mut Value) {
    match value {
        Value::String(v) => v.zeroize(),
        Value::Object(v) => v.values_mut().for_each(zeroize_value),
        Value::Array(v) => v.iter_mut().for_each(zeroize_value),
        _ => {}
    }
}
fn validate_endpoint(value: &str, issuer: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Grok 认证端点地址无效")?;
    if url.origin().ascii_serialization() != issuer
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Grok 认证端点不受信任".into());
    }
    Ok(())
}
fn validate_verification(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Grok 授权地址无效")?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("auth.x.ai" | "accounts.x.ai"))
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Grok 授权地址不受信任".into());
    }
    Ok(())
}
async fn read_json(mut response: reqwest::Response) -> Result<Value, String> {
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Grok 认证响应读取失败")?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err("Grok 认证响应过大".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Grok 认证响应格式无效".into())
}

pub fn validate_provider(provider: &ProviderRecord) -> Result<(), String> {
    if provider.kind != ProviderKind::XaiOAuth
        || provider.base_url != BASE_URL
        || provider.credential_ref.is_some()
        || !matches!(
            provider.account_binding,
            Some(AccountBinding::Default | AccountBinding::Fixed(_))
        )
    {
        return Err("Grok OAuth 上游资料无效，请重新绑定账号".into());
    }
    Ok(())
}
pub fn save_provider(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    binding: AccountBinding,
) -> Result<(), String> {
    app::ensure_editable(data_dir)?;
    crate::direct::validate_provider(name, BASE_URL, "grok-4.5")?;
    AccountManager::open(data_dir)?.resolve_binding(&binding)?;
    let store = app::open_store(data_dir).map_err(|_| "无法读取 Grok 上游资料")?;
    let existing = id
        .map(|id| {
            store
                .provider(id)
                .map_err(|_| "无法读取 Grok 上游")
                .and_then(|v| v.ok_or("Grok 上游不存在"))
        })
        .transpose()?;
    if let Some(p) = &existing {
        validate_provider(p)?;
    }
    let provider = ProviderRecord {
        id: existing
            .as_ref()
            .map(|p| p.id.clone())
            .unwrap_or(app::new_id()?),
        name: name.trim().into(),
        base_url: BASE_URL.into(),
        model_id: existing
            .as_ref()
            .map(|p| p.model_id.clone())
            .unwrap_or("grok-4.5".into()),
        kind: ProviderKind::XaiOAuth,
        credential_ref: None,
        account_binding: Some(binding),
        icon_id: Some("grok".into()),
    };
    let models = if existing.is_none() {
        let mut metadata = crate::catalog::mapping_metadata(
            "grok-4.5",
            "Grok 4.5",
            &crate::catalog::MappingSettings {
                context_window: "500000",
                reasoning_levels: Some("low, medium, high, xhigh"),
                default_reasoning: Some("high"),
            },
            None,
        )?;
        metadata["supports_parallel_tool_calls"] = true.into();
        metadata["input_modalities"] = serde_json::json!(["text", "image"]);
        vec![ModelRecord {
            provider_id: provider.id.clone(),
            public_id: format!("sx-grok-{}", provider.id),
            display_name: format!("Grok 4.5 / {}", provider.name),
            upstream_model: "grok-4.5".into(),
            metadata: serde_json::to_string(&metadata).map_err(|_| "Grok 模型资料无效")?,
            enabled: false,
            fallback_provider_id: None,
        }]
    } else {
        vec![]
    };
    store
        .put_provider_with_models(&provider, &models)
        .map_err(|_| "无法保存 Grok 上游".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Form, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("switchx-xai-{}", app::new_id().unwrap()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Clone)]
    struct Mock {
        origin: String,
        refreshes: Arc<AtomicUsize>,
        reject: Arc<std::sync::atomic::AtomicBool>,
        polls: Arc<AtomicUsize>,
        hold_refresh: Arc<std::sync::atomic::AtomicBool>,
        refresh_started: Arc<tokio::sync::Notify>,
        release_refresh: Arc<tokio::sync::Notify>,
    }
    struct Server {
        manager: AccountManager,
        state: Mock,
        task: tokio::task::JoinHandle<()>,
        _temp: Temp,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    fn tokens(refresh: &str) -> Value {
        let jwt = format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"sub":"subject-one","email":"fixture@example.invalid"}"#)
        );
        json!({"access_token":jwt,"id_token":jwt,"refresh_token":refresh,"expires_in":3600})
    }
    async fn discovery(State(state): State<Mock>) -> Json<Value> {
        Json(
            json!({"issuer":state.origin,"token_endpoint":format!("{}/token",state.origin),"device_authorization_endpoint":format!("{}/device",state.origin)}),
        )
    }
    async fn device() -> Json<Value> {
        Json(
            json!({"device_code":"private-device-code","user_code":"PUBLIC-CODE","verification_uri":"https://accounts.x.ai/oauth2/device","expires_in":30,"interval":1}),
        )
    }
    async fn token(
        State(state): State<Mock>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Response {
        assert_eq!(form["client_id"], CLIENT_ID);
        if form["grant_type"] == "refresh_token" {
            state.refreshes.fetch_add(1, Ordering::SeqCst);
            state.refresh_started.notify_one();
            if state.hold_refresh.load(Ordering::SeqCst) {
                state.release_refresh.notified().await;
            }
            if state.reject.load(Ordering::SeqCst) {
                return (
                    StatusCode::UNAUTHORIZED,
                    "refresh_token=private-upstream-secret",
                )
                    .into_response();
            }
            assert_eq!(form["refresh_token"], "refresh-one");
            Json(tokens("refresh-two")).into_response()
        } else if state.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"authorization_pending"})),
            )
                .into_response()
        } else {
            Json(tokens("refresh-one")).into_response()
        }
    }
    impl Server {
        async fn new() -> Self {
            let temp = Temp::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let state = Mock {
                origin: origin.clone(),
                refreshes: Arc::new(AtomicUsize::new(0)),
                polls: Arc::new(AtomicUsize::new(0)),
                reject: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                hold_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                refresh_started: Arc::new(tokio::sync::Notify::new()),
                release_refresh: Arc::new(tokio::sync::Notify::new()),
            };
            let router = Router::new()
                .route("/.well-known/openid-configuration", get(discovery))
                .route("/device", post(device))
                .route("/token", post(token))
                .with_state(state.clone());
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let manager = AccountManager::open_mock(&temp.0, &origin).unwrap();
            Self {
                manager,
                state,
                task,
                _temp: temp,
            }
        }
        fn seed(&self) -> AccountInfo {
            let (_, cancel) = watch::channel(false);
            self.manager
                .accept_login(&token_reply(tokens("refresh-one")).unwrap(), &cancel)
                .unwrap()
        }
    }
    #[tokio::test]
    async fn device_login_cancellation_and_private_storage() {
        let server = Server::new().await;
        let login = server.manager.start_login().await.unwrap();
        assert_eq!(login.user_code, "PUBLIC-CODE");
        let (cancel, receiver) = watch::channel(false);
        cancel.send_replace(true);
        assert!(login.finish(receiver).await.unwrap_err().contains("取消"));
        assert!(!server.manager.0.path.exists());
        let login = server.manager.start_login().await.unwrap();
        let (_cancel, receiver) = watch::channel(false);
        let account = login.finish(receiver).await.unwrap();
        assert!(account.is_default);
        assert_eq!(server.manager.list().unwrap(), vec![account.clone()]);
        let bytes = std::fs::read_to_string(&server.manager.0.path).unwrap();
        assert!(bytes.contains("refresh-one"));
        assert!(!bytes.contains("access_token"));
        assert!(!bytes.contains("PUBLIC-CODE"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&server.manager.0.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        server.seed();
        assert_eq!(server.manager.list().unwrap().len(), 1);
        assert_eq!(
            server
                .manager
                .resolve_binding(&AccountBinding::Default)
                .unwrap()
                .id,
            account.id
        );
    }
    #[tokio::test]
    async fn refresh_is_serialized_rotations_persist_and_rejections_require_reauth() {
        let server = Server::new().await;
        let account = server.seed();
        server.manager.0.cache.lock().unwrap().clear();
        let (left, right) = tokio::join!(
            server.manager.credential(&account.id),
            server.manager.credential(&account.id)
        );
        assert_eq!(left.unwrap().expose(), right.unwrap().expose());
        assert_eq!(server.state.refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.manager.read().unwrap().0.accounts[&account.id].refresh_token,
            "refresh-two"
        );
        server.state.reject.store(true, Ordering::SeqCst);
        server.manager.0.cache.lock().unwrap().clear();
        let error = server.manager.credential(&account.id).await.unwrap_err();
        assert!(!error.contains("private-upstream-secret"));
        assert!(server.manager.list().unwrap()[0].requires_reauth);
        assert!(server.manager.set_default(&account.id).await.is_err());
        assert!(
            server
                .manager
                .resolve_binding(&AccountBinding::Default)
                .is_err()
        );
        server.seed();
        assert!(!server.manager.list().unwrap()[0].requires_reauth);
        server.manager.remove(&account.id).await.unwrap();
        assert!(server.manager.credential(&account.id).await.is_err());
        assert!(
            server
                .manager
                .resolve_binding(&AccountBinding::Default)
                .is_err()
        );
    }
    #[tokio::test]
    async fn canceled_refresh_finishes_rotation_but_never_overwrites_an_external_generation() {
        for external_rotation in [false, true] {
            let server = Server::new().await;
            let account = server.seed();
            server.manager.0.cache.lock().unwrap().clear();
            server.state.hold_refresh.store(true, Ordering::SeqCst);
            let manager = server.manager.clone();
            let id = account.id.clone();
            let caller = tokio::spawn(async move { manager.credential(&id).await });
            tokio::time::timeout(
                Duration::from_secs(2),
                server.state.refresh_started.notified(),
            )
            .await
            .unwrap();
            caller.abort();
            let _ = caller.await;
            if external_rotation {
                let (mut store, bytes) = server.manager.read().unwrap();
                store.accounts.get_mut(&account.id).unwrap().refresh_token =
                    "external-rotation".into();
                server.manager.save(&store, &bytes).unwrap();
            }
            server.state.release_refresh.notify_one();
            tokio::time::timeout(Duration::from_secs(2), server.manager.wait_for_idle())
                .await
                .unwrap();
            assert_eq!(
                server.manager.read().unwrap().0.accounts[&account.id].refresh_token,
                if external_rotation {
                    "external-rotation"
                } else {
                    "refresh-two"
                }
            );
            assert_eq!(
                server
                    .manager
                    .0
                    .cache
                    .lock()
                    .unwrap()
                    .contains_key(&account.id),
                !external_rotation
            );
        }
    }

    #[test]
    fn rejects_untrusted_endpoints_secrets_and_symlinked_stores() {
        assert!(validate_endpoint("https://evil.invalid/oauth/token", ISSUER).is_err());
        assert!(validate_endpoint("https://auth.x.ai@evil.invalid/token", ISSUER).is_err());
        assert!(validate_verification("https://accounts.x.ai.evil.invalid/device").is_err());
        assert!(token_reply(json!({"access_token":"a\nb","refresh_token":"r"})).is_err());
        assert!(AccountManager::open(Path::new("relative")).is_err());
        #[cfg(unix)]
        {
            let temp = Temp::new();
            let target = temp.0.join("foreign.json");
            std::fs::write(&target, "{}").unwrap();
            std::os::unix::fs::symlink(&target, temp.0.join("xai_oauth_auth.json")).unwrap();
            assert!(AccountManager::open(&temp.0).is_err());
        }
    }
    #[tokio::test]
    async fn grok_bindings_and_provider_save_never_activate_codex_or_store_tokens_in_sqlite() {
        let server = Server::new().await;
        let account = server.seed();
        let home = server._temp.0.join("codex");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("config.toml"), "model = 'original'\n").unwrap();
        app::open_store(&server._temp.0).unwrap();
        save_provider(
            &server._temp.0,
            None,
            "Grok fixture",
            AccountBinding::Fixed(account.id.clone()),
        )
        .unwrap();
        let store = app::open_store(&server._temp.0).unwrap();
        let providers = store.providers().unwrap();
        assert_eq!(providers[0].kind, ProviderKind::XaiOAuth);
        assert!(!store.models().unwrap()[0].enabled);
        assert!(store.provider_api_key(&providers[0].id).unwrap().is_none());
        assert!(save_provider(&server._temp.0, None, "bad", AccountBinding::Native).is_err());
        assert!(
            save_provider(
                &server._temp.0,
                None,
                "bad",
                AccountBinding::Fixed("deleted".into())
            )
            .is_err()
        );
        let bytes = std::fs::read(server._temp.0.join("switchx.sqlite")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("refresh-one"));
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "model = 'original'\n"
        );
        assert!(!home.join("auth.json").exists());
        server.manager.set_default(&account.id).await.unwrap();
    }
}
