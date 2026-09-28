use std::{
    collections::HashMap,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use reqwest::{Client, Url, redirect::Policy};
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
    storage::{RequestStatus, Store},
};

pub const LOCAL_TOKEN_HEADER: &str = "x-switchx-local-token";
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct Upstream {
    responses_url: Url,
    api_key: Secret,
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
            api_key,
        })
    }
}

pub struct RouterState {
    expected_host: String,
    local_token: Secret,
    publication: Publication,
    upstreams: HashMap<String, Upstream>,
    client: Client,
    accepting: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    request_log: Option<Arc<RequestLog>>,
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
            if !upstreams.contains_key(&binding.provider_id) {
                return Err(format!(
                    "missing provider for route: {}",
                    binding.provider_id
                ));
            }
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| "failed to create upstream client")?;
        Ok(Self {
            expected_host: address.to_string(),
            local_token: Secret::new(local_token),
            publication,
            upstreams,
            client,
            accepting: Arc::new(AtomicBool::new(true)),
            cancel: watch::channel(false).0,
            request_log: None,
        })
    }

    pub fn with_request_log(mut self, store: Store, generation: String) -> Self {
        self.request_log = Some(Arc::new(RequestLog::new(store, generation)));
        self
    }
}

pub struct RunningRouter {
    accepting: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    request_log: Option<Arc<RequestLog>>,
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
    } else {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
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

async fn compact(State(state): State<Arc<RouterState>>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&headers, &state) {
        return response;
    }
    error(
        StatusCode::NOT_IMPLEMENTED,
        "unsupported_endpoint",
        "responses/compact is not implemented",
    )
}

async fn responses(
    State(state): State<Arc<RouterState>>,
    headers: HeaderMap,
    body: Bytes,
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
    let Some(public_id) = object.get("model").and_then(Value::as_str) else {
        return request_error(
            &mut tracker,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "model is required",
        );
    };
    tracker.record.public_model = Some(public_id.chars().take(256).collect());
    let Some(binding) = state.publication.routes.get(public_id) else {
        return request_error(
            &mut tracker,
            StatusCode::NOT_FOUND,
            "unknown_model",
            "model is not published",
        );
    };
    tracker.record.provider_id = Some(binding.provider_id.clone());
    tracker.record.upstream_model = Some(binding.upstream_model.clone());
    if object
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
        || object.get("conversation").is_some_and(|v| !v.is_null())
        || object
            .get("input")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.get("encrypted_content")
                        .is_some_and(|value| !value.is_null())
                        || item["type"] == "compaction"
                })
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
    let Some(upstream) = state.upstreams.get(&binding.provider_id) else {
        return request_error(
            &mut tracker,
            StatusCode::SERVICE_UNAVAILABLE,
            "no_eligible_upstream",
            "provider is unavailable",
        );
    };
    let Ok(outbound_body) = serde_json::to_vec(&request) else {
        return request_error(
            &mut tracker,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not encode request",
        );
    };

    let mut cancellation = state.cancel.subscribe();
    let outbound = state
        .client
        .post(upstream.responses_url.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .bearer_auth(upstream.api_key.expose())
        .body(outbound_body)
        .send();
    let result = tokio::select! {
        result = outbound => result,
        _ = cancellation.wait_for(|cancel| *cancel) => {
            tracker.finish(RequestStatus::Interrupted, Some("router_stopping"));
            return request_error(&mut tracker, StatusCode::SERVICE_UNAVAILABLE, "router_stopping", "router is stopping");
        },
    };
    let upstream_response = match result {
        Ok(response) => response,
        Err(error) => {
            let code = if error.is_timeout() {
                "upstream_timeout"
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
    };

    let status = upstream_response.status();
    tracker.record.http_status = Some(status.as_u16());
    tracker.record.headers_ms = Some(tracker.elapsed_ms());
    if !status.is_success() {
        tracker.finish(RequestStatus::Failed, Some("upstream_http_error"));
    }
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let sse = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
    let observer = ResponseObserver::new(sse, status.is_success());
    let mut response = Response::builder()
        .status(status)
        .header(REQUEST_ID_HEADER, &tracker.record.id);
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
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
        ),
        |(mut upstream, mut observer, mut tracker, mut cancellation, ended)| async move {
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
                Some(Ok(bytes)) => match observer.feed(&bytes, &mut tracker) {
                    Ok(()) => (Ok(bytes), false),
                    Err(code) => {
                        tracker.finish(RequestStatus::Interrupted, Some(code));
                        (Err(std::io::Error::other(code)), true)
                    }
                },
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
                    observer.eof(&mut tracker);
                    return None;
                }
            };
            Some((item, (upstream, observer, tracker, cancellation, ended)))
        },
    );
    response
        .body(Body::from_stream(stream))
        .expect("valid upstream response")
}
