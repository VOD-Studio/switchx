use std::{
    collections::HashMap,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::{
    net::TcpListener,
    sync::{oneshot, watch},
    task::JoinHandle,
};

use crate::{
    catalog::Publication,
    credentials::Secret,
    requests::{REQUEST_ID_HEADER, RequestLog, RequestTracker, ResponseObserver},
    storage::{RequestStatus, SessionAuthContext, Store},
};

pub const LOCAL_TOKEN_HEADER: &str = "x-switchx-local-token";
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct Upstream {
    responses_url: Url,
    auth: UpstreamAuth,
}

enum UpstreamAuth {
    ApiKey(Secret),
    XaiOAuth {
        manager: crate::xai::AccountManager,
        id: String,
    },
    Chatgpt {
        account: Mutex<Option<HeaderValue>>,
        routing_override: Option<HeaderValue>,
    },
    ManagedChatgpt {
        manager: crate::accounts::AccountManager,
        id: String,
        home: PathBuf,
        account: Mutex<Option<HeaderValue>>,
        routing_override: Option<HeaderValue>,
    },
}

impl Upstream {
    pub fn new(base_url: &str, api_key: String) -> Result<Self, String> {
        Self::with_secret(base_url, Secret::new(api_key))
    }

    pub fn with_secret(base_url: &str, api_key: Secret) -> Result<Self, String> {
        let mut url = Url::parse(base_url).map_err(|_| "invalid upstream URL")?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("upstream URL must not contain credentials, query or fragment".into());
        }
        match url.scheme() {
            "https" => {}
            "http" if url.host_str() == Some("127.0.0.1") => {}
            _ => return Err("upstream must use HTTPS or IPv4 loopback HTTP".into()),
        }
        if api_key.expose().is_empty() {
            return Err("upstream credential is missing".into());
        }
        let path = format!("{}/responses", url.path().trim_end_matches('/'));
        url.set_path(&path);
        Ok(Self {
            responses_url: url,
            auth: UpstreamAuth::ApiKey(api_key),
        })
    }

    pub fn chatgpt() -> Self {
        Self {
            responses_url: Url::parse(&format!("{}/responses", crate::chatgpt::BASE_URL)).unwrap(),
            auth: UpstreamAuth::Chatgpt {
                account: Mutex::new(None),
                routing_override: None,
            },
        }
    }

    pub fn chatgpt_for_workspace(workspace: &crate::chatgpt::Workspace) -> Result<Self, String> {
        workspace.validate()?;
        Ok(Self {
            responses_url: Url::parse(&format!(
                "{}/backend-api/codex/responses",
                workspace.backend_origin
            ))
            .map_err(|_| "invalid official workspace origin")?,
            auth: UpstreamAuth::Chatgpt {
                account: Mutex::new(Some(
                    HeaderValue::from_str(&workspace.account_id)
                        .map_err(|_| "invalid official workspace")?,
                )),
                routing_override: Some(
                    HeaderValue::from_str(&workspace.routing_override)
                        .map_err(|_| "invalid official workspace routing")?,
                ),
            },
        })
    }

    /// Synthetic probes only. The product uses a validated official HTTPS destination.
    pub fn chatgpt_mock(address: SocketAddr) -> Result<Self, String> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err("mock must use IPv4 loopback".into());
        }
        let mut upstream = Self::chatgpt();
        upstream.responses_url = Url::parse(&format!("http://{address}/responses")).unwrap();
        Ok(upstream)
    }

    pub fn managed_chatgpt(
        manager: crate::accounts::AccountManager,
        id: String,
        home: PathBuf,
        workspace: &crate::chatgpt::Workspace,
    ) -> Result<Self, String> {
        let mut upstream = Self::chatgpt_for_workspace(workspace)?;
        let UpstreamAuth::Chatgpt {
            account,
            routing_override,
        } = upstream.auth
        else {
            unreachable!()
        };
        upstream.auth = UpstreamAuth::ManagedChatgpt {
            manager,
            id,
            home,
            account,
            routing_override,
        };
        Ok(upstream)
    }

    /// Synthetic probes only; managed credentials still follow the production account path.
    pub fn managed_chatgpt_mock(
        address: SocketAddr,
        manager: crate::accounts::AccountManager,
        id: String,
        home: PathBuf,
        workspace: &crate::chatgpt::Workspace,
    ) -> Result<Self, String> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err("mock must use IPv4 loopback".into());
        }
        let mut upstream = Self::managed_chatgpt(manager, id, home, workspace)?;
        upstream.responses_url = Url::parse(&format!("http://{address}/responses")).unwrap();
        Ok(upstream)
    }

    pub fn xai(manager: crate::xai::AccountManager, id: String) -> Result<Self, String> {
        manager.resolve_binding(&crate::storage::AccountBinding::Fixed(id.clone()))?;
        Ok(Self {
            responses_url: Url::parse(&format!("{}/responses", crate::xai::BASE_URL)).unwrap(),
            auth: UpstreamAuth::XaiOAuth { manager, id },
        })
    }

    /// Synthetic inference only; OAuth credentials still follow the production manager.
    pub fn xai_mock(
        address: SocketAddr,
        manager: crate::xai::AccountManager,
        id: String,
    ) -> Result<Self, String> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err("mock must use IPv4 loopback".into());
        }
        let mut upstream = Self::xai(manager, id)?;
        upstream.responses_url = Url::parse(&format!("http://{address}/responses")).unwrap();
        Ok(upstream)
    }

    fn is_chatgpt(&self) -> bool {
        matches!(
            self.auth,
            UpstreamAuth::Chatgpt { .. } | UpstreamAuth::ManagedChatgpt { .. }
        )
    }
    fn is_xai(&self) -> bool {
        matches!(self.auth, UpstreamAuth::XaiOAuth { .. })
    }
    fn is_oauth(&self) -> bool {
        !matches!(self.auth, UpstreamAuth::ApiKey(_))
    }

    fn session_context(
        &self,
        provider_id: &str,
        headers: &HeaderMap,
    ) -> Result<SessionAuthContext, &'static str> {
        let mut context = SessionAuthContext {
            provider_id: provider_id.into(),
            account_id: None,
            workspace_id: None,
            backend_origin: None,
            routing_override: None,
            upstream_model: None,
        };
        let (account, routing_override) = match &self.auth {
            UpstreamAuth::ApiKey(_) => return Ok(context),
            UpstreamAuth::XaiOAuth { id, .. } => {
                context.account_id = Some(id.clone());
                context.backend_origin = Some(self.responses_url.origin().ascii_serialization());
                return Ok(context);
            }
            UpstreamAuth::Chatgpt {
                account,
                routing_override,
            } => (account, routing_override),
            UpstreamAuth::ManagedChatgpt {
                id,
                account,
                routing_override,
                ..
            } => {
                context.account_id = Some(id.clone());
                (account, routing_override)
            }
        };
        let pinned = account.lock().map_err(|_| "chatgpt_account_unavailable")?;
        let workspace = match pinned.as_ref() {
            Some(value) => value,
            None => single_header(headers, crate::chatgpt::ACCOUNT_HEADER)
                .map_err(|_| "chatgpt_account_required")?
                .ok_or("chatgpt_account_required")?,
        };
        let workspace = workspace.to_str().map_err(|_| "chatgpt_account_required")?;
        if workspace.is_empty()
            || workspace.len() > 256
            || !workspace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
        {
            return Err("chatgpt_account_required");
        }
        context.workspace_id = Some(workspace.into());
        context.backend_origin = Some(self.responses_url.origin().ascii_serialization());
        let routing = match routing_override.as_ref() {
            Some(value) => Some(value),
            None => single_header(headers, "x-openai-account-routing-override")
                .map_err(|_| "chatgpt_account_changed")?,
        };
        let routing = routing
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| "chatgpt_account_changed")?
            .unwrap_or("NO_CONSTRAINT");
        if !matches!(routing, "NO_CONSTRAINT" | "us" | "us_cr") {
            return Err("chatgpt_account_changed");
        }
        context.routing_override = Some(routing.into());
        Ok(context)
    }

    async fn authenticated_headers(
        &self,
        headers: &HeaderMap,
        local_token: &Secret,
    ) -> Result<HeaderMap, &'static str> {
        if let UpstreamAuth::XaiOAuth { manager, id } = &self.auth {
            let token = manager
                .credential(id)
                .await
                .map_err(|_| "xai_auth_required")?;
            let mut output = HeaderMap::new();
            let mut bearer = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                .map_err(|_| "xai_auth_required")?;
            bearer.set_sensitive(true);
            output.insert(header::AUTHORIZATION, bearer);
            return Ok(output);
        }
        if let UpstreamAuth::ManagedChatgpt {
            manager,
            id,
            home,
            account,
            routing_override,
        } = &self.auth
        {
            let credential = manager
                .credential_for_route_with_native_sync(id, home)
                .await
                .map_err(|_| "chatgpt_auth_required")?;
            let expected = account.lock().map_err(|_| "chatgpt_account_unavailable")?;
            let workspace = HeaderValue::from_str(&credential.workspace_id)
                .map_err(|_| "chatgpt_account_changed")?;
            if expected.as_ref() != Some(&workspace) {
                return Err("chatgpt_account_changed");
            }
            drop(expected);
            // Managed routes use their explicit binding, never a client's unrelated official login.
            let mut input = headers.clone();
            let mut bearer =
                HeaderValue::from_str(&format!("Bearer {}", credential.access_token.expose()))
                    .map_err(|_| "chatgpt_auth_required")?;
            bearer.set_sensitive(true);
            input.insert(header::AUTHORIZATION, bearer);
            input.insert(crate::chatgpt::ACCOUNT_HEADER, workspace);
            input.remove("x-openai-account-routing-override");
            if let Some(value) = routing_override {
                input.insert("x-openai-account-routing-override", value.clone());
            }
            self.outbound_headers(&input, local_token)
        } else {
            self.outbound_headers(headers, local_token)
        }
    }

    fn outbound_headers(
        &self,
        headers: &HeaderMap,
        local_token: &Secret,
    ) -> Result<HeaderMap, &'static str> {
        let mut output = HeaderMap::new();
        match &self.auth {
            UpstreamAuth::XaiOAuth { .. } => return Err("xai_auth_required"),
            UpstreamAuth::ApiKey(key) => {
                let mut bearer = HeaderValue::from_str(&format!("Bearer {}", key.expose()))
                    .map_err(|_| "invalid_upstream_credential")?;
                bearer.set_sensitive(true);
                output.insert(header::AUTHORIZATION, bearer);
            }
            UpstreamAuth::Chatgpt {
                account,
                routing_override,
            }
            | UpstreamAuth::ManagedChatgpt {
                account,
                routing_override,
                ..
            } => {
                let mut values = headers.get_all(header::AUTHORIZATION).iter();
                let bearer = values
                    .next()
                    .filter(|_| values.next().is_none())
                    .ok_or("chatgpt_auth_required")?;
                if bearer
                    .to_str()
                    .ok()
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .is_none_or(|value| {
                        value.is_empty()
                            || value.len() > 16384
                            || value.contains(char::is_whitespace)
                            || value.starts_with("sk-")
                            || value.contains("PROXY_MANAGED")
                            || value == local_token.expose()
                    })
                {
                    return Err("chatgpt_auth_required");
                }
                let mut values = headers.get_all(crate::chatgpt::ACCOUNT_HEADER).iter();
                let supplied = values
                    .next()
                    .filter(|_| values.next().is_none())
                    .ok_or("chatgpt_account_required")?;
                if !supplied.to_str().is_ok_and(|value| {
                    !value.is_empty()
                        && value.len() <= 256
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
                }) {
                    return Err("chatgpt_account_required");
                }
                let mut pinned = account.lock().map_err(|_| "chatgpt_account_unavailable")?;
                if pinned.as_ref().is_some_and(|account| account != supplied) {
                    return Err("chatgpt_account_changed");
                }
                if pinned.is_none() {
                    *pinned = Some(supplied.clone());
                }
                let mut overrides = headers.get_all("x-openai-account-routing-override").iter();
                let supplied_override = overrides.next();
                if overrides.next().is_some()
                    || supplied_override.is_some_and(|value| {
                        !matches!(value.to_str(), Ok("NO_CONSTRAINT" | "us" | "us_cr"))
                            || routing_override
                                .as_ref()
                                .is_some_and(|expected| expected != value)
                    })
                {
                    return Err("chatgpt_account_changed");
                }
                if let Some(value) = routing_override.as_ref().or(supplied_override) {
                    output.insert("x-openai-account-routing-override", value.clone());
                }
                let mut bearer = bearer.clone();
                bearer.set_sensitive(true);
                output.insert(header::AUTHORIZATION, bearer);
                // No cookies, local credentials, proxy auth or arbitrary client headers.
                for name in [
                    crate::chatgpt::ACCOUNT_HEADER,
                    "accept",
                    "user-agent",
                    "openai-beta",
                    "originator",
                    "version",
                    "session-id",
                    "thread-id",
                    "session_id",
                    "x-client-request-id",
                    "x-codex-turn-metadata",
                    "x-codex-turn-state",
                    "x-codex-routing-hint",
                    "x-codex-beta-features",
                    "x-codex-session-id",
                    "x-codex-turn-id",
                ] {
                    if let Some(value) = headers.get(name) {
                        output.insert(axum::http::HeaderName::from_static(name), value.clone());
                    }
                }
            }
        }
        Ok(output)
    }
}

pub struct RouterState {
    expected_host: String,
    local_token: Secret,
    publication: Publication,
    upstreams: HashMap<String, Upstream>,
    client: Client,
    request_budget: Duration,
    accepting: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    request_log: Option<Arc<RequestLog>>,
    session_store: Option<Mutex<Store>>,
    chatgpt_error: Arc<AtomicU8>,
    chatgpt_errors: Arc<Mutex<HashMap<String, u8>>>,
    uses_chatgpt: bool,
}

impl RouterState {
    pub fn new(
        address: SocketAddr,
        local_token: String,
        publication: Publication,
        upstreams: HashMap<String, Upstream>,
    ) -> Result<Self, String> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err("router must bind 127.0.0.1".into());
        }
        if local_token.len() < 32 {
            return Err("local token must be at least 32 bytes".into());
        }
        for binding in publication.routes.values() {
            if !upstreams.contains_key(&binding.provider_id)
                || binding
                    .fallback_provider_id
                    .as_ref()
                    .is_some_and(|id| id == &binding.provider_id || !upstreams.contains_key(id))
            {
                return Err(format!(
                    "missing provider for route: {}",
                    binding.provider_id
                ));
            }
            if binding.fallback_provider_id.as_ref().is_some_and(|id| {
                upstreams[&binding.provider_id].is_oauth() || upstreams[id].is_oauth()
            }) {
                return Err("OAuth accounts cannot participate in automatic fallback".into());
            }
        }
        let uses_chatgpt = upstreams.values().any(Upstream::is_chatgpt);
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "failed to create upstream client")?;
        Ok(Self {
            expected_host: address.to_string(),
            local_token: Secret::new(local_token),
            publication,
            upstreams,
            client,
            request_budget: Duration::from_secs(120),
            accepting: Arc::new(AtomicBool::new(true)),
            cancel: watch::channel(false).0,
            request_log: None,
            session_store: None,
            chatgpt_error: Arc::new(AtomicU8::new(0)),
            chatgpt_errors: Arc::new(Mutex::new(HashMap::new())),
            uses_chatgpt,
        })
    }

    pub fn with_request_log(mut self, store: Store, generation: String) -> Self {
        self.request_log = Some(Arc::new(RequestLog::new(store, generation)));
        self
    }

    pub fn with_session_store(mut self, store: Store) -> Self {
        self.session_store = Some(Mutex::new(store));
        self
    }

    fn set_chatgpt_error(&self, provider_id: &str, code: u8) {
        if let Ok(mut errors) = self.chatgpt_errors.lock() {
            if code == 0 {
                errors.remove(provider_id);
            } else {
                errors.insert(provider_id.into(), code);
            }
            self.chatgpt_error.store(
                errors.values().copied().max().unwrap_or(0),
                Ordering::Relaxed,
            );
        }
    }
}

pub struct RunningRouter {
    accepting: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    request_log: Option<Arc<RequestLog>>,
    chatgpt_error: Arc<AtomicU8>,
    chatgpt_errors: Arc<Mutex<HashMap<String, u8>>>,
}

impl RunningRouter {
    pub fn start(listener: TcpListener, state: RouterState) -> Result<Self, String> {
        if listener
            .local_addr()
            .map_err(|_| "cannot read router address")?
            .to_string()
            != state.expected_host
        {
            return Err("listener does not match router address".into());
        }
        let accepting = state.accepting.clone();
        let cancel = state.cancel.clone();
        let request_log = state.request_log.clone();
        let chatgpt_error = state.chatgpt_error.clone();
        let chatgpt_errors = state.chatgpt_errors.clone();
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router(state))
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Ok(Self {
            accepting,
            cancel,
            shutdown: Some(shutdown),
            task,
            request_log,
            chatgpt_error,
            chatgpt_errors,
        })
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn recording_failed(&self) -> bool {
        self.request_log
            .as_ref()
            .is_some_and(|log| log.failed.load(Ordering::Relaxed))
    }

    pub fn pause(&self) {
        self.accepting.store(false, Ordering::SeqCst);
    }

    pub fn chatgpt_error(&self) -> Option<&'static str> {
        chatgpt_error_message(self.chatgpt_error.load(Ordering::Relaxed))
    }

    pub fn chatgpt_error_for(&self, provider_id: &str) -> Option<&'static str> {
        let code = self.chatgpt_errors.lock().ok()?.get(provider_id).copied()?;
        chatgpt_error_message(code)
    }

    pub fn resume(&self) {
        self.accepting.store(true, Ordering::SeqCst);
    }

    pub async fn stop(mut self) {
        self.pause();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // Connections get a bounded drain. Cancellation also reaches child
        // connections spawned by Axum if the server task must be aborted.
        if tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            self.cancel.send_replace(true);
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}

impl Drop for RunningRouter {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
        self.task.abort();
    }
}

fn chatgpt_error_message(code: u8) -> Option<&'static str> {
    match code {
        1 => Some("官方认证被拒绝；Codex 会尝试续期，仍失败时请恢复并重新登录"),
        2 => Some("官方账号没有此模型或工作区权限，请检查订阅与模型选择"),
        3 => Some("官方工作区已变化，请恢复并重新发布路由"),
        _ => None,
    }
}

fn single_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a HeaderValue>, &'static str> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err("invalid_session");
    }
    Ok(first)
}

fn session_uuid(value: &str) -> Result<String, &'static str> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err("invalid_session");
    }
    Ok(value.to_ascii_lowercase())
}

fn header_session_id(headers: &HeaderMap, name: &str) -> Result<Option<String>, &'static str> {
    single_header(headers, name)?
        .map(|value| session_uuid(value.to_str().map_err(|_| "invalid_session")?))
        .transpose()
}

fn session_id(headers: &HeaderMap) -> Result<String, &'static str> {
    let current = header_session_id(headers, "session-id")?;
    let legacy = header_session_id(headers, "session_id")?;
    if current.is_some() && legacy.is_some() && current != legacy {
        return Err("session_header_conflict");
    }
    let session = current.or(legacy).ok_or("session_required")?;
    // thread-id can identify a child agent while session-id identifies its root tree.
    let _ = header_session_id(headers, "thread-id")?;
    if let Some(alias) = header_session_id(headers, "x-codex-session-id")?
        && alias != session
    {
        return Err("session_header_conflict");
    }
    if let Some(metadata) = single_header(headers, "x-codex-turn-metadata")? {
        #[derive(Deserialize)]
        struct Metadata {
            session_id: Option<String>,
        }
        if metadata.as_bytes().len() > 16 * 1024 {
            return Err("invalid_session");
        }
        // Deserialize the identity field directly so duplicate session_id keys are rejected.
        let metadata: Metadata =
            serde_json::from_slice(metadata.as_bytes()).map_err(|_| "invalid_session")?;
        if let Some(id) = metadata.session_id
            && session_uuid(&id)? != session
        {
            return Err("session_header_conflict");
        }
    }
    Ok(session)
}

fn opaque_input(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(opaque_input),
        Value::Object(object) => {
            object
                .get("type")
                .is_some_and(|value| value == "compaction")
                || ["encrypted_content", "encrypted_function_args"]
                    .iter()
                    .any(|key| object.get(*key).is_some_and(|value| !value.is_null()))
                || object.values().any(opaque_input)
        }
        _ => false,
    }
}

pub fn router(state: RouterState) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/v1/responses/compact", post(compact))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(Arc::new(state))
}

pub async fn serve(
    listener: TcpListener,
    state: RouterState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if listener.local_addr()?.to_string() != state.expected_host {
        return Err("listener does not match router address".into());
    }
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn request_error(
    tracker: &mut RequestTracker,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response {
    tracker.finish(RequestStatus::Failed, Some(code));
    let mut response = error(status, code, message);
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, tracker.record.id.parse().unwrap());
    response
}

fn authorize(headers: &HeaderMap, state: &RouterState) -> Option<Response> {
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.expected_host.as_str())
    {
        return Some(error(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "unexpected Host",
        ));
    }
    if headers.contains_key(header::ORIGIN) {
        return Some(error(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "Origin is not allowed",
        ));
    }
    let mut local_headers = headers.get_all(LOCAL_TOKEN_HEADER).iter();
    let presented = if let Some(value) = local_headers.next() {
        if local_headers.next().is_some() {
            None
        } else {
            value.to_str().ok()
        }
    } else if !state.uses_chatgpt {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
    } else {
        None
    };
    let valid = presented
        .map(|token| {
            token
                .as_bytes()
                .ct_eq(state.local_token.expose().as_bytes())
                .unwrap_u8()
                == 1
        })
        .unwrap_or(false);
    if !valid {
        return Some(error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "local token is required",
        ));
    }
    if !state.accepting.load(Ordering::SeqCst) {
        return Some(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "router_stopping",
            "router is stopping",
        ));
    }
    None
}

async fn models(State(state): State<Arc<RouterState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&headers, &state) {
        return response;
    }
    let data: Vec<Value> = state.publication.catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["slug"].as_str())
        .map(|id| json!({ "id": id, "object": "model" }))
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

async fn compact(
    State(state): State<Arc<RouterState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, headers, body, true).await
}

async fn responses(
    State(state): State<Arc<RouterState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(state, headers, body, false).await
}

async fn forward(
    state: Arc<RouterState>,
    headers: HeaderMap,
    body: Bytes,
    compact: bool,
) -> Response {
    if let Some(response) = authorize(&headers, &state) {
        return response;
    }
    let mut tracker = match RequestTracker::new(state.request_log.clone(), state.cancel.subscribe())
    {
        Ok(tracker) => tracker,
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "request_id_unavailable",
                "could not create request ID",
            );
        }
    };
    let mut cancellation = state.cancel.subscribe();
    let deadline = Instant::now() + state.request_budget;
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return request_error(
            &mut tracker,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_content_type",
            "JSON is required",
        );
    }
    let mut encodings = headers.get_all(header::CONTENT_ENCODING).iter();
    let encoding = encodings.next().map(|value| value.to_str());
    if encodings.next().is_some() {
        return request_error(
            &mut tracker,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_content_encoding",
            "request content encoding is not supported",
        );
    }
    let body = match encoding {
        None => body,
        Some(Ok(value)) if value.eq_ignore_ascii_case("identity") => body,
        Some(Ok(value)) if value.eq_ignore_ascii_case("zstd") => {
            let decoded = tokio::task::spawn_blocking(move || {
                let decoder = zstd::stream::read::Decoder::new(body.as_ref())?;
                let mut output = Vec::new();
                decoder
                    .take(MAX_BODY_BYTES as u64 + 1)
                    .read_to_end(&mut output)?;
                Ok::<_, std::io::Error>(output)
            })
            .await;
            match decoded {
                Ok(Ok(output)) if output.len() <= MAX_BODY_BYTES => Bytes::from(output),
                Ok(Ok(_)) => {
                    return request_error(
                        &mut tracker,
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request_too_large",
                        "decoded request body is too large",
                    );
                }
                _ => {
                    return request_error(
                        &mut tracker,
                        StatusCode::BAD_REQUEST,
                        "invalid_compressed_body",
                        "invalid zstd request body",
                    );
                }
            }
        }
        _ => {
            return request_error(
                &mut tracker,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_content_encoding",
                "request content encoding is not supported",
            );
        }
    };
    let Ok(mut request) = serde_json::from_slice::<Value>(&body) else {
        return request_error(
            &mut tracker,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid JSON body",
        );
    };
    let Some(object) = request.as_object_mut() else {
        return request_error(
            &mut tracker,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON object is required",
        );
    };
    let Some(public_id) = object
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return request_error(
            &mut tracker,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "model is required",
        );
    };
    tracker.record.public_model = Some(public_id.chars().take(256).collect());
    let Some(binding) = state.publication.routes.get(&public_id) else {
        return request_error(
            &mut tracker,
            StatusCode::NOT_FOUND,
            "unknown_model",
            "model is not published",
        );
    };
    tracker.record.provider_id = Some(binding.provider_id.clone());
    tracker.record.upstream_model = Some(binding.upstream_model.clone());
    let official = state.upstreams[&binding.provider_id].is_chatgpt();
    let xai = state.upstreams[&binding.provider_id].is_xai();
    let compatibility = if xai {
        object.insert("model".into(), binding.upstream_model.clone().into());
        match crate::xai_responses::Compatibility::request(&mut request) {
            Ok(compatibility) => Some(compatibility),
            Err(code) => {
                return request_error(
                    &mut tracker,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    code,
                    "Grok cannot represent this tool or request; use a supported function-tool model profile",
                );
            }
        }
    } else {
        None
    };
    let object = request.as_object_mut().unwrap();
    if object
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
        || object
            .get("conversation")
            .is_some_and(|value| !value.is_null())
    {
        return request_error(
            &mut tracker,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_capability",
            "server-side state continuation is not available; start a new session",
        );
    }
    if let Some(store) = &state.session_store {
        let session = match session_id(&headers) {
            Ok(session) => session,
            Err(code) => {
                return request_error(
                    &mut tracker,
                    StatusCode::BAD_REQUEST,
                    code,
                    "a single consistent Codex session ID is required; start a supported Codex session",
                );
            }
        };
        let mut context = match state.upstreams[&binding.provider_id]
            .session_context(&binding.provider_id, &headers)
        {
            Ok(context) => context,
            Err(code) => {
                return request_error(
                    &mut tracker,
                    StatusCode::UNAUTHORIZED,
                    code,
                    "the selected account context is unavailable; restore and republish the route",
                );
            }
        };
        if xai {
            context.upstream_model = Some(binding.upstream_model.clone());
        }
        let routing_hint = match single_header(&headers, "x-codex-routing-hint") {
            Ok(value) => value,
            Err(code) => {
                return request_error(
                    &mut tracker,
                    StatusCode::BAD_REQUEST,
                    code,
                    "a single consistent routing hint is required",
                );
            }
        };
        // Codex 0.158 sends this model-only hint even on a new root session.
        let opaque_hint = routing_hint.is_some_and(|value| {
            value.to_str().ok() != Some(format!("model={public_id}").as_str())
        });
        let allow_new = !compact
            && !object.get("input").is_some_and(opaque_input)
            && !headers.contains_key("x-codex-turn-state")
            && !opaque_hint;
        let registered = store.lock().map_err(|_| ()).and_then(|store| {
            store
                .bind_session(&session, &context, allow_new)
                .map_err(|_| ())
        });
        match registered {
            Ok(true) => {}
            Ok(false) => {
                return request_error(
                    &mut tracker,
                    StatusCode::CONFLICT,
                    "session_context_mismatch",
                    "the session account context is unknown or changed; start a new session",
                );
            }
            Err(()) => {
                return request_error(
                    &mut tracker,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "session_store_unavailable",
                    "could not verify the session account context",
                );
            }
        }
    }
    if compact && !official {
        return request_error(
            &mut tracker,
            StatusCode::NOT_IMPLEMENTED,
            "unsupported_endpoint",
            "compact is only available for the official subscription route",
        );
    }
    let authenticated = tokio::select! {
        biased;
        _ = cancellation.wait_for(|cancel| *cancel) => {
            tracker.finish(RequestStatus::Interrupted, Some("router_stopping"));
            return request_error(&mut tracker, StatusCode::SERVICE_UNAVAILABLE,
                "router_stopping", "router is stopping");
        }
        result = tokio::time::timeout_at(deadline.into(),
            state.upstreams[&binding.provider_id].authenticated_headers(&headers, &state.local_token)) => {
            match result {
                Ok(result) => result,
                Err(_) => return request_error(&mut tracker, StatusCode::BAD_GATEWAY,
                    "upstream_timeout", "account authentication timed out"),
            }
        }
    };
    let outbound_headers = match authenticated {
        Ok(headers) => headers,
        Err(code) => {
            if official {
                state.set_chatgpt_error(
                    &binding.provider_id,
                    if code == "chatgpt_account_changed" {
                        3
                    } else {
                        1
                    },
                );
            }
            return request_error(
                &mut tracker,
                StatusCode::UNAUTHORIZED,
                code,
                "ChatGPT authentication is missing or the account changed; restore and sign in with Codex",
            );
        }
    };
    if !official
        && object.get("input").is_some_and(|input| {
            opaque_input(input)
                && !(xai
                    && state.session_store.is_some()
                    && crate::xai_responses::replayable_input(input))
        })
    {
        return request_error(
            &mut tracker,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_capability",
            "server-side or encrypted state continuation is not available; start a new session",
        );
    }
    object.insert("model".into(), binding.upstream_model.clone().into());
    let Ok(outbound_body) = serde_json::to_vec(&request) else {
        return request_error(
            &mut tracker,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not encode request",
        );
    };

    let outbound_body = Bytes::from(outbound_body);
    let mut provider_id = &binding.provider_id;
    let upstream_response = loop {
        // Both attempts share one budget, including the final response body.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return request_error(
                &mut tracker,
                StatusCode::BAD_GATEWAY,
                "upstream_timeout",
                "upstream request timed out",
            );
        }
        let upstream = &state.upstreams[provider_id];
        let mut destination = upstream.responses_url.clone();
        if compact {
            destination.set_path(&format!("{}/compact", destination.path()));
        }
        let outbound = state
            .client
            .post(destination)
            .header(header::CONTENT_TYPE, "application/json")
            .headers(if provider_id == &binding.provider_id {
                outbound_headers.clone()
            } else {
                // RouterState excludes subscription accounts from fallback.
                match upstream.outbound_headers(&headers, &state.local_token) {
                    Ok(headers) => headers,
                    Err(code) => {
                        return request_error(
                            &mut tracker,
                            StatusCode::BAD_GATEWAY,
                            code,
                            "upstream credentials are invalid",
                        );
                    }
                }
            })
            .timeout(remaining)
            .body(outbound_body.clone())
            .send();
        let result = tokio::select! {
            biased;
            _ = cancellation.wait_for(|cancel| *cancel) => {
                tracker.finish(RequestStatus::Interrupted, Some("router_stopping"));
                return request_error(&mut tracker, StatusCode::SERVICE_UNAVAILABLE, "router_stopping", "router is stopping");
            }
            result = outbound => result,
        };
        match result {
            Ok(response) => break response,
            Err(error) => {
                // Connector errors happen before an HTTP request is sent. An
                // ambiguous send/read failure or any timeout must never replay.
                if error.is_connect()
                    && !error.is_timeout()
                    && tracker.record.fallback_from.is_none()
                    && let Some(fallback_id) = &binding.fallback_provider_id
                {
                    tracker.record.fallback_from = Some(provider_id.clone());
                    provider_id = fallback_id;
                    tracker.record.provider_id = Some(provider_id.clone());
                    continue;
                }
                let code = if error.is_timeout() {
                    "upstream_timeout"
                } else if error.is_connect() && tracker.record.fallback_from.is_some() {
                    "no_eligible_upstream"
                } else {
                    "upstream_unavailable"
                };
                return request_error(
                    &mut tracker,
                    StatusCode::BAD_GATEWAY,
                    code,
                    "upstream request failed",
                );
            }
        }
    };

    let status = upstream_response.status();
    tracker.record.http_status = Some(status.as_u16());
    tracker.record.headers_ms = Some(tracker.elapsed_ms());
    if xai && !status.is_success() {
        let code = match status.as_u16() {
            401 => "xai_unauthorized",
            403 => "xai_forbidden",
            _ => "upstream_http_error",
        };
        if status == StatusCode::UNAUTHORIZED
            && let UpstreamAuth::XaiOAuth { manager, id } =
                &state.upstreams[&binding.provider_id].auth
            && let Some(rejected) = outbound_headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        {
            manager.invalidate_access(id, rejected);
        }
        let mut response = request_error(
            &mut tracker,
            status,
            code,
            "Grok upstream rejected the request; check account permissions and model availability",
        );
        if let Some(value) = upstream_response.headers().get(header::RETRY_AFTER) {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, value.clone());
        }
        return response;
    }
    if !status.is_success() {
        let code = if official && status == StatusCode::UNAUTHORIZED {
            state.set_chatgpt_error(&binding.provider_id, 1);
            "chatgpt_unauthorized"
        } else if official && status == StatusCode::FORBIDDEN {
            state.set_chatgpt_error(&binding.provider_id, 2);
            "chatgpt_forbidden"
        } else {
            "upstream_http_error"
        };
        tracker.finish(RequestStatus::Failed, Some(code));
    } else if official {
        state.set_chatgpt_error(&binding.provider_id, 0);
    }
    let compatibility = compatibility.map(|compatibility| {
        compatibility.with_sse(
            upstream_response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';')
                        .next()
                        .is_some_and(|v| v.trim().eq_ignore_ascii_case("text/event-stream"))
                }),
        )
    });
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let sse = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    let observer = ResponseObserver::new(sse, status.is_success()).with_compaction(compact);
    let mut response = Response::builder()
        .status(status)
        .header(REQUEST_ID_HEADER, &tracker.record.id);
    if let Some(retry_after) = upstream_response.headers().get(header::RETRY_AFTER) {
        response = response.header(header::RETRY_AFTER, retry_after);
    }
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    if official {
        for name in ["x-codex-turn-state", "x-codex-routing-hint", "x-request-id"] {
            if let Some(value) = upstream_response.headers().get(name) {
                response = response.header(name, value);
            }
        }
    }
    // The tracker lives in the response body, including before its first poll.
    // Dropping a downstream body drops the upstream immediately and records cancellation.
    let stream = futures_util::stream::unfold(
        (
            upstream_response.bytes_stream(),
            observer,
            tracker,
            cancellation,
            false,
            compatibility,
        ),
        |(mut upstream, mut observer, mut tracker, mut cancellation, ended, mut compatibility)| async move {
            if ended {
                return None;
            }
            let chunk = tokio::select! {
                biased;
                _ = cancellation.wait_for(|cancel| *cancel) => {
                    tracker.finish(RequestStatus::Interrupted, Some("router_stopping"));
                    return None;
                }
                chunk = upstream.next() => chunk,
            };
            let (item, ended) = match chunk {
                Some(Ok(bytes)) => {
                    let bytes = match compatibility
                        .as_mut()
                        .map(|compatibility| compatibility.feed(&bytes, false))
                    {
                        Some(Ok(bytes)) => Bytes::from(bytes),
                        Some(Err(code)) => {
                            tracker.finish(RequestStatus::Interrupted, Some(code));
                            return Some((
                                Err(std::io::Error::other(code)),
                                (
                                    upstream,
                                    observer,
                                    tracker,
                                    cancellation,
                                    true,
                                    compatibility,
                                ),
                            ));
                        }
                        None => bytes,
                    };
                    match observer.feed(&bytes, &mut tracker) {
                        Ok(()) => (Ok(bytes), false),
                        Err(code) => {
                            tracker.finish(RequestStatus::Interrupted, Some(code));
                            (Err(std::io::Error::other(code)), true)
                        }
                    }
                }
                Some(Err(error)) => {
                    let code = if error.is_timeout() {
                        "upstream_timeout"
                    } else {
                        "upstream_read_error"
                    };
                    tracker.finish(RequestStatus::Interrupted, Some(code));
                    (Err(std::io::Error::other(code)), true)
                }
                None => {
                    if let Some(compatibility) = &mut compatibility {
                        match compatibility.feed(&[], true) {
                            Ok(bytes) if !bytes.is_empty() => {
                                if let Err(code) = observer.feed(&bytes, &mut tracker) {
                                    tracker.finish(RequestStatus::Interrupted, Some(code));
                                    return None;
                                }
                                observer.eof(&mut tracker);
                                return Some((
                                    Ok(Bytes::from(bytes)),
                                    (upstream, observer, tracker, cancellation, true, None),
                                ));
                            }
                            Err(code) => {
                                tracker.finish(RequestStatus::Interrupted, Some(code));
                                return Some((
                                    Err(std::io::Error::other(code)),
                                    (upstream, observer, tracker, cancellation, true, None),
                                ));
                            }
                            _ => {}
                        }
                    }
                    observer.eof(&mut tracker);
                    return None;
                }
            };
            Some((
                item,
                (
                    upstream,
                    observer,
                    tracker,
                    cancellation,
                    ended,
                    compatibility,
                ),
            ))
        },
    );
    response
        .body(Body::from_stream(stream))
        .expect("valid upstream response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Selection, publish};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::{fs, sync::atomic::AtomicUsize};
    use tokio::sync::{Notify, mpsc};

    #[test]
    fn official_auth_rejects_placeholder_api_key_local_token_and_ambiguous_headers() {
        let upstream = Upstream::chatgpt();
        let local = Secret::new("synthetic-local-only".into());
        for token in [
            "PROXY_MANAGED",
            "sk-synthetic-api-key",
            "synthetic-local-only",
            "invalid token",
            "",
        ] {
            let headers = HeaderMap::from_iter([
                (
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
                ),
                (
                    axum::http::HeaderName::from_static(crate::chatgpt::ACCOUNT_HEADER),
                    "fixture-workspace".parse().unwrap(),
                ),
            ]);
            assert!(upstream.outbound_headers(&headers, &local).is_err());
        }
        let mut headers = HeaderMap::from_iter([
            (
                header::AUTHORIZATION,
                "Bearer synthetic-native-access".parse().unwrap(),
            ),
            (
                axum::http::HeaderName::from_static(crate::chatgpt::ACCOUNT_HEADER),
                "fixture-workspace".parse().unwrap(),
            ),
        ]);
        headers.append(
            header::AUTHORIZATION,
            "Bearer second-native-access".parse().unwrap(),
        );
        assert!(upstream.outbound_headers(&headers, &local).is_err());
        headers.remove(header::AUTHORIZATION);
        headers.insert(
            header::AUTHORIZATION,
            "Bearer synthetic-native-access".parse().unwrap(),
        );
        headers.append(
            crate::chatgpt::ACCOUNT_HEADER,
            "other-workspace".parse().unwrap(),
        );
        assert!(upstream.outbound_headers(&headers, &local).is_err());
        assert!(Upstream::chatgpt_mock("192.0.2.1:1234".parse().unwrap()).is_err());
        assert_eq!(
            upstream.responses_url.as_str(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn official_workspace_is_pinned_before_first_request_and_region_header_is_enforced() {
        let workspace = crate::chatgpt::Workspace {
            account_id: "fixture-workspace".into(),
            backend_origin: "https://us.chatgpt.com".into(),
            routing_override: "us".into(),
        };
        let upstream = Upstream::chatgpt_for_workspace(&workspace).unwrap();
        assert_eq!(
            upstream.responses_url.as_str(),
            "https://us.chatgpt.com/backend-api/codex/responses"
        );
        let token = Secret::new("synthetic-local-only".into());
        let mut headers = HeaderMap::from_iter([
            (
                header::AUTHORIZATION,
                "Bearer synthetic-native-access".parse().unwrap(),
            ),
            (
                axum::http::HeaderName::from_static(crate::chatgpt::ACCOUNT_HEADER),
                "other-workspace".parse().unwrap(),
            ),
        ]);
        assert_eq!(
            upstream.outbound_headers(&headers, &token).unwrap_err(),
            "chatgpt_account_changed"
        );
        headers.insert(
            crate::chatgpt::ACCOUNT_HEADER,
            "fixture-workspace".parse().unwrap(),
        );
        assert_eq!(
            upstream.outbound_headers(&headers, &token).unwrap()["x-openai-account-routing-override"],
            "us"
        );
        headers.insert(
            "x-openai-account-routing-override",
            "us_cr".parse().unwrap(),
        );
        assert_eq!(
            upstream.outbound_headers(&headers, &token).unwrap_err(),
            "chatgpt_account_changed"
        );
    }

    #[test]
    fn official_and_api_connections_cannot_automatically_fallback_to_each_other() {
        let templates =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        for primary_is_official in [true, false] {
            let mut publication = publish(
                &templates,
                &[Selection {
                    public_id: "sx-account",
                    display_name: "Account",
                    provider_id: "primary",
                    upstream_model: "gpt-5.5",
                }],
            )
            .unwrap();
            publication
                .routes
                .get_mut("sx-account")
                .unwrap()
                .fallback_provider_id = Some("backup".into());
            let api = Upstream::new("http://127.0.0.1:1234", "synthetic-api-key".into()).unwrap();
            let official = Upstream::chatgpt();
            let (primary, backup) = if primary_is_official {
                (official, api)
            } else {
                (api, official)
            };
            let result = RouterState::new(
                "127.0.0.1:18731".parse().unwrap(),
                "synthetic-local-token-at-least-32-bytes".into(),
                publication,
                HashMap::from([("primary".into(), primary), ("backup".into(), backup)]),
            );
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn timeout_after_sending_does_not_try_explicit_backup() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary = listener.local_addr().unwrap();
        let called = Arc::new(AtomicUsize::new(0));
        let calls = called.clone();
        let mock = Router::new().route(
            "/responses",
            post(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<String>()
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        let backup = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let template =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        let mut publication = publish(
            &template,
            &[Selection {
                public_id: "sx-test",
                display_name: "Test",
                provider_id: "primary",
                upstream_model: "deepseek-flash",
            }],
        )
        .unwrap();
        publication
            .routes
            .get_mut("sx-test")
            .unwrap()
            .fallback_provider_id = Some("backup".into());
        let mut state = RouterState::new(
            "127.0.0.1:18731".parse().unwrap(),
            "synthetic-token-with-at-least-32-bytes".into(),
            publication,
            HashMap::from([
                (
                    "primary".into(),
                    Upstream::new(&format!("http://{primary}"), "primary-key".into()).unwrap(),
                ),
                (
                    "backup".into(),
                    Upstream::new(
                        &format!("http://{}", backup.local_addr().unwrap()),
                        "backup-key".into(),
                    )
                    .unwrap(),
                ),
            ]),
        )
        .unwrap();
        state.request_budget = Duration::from_millis(100);
        let headers = HeaderMap::from_iter([
            (header::HOST, "127.0.0.1:18731".parse().unwrap()),
            (header::CONTENT_TYPE, "application/json".parse().unwrap()),
            (
                header::AUTHORIZATION,
                "Bearer synthetic-token-with-at-least-32-bytes"
                    .parse()
                    .unwrap(),
            ),
        ]);
        let response = responses(
            State(Arc::new(state)),
            headers,
            Bytes::from_static(b"{\"model\":\"sx-test\"}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("upstream_timeout"));
        assert_eq!(called.load(Ordering::SeqCst), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), backup.accept())
                .await
                .is_err()
        );
        server.abort();
    }

    struct RefreshReplyFinished(mpsc::Sender<()>);

    impl Drop for RefreshReplyFinished {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }

    struct RefreshFixture {
        directory: PathBuf,
        home: PathBuf,
        manager: crate::accounts::AccountManager,
        id: String,
        address: SocketAddr,
        started: mpsc::Receiver<()>,
        finished: mpsc::Receiver<()>,
        release: Arc<Notify>,
        model_calls: Arc<AtomicUsize>,
        original_auth: Vec<u8>,
        original_store: Vec<u8>,
        server: JoinHandle<()>,
    }

    impl RefreshFixture {
        async fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "switchx-refresh-routing-{}",
                crate::app::new_id().unwrap()
            ));
            let home = directory.join("home");
            let data = directory.join("data");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&data).unwrap();
            let jwt = |payload: Value| {
                format!(
                    "{}.{}.synthetic-signature",
                    URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
                    URL_SAFE_NO_PAD.encode(payload.to_string())
                )
            };
            let identity = jwt(json!({"sub":"synthetic-refresh-user",
                "https://api.openai.com/auth":{"chatgpt_account_id":"fixture-workspace"}}));
            let original_auth = serde_json::to_vec(&json!({"auth_mode":"chatgpt",
                "OPENAI_API_KEY":null,"tokens":{"id_token":identity,
                    "access_token":jwt(json!({"exp":1})),
                    "refresh_token":"synthetic-original-refresh","account_id":"fixture-workspace"},
                "last_refresh":"2000-01-01T00:00:00.000Z"}))
            .unwrap();
            fs::write(home.join("auth.json"), &original_auth).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let release = Arc::new(Notify::new());
            let gate = release.clone();
            let (started_tx, started) = mpsc::channel(1);
            let (finished_tx, finished) = mpsc::channel(1);
            let renewed = json!({"id_token":identity,"access_token":jwt(json!({"exp":4102444800_i64})),
                "refresh_token":"synthetic-uncommitted-refresh","expires_in":3600});
            let model_calls = Arc::new(AtomicUsize::new(0));
            let calls = model_calls.clone();
            let mock = Router::new()
                .route(
                    "/token",
                    post(move || {
                        let started = started_tx.clone();
                        let finished = finished_tx.clone();
                        let gate = gate.clone();
                        let renewed = renewed.clone();
                        async move {
                            let _finished = RefreshReplyFinished(finished);
                            started.send(()).await.unwrap();
                            gate.notified().await;
                            Json(renewed)
                        }
                    }),
                )
                .route(
                    "/responses",
                    post(move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async { Json(json!({"status":"completed","output":[]})) }
                    }),
                );
            let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
            let manager =
                crate::accounts::AccountManager::open_mock(&data, &format!("http://{address}"))
                    .unwrap();
            let id = manager.import_current(&home).unwrap().id;
            let original_store = fs::read(data.join("codex_oauth_auth.json")).unwrap();
            Self {
                directory,
                home,
                manager,
                id,
                address,
                started,
                finished,
                release,
                model_calls,
                original_auth,
                original_store,
                server,
            }
        }

        fn state(&self, address: SocketAddr, budget: Duration) -> RouterState {
            let templates =
                serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))
                    .unwrap();
            let publication = publish(
                &templates,
                &[Selection {
                    public_id: "sx-account",
                    display_name: "Account",
                    provider_id: "official",
                    upstream_model: "gpt-5.5",
                }],
            )
            .unwrap();
            let workspace = crate::chatgpt::Workspace {
                account_id: "fixture-workspace".into(),
                backend_origin: "https://chatgpt.com".into(),
                routing_override: "NO_CONSTRAINT".into(),
            };
            let upstream = Upstream::managed_chatgpt_mock(
                self.address,
                self.manager.clone(),
                self.id.clone(),
                self.home.clone(),
                &workspace,
            )
            .unwrap();
            let mut state = RouterState::new(
                address,
                "synthetic-local-token-at-least-32-bytes".into(),
                publication,
                HashMap::from([("official".into(), upstream)]),
            )
            .unwrap();
            state.request_budget = budget;
            state
        }

        fn assert_native_and_model_unchanged(&self) {
            assert_eq!(
                fs::read(self.home.join("auth.json")).unwrap(),
                self.original_auth
            );
            assert_eq!(self.model_calls.load(Ordering::SeqCst), 0);
        }

        async fn finish_abandoned_refresh(&mut self) {
            self.release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), self.finished.recv())
                .await
                .unwrap()
                .unwrap();
            self.manager.wait_for_idle().await.unwrap();
            self.manager
                .set_default(&self.id)
                .expect("completed refresh must release the account operation and file lock");
            self.manager
                .sync_current(&self.home)
                .expect("completed refresh must release the Codex config lock");
            assert_eq!(self.model_calls.load(Ordering::SeqCst), 0);
            let native: Value =
                serde_json::from_slice(&fs::read(self.home.join("auth.json")).unwrap()).unwrap();
            assert_eq!(
                native["tokens"]["refresh_token"],
                "synthetic-uncommitted-refresh"
            );
            let saved = fs::read(self.directory.join("data/codex_oauth_auth.json")).unwrap();
            assert_ne!(saved, self.original_store);
            assert!(
                String::from_utf8(saved)
                    .unwrap()
                    .contains("synthetic-uncommitted-refresh")
            );
        }
    }

    impl Drop for RefreshFixture {
        fn drop(&mut self) {
            self.server.abort();
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[tokio::test]
    async fn managed_auth_refresh_uses_the_request_budget_and_never_reaches_the_model() {
        let mut fixture = RefreshFixture::new().await;
        let address = "127.0.0.1:18731".parse().unwrap();
        let state = fixture.state(address, Duration::from_millis(250));
        let headers = HeaderMap::from_iter([
            (header::HOST, address.to_string().parse().unwrap()),
            (header::CONTENT_TYPE, "application/json".parse().unwrap()),
            (
                axum::http::HeaderName::from_static(LOCAL_TOKEN_HEADER),
                "synthetic-local-token-at-least-32-bytes".parse().unwrap(),
            ),
        ]);
        let request = tokio::spawn(responses(
            State(Arc::new(state)),
            headers,
            Bytes::from_static(b"{\"model\":\"sx-account\"}"),
        ));
        tokio::time::timeout(Duration::from_secs(2), fixture.started.recv())
            .await
            .unwrap()
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
            "upstream_timeout"
        );
        fixture.assert_native_and_model_unchanged();
        fixture.finish_abandoned_refresh().await;
    }

    #[tokio::test]
    async fn stopping_the_router_preserves_a_detached_managed_refresh() {
        let mut fixture = RefreshFixture::new().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let database = fixture.directory.join("requests.sqlite");
        let state = fixture
            .state(address, Duration::from_secs(120))
            .with_request_log(Store::open(&database).unwrap(), "cancel-refresh".into());
        let running = RunningRouter::start(listener, state).unwrap();
        let mut request = tokio::spawn(async move {
            Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .post(format!("http://{address}/v1/responses"))
                .header(
                    LOCAL_TOKEN_HEADER,
                    "synthetic-local-token-at-least-32-bytes",
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body("{\"model\":\"sx-account\"}")
                .send()
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), fixture.started.recv())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(8), running.stop())
            .await
            .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(2), &mut request).await;
        if received.is_err() {
            request.abort();
            let _ = request.await;
            panic!("router shutdown must end the pending HTTP request");
        }
        if let Ok(response) = received.unwrap().unwrap() {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        let records = Store::open(&database).unwrap().requests(10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, RequestStatus::Interrupted);
        assert_eq!(records[0].error_code.as_deref(), Some("router_stopping"));
        fixture.assert_native_and_model_unchanged();
        fixture.finish_abandoned_refresh().await;
    }
}
