//! Managed ChatGPT accounts. OAuth credentials stay in a private JSON file;
//! neither the account marker nor SQLite/config recovery journals contain OAuth tokens.

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions, Permissions},
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, oneshot, watch};
use zeroize::{Zeroize, Zeroizing};

use crate::{app, config_transaction as files, credentials::Secret, storage::AccountBinding};

const STORE_NAME: &str = "codex_oauth_auth.json";
const MARKER_NAME: &str = ".switchx-account.json";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const MAX_BYTES: usize = 1024 * 1024;
const REFRESH_BUFFER_MS: i64 = 60_000;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub id: String,
    pub label: String,
    pub workspace_id: String,
    pub is_default: bool,
}

#[derive(Debug)]
pub struct ManagedCredential {
    pub access_token: Secret,
    pub workspace_id: String,
}

#[derive(Clone)]
pub struct AccountManager(Arc<Inner>);

struct Inner {
    path: PathBuf,
    state: Mutex<State>,
    // ponytail: one operation at a time; use per-account locks if throughput matters.
    operation: Arc<AsyncMutex<()>>,
    client: reqwest::Client,
    endpoints: Endpoints,
}

#[derive(Clone)]
struct Endpoints {
    usercode: String,
    poll: String,
    token: String,
    verification: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            usercode: "https://auth.openai.com/api/accounts/deviceauth/usercode".into(),
            poll: "https://auth.openai.com/api/accounts/deviceauth/token".into(),
            token: "https://auth.openai.com/oauth/token".into(),
            verification: "https://auth.openai.com/codex/device".into(),
        }
    }
}

#[derive(Clone, Default)]
struct State {
    store: Store,
    access: HashMap<String, CachedToken>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Store {
    version: u8,
    accounts: BTreeMap<String, StoredAccount>,
    default_account_id: Option<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            accounts: BTreeMap::new(),
            default_account_id: None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredAccount {
    id: String,
    label: String,
    workspace_id: String,
    subject: String,
    refresh_token: String,
    id_token: String,
    authenticated_at_ms: i64,
    token_updated_at_ms: i64,
    // Dedicated credential editor only; never copied into SQLite or previews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_json: Option<String>,
}

impl Drop for StoredAccount {
    fn drop(&mut self) {
        self.refresh_token.zeroize();
        self.id_token.zeroize();
        if let Some(auth_json) = &mut self.auth_json {
            auth_json.zeroize();
        }
    }
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at_ms: i64,
    obtained_at_ms: i64,
}

impl Drop for CachedToken {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

struct StoreSession {
    state: State,
    expected: Option<Vec<u8>>,
}

impl Drop for StoreSession {
    fn drop(&mut self) {
        if let Some(bytes) = &mut self.expected {
            bytes.zeroize();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialUse {
    Native,
    Preview,
}

struct CancelOnDrop(watch::Sender<bool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

struct PrivateCliHome(PathBuf);

impl PrivateCliHome {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("switchx-account-context-{}", app::new_id()?));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "无法创建私有账号检查目录")?;
        Ok(Self(path))
    }
}

impl Drop for PrivateCliHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum RefreshFailure {
    Rejected,
    Other(String),
}

#[derive(Serialize, Deserialize)]
struct Marker {
    version: u8,
    account_id: String,
    workspace_id: String,
    subject: String,
}

struct HomeSnapshot {
    auth: Option<Vec<u8>>,
    marker_bytes: Option<Vec<u8>>,
    marker: Option<Marker>,
    live: Option<LiveAuth>,
    active_id: Option<String>,
}

struct Identity {
    workspace_id: String,
    subject: String,
    label: String,
}

struct LiveAuth {
    identity: Identity,
    access_token: String,
    refresh_token: String,
    id_token: String,
    updated_at_ms: Option<i64>,
    auth_json: String,
}

impl Drop for LiveAuth {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
        self.id_token.zeroize();
        self.auth_json.zeroize();
    }
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
        if let Some(value) = &mut self.refresh_token {
            value.zeroize();
        }
        if let Some(value) = &mut self.id_token {
            value.zeroize();
        }
    }
}

/// Device authorization is independent of Codex CLI and its config directory.
pub struct DeviceLogin {
    pub verification_url: String,
    pub user_code: String,
    manager: AccountManager,
    device_id: String,
    interval: Duration,
    expires_at: tokio::time::Instant,
    started_at_ms: i64,
}

impl AccountManager {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        Self::open_with_endpoints(data_dir, Endpoints::default())
    }

    /// Synthetic local probes only; the product always uses the official endpoints.
    #[doc(hidden)]
    pub fn open_mock(data_dir: &Path, origin: &str) -> Result<Self, String> {
        let url = reqwest::Url::parse(origin).map_err(|_| "账号测试地址无效")?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.origin().ascii_serialization() != origin
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("账号测试地址必须是本地回环地址".into());
        }
        Self::open_with_endpoints(
            data_dir,
            Endpoints {
                usercode: format!("{origin}/usercode"),
                poll: format!("{origin}/poll"),
                token: format!("{origin}/token"),
                verification: "https://auth.openai.com/codex/device".into(),
            },
        )
    }

    fn open_with_endpoints(data_dir: &Path, endpoints: Endpoints) -> Result<Self, String> {
        ensure_directory(data_dir)?;
        let path = data_dir.join(STORE_NAME);
        let store = decode_store(read_private(&path)?.as_deref())?;
        let client = reqwest::Client::builder()
            .timeout(REFRESH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("switchx-codex-oauth")
            .build()
            .map_err(|_| "无法创建账号登录连接")?;
        Ok(Self(Arc::new(Inner {
            path,
            state: Mutex::new(State {
                store,
                access: HashMap::new(),
            }),
            operation: Arc::new(AsyncMutex::new(())),
            client,
            endpoints,
        })))
    }

    pub fn list(&self) -> Result<Vec<AccountInfo>, String> {
        // Metadata stays available while a network refresh owns the operation lock.
        let state = self.0.state.lock().map_err(|_| "账号状态不可用")?;
        let mut accounts: Vec<_> = state
            .store
            .accounts
            .values()
            .map(|account| info(account, &state.store))
            .collect();
        accounts.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then_with(|| a.label.cmp(&b.label))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(accounts)
    }

    pub fn default_id(&self) -> Result<Option<String>, String> {
        Ok(self
            .0
            .state
            .lock()
            .map_err(|_| "账号状态不可用")?
            .store
            .default_account_id
            .clone())
    }

    /// Credentials are returned only to the explicit auth.json editor, never a preview.
    /// Reading the selected home does not create files, claim ownership, or renew tokens.
    pub fn editor_auth(&self, binding: &AccountBinding, home: &Path) -> Result<Secret, String> {
        if *binding == AccountBinding::Native {
            return Ok(Secret::new(
                read_editor_native(home)?
                    .map_or_else(|| "{}".into(), |live| live.auth_json.clone()),
            ));
        }
        let _operation = self
            .0
            .operation
            .try_lock()
            .map_err(|_| "账号操作进行中，请稍后重试")?;
        let (_store_lock, session) = self.begin()?;
        let id = match binding {
            AccountBinding::Native => unreachable!(),
            AccountBinding::Default => session
                .state
                .store
                .default_account_id
                .as_deref()
                .ok_or("尚未保存默认 ChatGPT 账号，请添加或选择账号")?,
            AccountBinding::Fixed(id) => id,
        };
        let stored = account(&session.state.store, id)?;
        if let Some(auth_json) = &stored.auth_json {
            return Ok(Secret::new(auth_json.clone()));
        }
        if let Some(token) = session.state.access.get(id) {
            return auth_secret(stored, token);
        }
        if let Some(live) = read_editor_native(home)?
            && matches_identity(stored, &live.identity)
        {
            return Ok(Secret::new(live.auth_json.clone()));
        }
        // Older private stores did not keep access tokens. Open an editable draft
        // instead of renewing a login merely to display the credential editor.
        auth_secret(
            stored,
            &CachedToken {
                value: String::new(),
                expires_at_ms: 0,
                obtained_at_ms: stored.token_updated_at_ms,
            },
        )
    }

    /// Formatting supports incomplete drafts; saving requires a complete login bundle.
    pub fn format_editor_auth(raw: &str) -> Result<Secret, String> {
        if raw.len() > MAX_BYTES {
            return Err("auth.json 超过大小上限".into());
        }
        let mut value: Value =
            serde_json::from_str(raw).map_err(|_| "auth.json 必须是有效的 JSON 对象")?;
        let result = if value.is_object() {
            serde_json::to_string_pretty(&value)
                .map(Secret::new)
                .map_err(|_| "无法格式化 auth.json".into())
        } else {
            Err("auth.json 必须是有效的 JSON 对象".into())
        };
        zeroize_json(&mut value);
        result
    }

    /// Offline structure and claimed-identity validation; no token renewal or request.
    pub fn validate_editor_auth(raw: &str) -> Result<(), String> {
        parse_editor_auth(raw).map(|_| ())
    }

    /// Manual edits stay in the private account store. Activating Codex is separate.
    /// An existing binding may update tokens, but cannot silently change its identity.
    pub fn save_editor_auth(
        &self,
        raw: &str,
        expected_account_id: Option<&str>,
    ) -> Result<AccountInfo, String> {
        self.save_editor_auth_inner(raw, expected_account_id, None, None)
    }

    /// The editor's original snapshot prevents an old draft overwriting a renewal.
    pub fn save_editor_auth_if_unchanged(
        &self,
        raw: &str,
        expected_account_id: Option<&str>,
        expected_raw: &str,
        native_home: Option<&Path>,
    ) -> Result<AccountInfo, String> {
        self.save_editor_auth_inner(raw, expected_account_id, Some(expected_raw), native_home)
    }

    fn save_editor_auth_inner(
        &self,
        raw: &str,
        expected_account_id: Option<&str>,
        expected_raw: Option<&str>,
        native_home: Option<&Path>,
    ) -> Result<AccountInfo, String> {
        let live = parse_editor_auth(raw)?;
        let _operation = self
            .0
            .operation
            .try_lock()
            .map_err(|_| "账号操作进行中，请稍后重试")?;
        let (_store_lock, mut session) = self.begin()?;
        let _home_lock = if let Some(home) = native_home {
            // Match account/import lock order; an absent home is read without
            // creating it merely to save a separate managed account.
            read_editor_native(home)?;
            let lock = if home.is_dir() {
                Some(files::lock_config(&home.join("config.toml"))?)
            } else {
                None
            };
            check_editor_native(home, expected_raw.ok_or("缺少原生登录编辑快照")?)?;
            lock
        } else {
            None
        };
        let existing_id = if let Some(id) = expected_account_id {
            let stored = account(&session.state.store, id)?;
            if !matches_identity(stored, &live.identity) {
                return Err("auth.json 的用户或工作区与绑定账号不一致，请选择新账号后保存".into());
            }
            if let Some(expected_raw) = expected_raw
                && !editor_baseline_matches(stored, expected_raw)?
            {
                return Err("账号登录资料在编辑期间已变化，请重新打开编辑器".into());
            }
            Some(id.to_owned())
        } else {
            session
                .state
                .store
                .accounts
                .values()
                .find(|stored| matches_identity(stored, &live.identity))
                .map(|stored| stored.id.clone())
        };
        if let Some(stored) = existing_id
            .as_deref()
            .and_then(|id| session.state.store.accounts.get(id))
        {
            let previous = stored
                .auth_json
                .as_deref()
                .map(|raw| parse_live(raw.as_bytes()))
                .transpose()?;
            let previous_time = previous
                .as_ref()
                .map_or(Some(stored.token_updated_at_ms), |live| live.updated_at_ms);
            let changed = stored.refresh_token != live.refresh_token
                || stored.id_token != live.id_token
                || previous
                    .as_ref()
                    .is_some_and(|previous| previous.access_token != live.access_token);
            if changed
                && live
                    .updated_at_ms
                    .zip(previous_time)
                    .is_some_and(|(incoming, current)| incoming < current)
            {
                return Err("auth.json 的登录资料早于已保存凭据，请重新获取完整登录资料".into());
            }
        }
        let id = existing_id.clone().map_or_else(app::new_id, Ok)?;
        let now = Utc::now().timestamp_millis();
        let updated_at_ms = existing_id
            .as_deref()
            .and_then(|id| session.state.store.accounts.get(id))
            .map_or(now, |stored| {
                now.max(stored.token_updated_at_ms.saturating_add(1))
            });
        session.state.store.accounts.insert(
            id.clone(),
            StoredAccount {
                id: id.clone(),
                label: live.identity.label.clone(),
                workspace_id: live.identity.workspace_id.clone(),
                subject: live.identity.subject.clone(),
                refresh_token: live.refresh_token.clone(),
                id_token: live.id_token.clone(),
                authenticated_at_ms: now,
                token_updated_at_ms: updated_at_ms,
                auth_json: Some(live.auth_json.clone()),
            },
        );
        session.state.access.remove(&id);
        cache_live(&mut session.state, &id, &live);
        if session.state.store.default_account_id.is_none() {
            session.state.store.default_account_id = Some(id.clone());
        }
        if let Some(home) = native_home {
            check_editor_native(home, expected_raw.ok_or("缺少原生登录编辑快照")?)?;
        }
        self.save(&mut session)?;
        Ok(info(
            account(&session.state.store, &id)?,
            &session.state.store,
        ))
    }

    /// Resolve a provider's choice against the latest account file, not an old UI cache.
    pub async fn resolve_binding(
        &self,
        binding: &AccountBinding,
    ) -> Result<Option<AccountInfo>, String> {
        if *binding == AccountBinding::Native {
            return Ok(None);
        }
        let _operation = self.0.operation.lock().await;
        let (_store_lock, session) = self.begin()?;
        let id = match binding {
            AccountBinding::Native => return Ok(None),
            AccountBinding::Default => session
                .state
                .store
                .default_account_id
                .as_deref()
                .ok_or("尚未保存默认 ChatGPT 账号，请添加或选择账号")?,
            AccountBinding::Fixed(id) => id,
        };
        Ok(Some(info(
            account(&session.state.store, id)?,
            &session.state.store,
        )))
    }

    /// Wait after pausing new work so an already submitted refresh can save its rotation.
    pub async fn wait_for_idle(&self) -> Result<(), String> {
        let _operation = self.0.operation.lock().await;
        Ok(())
    }

    async fn owned_operation<T, F, Fut>(&self, work: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(Self, watch::Receiver<bool>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, String>> + Send + 'static,
    {
        // Waiting for this lock is cancellable. Once submitted, the task owns the
        // lock until any refresh response has been validated and saved.
        let operation = self.0.operation.clone().lock_owned().await;
        let (cancel, receiver) = watch::channel(false);
        let cancel = CancelOnDrop(cancel);
        let (sender, result) = oneshot::channel();
        let manager = self.clone();
        tokio::spawn(async move {
            let _operation = operation;
            let result = if *receiver.borrow() {
                Err("账号操作已取消".into())
            } else {
                work(manager, receiver).await
            };
            let _ = sender.send(result);
        });
        let result = result.await.map_err(|_| "账号操作未完成，请检查账号状态")?;
        drop(cancel);
        result
    }

    pub fn set_default(&self, id: &str) -> Result<(), String> {
        let _operation = self
            .0
            .operation
            .try_lock()
            .map_err(|_| "账号操作进行中，请稍后重试")?;
        let (_lock, mut session) = self.begin()?;
        account(&session.state.store, id)?;
        session.state.store.default_account_id = Some(id.into());
        self.save(&mut session)
    }

    pub async fn start_login(&self) -> Result<DeviceLogin, String> {
        let started_at_ms = Utc::now().timestamp_millis();
        let response = self
            .0
            .client
            .post(&self.0.endpoints.usercode)
            .header("Content-Type", "application/json")
            .body(json!({"client_id": CLIENT_ID}).to_string())
            .send()
            .await
            .map_err(|_| "无法获取 ChatGPT 设备验证码，请稍后重试")?;
        if !response.status().is_success() {
            return Err(format!(
                "获取 ChatGPT 验证码失败（HTTP {}）",
                response.status()
            ));
        }
        let reply = response_value(response).await?;
        let device_id = field(&reply, "device_auth_id")?.to_owned();
        let user_code = field(&reply, "user_code")?.to_owned();
        let interval = reply["interval"]
            .as_u64()
            .or_else(|| {
                reply["interval"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
            })
            .unwrap_or(5)
            .saturating_add(3)
            .clamp(8, 60);
        let expires = reply["expires_in"].as_u64().unwrap_or(900).clamp(1, 900);
        Ok(DeviceLogin {
            verification_url: self.0.endpoints.verification.clone(),
            user_code,
            manager: self.clone(),
            device_id,
            interval: Duration::from_secs(interval),
            expires_at: tokio::time::Instant::now() + Duration::from_secs(expires),
            started_at_ms,
        })
    }

    pub fn import_current(&self, home: &Path) -> Result<AccountInfo, String> {
        let _operation = self
            .0
            .operation
            .try_lock()
            .map_err(|_| "账号操作进行中，请稍后重试")?;
        let (_store_lock, mut session) = self.begin()?;
        ensure_directory(home)?;
        let _home_lock = files::lock_config(&home.join("config.toml"))?;
        let snapshot = read_home(home, &session.state.store)?;
        let live = snapshot
            .live
            .as_ref()
            .ok_or("当前文件没有完整的 ChatGPT 登录；请先使用文件登录")?;
        let id = self.import_into(&mut session, live)?;
        // Import only claims a login file that still matches the inspected bytes.
        if read_private(&home.join("auth.json"))? != snapshot.auth {
            return Err("Codex 登录在导入期间已变化，请重试".into());
        }
        write_marker(home, &session.state.store, &id, &snapshot.marker_bytes)?;
        Ok(info(
            account(&session.state.store, &id)?,
            &session.state.store,
        ))
    }

    pub fn sync_current(&self, home: &Path) -> Result<(), String> {
        let _operation = self
            .0
            .operation
            .try_lock()
            .map_err(|_| "账号操作进行中，请稍后重试")?;
        let (_store_lock, mut session) = self.begin()?;
        ensure_directory(home)?;
        let _home_lock = files::lock_config(&home.join("config.toml"))?;
        let snapshot = read_home(home, &session.state.store)?;
        self.sync_snapshot(&mut session, &snapshot)
    }

    pub fn active_id(&self, home: &Path) -> Result<Option<String>, String> {
        if !home.is_absolute() {
            return Err("Codex 配置目录必须是绝对路径".into());
        }
        let state = self.0.state.lock().map_err(|_| "账号状态不可用")?;
        Ok(read_home(home, &state.store)?.active_id)
    }

    pub async fn activate(&self, id: &str, home: &Path) -> Result<(), String> {
        let id = id.to_owned();
        let home = home.to_path_buf();
        self.owned_operation(move |manager, cancel| async move {
            manager.activate_inner(&id, &home, &cancel).await
        })
        .await
    }

    async fn activate_inner(
        &self,
        id: &str,
        home: &Path,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), String> {
        let (_store_lock, mut session) = self.begin()?;
        account(&session.state.store, id)?;
        ensure_directory(home)?;
        let _home_lock = files::lock_config(&home.join("config.toml"))?;
        let mut snapshot = read_home(home, &session.state.store)?;
        if snapshot.auth.is_some() && snapshot.active_id.is_none() {
            if snapshot.marker.is_some() {
                return Err("Codex 当前登录与托管账号不一致；请先导入当前账号或退出登录".into());
            }
            let live = snapshot
                .live
                .as_ref()
                .ok_or("当前登录不是完整的 ChatGPT 文件登录，无法安全切换")?;
            self.import_into(&mut session, live)?;
        } else {
            self.sync_snapshot(&mut session, &snapshot)?;
        }
        let token = self
            .resolve_token(&mut session, id, home, &mut snapshot, false, cancel)
            .await?;
        if *cancel.borrow() {
            return Err("账号操作已取消；已完成的续期保留在账号存储中".into());
        }
        let target = account(&session.state.store, id)?;
        let auth_bytes = auth_bytes(target, &token)?;
        let config_path = home.join("config.toml");
        let original_config = read_private(&config_path)?;
        let text = original_config
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|_| "Codex 配置不是 UTF-8")?
            .unwrap_or("");
        let mut document: toml_edit::DocumentMut =
            text.parse().map_err(|_| "Codex 配置 TOML 无效")?;
        document["cli_auth_credentials_store"] = toml_edit::value("file");
        let config_bytes = document.to_string().into_bytes();
        // No token-bearing journal: each write compares its original, and failed
        // multi-file activation rolls back only bytes that are still ours.
        files::replace(&config_path, &config_bytes, None, &original_config)?;
        if let Err(error) = private_replace(&home.join("auth.json"), &auth_bytes, &snapshot.auth) {
            let _ = restore_file(&config_path, &original_config, &config_bytes);
            return Err(error);
        }
        if let Err(error) = write_marker(home, &session.state.store, id, &snapshot.marker_bytes) {
            let _ = restore_file(&home.join("auth.json"), &snapshot.auth, &auth_bytes);
            let _ = restore_file(&config_path, &original_config, &config_bytes);
            return Err(error);
        }
        Ok(())
    }

    pub async fn credential(&self, id: &str, home: &Path) -> Result<ManagedCredential, String> {
        self.credential_operation(id, home, false, CredentialUse::Native)
            .await
    }

    pub async fn refresh(&self, id: &str, home: &Path) -> Result<(), String> {
        self.credential_operation(id, home, true, CredentialUse::Native)
            .await
            .map(|_| ())
    }

    /// Preparation can adopt this account's native renewal but never writes the native home.
    pub async fn credential_for_route(
        &self,
        id: &str,
        native_home: &Path,
    ) -> Result<ManagedCredential, String> {
        self.credential_operation(id, native_home, false, CredentialUse::Preview)
            .await
    }

    /// Runtime renewal updates native auth only while its exact ownership and bytes still match.
    pub async fn credential_for_route_with_native_sync(
        &self,
        id: &str,
        native_home: &Path,
    ) -> Result<ManagedCredential, String> {
        self.credential_operation(id, native_home, false, CredentialUse::Native)
            .await
    }

    async fn credential_operation(
        &self,
        id: &str,
        home: &Path,
        force: bool,
        usage: CredentialUse,
    ) -> Result<ManagedCredential, String> {
        let id = id.to_owned();
        let home = home.to_path_buf();
        self.owned_operation(move |manager, cancel| async move {
            manager
                .credential_inner(&id, &home, force, usage, &cancel)
                .await
        })
        .await
    }

    async fn credential_inner(
        &self,
        id: &str,
        home: &Path,
        force: bool,
        usage: CredentialUse,
        cancel: &watch::Receiver<bool>,
    ) -> Result<ManagedCredential, String> {
        let (_store_lock, mut session) = self.begin()?;
        account(&session.state.store, id)?;
        if usage == CredentialUse::Native {
            ensure_directory(home)?;
        } else if !home.is_absolute()
            || !fs::symlink_metadata(home)
                .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            return Err("Codex 配置目录必须是存在的绝对路径普通目录".into());
        }
        let _home_lock = if usage == CredentialUse::Native {
            Some(files::lock_config(&home.join("config.toml"))?)
        } else {
            None
        };
        let mut snapshot = read_home(home, &session.state.store)?;
        if usage == CredentialUse::Preview
            && snapshot.live.as_ref().is_some_and(|live| {
                matches_identity(&session.state.store.accounts[id], &live.identity)
                    && token_expiry(&live.access_token).is_none_or(|expiry| {
                        expiry <= Utc::now().timestamp_millis() + REFRESH_BUFFER_MS
                    })
            })
        {
            return Err(
                "原生登录与绑定账号共用凭据且需要续期或同步；请先检查并续期原生登录，再重新预览"
                    .into(),
            );
        }
        if snapshot.active_id.as_deref() == Some(id) {
            self.sync_snapshot(&mut session, &snapshot)?;
        }
        if usage == CredentialUse::Preview {
            self.guard_native_preview(&mut session, id, &snapshot)?;
        }
        let token = self
            .resolve_token(&mut session, id, home, &mut snapshot, force, cancel)
            .await?;
        let target = account(&session.state.store, id)?;
        if usage == CredentialUse::Native && snapshot.active_id.as_deref() == Some(id) {
            let bytes = auth_bytes(target, &token)?;
            let current = read_home(home, &session.state.store)?;
            if current.active_id.as_deref() == Some(id)
                && current.auth == snapshot.auth
                && current.marker_bytes == snapshot.marker_bytes
                && snapshot.auth.as_deref() != Some(bytes.as_slice())
            {
                private_replace(&home.join("auth.json"), &bytes, &snapshot.auth)?;
            }
        }
        Ok(ManagedCredential {
            access_token: Secret::new(token.value.clone()),
            workspace_id: target.workspace_id.clone(),
        })
    }

    fn guard_native_preview(
        &self,
        session: &mut StoreSession,
        id: &str,
        snapshot: &HomeSnapshot,
    ) -> Result<(), String> {
        let stored = account(&session.state.store, id)?;
        let Some(live) = snapshot
            .live
            .as_ref()
            .filter(|live| matches_identity(stored, &live.identity))
        else {
            return Ok(());
        };
        // An entry login can share a bundle with this bound account. Preparing
        // must not rotate it behind an unchanged native auth.json.
        if live.refresh_token != stored.refresh_token
            || live.id_token != stored.id_token
            || token_expiry(&live.access_token)
                .is_none_or(|expiry| expiry <= Utc::now().timestamp_millis() + REFRESH_BUFFER_MS)
        {
            return Err(
                "原生登录与绑定账号共用凭据且需要续期或同步；请先检查并续期原生登录，再重新预览"
                    .into(),
            );
        }
        cache_live(&mut session.state, id, live);
        self.publish(&session.state)
    }

    /// Discover the selected account's official destination without activating it in Codex.
    pub async fn workspace_for_route(
        &self,
        id: &str,
        native_home: &Path,
        cli_path: &Path,
    ) -> Result<crate::chatgpt::Workspace, String> {
        self.workspace_operation(id, native_home, cli_path, None)
            .await
    }

    /// Synthetic probes only. The returned workspace still passes official HTTPS validation.
    #[doc(hidden)]
    pub async fn workspace_for_route_mock(
        &self,
        id: &str,
        native_home: &Path,
        cli_path: &Path,
        discovery_address: SocketAddr,
    ) -> Result<crate::chatgpt::Workspace, String> {
        if discovery_address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err("账号测试发现地址必须是 IPv4 回环地址".into());
        }
        self.workspace_operation(id, native_home, cli_path, Some(discovery_address))
            .await
    }

    async fn workspace_operation(
        &self,
        id: &str,
        native_home: &Path,
        cli_path: &Path,
        discovery_address: Option<SocketAddr>,
    ) -> Result<crate::chatgpt::Workspace, String> {
        if !cli_path.is_absolute() || !cli_path.is_file() {
            return Err("Codex CLI 必须是存在的绝对路径文件".into());
        }
        let id = id.to_owned();
        let native_home = native_home.to_path_buf();
        let cli_path = cli_path.to_path_buf();
        self.owned_operation(move |manager, mut cancel| async move {
            let native_before = read_private(&native_home.join("auth.json"))?;
            let credential = manager
                .credential_inner(&id, &native_home, false, CredentialUse::Preview, &cancel)
                .await?;
            if *cancel.borrow() {
                return Err("账号检查已取消；已完成的续期保留在账号存储中".into());
            }
            let context = PrivateCliHome::new()?;
            {
                let (_store_lock, session) = manager.begin()?;
                let stored = account(&session.state.store, &id)?;
                let token = session
                    .state
                    .access
                    .get(&id)
                    .ok_or("账号检查缺少登录缓存")?;
                private_replace(
                    &context.0.join("auth.json"),
                    &auth_bytes(stored, token)?,
                    &None,
                )?;
                write_marker(&context.0, &session.state.store, &id, &None)?;
            }
            let mut config = "cli_auth_credentials_store = \"file\"\n".to_owned();
            if let Some(address) = discovery_address {
                config.push_str(&format!("chatgpt_base_url = \"http://{address}\"\n"));
            }
            private_replace(&context.0.join("config.toml"), config.as_bytes(), &None)?;
            let discovered = tokio::select! {
                biased;
                _ = cancel.wait_for(|value| *value) => Err("账号检查已取消".to_owned()),
                result = crate::chatgpt::workspace_using(&context.0, &cli_path) => result,
            };
            // A CLI can renew during account/read. Adopt only this temporary
            // home's proven identity before its token-bearing files are removed.
            {
                let (_store_lock, mut session) = manager.begin()?;
                let snapshot = read_home(&context.0, &session.state.store)?;
                if snapshot.active_id.as_deref() != Some(&id) {
                    return Err("账号检查期间临时登录身份已变化，请重新检查".into());
                }
                manager.sync_snapshot(&mut session, &snapshot)?;
                let native = read_home(&native_home, &session.state.store)?;
                manager.guard_native_preview(&mut session, &id, &native)?;
            }
            if read_private(&native_home.join("auth.json"))? != native_before {
                return Err("原生登录在账号检查期间已变化，请重新预览".into());
            }
            let workspace =
                discovered?.ok_or("当前 Codex CLI 未返回该账号的工作区路由资料，请更新 CLI")?;
            workspace.validate()?;
            if workspace.account_id != credential.workspace_id {
                return Err("账号与发现的工作区不一致，请重新检查".into());
            }
            Ok(workspace)
        })
        .await
    }

    pub async fn remove(&self, id: &str, home: &Path) -> Result<(), String> {
        let _operation = self.0.operation.lock().await;
        let (_store_lock, mut session) = self.begin()?;
        account(&session.state.store, id)?;
        ensure_directory(home)?;
        let _home_lock = files::lock_config(&home.join("config.toml"))?;
        let snapshot = read_home(home, &session.state.store)?;
        if snapshot
            .marker
            .as_ref()
            .is_some_and(|marker| marker.account_id == id)
        {
            if snapshot.auth.is_some() && snapshot.active_id.as_deref() != Some(id) {
                return Err("Codex 登录已被外部更改；为避免删除其他账号，本次移除已取消".into());
            }
            remove_unchanged(&home.join("auth.json"), &snapshot.auth)?;
            remove_unchanged(&home.join(MARKER_NAME), &snapshot.marker_bytes)?;
        }
        session.state.store.accounts.remove(id);
        session.state.access.remove(id);
        if session.state.store.default_account_id.as_deref() == Some(id) {
            session.state.store.default_account_id = session
                .state
                .store
                .accounts
                .values()
                .max_by_key(|account| (account.authenticated_at_ms, &account.id))
                .map(|account| account.id.clone());
        }
        self.save(&mut session)
    }

    fn begin(&self) -> Result<(File, StoreSession), String> {
        let lock = lock_store(self.0.path.parent().ok_or("账号目录无效")?)?;
        let expected = read_private(&self.0.path)?;
        let store = decode_store(expected.as_deref())?;
        let mut state = self.0.state.lock().map_err(|_| "账号状态不可用")?.clone();
        state.access.retain(|id, _| {
            matches!((state.store.accounts.get(id), store.accounts.get(id)), (Some(old), Some(new))
                if old.refresh_token == new.refresh_token && old.id_token == new.id_token)
        });
        state.store = store;
        self.publish(&state)?;
        Ok((lock, StoreSession { state, expected }))
    }

    fn publish(&self, state: &State) -> Result<(), String> {
        *self.0.state.lock().map_err(|_| "账号状态不可用")? = state.clone();
        Ok(())
    }

    fn save(&self, session: &mut StoreSession) -> Result<(), String> {
        if session.state.store.accounts.len() > 256 {
            return Err("保存的 ChatGPT 账号已达 256 个上限；本次账号变更未写入".into());
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec_pretty(&session.state.store).map_err(|_| "无法保存账号资料")?,
        );
        if bytes.len() > MAX_BYTES {
            return Err("账号凭据文件已达大小上限；本次账号变更未写入".into());
        }
        private_replace(&self.0.path, &bytes, &session.expected)?;
        if let Some(previous) = &mut session.expected {
            previous.zeroize();
        }
        session.expected = Some(bytes.to_vec());
        self.publish(&session.state)
    }

    fn import_into(&self, session: &mut StoreSession, live: &LiveAuth) -> Result<String, String> {
        if let Some(id) = session
            .state
            .store
            .accounts
            .values()
            .find(|account| matches_identity(account, &live.identity))
            .map(|account| account.id.clone())
        {
            self.adopt_live(session, &id, live, false)?;
            return Ok(id);
        }
        let now = Utc::now().timestamp_millis();
        let id = app::new_id()?;
        session.state.store.accounts.insert(
            id.clone(),
            StoredAccount {
                id: id.clone(),
                label: live.identity.label.clone(),
                workspace_id: live.identity.workspace_id.clone(),
                subject: live.identity.subject.clone(),
                refresh_token: live.refresh_token.clone(),
                id_token: live.id_token.clone(),
                authenticated_at_ms: now,
                token_updated_at_ms: live.updated_at_ms.unwrap_or(now),
                auth_json: Some(live.auth_json.clone()),
            },
        );
        cache_live(&mut session.state, &id, live);
        if session.state.store.default_account_id.is_none() {
            session.state.store.default_account_id = Some(id.clone());
        }
        self.save(session)?;
        Ok(id)
    }

    fn sync_snapshot(
        &self,
        session: &mut StoreSession,
        snapshot: &HomeSnapshot,
    ) -> Result<(), String> {
        if snapshot.marker.is_none() || snapshot.auth.is_none() {
            return Ok(());
        }
        let id = snapshot
            .active_id
            .as_deref()
            .ok_or("当前 Codex 登录与托管标记不一致，请先导入当前账号")?;
        self.adopt_live(
            session,
            id,
            snapshot.live.as_ref().ok_or("当前登录资料无效")?,
            true,
        )
    }

    fn adopt_live(
        &self,
        session: &mut StoreSession,
        id: &str,
        live: &LiveAuth,
        strict: bool,
    ) -> Result<(), String> {
        let account = session
            .state
            .store
            .accounts
            .get_mut(id)
            .ok_or("所选 ChatGPT 账号已删除；请重新选择")?;
        if !matches_identity(account, &live.identity) {
            return Err("当前登录的用户或工作区与所选账号不一致".into());
        }
        let changed =
            account.refresh_token != live.refresh_token || account.id_token != live.id_token;
        let newer = live
            .updated_at_ms
            .is_some_and(|time| time > account.token_updated_at_ms);
        if changed && !newer {
            if live
                .updated_at_ms
                .is_some_and(|time| time < account.token_updated_at_ms)
            {
                return Ok(());
            }
            if strict {
                return Err("无法安全判断 Codex 凭据新旧；请重新登录后导入当前账号".into());
            }
            return Ok(());
        }
        if changed || newer {
            account.refresh_token.clone_from(&live.refresh_token);
            account.id_token.clone_from(&live.id_token);
            account.label.clone_from(&live.identity.label);
            account.token_updated_at_ms = live.updated_at_ms.unwrap_or(account.token_updated_at_ms);
            set_auth_snapshot(account, &live.auth_json);
            session.state.access.remove(id);
            cache_live(&mut session.state, id, live);
            self.save(session)?;
        } else {
            let capture_snapshot = account.auth_json.is_none()
                || (account.auth_json.as_deref() != Some(&live.auth_json)
                    && live
                        .updated_at_ms
                        .is_some_and(|time| time >= account.token_updated_at_ms));
            if capture_snapshot {
                set_auth_snapshot(account, &live.auth_json);
            }
            cache_live(&mut session.state, id, live);
            if capture_snapshot {
                self.save(session)?;
            } else {
                self.publish(&session.state)?;
            }
        }
        Ok(())
    }

    async fn resolve_token(
        &self,
        session: &mut StoreSession,
        id: &str,
        home: &Path,
        snapshot: &mut HomeSnapshot,
        force: bool,
        cancel: &watch::Receiver<bool>,
    ) -> Result<CachedToken, String> {
        let mut stored = account(&session.state.store, id)?.clone();
        if !force
            && let Some(cached) = session.state.access.get(id)
            && cached.expires_at_ms > Utc::now().timestamp_millis() + REFRESH_BUFFER_MS
        {
            return Ok(cached.clone());
        }
        if *cancel.borrow() {
            return Err("账号操作已取消".into());
        }
        let deadline = tokio::time::Instant::now() + REFRESH_TIMEOUT;
        let tokens = match self.refresh_tokens(&stored.refresh_token, deadline).await {
            Ok(tokens) => tokens,
            Err(RefreshFailure::Other(error)) => return Err(error),
            Err(RefreshFailure::Rejected) => {
                // A native CLI can rotate the same refresh token while our
                // request is in flight. Adopt its newer same-user generation
                // and retry once, never use another account's live credentials.
                let latest = read_home(home, &session.state.store)?;
                let live = latest
                    .live
                    .as_ref()
                    .filter(|live| {
                        latest.active_id.as_deref() == Some(id)
                            && live.refresh_token != stored.refresh_token
                    })
                    .ok_or("ChatGPT 账号续期失败；请重新登录该账号")?;
                self.adopt_live(session, id, live, true)?;
                let next = account(&session.state.store, id)?.clone();
                if next.refresh_token == stored.refresh_token {
                    return Err("ChatGPT 账号续期失败；请重新登录该账号".into());
                }
                stored = next;
                *snapshot = latest;
                if *cancel.borrow() {
                    return Err("账号操作已取消；已采纳原生客户端的新凭据".into());
                }
                self.refresh_tokens(&stored.refresh_token, deadline)
                    .await
                    .map_err(|failure| match failure {
                        RefreshFailure::Other(error) => error,
                        RefreshFailure::Rejected => "ChatGPT 账号续期失败；请重新登录该账号".into(),
                    })?
            }
        };
        validate_secret(&tokens.access_token)?;
        let next_id = tokens
            .id_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&stored.id_token);
        let identity = identity(next_id)?;
        if !matches_identity(&stored, &identity) {
            return Err("ChatGPT 续期返回了不同用户或工作区；原账号保持不变".into());
        }
        let next_refresh = tokens
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&stored.refresh_token);
        validate_secret(next_refresh)?;
        if snapshot.active_id.as_deref() == Some(id)
            && read_private(&home.join("auth.json"))? != snapshot.auth
        {
            // Do not publish a response for the old native generation. Saving
            // it first could make a newer CLI token look older on the next sync.
            let latest = read_home(home, &session.state.store)?;
            if latest.active_id.as_deref() == Some(id) {
                if let Some(live) = latest.live.as_ref() {
                    self.adopt_live(session, id, live, true)?;
                }
                return Err("Codex 登录在续期期间已变化，旧续期响应已丢弃，请重试".into());
            }
            // A different native login does not own this verified rotation.
            // Save it for the bound account without touching the foreign login.
            *snapshot = latest;
        }
        let now = Utc::now()
            .timestamp_millis()
            .max(stored.token_updated_at_ms.saturating_add(1));
        let current = session
            .state
            .store
            .accounts
            .get_mut(id)
            .ok_or("所选 ChatGPT 账号已删除")?;
        current.refresh_token = next_refresh.to_owned();
        current.id_token = next_id.to_owned();
        current.label = identity.label;
        current.token_updated_at_ms = now;
        let cached = CachedToken {
            value: tokens.access_token.clone(),
            expires_at_ms: tokens
                .expires_in
                .filter(|seconds| *seconds > 0)
                .map(|seconds| now.saturating_add(seconds.min(86400).saturating_mul(1000)))
                .or_else(|| token_expiry(&tokens.access_token))
                .unwrap_or(now + 300_000),
            obtained_at_ms: now,
        };
        update_auth_snapshot(current, &cached)?;
        session.state.access.insert(id.into(), cached.clone());
        self.save(session)?;
        Ok(cached)
    }

    async fn refresh_tokens(
        &self,
        refresh: &str,
        deadline: tokio::time::Instant,
    ) -> Result<TokenReply, RefreshFailure> {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(RefreshFailure::Other(
                "ChatGPT 账号续期超时，请稍后重试".into(),
            ));
        }
        let response = self
            .0
            .client
            .post(&self.0.endpoints.token)
            .timeout(remaining)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", CLIENT_ID),
                ("scope", "openid profile email"),
            ]))
            .send()
            .await
            .map_err(|_| RefreshFailure::Other("ChatGPT 账号续期连接失败，请稍后重试".into()))?;
        let status = response.status();
        if !response.status().is_success() {
            let body = response_value(response).await.unwrap_or(Value::Null);
            let code = body["error"]["code"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .or_else(|| body["code"].as_str());
            return Err(
                if matches!(
                    status,
                    reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
                ) || matches!(
                    code,
                    Some(
                        "refresh_token_expired"
                            | "refresh_token_reused"
                            | "refresh_token_invalidated"
                            | "invalid_grant"
                    )
                ) {
                    RefreshFailure::Rejected
                } else {
                    RefreshFailure::Other(format!("ChatGPT 账号续期失败（HTTP {status}）"))
                },
            );
        }
        let tokens: TokenReply = serde_json::from_value(
            response_value(response)
                .await
                .map_err(RefreshFailure::Other)?,
        )
        .map_err(|_| RefreshFailure::Other("ChatGPT 账号续期响应无效".into()))?;
        Ok(tokens)
    }
}

impl DeviceLogin {
    pub async fn finish(self, mut cancel: watch::Receiver<bool>) -> Result<AccountInfo, String> {
        let tokens = tokio::select! {
            biased;
            _ = cancel.wait_for(|value| *value) => return Err("ChatGPT 登录已取消".into()),
            result = tokio::time::timeout_at(self.expires_at, self.poll()) =>
                result.map_err(|_| "ChatGPT 验证码已过期，请重新登录")??,
        };
        let _operation = tokio::select! {
            biased;
            _ = cancel.wait_for(|value| *value) => return Err("ChatGPT 登录已取消".into()),
            guard = self.manager.0.operation.lock() => guard,
        };
        if *cancel.borrow() {
            return Err("ChatGPT 登录已取消".into());
        }
        let (_store_lock, mut session) = self.manager.begin()?;
        let identity = identity(
            tokens
                .id_token
                .as_deref()
                .ok_or("ChatGPT 登录缺少用户身份资料")?,
        )?;
        let existing = session
            .state
            .store
            .accounts
            .values()
            .find(|stored| matches_identity(stored, &identity))
            .cloned();
        if existing
            .as_ref()
            .is_some_and(|stored| stored.authenticated_at_ms > self.started_at_ms)
        {
            return Err("该账号已有较新的登录授权，本次旧登录结果已丢弃".into());
        }
        validate_secret(&tokens.access_token)?;
        let refresh = tokens
            .refresh_token
            .as_deref()
            .ok_or("ChatGPT 登录缺少续期凭据")?;
        validate_secret(refresh)?;
        let id = match &existing {
            Some(stored) => stored.id.clone(),
            None => app::new_id()?,
        };
        let now = Utc::now().timestamp_millis();
        let mut stored = StoredAccount {
            id: id.clone(),
            label: identity.label,
            workspace_id: identity.workspace_id,
            subject: identity.subject,
            refresh_token: refresh.to_owned(),
            id_token: tokens
                .id_token
                .clone()
                .ok_or("ChatGPT 登录缺少用户身份资料")?,
            authenticated_at_ms: now,
            token_updated_at_ms: existing.as_ref().map_or(now, |stored| {
                now.max(stored.token_updated_at_ms.saturating_add(1))
            }),
            auth_json: None,
        };
        let cached = CachedToken {
            value: tokens.access_token.clone(),
            expires_at_ms: tokens
                .expires_in
                .filter(|seconds| *seconds > 0)
                .map(|seconds| now + seconds.min(86400) * 1000)
                .or_else(|| token_expiry(&tokens.access_token))
                .unwrap_or(now + 300_000),
            obtained_at_ms: now,
        };
        update_auth_snapshot(&mut stored, &cached)?;
        session.state.store.accounts.insert(id.clone(), stored);
        session.state.access.insert(id.clone(), cached);
        if session.state.store.default_account_id.is_none() {
            session.state.store.default_account_id = Some(id.clone());
        }
        if *cancel.borrow() {
            return Err("ChatGPT 登录已取消".into());
        }
        self.manager.save(&mut session)?;
        Ok(info(
            account(&session.state.store, &id)?,
            &session.state.store,
        ))
    }

    async fn poll(&self) -> Result<TokenReply, String> {
        loop {
            let response = self
                .manager
                .0
                .client
                .post(&self.manager.0.endpoints.poll)
                .header("Content-Type", "application/json")
                .body(
                    json!({"device_auth_id": self.device_id, "user_code": self.user_code})
                        .to_string(),
                )
                .send()
                .await
                .map_err(|_| "ChatGPT 登录连接失败，请重新登录")?;
            let status = response.status();
            if matches!(
                status,
                reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND
            ) {
                tokio::time::sleep(self.interval).await;
                continue;
            }
            if !status.is_success() {
                return Err("ChatGPT 授权未完成或验证码已过期，请重新登录".into());
            }
            let reply = response_value(response).await?;
            let response = self
                .manager
                .0
                .client
                .post(&self.manager.0.endpoints.token)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body(&[
                    ("grant_type", "authorization_code"),
                    ("code", field(&reply, "authorization_code")?),
                    ("code_verifier", field(&reply, "code_verifier")?),
                    ("client_id", CLIENT_ID),
                    ("redirect_uri", REDIRECT_URI),
                ]))
                .send()
                .await
                .map_err(|_| "无法完成 ChatGPT 授权，请重新登录")?;
            if !response.status().is_success() {
                return Err("ChatGPT 授权交换失败，请重新登录".into());
            }
            return serde_json::from_value(response_value(response).await?)
                .map_err(|_| "ChatGPT 登录响应格式无效".into());
        }
    }
}

fn info(account: &StoredAccount, store: &Store) -> AccountInfo {
    AccountInfo {
        id: account.id.clone(),
        label: account.label.clone(),
        workspace_id: account.workspace_id.clone(),
        is_default: store.default_account_id.as_deref() == Some(&account.id),
    }
}

fn account<'a>(store: &'a Store, id: &str) -> Result<&'a StoredAccount, String> {
    store
        .accounts
        .get(id)
        .ok_or_else(|| "所选 ChatGPT 账号已删除；请重新选择".into())
}

fn matches_identity(account: &StoredAccount, identity: &Identity) -> bool {
    account.workspace_id == identity.workspace_id && account.subject == identity.subject
}

fn decode_store(bytes: Option<&[u8]>) -> Result<Store, String> {
    let Some(bytes) = bytes else {
        return Ok(Store::default());
    };
    let store: Store = serde_json::from_slice(bytes).map_err(|_| "账号资料文件格式无效")?;
    if store.version != 1 || store.accounts.len() > 256 {
        return Err("账号资料文件版本或数量无效".into());
    }
    for (id, stored) in &store.accounts {
        let claimed_identity = identity(&stored.id_token)?;
        if id != &stored.id
            || id.len() != 32
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !matches_identity(stored, &claimed_identity)
            || stored.label != claimed_identity.label
        {
            return Err("账号资料索引或用户身份无效".into());
        }
        validate_secret(&stored.refresh_token)?;
        if let Some(auth_json) = &stored.auth_json {
            let live = parse_live(auth_json.as_bytes())?;
            if !matches_identity(stored, &live.identity)
                || stored.refresh_token != live.refresh_token
                || stored.id_token != live.id_token
            {
                return Err("账号 auth.json 与保存身份不一致".into());
            }
        }
    }
    if store
        .default_account_id
        .as_ref()
        .is_some_and(|id| !store.accounts.contains_key(id))
    {
        return Err("默认账号不存在，请修复账号资料文件".into());
    }
    Ok(store)
}

fn jwt_payload(token: &str) -> Result<Value, String> {
    validate_secret(token)?;
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err("ChatGPT 用户身份凭据格式无效".into());
    }
    let header: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[0])
            .map_err(|_| "用户身份凭据编码无效")?,
    )
    .map_err(|_| "用户身份凭据格式无效")?;
    if header["alg"]
        .as_str()
        .is_none_or(|alg| alg.is_empty() || alg.eq_ignore_ascii_case("none"))
    {
        return Err("用户身份凭据算法无效".into());
    }
    serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(parts[1])
            .map_err(|_| "用户身份凭据编码无效")?,
    )
    .map_err(|_| "用户身份凭据格式无效".into())
}

fn identity(id_token: &str) -> Result<Identity, String> {
    // JWT claims are an ownership key, not signature verification. Tokens enter
    // through OpenAI TLS responses or an explicit import of the selected home.
    let claims = jwt_payload(id_token)?;
    let subject = field(&claims, "sub")?.to_owned();
    let workspace = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .or_else(|| claims["chatgpt_account_id"].as_str())
        .ok_or("ChatGPT 登录缺少工作区身份")?;
    if subject.len() > 512
        || subject.chars().any(char::is_control)
        || workspace.is_empty()
        || workspace.len() > 256
        || !workspace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err("ChatGPT 用户或工作区身份无效".into());
    }
    let label = claims["email"]
        .as_str()
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .unwrap_or_else(|| format!("ChatGPT ({workspace})"));
    Ok(Identity {
        workspace_id: workspace.into(),
        subject,
        label,
    })
}

fn parse_live(bytes: &[u8]) -> Result<LiveAuth, String> {
    if bytes.len() > MAX_BYTES {
        return Err("Codex 登录文件超过大小上限".into());
    }
    let mut auth: Value = serde_json::from_slice(bytes).map_err(|_| "Codex 登录文件格式无效")?;
    let result = (|| {
        if !auth.is_object()
            || auth.get("auth_mode").is_some_and(|mode| mode != "chatgpt")
            || !auth["OPENAI_API_KEY"].is_null()
        {
            return Err("当前 Codex 不是 ChatGPT 文件登录".into());
        }
        let tokens = &auth["tokens"];
        let id_token = field(tokens, "id_token")?;
        let identity = identity(id_token)?;
        if field(tokens, "account_id")? != identity.workspace_id {
            return Err("Codex 登录文件的工作区与用户身份不一致".into());
        }
        let access_token = field(tokens, "access_token")?;
        let refresh_token = field(tokens, "refresh_token")?;
        let auth_json = serde_json::to_string_pretty(&auth).map_err(|_| "无法生成 auth.json")?;
        Ok(LiveAuth {
            identity,
            id_token: id_token.to_owned(),
            access_token: access_token.to_owned(),
            refresh_token: refresh_token.to_owned(),
            updated_at_ms: auth["last_refresh"]
                .as_str()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|date| date.timestamp_millis()),
            auth_json,
        })
    })();
    zeroize_json(&mut auth);
    result
}

fn parse_editor_auth(raw: &str) -> Result<LiveAuth, String> {
    let live = parse_live(raw.as_bytes())?;
    if [&live.id_token, &live.access_token, &live.refresh_token]
        .iter()
        .any(|token| token.chars().any(char::is_whitespace))
    {
        return Err("ChatGPT 登录凭据格式无效".into());
    }
    if live.access_token.contains('.') {
        let mut claims = jwt_payload(&live.access_token)?;
        let is_object = claims.is_object();
        zeroize_json(&mut claims);
        if !is_object {
            return Err("ChatGPT 登录凭据格式无效".into());
        }
    }
    Ok(live)
}

fn editor_baseline_matches(stored: &StoredAccount, raw: &str) -> Result<bool, String> {
    if let Some(current) = &stored.auth_json {
        return Ok(AccountManager::format_editor_auth(raw)?.expose() == current);
    }
    // Legacy drafts may have an empty access_token; their persisted ownership
    // still consists of the exact saved refresh and ID tokens.
    let mut value: Value = serde_json::from_str(raw).map_err(|_| "编辑器的原始登录资料无效")?;
    let matches = value["tokens"]["refresh_token"].as_str() == Some(&stored.refresh_token)
        && value["tokens"]["id_token"].as_str() == Some(&stored.id_token);
    zeroize_json(&mut value);
    Ok(matches)
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(value) => value.zeroize(),
        Value::Array(values) => values.iter_mut().for_each(zeroize_json),
        Value::Object(values) => values.values_mut().for_each(zeroize_json),
        _ => {}
    }
}

fn read_editor_native(home: &Path) -> Result<Option<LiveAuth>, String> {
    if !home.is_absolute() {
        return Err("Codex 配置目录必须是绝对路径".into());
    }
    if let Ok(metadata) = fs::symlink_metadata(home)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err("Codex 配置目录必须是普通目录".into());
    }
    let Some(bytes) = read_private(&home.join("auth.json"))? else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(bytes);
    Ok(parse_live(&bytes).ok())
}

fn check_editor_native(home: &Path, expected_raw: &str) -> Result<(), String> {
    let current = read_editor_native(home)?;
    let current = current.as_ref().map_or("{}", |live| &live.auth_json);
    if AccountManager::format_editor_auth(expected_raw)?.expose() != current {
        return Err("Codex 原生登录在编辑期间已变化，请重新打开编辑器".into());
    }
    Ok(())
}

fn token_expiry(token: &str) -> Option<i64> {
    jwt_payload(token).ok()?["exp"].as_i64()?.checked_mul(1000)
}

fn cache_live(state: &mut State, id: &str, live: &LiveAuth) {
    if let Some(expires_at_ms) = token_expiry(&live.access_token) {
        let obtained_at_ms = live.updated_at_ms.unwrap_or(0);
        if state
            .access
            .get(id)
            .is_none_or(|cached| cached.obtained_at_ms <= obtained_at_ms)
        {
            state.access.insert(
                id.into(),
                CachedToken {
                    value: live.access_token.clone(),
                    expires_at_ms,
                    obtained_at_ms,
                },
            );
        }
    }
}

fn read_home(home: &Path, store: &Store) -> Result<HomeSnapshot, String> {
    if !home.is_absolute() {
        return Err("Codex 配置目录必须是绝对路径".into());
    }
    if let Ok(metadata) = fs::symlink_metadata(home)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err("Codex 配置目录必须是普通目录".into());
    }
    let auth = read_private(&home.join("auth.json"))?;
    let marker_bytes = read_private(&home.join(MARKER_NAME))?;
    let marker: Option<Marker> = marker_bytes
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_| "SwitchX 账号归属标记无效")?;
    let live = auth.as_deref().and_then(|bytes| parse_live(bytes).ok());
    let active_id = marker.as_ref().and_then(|marker| {
        let stored = store.accounts.get(&marker.account_id)?;
        let live = live.as_ref()?;
        (marker.version == 1
            && marker.workspace_id == stored.workspace_id
            && marker.subject == stored.subject
            && matches_identity(stored, &live.identity))
        .then(|| marker.account_id.clone())
    });
    Ok(HomeSnapshot {
        auth,
        marker_bytes,
        marker,
        live,
        active_id,
    })
}

fn auth_bytes(account: &StoredAccount, token: &CachedToken) -> Result<Vec<u8>, String> {
    let refreshed = DateTime::<Utc>::from_timestamp_millis(token.obtained_at_ms)
        .ok_or("账号续期时间无效")?
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut auth = account
        .auth_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| "保存的 auth.json 无效")?
        .unwrap_or_else(|| json!({}));
    for (key, value) in [
        ("access_token", &token.value),
        ("refresh_token", &account.refresh_token),
        ("id_token", &account.id_token),
        ("account_id", &account.workspace_id),
    ] {
        zeroize_json(&mut auth["tokens"][key]);
        auth["tokens"][key] = Value::String(value.clone());
    }
    auth["auth_mode"] = "chatgpt".into();
    auth["OPENAI_API_KEY"] = Value::Null;
    auth["last_refresh"] = refreshed.into();
    let result = serde_json::to_vec_pretty(&auth).map_err(|_| "无法生成 Codex 登录文件".into());
    zeroize_json(&mut auth);
    result
}

fn auth_secret(account: &StoredAccount, token: &CachedToken) -> Result<Secret, String> {
    let bytes = Zeroizing::new(auth_bytes(account, token)?);
    let raw = std::str::from_utf8(&bytes).map_err(|_| "无法生成 auth.json")?;
    Ok(Secret::new(raw.to_owned()))
}

fn set_auth_snapshot(account: &mut StoredAccount, raw: &str) {
    if let Some(previous) = &mut account.auth_json {
        previous.zeroize();
    }
    account.auth_json = Some(raw.to_owned());
}

fn update_auth_snapshot(account: &mut StoredAccount, token: &CachedToken) -> Result<(), String> {
    let raw = auth_secret(account, token)?;
    set_auth_snapshot(account, raw.expose());
    Ok(())
}

fn write_marker(
    home: &Path,
    store: &Store,
    id: &str,
    expected: &Option<Vec<u8>>,
) -> Result<(), String> {
    let stored = account(store, id)?;
    let bytes = serde_json::to_vec_pretty(&Marker {
        version: 1,
        account_id: id.into(),
        workspace_id: stored.workspace_id.clone(),
        subject: stored.subject.clone(),
    })
    .map_err(|_| "无法生成账号归属标记")?;
    private_replace(&home.join(MARKER_NAME), &bytes, expected)
}

pub(crate) fn ensure_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("账号或 Codex 数据目录必须是绝对路径".into());
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err("账号或 Codex 数据目录必须是普通目录".into());
    }
    fs::create_dir_all(path).map_err(|_| "无法创建账号或 Codex 数据目录".into())
}

fn lock_store(dir: &Path) -> Result<File, String> {
    let path = dir.join(".switchx-accounts.lock");
    if let Ok(meta) = fs::symlink_metadata(&path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err("账号存储锁必须是普通文件".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| "无法打开账号存储锁")?;
    file.try_lock()
        .map_err(|_| "另一个 SwitchX 实例正在修改账号，请稍后重试")?;
    Ok(file)
}

pub(crate) fn read_private(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let bytes =
        files::read_config(path).map_err(|_| "无法读取账号或 Codex 文件；必须是普通文件")?;
    if bytes.as_ref().is_some_and(|bytes| bytes.len() > MAX_BYTES) {
        return Err("账号或 Codex 文件超过大小上限".into());
    }
    Ok(bytes)
}

pub(crate) fn private_replace(
    path: &Path,
    bytes: &[u8],
    expected: &Option<Vec<u8>>,
) -> Result<(), String> {
    let permissions: Option<Permissions> = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Some(Permissions::from_mode(0o600))
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    files::replace(path, bytes, permissions, expected)
        .map_err(|_| "账号或 Codex 文件已变化或无法保存，请重试".into())
}

fn remove_unchanged(path: &Path, expected: &Option<Vec<u8>>) -> Result<(), String> {
    if read_private(path)? != *expected {
        return Err("Codex 登录在操作期间已变化，请重试".into());
    }
    if expected.is_some() {
        fs::remove_file(path).map_err(|_| "无法移除账号登录文件")?;
        files::sync_parent(path).map_err(|_| "无法同步账号登录目录")?;
    }
    Ok(())
}

fn restore_file(path: &Path, original: &Option<Vec<u8>>, applied: &[u8]) -> Result<(), String> {
    let expected = Some(applied.to_vec());
    match original {
        Some(bytes) => private_replace(path, bytes, &expected),
        None => remove_unchanged(path, &expected),
    }
}

fn validate_secret(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 65536 || value.chars().any(char::is_control) {
        return Err("ChatGPT 登录凭据格式无效".into());
    }
    Ok(())
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    let field = value[key].as_str().ok_or("ChatGPT 登录响应缺少必要字段")?;
    validate_secret(field)?;
    Ok(field)
}

fn form_body(pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("https://auth.openai.com/").expect("fixed URL");
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().unwrap_or("").to_owned()
}

async fn response_value(mut response: reqwest::Response) -> Result<Value, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "无法读取 ChatGPT 登录响应")?
    {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Err("ChatGPT 登录响应超过大小上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "ChatGPT 登录响应格式无效".into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Json, Router, http::StatusCode, routing::post};
    use tokio::sync::Notify;

    use super::*;

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("switchx-account-test-{}", app::new_id().unwrap()));
            fs::create_dir_all(path.join("home")).unwrap();
            fs::create_dir_all(path.join("data")).unwrap();
            Self(path)
        }

        fn home(&self) -> PathBuf {
            self.0.join("home")
        }
        fn data(&self) -> PathBuf {
            self.0.join("data")
        }

        fn seed(&self, subject: &str, workspace: &str, refresh: &str, timestamp: i64) {
            fs::write(
                self.home().join("auth.json"),
                synthetic_auth(subject, workspace, refresh, timestamp),
            )
            .unwrap();
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn jwt(payload: Value) -> String {
        format!(
            "{}.{}.synthetic-signature",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#),
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }

    fn id_token(subject: &str, workspace: &str) -> String {
        jwt(
            json!({"sub":subject,"email":format!("{subject}@example.invalid"),
            "https://api.openai.com/auth":{"chatgpt_account_id":workspace}}),
        )
    }

    fn access_token(generation: &str) -> String {
        jwt(json!({"exp":4102444800_i64,"synthetic_generation":generation}))
    }

    fn synthetic_auth(subject: &str, workspace: &str, refresh: &str, timestamp: i64) -> Vec<u8> {
        serde_json::to_vec(&json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
            "tokens":{"id_token":id_token(subject,workspace),"access_token":access_token(refresh),
                "refresh_token":refresh,"account_id":workspace},
            "last_refresh":DateTime::<Utc>::from_timestamp_millis(timestamp).unwrap().to_rfc3339_opts(SecondsFormat::Millis,true)})).unwrap()
    }

    fn token_reply(subject: &str, workspace: &str, refresh: &str) -> Value {
        json!({"id_token":id_token(subject,workspace),"access_token":access_token(refresh),
            "refresh_token":refresh,"expires_in":3600})
    }

    struct MockServer {
        endpoints: Endpoints,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockServer {
        async fn new(router: Router) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            Self {
                endpoints: Endpoints {
                    usercode: format!("{origin}/usercode"),
                    poll: format!("{origin}/poll"),
                    token: format!("{origin}/token"),
                    verification: "https://auth.openai.com/codex/device".into(),
                },
                task,
            }
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[test]
    fn auth_editor_roundtrips_imported_credentials_and_preserves_bound_identity() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let native = fs::read(home.home().join("auth.json")).unwrap();
        let marker = fs::read(home.home().join(MARKER_NAME)).unwrap();
        let config = b"# selected home stays unchanged\n";
        fs::write(home.home().join("config.toml"), config).unwrap();

        let reopened = AccountManager::open(&home.data()).unwrap();
        for binding in [AccountBinding::Fixed(a.id.clone()), AccountBinding::Default] {
            let raw = reopened.editor_auth(&binding, &home.home()).unwrap();
            AccountManager::validate_editor_auth(raw.expose()).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(raw.expose()).unwrap(),
                serde_json::from_slice::<Value>(&native).unwrap()
            );
        }
        let raw = manager
            .editor_auth(&AccountBinding::Native, &home.home())
            .unwrap();
        assert_eq!(format!("{raw:?}"), "Secret([redacted])");
        let mut edited: Value = serde_json::from_str(raw.expose()).unwrap();
        edited["tokens"]["refresh_token"] = "synthetic-edited-a".into();
        edited["tokens"]["access_token"] = access_token("synthetic-edited-a").into();
        edited["editor_extra"] = json!({"preserved":true});
        let raw = Secret::new(edited.to_string());
        let formatted = AccountManager::format_editor_auth(raw.expose()).unwrap();
        assert!(formatted.expose().contains('\n'));
        let saved = manager
            .save_editor_auth(formatted.expose(), Some(&a.id))
            .unwrap();
        assert_eq!(saved.id, a.id);
        assert!(saved.is_default);
        assert_eq!(manager.list().unwrap().len(), 1);
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), native);
        assert_eq!(fs::read(home.home().join(MARKER_NAME)).unwrap(), marker);
        assert_eq!(fs::read(home.home().join("config.toml")).unwrap(), config);
        let reopened = AccountManager::open(&home.data()).unwrap();
        let loaded = reopened
            .editor_auth(&AccountBinding::Fixed(a.id.clone()), &home.home())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(loaded.expose()).unwrap(),
            edited
        );
        assert_eq!(
            reopened
                .save_editor_auth(loaded.expose(), Some(&a.id))
                .unwrap()
                .id,
            a.id
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(home.data().join(STORE_NAME))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn auth_editor_validates_complete_login_and_rejects_accidental_identity_changes() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        let raw = String::from_utf8(synthetic_auth(
            "user-a",
            "workspace-a",
            "synthetic-a",
            1_700_000_000_000,
        ))
        .unwrap();
        let a = manager.save_editor_auth(&raw, None).unwrap();
        assert!(!home.home().join("auth.json").exists());
        assert!(!home.home().join(MARKER_NAME).exists());
        let before = fs::read(home.data().join(STORE_NAME)).unwrap();
        for (subject, workspace) in [("user-b", "workspace-a"), ("user-a", "workspace-b")] {
            let changed = String::from_utf8(synthetic_auth(
                subject,
                workspace,
                "synthetic-secret-do-not-log",
                1_700_000_001_000,
            ))
            .unwrap();
            let error = manager.save_editor_auth(&changed, Some(&a.id)).unwrap_err();
            assert!(error.contains("用户或工作区"));
            assert!(!error.contains("synthetic-secret-do-not-log"));
        }
        let valid: Value = serde_json::from_str(&raw).unwrap();
        let mut invalid = Vec::new();
        for field in ["access_token", "refresh_token", "id_token", "account_id"] {
            let mut value = valid.clone();
            value["tokens"].as_object_mut().unwrap().remove(field);
            invalid.push(value);
        }
        for (field, value) in [
            ("access_token", "invalid.jwt.signature"),
            ("refresh_token", "synthetic whitespace"),
            ("id_token", "synthetic-secret-do-not-log"),
            ("account_id", "other-workspace"),
        ] {
            let mut invalid_value = valid.clone();
            invalid_value["tokens"][field] = value.into();
            invalid.push(invalid_value);
        }
        let mut api = valid.clone();
        api["OPENAI_API_KEY"] = "synthetic-secret-do-not-log".into();
        invalid.push(api);
        let mut wrong_mode = valid.clone();
        wrong_mode["auth_mode"] = "apikey".into();
        invalid.push(wrong_mode);
        invalid.extend([json!({}), json!([]), json!("synthetic-secret-do-not-log")]);
        for value in invalid {
            let error = manager
                .save_editor_auth(&value.to_string(), Some(&a.id))
                .unwrap_err();
            assert!(!error.contains("synthetic-secret-do-not-log"));
        }
        assert!(AccountManager::format_editor_auth("{\"tokens\":{}}").is_ok());
        assert!(AccountManager::validate_editor_auth("{\"tokens\":{}}").is_err());
        assert!(AccountManager::format_editor_auth("[]").is_err());
        assert!(AccountManager::format_editor_auth("{invalid").is_err());
        assert!(AccountManager::validate_editor_auth(&"x".repeat(MAX_BYTES + 1)).is_err());
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), before);
    }

    #[test]
    fn auth_editor_opens_legacy_and_signed_out_drafts_without_renewal_or_home_writes() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        let missing_home = home.0.join("missing-home");
        assert_eq!(
            manager
                .editor_auth(&AccountBinding::Native, &missing_home)
                .unwrap()
                .expose(),
            "{}"
        );
        assert!(!missing_home.exists());
        assert!(
            manager
                .editor_auth(&AccountBinding::Native, Path::new("relative"))
                .is_err()
        );
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let mut legacy: Value =
            serde_json::from_slice(&fs::read(home.data().join(STORE_NAME)).unwrap()).unwrap();
        legacy["accounts"][&a.id]
            .as_object_mut()
            .unwrap()
            .remove("auth_json");
        fs::write(home.data().join(STORE_NAME), legacy.to_string()).unwrap();
        let reopened = AccountManager::open(&home.data()).unwrap();
        let reconstructed = reopened
            .editor_auth(&AccountBinding::Fixed(a.id.clone()), &home.home())
            .unwrap();
        AccountManager::validate_editor_auth(reconstructed.expose()).unwrap();
        let signed_out = b"{\"OPENAI_API_KEY\":\"synthetic-secret-do-not-log\"}";
        fs::write(home.home().join("auth.json"), signed_out).unwrap();
        let store_before = fs::read(home.data().join(STORE_NAME)).unwrap();
        let draft = reopened
            .editor_auth(&AccountBinding::Fixed(a.id), &home.home())
            .unwrap();
        let draft: Value = serde_json::from_str(draft.expose()).unwrap();
        assert_eq!(draft["tokens"]["access_token"], "");
        assert_eq!(draft["tokens"]["refresh_token"], "synthetic-a");
        assert_eq!(draft["tokens"]["account_id"], "workspace-a");
        assert_eq!(
            reopened
                .editor_auth(&AccountBinding::Native, &home.home())
                .unwrap()
                .expose(),
            "{}"
        );
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), signed_out);
        assert_eq!(
            fs::read(home.data().join(STORE_NAME)).unwrap(),
            store_before
        );
    }

    #[test]
    fn auth_editor_rejects_stale_drafts_after_a_saved_token_rotation() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let original = manager
            .editor_auth(&AccountBinding::Fixed(a.id.clone()), &home.home())
            .unwrap();
        let mut edited: Value = serde_json::from_str(original.expose()).unwrap();
        edited["tokens"]["refresh_token"] = "synthetic-edited-old".into();
        edited["tokens"]["access_token"] = access_token("synthetic-edited-old").into();
        let newer = String::from_utf8(synthetic_auth(
            "user-a",
            "workspace-a",
            "synthetic-newer",
            1_700_000_001_000,
        ))
        .unwrap();
        let external = AccountManager::open(&home.data()).unwrap();
        external.save_editor_auth(&newer, Some(&a.id)).unwrap();
        let before = fs::read(home.data().join(STORE_NAME)).unwrap();
        let edited = Secret::new(edited.to_string());
        let error = manager
            .save_editor_auth_if_unchanged(edited.expose(), Some(&a.id), original.expose(), None)
            .unwrap_err();
        assert!(error.contains("编辑期间已变化"));
        assert!(
            manager
                .save_editor_auth(edited.expose(), Some(&a.id))
                .unwrap_err()
                .contains("早于已保存")
        );
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), before);
    }

    #[test]
    fn auth_editor_guards_changed_native_login_and_never_rewrites_it_on_save() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let original = manager
            .editor_auth(&AccountBinding::Native, &home.home())
            .unwrap();
        let mut edited: Value = serde_json::from_str(original.expose()).unwrap();
        edited["tokens"]["refresh_token"] = "synthetic-edited".into();
        edited["tokens"]["access_token"] = access_token("synthetic-edited").into();
        let edited = Secret::new(edited.to_string());
        home.seed(
            "user-a",
            "workspace-a",
            "synthetic-newer",
            1_700_000_001_000,
        );
        let newer = fs::read(home.home().join("auth.json")).unwrap();
        let error = manager
            .save_editor_auth_if_unchanged(
                edited.expose(),
                None,
                original.expose(),
                Some(&home.home()),
            )
            .unwrap_err();
        assert!(error.contains("原生登录在编辑期间已变化"));
        assert!(!home.data().join(STORE_NAME).exists());
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), newer);
        assert!(!home.home().join(MARKER_NAME).exists());

        let reformatted = format!("\n{}\n", original.expose());
        fs::write(home.home().join("auth.json"), &reformatted).unwrap();
        manager
            .save_editor_auth_if_unchanged(
                edited.expose(),
                None,
                original.expose(),
                Some(&home.home()),
            )
            .unwrap();
        assert_eq!(
            fs::read_to_string(home.home().join("auth.json")).unwrap(),
            reformatted
        );
        assert!(!home.home().join(MARKER_NAME).exists());
        let missing = home.0.join("missing-home");
        let raw = String::from_utf8(synthetic_auth(
            "user-b",
            "workspace-b",
            "synthetic-b",
            1_700_000_002_000,
        ))
        .unwrap();
        manager
            .save_editor_auth_if_unchanged(&raw, None, "{}", Some(&missing))
            .unwrap();
        assert!(!missing.exists());
    }

    #[tokio::test]
    async fn provider_bindings_resolve_latest_default_and_deleted_fixed_accounts() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        home.seed("user-b", "workspace-b", "synthetic-b", 1_700_000_001_000);
        let b = manager.import_current(&home.home()).unwrap();
        let stale = AccountManager::open(&home.data()).unwrap();
        manager.set_default(&b.id).unwrap();
        assert!(
            stale
                .resolve_binding(&AccountBinding::Native)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            stale
                .resolve_binding(&AccountBinding::Default)
                .await
                .unwrap()
                .unwrap()
                .id,
            b.id
        );
        assert_eq!(
            stale
                .resolve_binding(&AccountBinding::Fixed(a.id.clone()))
                .await
                .unwrap()
                .unwrap()
                .id,
            a.id
        );
        manager.remove(&a.id, &home.home()).await.unwrap();
        assert!(
            stale
                .resolve_binding(&AccountBinding::Fixed(a.id))
                .await
                .unwrap_err()
                .contains("已删除")
        );
        assert_eq!(
            fs::read(home.home().join("auth.json")).unwrap(),
            synthetic_auth("user-b", "workspace-b", "synthetic-b", 1_700_000_001_000)
        );
    }

    #[tokio::test]
    async fn route_preparation_is_native_read_only_and_refuses_shared_bundle_refresh() {
        let home = TestHome::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = MockServer::new(Router::new().route(
            "/token",
            post({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(token_reply(
                            "user-a",
                            "workspace-a",
                            "synthetic-refreshed-a",
                        ))
                    }
                }
            }),
        ))
        .await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        home.seed("user-c", "workspace-c", "synthetic-c", 1_700_000_001_000);
        let config = b"# unchanged target\ncli_auth_credentials_store = \"file\"\n";
        fs::write(home.home().join("config.toml"), config).unwrap();
        let native = fs::read(home.home().join("auth.json")).unwrap();
        let marker = fs::read(home.home().join(MARKER_NAME)).unwrap();
        let mut files_before: Vec<_> = fs::read_dir(home.home())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        files_before.sort();
        // Opening anew loses the in-memory A token and forces only A's saved bundle to refresh.
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        let credential = manager
            .credential_for_route(&a.id, &home.home())
            .await
            .unwrap();
        assert_eq!(
            credential.access_token.expose(),
            access_token("synthetic-refreshed-a")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), native);
        assert_eq!(fs::read(home.home().join(MARKER_NAME)).unwrap(), marker);
        assert_eq!(fs::read(home.home().join("config.toml")).unwrap(), config);
        let mut files_after: Vec<_> = fs::read_dir(home.home())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        files_after.sort();
        assert_eq!(files_after, files_before);

        // Restore the same account as native, with an expired access token. Preparing
        // must ask for explicit native renewal rather than rotate shared credentials.
        let mut expired: Value = serde_json::from_slice(&synthetic_auth(
            "user-a",
            "workspace-a",
            "synthetic-refreshed-a",
            Utc::now().timestamp_millis(),
        ))
        .unwrap();
        expired["tokens"]["access_token"] = jwt(json!({"exp":1})).into();
        let expired = serde_json::to_vec(&expired).unwrap();
        fs::write(home.home().join("auth.json"), &expired).unwrap();
        let stored = fs::read(home.data().join(STORE_NAME)).unwrap();
        let error = manager
            .credential_for_route(&a.id, &home.home())
            .await
            .unwrap_err();
        assert!(error.contains("请先检查并续期原生登录"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), expired);
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), stored);
    }

    #[tokio::test]
    async fn canceled_waiters_keep_submitted_rotation_and_cancel_queued_work() {
        let home = TestHome::new();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = MockServer::new(Router::new().route(
            "/token",
            post({
                let started = started.clone();
                let release = release.clone();
                let calls = calls.clone();
                move || {
                    let started = started.clone();
                    let release = release.clone();
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        started.notify_one();
                        release.notified().await;
                        Json(token_reply(
                            "user-a",
                            "workspace-a",
                            "synthetic-owned-rotation",
                        ))
                    }
                }
            }),
        ))
        .await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let waiter = tokio::spawn({
            let manager = manager.clone();
            let id = a.id.clone();
            let native = home.home();
            async move { manager.refresh(&id, &native).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        let queued = tokio::spawn({
            let manager = manager.clone();
            let id = a.id.clone();
            let native = home.home();
            async move { manager.refresh(&id, &native).await }
        });
        tokio::task::yield_now().await;
        queued.abort();
        assert!(queued.await.unwrap_err().is_cancelled());
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        let mut idle = tokio::spawn({
            let manager = manager.clone();
            async move { manager.wait_for_idle().await }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut idle)
                .await
                .is_err()
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), idle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let native: Value =
            serde_json::from_slice(&fs::read(home.home().join("auth.json")).unwrap()).unwrap();
        assert_eq!(
            native["tokens"]["refresh_token"],
            "synthetic-owned-rotation"
        );
        assert!(
            fs::read_to_string(home.data().join(STORE_NAME))
                .unwrap()
                .contains("synthetic-owned-rotation")
        );
    }

    #[tokio::test]
    async fn cancellation_before_owned_task_starts_submits_no_refresh() {
        let home = TestHome::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let server = MockServer::new(Router::new().route(
            "/token",
            post({
                let calls = calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(token_reply(
                            "user-a",
                            "workspace-a",
                            "synthetic-must-not-send",
                        ))
                    }
                }
            }),
        ))
        .await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let before = fs::read(home.data().join(STORE_NAME)).unwrap();
        let native = home.home();
        let mut future = Box::pin(manager.refresh(&a.id, &native));
        assert!(futures_util::poll!(future.as_mut()).is_pending());
        drop(future);
        manager.wait_for_idle().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), before);
    }

    #[cfg(unix)]
    fn workspace_cli(
        home: &TestHome,
        origin: &str,
        override_account: Option<&str>,
    ) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let path = home
            .0
            .join(format!("workspace-cli-{}", app::new_id().unwrap()));
        let capture = home
            .0
            .join(format!("workspace-capture-{}", app::new_id().unwrap()));
        let script = r#"#!/usr/bin/python3
import json, os, pathlib, stat, sys
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    result = {}
    if request['method'] == 'account/read':
        assert request['params']['refreshToken'] is False
        home = pathlib.Path(os.environ['CODEX_HOME'])
        account = json.loads((home / 'auth.json').read_text())['tokens']['account_id']
        capture = pathlib.Path(__CAPTURE__)
        staging = capture.with_suffix('.tmp')
        staging.write_text(json.dumps({'home': str(home), 'workspace': account,
            'directory_mode': stat.S_IMODE(home.stat().st_mode),
            'auth_mode': stat.S_IMODE((home / 'auth.json').stat().st_mode),
            'config': (home / 'config.toml').read_text(),
            'credentials_in_environment': any(os.environ.get(key) for key in ['OPENAI_API_KEY', 'CODEX_API_KEY', 'CODEX_ACCESS_TOKEN', 'OPENAI_BASE_URL'])}))
        staging.replace(capture)
        result = {'account': {'type': 'chatgpt'}, 'workspaceRouting': {
            'chatgptAccountId': __ACCOUNT__ or account, 'backendOrigin': __ORIGIN__, 'accountRoutingOverride': 'us'}}
    print(json.dumps({'id': request['id'], 'result': result}), flush=True)
"#
            .replace("__CAPTURE__", &serde_json::to_string(capture.to_str().unwrap()).unwrap())
            .replace("__ACCOUNT__", &override_account.map(|id| serde_json::to_string(id).unwrap()).unwrap_or("None".into()))
            .replace("__ORIGIN__", &serde_json::to_string(origin).unwrap());
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
        (path, capture)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_discovery_uses_private_account_context_and_validates_destination() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        home.seed("user-c", "workspace-c", "synthetic-c", 1_700_000_001_000);
        let native = fs::read(home.home().join("auth.json")).unwrap();
        let marker = fs::read(home.home().join(MARKER_NAME)).unwrap();
        let config = b"# original native configuration\n";
        fs::write(home.home().join("config.toml"), config).unwrap();
        for (origin, account, valid) in [
            ("https://us.chatgpt.com", None, true),
            ("https://chatgpt.com.evil.invalid", None, false),
            ("https://us.chatgpt.com", Some("workspace-wrong"), false),
        ] {
            let (cli, capture) = workspace_cli(&home, origin, account);
            let result = manager.workspace_for_route(&a.id, &home.home(), &cli).await;
            assert_eq!(result.is_ok(), valid);
            if let Ok(workspace) = result {
                assert_eq!(workspace.account_id, "workspace-a");
                assert_eq!(workspace.backend_origin, "https://us.chatgpt.com");
                assert_eq!(workspace.routing_override, "us");
            }
            let capture: Value = serde_json::from_slice(&fs::read(capture).unwrap()).unwrap();
            assert_eq!(capture["workspace"], "workspace-a");
            assert_eq!(capture["directory_mode"], 0o700);
            assert_eq!(capture["auth_mode"], 0o600);
            assert_eq!(capture["config"], "cli_auth_credentials_store = \"file\"\n");
            assert_eq!(capture["credentials_in_environment"], false);
            assert!(!Path::new(capture["home"].as_str().unwrap()).exists());
            assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), native);
            assert_eq!(fs::read(home.home().join(MARKER_NAME)).unwrap(), marker);
            assert_eq!(fs::read(home.home().join("config.toml")).unwrap(), config);
        }
        assert!(
            manager
                .workspace_for_route_mock(
                    &a.id,
                    &home.home(),
                    &workspace_cli(&home, "https://chatgpt.com", None).0,
                    "192.0.2.1:1234".parse().unwrap()
                )
                .await
                .is_err()
        );
        assert!(AccountManager::open_mock(&home.data(), "https://auth.openai.com").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canceled_workspace_discovery_cleans_private_context_before_idle() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-a", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        home.seed("user-c", "workspace-c", "synthetic-c", 1_700_000_001_000);
        let native = fs::read(home.home().join("auth.json")).unwrap();
        let (cli, capture) = workspace_cli(&home, "https://chatgpt.com", None);
        let script = fs::read_to_string(&cli).unwrap().replace(
            "    print(json.dumps",
            "    if request['method'] == 'account/read':\n        import time\n        time.sleep(60)\n    print(json.dumps",
        );
        fs::write(&cli, script).unwrap();
        let waiter = tokio::spawn({
            let manager = manager.clone();
            let native_home = home.home();
            async move { manager.workspace_for_route(&a.id, &native_home, &cli).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !capture.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let capture: Value = serde_json::from_slice(&fs::read(capture).unwrap()).unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), manager.wait_for_idle())
            .await
            .unwrap()
            .unwrap();
        assert!(!Path::new(capture["home"].as_str().unwrap()).exists());
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), native);
    }

    #[tokio::test]
    async fn successful_rotation_keeps_foreign_native_login_and_saves_bound_account() {
        let home = TestHome::new();
        let now = Utc::now().timestamp_millis();
        let foreign = synthetic_auth("user-c", "workspace-c", "synthetic-foreign-c", now);
        let server = MockServer::new(Router::new().route(
            "/token",
            post({
                let native_home = home.home();
                let foreign = foreign.clone();
                move || {
                    let native_home = native_home.clone();
                    let foreign = foreign.clone();
                    async move {
                        fs::write(native_home.join("auth.json"), foreign).unwrap();
                        let mut reply =
                            token_reply("user-a", "workspace-a", "synthetic-verified-a");
                        reply["id_token"] = jwt(
                            json!({"sub":"user-a", "email":"updated-label@example.invalid",
                            "https://api.openai.com/auth":{"chatgpt_account_id":"workspace-a"}}),
                        )
                        .into();
                        Json(reply)
                    }
                }
            }),
        ))
        .await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old-a", now - 10_000);
        let a = manager.import_current(&home.home()).unwrap();
        manager.refresh(&a.id, &home.home()).await.unwrap();
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), foreign);
        let stored = fs::read_to_string(home.data().join(STORE_NAME)).unwrap();
        assert!(stored.contains("synthetic-verified-a"));
        assert!(!stored.contains("synthetic-foreign-c"));
        let reopened = AccountManager::open(&home.data()).unwrap();
        let raw = reopened
            .editor_auth(&AccountBinding::Fixed(a.id.clone()), &home.home())
            .unwrap();
        AccountManager::validate_editor_auth(raw.expose()).unwrap();
        let auth: Value = serde_json::from_str(raw.expose()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "synthetic-verified-a");
        assert_eq!(
            auth["tokens"]["access_token"],
            access_token("synthetic-verified-a")
        );
        assert_eq!(reopened.list().unwrap()[0].id, a.id);
        assert_eq!(
            reopened.list().unwrap()[0].label,
            "updated-label@example.invalid"
        );
        assert_eq!(
            manager
                .credential_for_route_with_native_sync(&a.id, &home.home())
                .await
                .unwrap()
                .access_token
                .expose(),
            access_token("synthetic-verified-a")
        );
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), foreign);
    }

    #[tokio::test]
    async fn imports_are_idempotent_and_distinguish_users_in_one_workspace() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed(
            "user-a",
            "shared-workspace",
            "synthetic-refresh-a",
            1_700_000_000_000,
        );
        let a = manager.import_current(&home.home()).unwrap();
        home.seed(
            "user-a",
            "shared-workspace",
            "synthetic-refresh-a-new",
            1_700_000_001_000,
        );
        assert_eq!(manager.import_current(&home.home()).unwrap().id, a.id);
        assert_eq!(manager.list().unwrap().len(), 1);
        home.seed(
            "user-b",
            "shared-workspace",
            "synthetic-refresh-b",
            1_700_000_002_000,
        );
        let b = manager.import_current(&home.home()).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(manager.active_id(&home.home()).unwrap(), Some(b.id.clone()));
        manager.set_default(&b.id).unwrap();
        assert!(manager.list().unwrap()[0].is_default);

        let stored = fs::read_to_string(home.data().join(STORE_NAME)).unwrap();
        assert!(stored.contains("synthetic-refresh-a-new"));
        let stored: Value = serde_json::from_str(&stored).unwrap();
        let auth: Value =
            serde_json::from_str(stored["accounts"][&a.id]["auth_json"].as_str().unwrap()).unwrap();
        assert_eq!(
            auth["tokens"]["access_token"],
            access_token("synthetic-refresh-a-new")
        );
        let marker = fs::read_to_string(home.home().join(MARKER_NAME)).unwrap();
        assert!(!marker.contains("token"));
        let stale_instance = AccountManager::open(&home.data()).unwrap();
        manager.remove(&b.id, &home.home()).await.unwrap();
        assert_eq!(manager.default_id().unwrap(), Some(a.id.clone()));
        assert!(!home.home().join("auth.json").exists());
        assert!(
            stale_instance
                .credential(&b.id, &home.home())
                .await
                .unwrap_err()
                .contains("已删除")
        );
        let credential = manager.credential(&a.id, &home.home()).await.unwrap();
        assert_eq!(
            credential.access_token.expose(),
            access_token("synthetic-refresh-a-new")
        );
        assert!(!format!("{credential:?}").contains("synthetic_generation"));
        manager.remove(&a.id, &home.home()).await.unwrap();
        assert!(manager.list().unwrap().is_empty());
        assert!(manager.default_id().unwrap().is_none());
    }

    #[tokio::test]
    async fn activation_preserves_config_and_refuses_foreign_or_api_login() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed(
            "user-a",
            "workspace-a",
            "synthetic-refresh-a",
            1_700_000_000_000,
        );
        let a = manager.import_current(&home.home()).unwrap();
        fs::write(
            home.home().join("config.toml"),
            "model = \"keep-model\" # keep-comment\ncli_auth_credentials_store = \"keyring\"\n",
        )
        .unwrap();
        manager.activate(&a.id, &home.home()).await.unwrap();
        let config = fs::read_to_string(home.home().join("config.toml")).unwrap();
        assert!(config.contains("keep-model"));
        assert!(config.contains("keep-comment"));
        assert_eq!(
            config.parse::<toml_edit::DocumentMut>().unwrap()["cli_auth_credentials_store"]
                .as_str(),
            Some("file")
        );

        home.seed(
            "other-user",
            "workspace-a",
            "foreign-refresh",
            1_700_000_001_000,
        );
        let foreign = fs::read(home.home().join("auth.json")).unwrap();
        assert!(manager.activate(&a.id, &home.home()).await.is_err());
        assert!(manager.remove(&a.id, &home.home()).await.is_err());
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), foreign);
        fs::remove_file(home.home().join(MARKER_NAME)).unwrap();
        fs::write(
            home.home().join("auth.json"),
            r#"{"OPENAI_API_KEY":"synthetic-api-key"}"#,
        )
        .unwrap();
        let api = fs::read(home.home().join("auth.json")).unwrap();
        assert!(manager.activate(&a.id, &home.home()).await.is_err());
        assert_eq!(fs::read(home.home().join("auth.json")).unwrap(), api);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credential_files_are_private_and_symlinks_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed(
            "user-a",
            "workspace-a",
            "synthetic-refresh",
            1_700_000_000_000,
        );
        let a = manager.import_current(&home.home()).unwrap();
        manager.activate(&a.id, &home.home()).await.unwrap();
        for path in [
            home.data().join(STORE_NAME),
            home.data().join(".switchx-accounts.lock"),
            home.home().join("auth.json"),
            home.home().join(MARKER_NAME),
            home.home().join(".switchx-config.lock"),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let marker = home.home().join(MARKER_NAME);
        fs::remove_file(&marker).unwrap();
        symlink(home.data().join(STORE_NAME), &marker).unwrap();
        assert!(manager.active_id(&home.home()).is_err());
        fs::remove_file(&marker).unwrap();
        let store_path = home.data().join(STORE_NAME);
        fs::rename(&store_path, home.data().join("original.json")).unwrap();
        symlink(home.data().join("original.json"), &store_path).unwrap();
        assert!(AccountManager::open(&home.data()).is_err());
        assert!(AccountManager::open(Path::new("relative-data")).is_err());
    }

    #[tokio::test]
    async fn device_flow_reauthorizes_without_duplicates_and_cancel_writes_nothing() {
        let home = TestHome::new();
        let polls = Arc::new(AtomicUsize::new(0));
        let generations = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/usercode", post(|| async { Json(json!({"device_auth_id":"synthetic-device", "user_code":"TEST-CODE", "expires_in":900, "interval":"5"})) }))
            .route("/poll", post({ let polls = polls.clone(); move |body: String| { let polls = polls.clone(); async move {
                assert!(body.contains("synthetic-device"));
                polls.fetch_add(1, Ordering::SeqCst);
                Json(json!({"authorization_code":"synthetic-code", "code_verifier":"synthetic-verifier"}))
            } } }))
            .route("/token", post({ let generations = generations.clone(); move |body: String| { let generations = generations.clone(); async move {
                assert!(body.contains("grant_type=authorization_code"));
                assert!(body.contains("code_verifier=synthetic-verifier"));
                let generation = generations.fetch_add(1, Ordering::SeqCst);
                Json(token_reply("user-a", "workspace-a", &format!("synthetic-refresh-{generation}")))
            } } }));
        let server = MockServer::new(router).await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        let login = manager.start_login().await.unwrap();
        assert_eq!(login.user_code, "TEST-CODE");
        let (_sender, cancel) = watch::channel(false);
        let first = login.finish(cancel).await.unwrap();
        let login = manager.start_login().await.unwrap();
        let (_sender, cancel) = watch::channel(false);
        assert_eq!(login.finish(cancel).await.unwrap().id, first.id);
        assert_eq!(manager.list().unwrap().len(), 1);
        assert!(
            fs::read_to_string(home.data().join(STORE_NAME))
                .unwrap()
                .contains("synthetic-refresh-1")
        );
        let reopened = AccountManager::open(&home.data()).unwrap();
        let raw = reopened
            .editor_auth(&AccountBinding::Fixed(first.id.clone()), &home.home())
            .unwrap();
        AccountManager::validate_editor_auth(raw.expose()).unwrap();
        let auth: Value = serde_json::from_str(raw.expose()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "synthetic-refresh-1");
        assert_eq!(
            auth["tokens"]["access_token"],
            access_token("synthetic-refresh-1")
        );
        let before = fs::read(home.data().join(STORE_NAME)).unwrap();
        let login = manager.start_login().await.unwrap();
        let (_sender, cancel) = watch::channel(true);
        assert!(login.finish(cancel).await.unwrap_err().contains("取消"));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), before);
    }

    #[tokio::test]
    async fn refresh_delete_are_serialized_and_metadata_stays_available() {
        let home = TestHome::new();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let router = Router::new().route(
            "/token",
            post({
                let started = started.clone();
                let release = release.clone();
                move |body: String| {
                    let started = started.clone();
                    let release = release.clone();
                    async move {
                        assert!(body.contains("refresh_token=synthetic-old"));
                        started.notify_one();
                        release.notified().await;
                        Json(token_reply("user-a", "workspace-a", "synthetic-new"))
                    }
                }
            }),
        );
        let server = MockServer::new(router).await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let refresh = tokio::spawn({
            let manager = manager.clone();
            let id = a.id.clone();
            let path = home.home();
            async move { manager.refresh(&id, &path).await }
        });
        tokio::time::timeout(Duration::from_secs(2), started.notified())
            .await
            .unwrap();
        assert_eq!(manager.list().unwrap().len(), 1);
        let stale = AccountManager::open(&home.data()).unwrap();
        assert!(stale.set_default(&a.id).unwrap_err().contains("另一个"));
        let remove = tokio::spawn({
            let manager = manager.clone();
            let id = a.id.clone();
            let path = home.home();
            async move { manager.remove(&id, &path).await }
        });
        release.notify_one();
        refresh.await.unwrap().unwrap();
        remove.await.unwrap().unwrap();
        assert!(manager.list().unwrap().is_empty());
        assert!(!home.home().join("auth.json").exists());
        assert!(
            stale
                .credential(&a.id, &home.home())
                .await
                .unwrap_err()
                .contains("已删除")
        );
    }

    #[tokio::test]
    async fn native_rotation_recovers_rejected_refresh_once() {
        let home = TestHome::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let now = Utc::now().timestamp_millis();
        let router = Router::new().route("/token", post({ let calls = calls.clone(); let path = home.home();
            move |body: String| { let calls = calls.clone(); let path = path.clone(); async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    assert!(body.contains("refresh_token=synthetic-old"));
                    fs::write(path.join("auth.json"), synthetic_auth("user-a", "workspace-a", "synthetic-native", now)).unwrap();
                    (StatusCode::BAD_REQUEST, Json(json!({"error":{"code":"refresh_token_reused", "message":"DO-NOT-EXPOSE synthetic-old"}})))
                } else {
                    assert!(body.contains("refresh_token=synthetic-native"));
                    (StatusCode::OK, Json(token_reply("user-a", "workspace-a", "synthetic-final")))
                }
            } }
        }));
        let server = MockServer::new(router).await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", now - 10_000);
        let a = manager.import_current(&home.home()).unwrap();
        manager.refresh(&a.id, &home.home()).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let auth: Value =
            serde_json::from_slice(&fs::read(home.home().join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "synthetic-final");
        assert!(
            fs::read_to_string(home.data().join(STORE_NAME))
                .unwrap()
                .contains("synthetic-final")
        );
    }

    #[tokio::test]
    async fn foreign_login_during_refresh_is_not_overwritten_and_errors_are_redacted() {
        let home = TestHome::new();
        let router = Router::new().route(
            "/token",
            post({
                let path = home.home();
                move || {
                    let path = path.clone();
                    async move {
                        fs::write(
                            path.join("auth.json"),
                            synthetic_auth(
                                "other-user",
                                "workspace-a",
                                "foreign-refresh",
                                Utc::now().timestamp_millis(),
                            ),
                        )
                        .unwrap();
                        (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error":{"message":"DO-NOT-EXPOSE foreign-refresh"}})),
                        )
                    }
                }
            }),
        );
        let server = MockServer::new(router).await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", 1_700_000_000_000);
        let a = manager.import_current(&home.home()).unwrap();
        let error = manager.refresh(&a.id, &home.home()).await.unwrap_err();
        assert!(!error.contains("DO-NOT-EXPOSE"));
        assert!(!error.contains("foreign-refresh"));
        let auth: Value =
            serde_json::from_slice(&fs::read(home.home().join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "foreign-refresh");
        assert!(
            fs::read_to_string(home.data().join(STORE_NAME))
                .unwrap()
                .contains("synthetic-old")
        );
    }

    #[tokio::test]
    async fn successful_old_refresh_response_cannot_replace_new_native_generation() {
        let home = TestHome::new();
        let now = Utc::now().timestamp_millis();
        let router = Router::new().route(
            "/token",
            post({
                let path = home.home();
                move || {
                    let path = path.clone();
                    async move {
                        fs::write(
                            path.join("auth.json"),
                            synthetic_auth("user-a", "workspace-a", "synthetic-native", now),
                        )
                        .unwrap();
                        Json(token_reply(
                            "user-a",
                            "workspace-a",
                            "synthetic-stale-response",
                        ))
                    }
                }
            }),
        );
        let server = MockServer::new(router).await;
        let manager =
            AccountManager::open_with_endpoints(&home.data(), server.endpoints.clone()).unwrap();
        home.seed("user-a", "workspace-a", "synthetic-old", now - 10_000);
        let a = manager.import_current(&home.home()).unwrap();
        assert!(
            manager
                .refresh(&a.id, &home.home())
                .await
                .unwrap_err()
                .contains("已丢弃")
        );
        let stored = fs::read_to_string(home.data().join(STORE_NAME)).unwrap();
        assert!(stored.contains("synthetic-native"));
        assert!(!stored.contains("synthetic-stale-response"));
        let auth: Value =
            serde_json::from_slice(&fs::read(home.home().join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth["tokens"]["refresh_token"], "synthetic-native");
    }

    #[test]
    fn store_limits_do_not_publish_an_unreadable_file() {
        let home = TestHome::new();
        let manager = AccountManager::open(&home.data()).unwrap();
        home.seed(
            "user-a",
            "workspace-a",
            "synthetic-refresh",
            1_700_000_000_000,
        );
        manager.import_current(&home.home()).unwrap();
        let original = fs::read(home.data().join(STORE_NAME)).unwrap();
        {
            let (_lock, mut session) = manager.begin().unwrap();
            let template = session
                .state
                .store
                .accounts
                .values()
                .next()
                .unwrap()
                .clone();
            session.state.store.accounts.clear();
            for index in 0..257 {
                let mut account = template.clone();
                account.id = format!("{index:032x}");
                session
                    .state
                    .store
                    .accounts
                    .insert(account.id.clone(), account);
            }
            assert!(manager.save(&mut session).unwrap_err().contains("上限"));
        }
        {
            let (_lock, mut session) = manager.begin().unwrap();
            session
                .state
                .store
                .accounts
                .values_mut()
                .next()
                .unwrap()
                .refresh_token = "x".repeat(MAX_BYTES);
            assert!(manager.save(&mut session).unwrap_err().contains("上限"));
        }
        assert_eq!(fs::read(home.data().join(STORE_NAME)).unwrap(), original);
        assert_eq!(
            AccountManager::open(&home.data())
                .unwrap()
                .list()
                .unwrap()
                .len(),
            1
        );
    }
}
