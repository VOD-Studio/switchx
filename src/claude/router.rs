//! A loopback-only Claude router with explicit per-connection upstream protocols.
use super::{AuthMode, UpstreamProtocol, messages, publication::PublishedModel, responses};
use crate::{
    credentials::Secret,
    requests::{REQUEST_ID_HEADER, RequestLog, RequestTracker},
    storage::{RequestStatus, Store, UsageClient},
};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, oneshot, watch},
    task::JoinHandle,
};
const BODY_LIMIT: usize = 16 * 1024 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

pub struct Destination {
    pub model: PublishedModel,
    pub base_url: String,
    pub auth: AuthMode,
    pub protocol: UpstreamProtocol,
    pub key: Secret,
}
pub struct Snapshot {
    pub published_at: String,
    pub generation: String,
    pub destinations: BTreeMap<String, Destination>,
    pub aliases: BTreeMap<String, String>,
}
impl Snapshot {
    pub fn new(
        generation: String,
        destinations: Vec<Destination>,
        aliases: BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut mapped = BTreeMap::new();
        for dest in destinations {
            super::types::validate_model(&dest.model.public_id)?;
            crate::app::claude::validate_claude_endpoint(&dest.base_url)?;
            if dest.auth == AuthMode::Native || dest.key.expose().is_empty() {
                return Err("路由需要明确的 API 凭据".into());
            }
            if dest.protocol == UpstreamProtocol::Responses && dest.auth != AuthMode::Bearer {
                return Err("Responses 路由需要 Bearer Token".into());
            }
            if mapped.insert(dest.model.public_id.clone(), dest).is_some() {
                return Err("路由模型 ID 重复".into());
            }
        }
        if mapped.is_empty() || aliases.values().any(|id| !mapped.contains_key(id)) {
            return Err("路由模型绑定无效".into());
        }
        Ok(Self {
            published_at: chrono::Utc::now().to_rfc3339(),
            generation,
            destinations: mapped,
            aliases,
        })
    }
}
struct Shared {
    snapshot: RwLock<Arc<Snapshot>>,
    token: Secret,
    address: SocketAddr,
    client: reqwest::Client,
    accepting: AtomicBool,
    permits: Arc<Semaphore>,
    cancel: watch::Sender<bool>,
    log: Option<Arc<RequestLog>>,
    workspace: String,
}
pub struct Running {
    shared: Arc<Shared>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}
impl Running {
    pub fn start(
        listener: TcpListener,
        token: Secret,
        snapshot: Arc<Snapshot>,
        workspace: String,
        store: Option<Store>,
    ) -> Result<Self, String> {
        let address = listener.local_addr().map_err(|_| "无法读取路由地址")?;
        if address.ip() != Ipv4Addr::LOCALHOST || token.expose().len() != 64 {
            return Err("Claude 路由必须绑定 IPv4 loopback 并设置本地令牌".into());
        }
        let shared = Arc::new(Shared {
            snapshot: RwLock::new(snapshot),
            token,
            address,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|_| "无法初始化路由客户端")?,
            accepting: AtomicBool::new(false),
            permits: Arc::new(Semaphore::new(8)),
            cancel: watch::channel(false).0,
            log: store.map(|store| Arc::new(RequestLog::new(store, String::new()))),
            workspace,
        });
        let app = Router::new()
            .route("/v1/messages", post(forward))
            .route("/v1/messages/count_tokens", post(forward))
            .route("/v1/models", get(models))
            .route("/api/hello", get(hello))
            .fallback(not_found)
            .with_state(shared.clone());
        let (shutdown, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Ok(Self {
            shared,
            shutdown: Some(shutdown),
            task,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.shared.address
    }
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
    pub fn recording_failed(&self) -> bool {
        self.shared
            .log
            .as_ref()
            .is_some_and(|log| log.failed.load(Ordering::Relaxed))
    }
    pub fn pause(&self) {
        self.shared.accepting.store(false, Ordering::SeqCst);
    }
    pub fn resume(&self) {
        self.shared.accepting.store(true, Ordering::SeqCst);
    }
    pub fn publish(&self, snapshot: Arc<Snapshot>) {
        *self.shared.snapshot.write().unwrap() = snapshot;
    }
    pub async fn stop(mut self) {
        self.pause();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            self.shared.cancel.send_replace(true);
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.shared.cancel.send_replace(true);
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}
fn error(status: StatusCode, message: &'static str) -> Response {
    (status, Json(messages::error_body(message))).into_response()
}
fn authorize(headers: &HeaderMap, state: &Shared) -> Option<Response> {
    // Browser-originated calls and rebinding hosts are never inference clients.
    if headers.contains_key(header::ORIGIN)
        || headers.get(header::HOST).and_then(|v| v.to_str().ok())
            != Some(state.address.to_string().as_str())
    {
        return Some(error(StatusCode::FORBIDDEN, "Invalid local router origin."));
    }
    let mut found = false;
    for name in ["authorization", "x-api-key"] {
        let all: Vec<_> = headers.get_all(name).iter().collect();
        if all.len() > 1 {
            return Some(error(
                StatusCode::UNAUTHORIZED,
                "Invalid local credentials.",
            ));
        }
        if let Some(value) = all.first() {
            let value = value.to_str().unwrap_or("");
            let value = if name == "authorization" {
                value.strip_prefix("Bearer ").unwrap_or("")
            } else {
                value
            };
            if !crate::control::verify(state.token.expose(), value) {
                return Some(error(
                    StatusCode::UNAUTHORIZED,
                    "Invalid local credentials.",
                ));
            }
            found = true;
        }
    }
    if !found {
        return Some(error(
            StatusCode::UNAUTHORIZED,
            "Local credentials are required.",
        ));
    }
    None
}
async fn hello(State(state): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    match authorize(&headers, &state) {
        None => StatusCode::NO_CONTENT.into_response(),
        Some(response) => response,
    }
}
async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "Unsupported Claude endpoint.")
}
async fn models(State(state): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&headers, &state) {
        return response;
    }
    let snapshot = state.snapshot.read().unwrap().clone();
    let data:Vec<_>=snapshot.destinations.values().filter(|d|d.model.menu).map(|d|json!({"id":d.model.public_id,"type":"model","display_name":d.model.label,"created_at":snapshot.published_at})).collect();
    Json(json!({"first_id":data.first().map(|m|&m["id"]),"last_id":data.last().map(|m|&m["id"]),"data":data,"has_more":false})).into_response()
}
fn tracked_error(mut tracker: RequestTracker, status: StatusCode, code: &'static str) -> Response {
    tracker.finish(
        if matches!(
            code,
            "router_stopping" | "upstream_body_failed" | "upstream_idle_timeout"
        ) {
            RequestStatus::Interrupted
        } else {
            RequestStatus::Failed
        },
        Some(code),
    );
    let id = tracker.record.id.clone();
    let message = match code {
        "invalid_responses_input" => "The Claude request is invalid for Responses conversion.",
        "unsupported_responses_input" => {
            "This Claude request contains content the Responses adapter cannot preserve."
        }
        "unsupported_responses_image" => "This image source cannot be sent to Responses.",
        "unsupported_responses_tool_error" => {
            "Failed Claude tool results cannot be represented by this Responses adapter."
        }
        "unsupported_responses_tool_choice" => {
            "This Claude tool choice cannot be represented by Responses."
        }
        "unsupported_responses_max_tokens" => {
            "This max_tokens value is below the Responses minimum."
        }
        "responses_count_unsupported" => {
            "This Responses connection does not support Anthropic token counting."
        }
        "responses_query_unsupported" => {
            "Claude request query parameters cannot be sent to Responses."
        }
        "responses_upstream_incomplete" => "The Responses upstream did not complete its output.",
        "invalid_responses_output" | "unsupported_responses_output" | "invalid_responses_usage" => {
            "The Responses upstream returned content this adapter cannot preserve."
        }
        _ => code,
    };
    let mut response = error(status, message);
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, HeaderValue::from_str(&id).unwrap());
    response
}
fn response_headers(source: &HeaderMap, target: &mut HeaderMap) {
    for name in [
        "retry-after",
        "x-should-retry",
        "anthropic-ratelimit-requests-limit",
        "anthropic-ratelimit-requests-remaining",
        "anthropic-ratelimit-requests-reset",
        "anthropic-ratelimit-tokens-limit",
        "anthropic-ratelimit-tokens-remaining",
        "anthropic-ratelimit-tokens-reset",
        "anthropic-ratelimit-unified-status",
        "anthropic-ratelimit-unified-reset",
    ] {
        if let Some(value) = source.get(name).filter(|v| v.len() < 256) {
            target.insert(name, value.clone());
        }
    }
}
async fn forward(State(state): State<Arc<Shared>>, request: Request) -> Response {
    if let Some(response) = authorize(request.headers(), &state) {
        return response;
    }
    if !state.accepting.load(Ordering::SeqCst) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Router publication is changing.",
        );
    }
    let permit = match state.permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "Local concurrency limit reached.",
            );
        }
    };
    let snapshot = state.snapshot.read().unwrap().clone();
    let count = request.uri().path().ends_with("/count_tokens");
    let mut cancel = state.cancel.subscribe();
    let mut tracker =
        match RequestTracker::new(if count { None } else { state.log.clone() }, cancel.clone()) {
            Ok(t) => t,
            Err(_) => {
                return error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Cannot allocate request.",
                );
            }
        };
    tracker.record.client = UsageClient::ClaudeCode;
    tracker.record.workspace_id = Some(state.workspace.clone());
    tracker.record.generation = snapshot.generation.clone();
    let (parts, body) = request.into_parts();
    let bytes = tokio::select! {_ = cancel.changed()=>return tracked_error(tracker,StatusCode::SERVICE_UNAVAILABLE,"router_stopping"), result=tokio::time::timeout(HEADER_TIMEOUT,to_bytes(body,BODY_LIMIT))=>match result {Ok(Ok(bytes))=>bytes,Ok(Err(_))=>return tracked_error(tracker,StatusCode::PAYLOAD_TOO_LARGE,"request_too_large"),Err(_)=>return tracked_error(tracker,StatusCode::REQUEST_TIMEOUT,"request_body_timeout")}};
    let mut value: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(v)) => Value::Object(v),
        _ => return tracked_error(tracker, StatusCode::BAD_REQUEST, "invalid_json"),
    };
    let Some(model) = value["model"].as_str() else {
        return tracked_error(tracker, StatusCode::BAD_REQUEST, "model_required");
    };
    let public = snapshot.aliases.get(model).map_or(model, String::as_str);
    let Some(destination) = snapshot.destinations.get(public) else {
        return tracked_error(tracker, StatusCode::NOT_FOUND, "unknown_model");
    };
    let public = public.to_owned();
    tracker.record.public_model = Some(public.clone());
    tracker.record.provider_id = Some(destination.model.provider_id.clone());
    tracker.record.upstream_model = Some(destination.model.upstream_model.clone());
    let stream = match value.get("stream") {
        None => false,
        Some(Value::Bool(v)) => *v,
        _ => return tracked_error(tracker, StatusCode::BAD_REQUEST, "invalid_stream"),
    };
    if !value["messages"].is_array() {
        return tracked_error(tracker, StatusCode::BAD_REQUEST, "messages_required");
    }
    value["model"] = destination.model.upstream_model.clone().into();
    if count && destination.protocol == UpstreamProtocol::Responses {
        return tracked_error(
            tracker,
            StatusCode::NOT_IMPLEMENTED,
            "responses_count_unsupported",
        );
    }
    let body = if destination.protocol == UpstreamProtocol::Responses {
        match responses::request(&value) {
            Ok(body) => body,
            Err(code) => return tracked_error(tracker, StatusCode::BAD_REQUEST, code),
        }
    } else {
        value
    };
    let upstream_path = if destination.protocol == UpstreamProtocol::Responses {
        "/v1/responses"
    } else {
        parts.uri.path()
    };
    let mut url = reqwest::Url::parse(&format!(
        "{}{}",
        destination.base_url.trim_end_matches('/'),
        upstream_path
    ))
    .unwrap();
    if parts
        .uri
        .query()
        .is_some_and(|q| q.contains(state.token.expose()))
    {
        return tracked_error(tracker, StatusCode::BAD_REQUEST, "credential_in_query");
    }
    url.set_query(parts.uri.query());
    if url
        .query_pairs()
        .any(|(_, value)| value.contains(state.token.expose()))
    {
        return tracked_error(tracker, StatusCode::BAD_REQUEST, "credential_in_query");
    }
    if destination.protocol == UpstreamProtocol::Responses {
        // The Anthropic beta Messages SDK adds this marker to ordinary CLI calls.
        // It describes the client protocol, so it must not reach a Responses server.
        if parts.uri.query().is_some_and(|query| query != "beta=true") {
            return tracked_error(
                tracker,
                StatusCode::BAD_REQUEST,
                "responses_query_unsupported",
            );
        }
        url.set_query(None);
    }
    let mut outgoing = state
        .client
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string());
    // Only protocol headers cross the trust boundary; credentials are rebuilt per destination.
    if destination.protocol == UpstreamProtocol::Messages {
        for name in ["anthropic-version", "anthropic-beta"] {
            for value in parts.headers.get_all(name) {
                outgoing = outgoing.header(name, value.clone());
            }
        }
        if !parts.headers.contains_key("anthropic-version") {
            outgoing = outgoing.header("anthropic-version", "2023-06-01");
        }
    }
    if destination.auth != AuthMode::ApiKey {
        outgoing = outgoing.bearer_auth(destination.key.expose());
    }
    if destination.auth != AuthMode::Bearer {
        outgoing = outgoing.header("x-api-key", destination.key.expose());
    }
    // A buffered Responses server may send its headers only after generation.
    let header_timeout = if destination.protocol == UpstreamProtocol::Responses {
        IDLE_TIMEOUT
    } else {
        HEADER_TIMEOUT
    };
    let upstream = tokio::select! {_ = cancel.changed()=>return tracked_error(tracker,StatusCode::SERVICE_UNAVAILABLE,"router_stopping"),result=tokio::time::timeout(header_timeout,outgoing.send())=>match result {Ok(Ok(response))=>response,Ok(Err(_))=>return tracked_error(tracker,StatusCode::BAD_GATEWAY,"upstream_connection_failed"),Err(_)=>return tracked_error(tracker,StatusCode::GATEWAY_TIMEOUT,"upstream_headers_timeout")}};
    tracker.record.headers_ms = Some(tracker.elapsed_ms());
    tracker.record.http_status = Some(upstream.status().as_u16());
    if !upstream.status().is_success() {
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let mut response = tracked_error(tracker, status, "upstream_rejected");
        response_headers(&headers, response.headers_mut());
        return response;
    }
    if destination.protocol == UpstreamProtocol::Responses {
        let headers = upstream.headers().clone();
        let mut upstream = upstream;
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {_ = cancel.changed()=>return tracked_error(tracker,StatusCode::SERVICE_UNAVAILABLE,"router_stopping"),chunk=tokio::time::timeout(IDLE_TIMEOUT,upstream.chunk())=>match chunk{Ok(Ok(c))=>c,Ok(Err(_))=>return tracked_error(tracker,StatusCode::BAD_GATEWAY,"upstream_body_failed"),Err(_)=>return tracked_error(tracker,StatusCode::GATEWAY_TIMEOUT,"upstream_idle_timeout")}};
            let Some(chunk) = chunk else { break };
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, "upstream_body_too_large");
            }
            bytes.extend_from_slice(&chunk);
        }
        let upstream: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, "invalid_upstream_json");
            }
        };
        let mut converted = match responses::message(&upstream) {
            Ok(value) => value,
            Err(code) => return tracked_error(tracker, StatusCode::BAD_GATEWAY, code),
        };
        let mut response = if stream {
            let events = match responses::sse(&converted) {
                Ok(events) => events,
                Err(code) => return tracked_error(tracker, StatusCode::BAD_GATEWAY, code),
            };
            let mut observer = messages::Stream::default();
            let events = match observer.feed(&events, &public, &mut tracker) {
                Ok(events) if observer.done => events,
                Ok(_) => {
                    return tracked_error(
                        tracker,
                        StatusCode::BAD_GATEWAY,
                        "incomplete_responses_stream",
                    );
                }
                Err(code) => return tracked_error(tracker, StatusCode::BAD_GATEWAY, code),
            };
            let mut response = Body::from(events).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        } else {
            if let Err(code) = messages::message(&mut converted, &public, &mut tracker) {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, code);
            }
            Json(converted).into_response()
        };
        response.headers_mut().insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_str(&tracker.record.id).unwrap(),
        );
        response_headers(&headers, response.headers_mut());
        return response;
    }
    let is_sse = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|t| {
            t.split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
        });
    if stream && !count && !is_sse {
        return tracked_error(tracker, StatusCode::BAD_GATEWAY, "expected_event_stream");
    }
    if !stream || count {
        let headers = upstream.headers().clone();
        let mut upstream = upstream;
        let mut bytes = Vec::new();
        loop {
            let chunk = tokio::select! {_ = cancel.changed()=>return tracked_error(tracker,StatusCode::SERVICE_UNAVAILABLE,"router_stopping"),chunk=tokio::time::timeout(IDLE_TIMEOUT,upstream.chunk())=>match chunk{Ok(Ok(c))=>c,Ok(Err(_))=>return tracked_error(tracker,StatusCode::BAD_GATEWAY,"upstream_body_failed"),Err(_)=>return tracked_error(tracker,StatusCode::GATEWAY_TIMEOUT,"upstream_idle_timeout")}};
            let Some(chunk) = chunk else { break };
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, "upstream_body_too_large");
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, "invalid_upstream_json");
            }
        };
        if count {
            if !value["input_tokens"].is_u64() {
                return tracked_error(tracker, StatusCode::BAD_GATEWAY, "invalid_token_count");
            }
        } else if let Err(code) = messages::message(&mut value, &public, &mut tracker) {
            return tracked_error(tracker, StatusCode::BAD_GATEWAY, code);
        }
        let mut response = Json(value).into_response();
        response_headers(&headers, response.headers_mut());
        response.headers_mut().insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_str(&tracker.record.id).unwrap(),
        );
        return response;
    }
    let headers = upstream.headers().clone();
    let request_id = tracker.record.id.clone();
    let body = futures_util::stream::unfold(
        (
            upstream.bytes_stream(),
            messages::Stream::default(),
            tracker,
            cancel,
            permit,
            false,
            public,
        ),
        |(mut upstream, mut observer, mut tracker, mut cancel, permit, ended, public)| async move {
            if ended {
                return None;
            }
            loop {
                let chunk = tokio::select! {_ = cancel.changed()=>{tracker.finish(RequestStatus::Interrupted,Some("router_stopping"));return None;},result=tokio::time::timeout(IDLE_TIMEOUT,upstream.next())=>result};
                let bytes = match chunk {
                    Ok(Some(Ok(bytes))) => bytes,
                    Ok(None) => {
                        observer.eof(&mut tracker);
                        return None;
                    }
                    other => {
                        tracker.finish(
                            RequestStatus::Interrupted,
                            Some(if other.is_err() {
                                "upstream_idle_timeout"
                            } else {
                                "upstream_stream_failed"
                            }),
                        );
                        return Some((
                            Ok::<Bytes, std::io::Error>(Bytes::from(messages::error_event())),
                            (upstream, observer, tracker, cancel, permit, true, public),
                        ));
                    }
                };
                match observer.feed(&bytes, &public, &mut tracker) {
                    Ok(out) if !out.is_empty() => {
                        let done = observer.done;
                        return Some((
                            Ok(Bytes::from(out)),
                            (upstream, observer, tracker, cancel, permit, done, public),
                        ));
                    }
                    Ok(_) => {}
                    Err(code) => {
                        tracker.finish(RequestStatus::Interrupted, Some(code));
                        return Some((
                            Ok(Bytes::from(messages::error_event())),
                            (upstream, observer, tracker, cancel, permit, true, public),
                        ));
                    }
                }
            }
        },
    );
    let mut response = Body::from_stream(body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, request_id.parse().unwrap());
    response_headers(&headers, response.headers_mut());
    response
}
