use std::{
    collections::HashMap,
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
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
use reqwest::{Client, Url, redirect::Policy};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;

use crate::catalog::Publication;

pub const LOCAL_TOKEN_HEADER: &str = "x-switchx-local-token";
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub struct Upstream {
    responses_url: Url,
    api_key: String,
}

impl Upstream {
    pub fn new(base_url: &str, api_key: String) -> Result<Self, String> {
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
        if api_key.is_empty() {
            return Err("upstream credential is missing".into());
        }
        let path = match url.path().trim_end_matches('/') {
            "" => "/responses",
            "/v1" => "/v1/responses",
            _ => return Err("upstream base URL must end at / or /v1".into()),
        };
        url.set_path(path);
        Ok(Self {
            responses_url: url,
            api_key,
        })
    }
}

pub struct RouterState {
    expected_host: String,
    local_token: String,
    publication: Publication,
    upstreams: HashMap<String, Upstream>,
    client: Client,
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
            local_token,
            publication,
            upstreams,
            client,
        })
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
                .ct_eq(state.local_token.as_bytes())
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
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_content_type",
            "JSON is required",
        );
    }
    let mut encodings = headers.get_all(header::CONTENT_ENCODING).iter();
    let encoding = encodings.next().map(|value| value.to_str());
    if encodings.next().is_some() {
        return error(
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
                    return error(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request_too_large",
                        "decoded request body is too large",
                    );
                }
                _ => {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "invalid_compressed_body",
                        "invalid zstd request body",
                    );
                }
            }
        }
        _ => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_content_encoding",
                "request content encoding is not supported",
            );
        }
    };
    let Ok(mut request) = serde_json::from_slice::<Value>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid JSON body",
        );
    };
    let Some(object) = request.as_object_mut() else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON object is required",
        );
    };
    let Some(public_id) = object.get("model").and_then(Value::as_str) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "model is required",
        );
    };
    let Some(binding) = state.publication.routes.get(public_id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_model",
            "model is not published",
        );
    };
    if object
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
        || object.get("conversation").is_some_and(|v| !v.is_null())
    {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_capability",
            "server-side state continuation is not available",
        );
    }
    object.insert("model".into(), binding.upstream_model.clone().into());
    let Some(upstream) = state.upstreams.get(&binding.provider_id) else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_eligible_upstream",
            "provider is unavailable",
        );
    };
    let Ok(outbound_body) = serde_json::to_vec(&request) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "could not encode request",
        );
    };

    let result = state
        .client
        .post(upstream.responses_url.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .bearer_auth(&upstream.api_key)
        .body(outbound_body)
        .send()
        .await;
    let Ok(upstream_response) = result else {
        return error(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "upstream request failed",
        );
    };

    let status = upstream_response.status();
    let content_type = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned();
    let mut response = Response::builder().status(status);
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from_stream(upstream_response.bytes_stream()))
        .expect("valid upstream response")
}
