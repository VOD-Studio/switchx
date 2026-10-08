use std::{collections::HashMap, io, sync::Arc, time::Duration};

#[path = "router/managed_accounts.rs"]
mod managed_accounts;
#[path = "router/xai.rs"]
mod xai;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::stream;
use serde_json::{Value, json};
use switchx::{
    catalog::{Selection, publish},
    requests::REQUEST_ID_HEADER,
    routing::{LOCAL_TOKEN_HEADER, RouterState, RunningRouter, Upstream, serve},
    storage::{RequestRecord, RequestStatus, Store},
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    time::timeout,
};

struct RequestDatabase(std::path::PathBuf);

impl RequestDatabase {
    fn new() -> Self {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("switchx-request-test-{nonce:02x?}.sqlite"));
        Self(path)
    }

    fn store(&self) -> Store {
        Store::open(&self.0).unwrap()
    }

    async fn wait_for(&self, id: &str) -> RequestRecord {
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(record) = self
                    .store()
                    .requests(100)
                    .unwrap()
                    .into_iter()
                    .find(|record| record.id == id)
                {
                    return record;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("request must reach a persisted terminal state")
    }
}

impl Drop for RequestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn records_real_terminal_signals_and_safe_errors_without_changing_stream_bytes() {
    let cases = [
        (
            "complete",
            "text/event-stream; charset=utf-8",
            200,
            "\u{feff}: keepalive\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"私密正文\"}\r\n\r\nevent: response.completed\r\ndata: {\"type\":\"response.completed\",\r\ndata: \"response\":{\"status\":\"completed\"}}\r\n\r\n",
            RequestStatus::Completed,
            None,
        ),
        (
            "failed",
            "text/event-stream",
            200,
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"private-output-secret\"}}}\n\n",
            RequestStatus::Failed,
            Some("upstream_response_failed"),
        ),
        (
            "incomplete",
            "text/event-stream",
            200,
            "data: {\"type\":\"response.incomplete\"}\n\n",
            RequestStatus::Failed,
            Some("upstream_response_incomplete"),
        ),
        (
            "error",
            "text/event-stream",
            200,
            "event: error\ndata: {\"message\":\"private-output-secret\"}\n\n",
            RequestStatus::Failed,
            Some("upstream_stream_error"),
        ),
        (
            "eof",
            "text/event-stream",
            200,
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"response.completed\"}\n\ndata: [DONE]\n\n",
            RequestStatus::Interrupted,
            Some("missing_completion"),
        ),
        (
            "json",
            "application/json",
            200,
            "{\"status\":\"completed\",\"output\":[]}",
            RequestStatus::Completed,
            None,
        ),
        (
            "json-failed",
            "application/json",
            200,
            "{\"status\":\"failed\",\"error\":{\"message\":\"private-output-secret\"}}",
            RequestStatus::Failed,
            Some("upstream_response_failed"),
        ),
        (
            "http-error",
            "application/json",
            429,
            "{\"error\":{\"message\":\"private-output-secret\"}}",
            RequestStatus::Failed,
            Some("upstream_http_error"),
        ),
    ];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = listener.local_addr().unwrap();
    let mock = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| async move {
            let case = cases
                .iter()
                .find(|case| body["input"]["case"] == case.0)
                .unwrap();
            let chunks = case
                .3
                .as_bytes()
                .chunks(7)
                .map(Bytes::copy_from_slice)
                .collect::<Vec<_>>();
            Response::builder()
                .status(case.2)
                .header(header::CONTENT_TYPE, case.1)
                .body(Body::from_stream(stream::iter(
                    chunks.into_iter().map(Ok::<_, io::Error>),
                )))
                .unwrap()
        }),
    );
    let upstream = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let records = RequestDatabase::new();
    let (running, address) = recording_router(upstream_address, &records).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for (index, case) in cases.iter().enumerate() {
        let response = request(&client, address, json!({"model":"sx-one", "input":{"case":case.0,"text":"private-prompt-secret"}, "stream":true}))
            .header(header::COOKIE, "private-cookie-secret")
            .send().await.unwrap();
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(response.status().as_u16(), case.2);
        assert_eq!(response.bytes().await.unwrap().as_ref(), case.3.as_bytes());
        let record = records.wait_for(&id).await;
        assert_eq!(record.status, case.4, "{}", case.0);
        assert_eq!(record.error_code.as_deref(), case.5);
        assert_eq!(record.public_model.as_deref(), Some("sx-one"));
        assert_eq!(record.provider_id.as_deref(), Some("one"));
        assert_eq!(record.upstream_model.as_deref(), Some("deepseek-flash"));
        assert_eq!(record.generation, "test-generation");
        assert_eq!(record.http_status, Some(case.2));
        assert!(record.headers_ms.unwrap() <= record.duration_ms);
        assert_eq!(
            record.first_event_ms.is_some(),
            case.1.starts_with("text/event-stream")
        );
        assert_eq!(records.store().requests(100).unwrap().len(), index + 1);
    }
    let rejected = request(&client, address, json!({"model":"sx-unknown"}))
        .send()
        .await
        .unwrap();
    let id = rejected.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(rejected.status(), StatusCode::NOT_FOUND);
    let record = records.wait_for(&id).await;
    assert_eq!(record.error_code.as_deref(), Some("unknown_model"));
    assert!(record.provider_id.is_none());
    assert!(record.http_status.is_none());
    running.stop().await;
    let bytes = std::fs::read(&records.0).unwrap();
    let database = String::from_utf8_lossy(&bytes);
    for secret in [
        "private-prompt-secret",
        "private-output-secret",
        "private-cookie-secret",
        "synthetic-upstream-key",
        "synthetic-local-token",
        "私密正文",
    ] {
        assert!(
            !database.contains(secret),
            "metadata must not include {secret}"
        );
    }
    upstream.abort();
}

const RECORD_TOKEN: &str = "synthetic-local-token-for-request-records-12345";

const OFFICIAL_SSE: &str = "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"name\":\"exec_command\",\"call_id\":\"fixture-call\",\"arguments\":\"{}\"}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-response\",\"status\":\"completed\"}}\n\n";

async fn official_mock(
    State(captured): State<mpsc::Sender<(HeaderMap, Value, String)>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    captured
        .send((headers, body.clone(), uri.path().into()))
        .await
        .unwrap();
    match body["input"].as_str() {
        Some("expire") => (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"synthetic-expired-body"}}))).into_response(),
        Some("denied") => (StatusCode::FORBIDDEN, Json(json!({"error":{"message":"synthetic-denied-body"}}))).into_response(),
        _ if uri.path().ends_with("/compact") => Json(json!({"object":"response.compaction","output":[{"type":"compaction","encrypted_content":"synthetic-compact-content"}]})).into_response(),
        _ => Response::builder().header(header::CONTENT_TYPE, "text/event-stream")
            .header("x-codex-turn-state", "synthetic-turn-state")
            .body(Body::from(OFFICIAL_SSE)).unwrap(),
    }
}

#[tokio::test]
async fn subscription_forwarding_refresh_failure_workspace_pin_and_api_switch_are_isolated() {
    let mock_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_address = mock_listener.local_addr().unwrap();
    let (captured, mut official_seen) = mpsc::channel(16);
    let mock = Router::new()
        .route("/responses", post(official_mock))
        .route("/responses/compact", post(official_mock))
        .with_state(captured);
    let official_task =
        tokio::spawn(async move { axum::serve(mock_listener, mock).await.unwrap() });
    let (api_address, mut api_seen, _, api_task) = spawn_mock("/responses", false).await;
    let templates = serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-account",
                display_name: "Account",
                provider_id: "official",
                upstream_model: "gpt-5.5",
            },
            Selection {
                public_id: "sx-api",
                display_name: "API",
                provider_id: "api",
                upstream_model: "gpt-5.5",
            },
        ],
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let records = RequestDatabase::new();
    let state = RouterState::new(
        address,
        RECORD_TOKEN.into(),
        publication,
        HashMap::from([
            (
                "official".into(),
                Upstream::chatgpt_mock(mock_address).unwrap(),
            ),
            (
                "api".into(),
                Upstream::new(
                    &format!("http://{api_address}"),
                    "synthetic-api-only-key".into(),
                )
                .unwrap(),
            ),
        ]),
    )
    .unwrap()
    .with_request_log(records.store(), "subscription-fixture".into());
    let running = RunningRouter::start(listener, state).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{address}/v1/responses");
    let authorized = |model: &str, input: Value, bearer: &str, workspace: &str| {
        client
            .post(&url)
            .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
            .bearer_auth(bearer)
            .header("chatgpt-account-id", workspace)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":model,"input":input}).to_string())
    };

    let missing_local = request(&client, address, json!({"model":"sx-account"}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_local.status(), StatusCode::UNAUTHORIZED);
    let invalid = authorized(
        "sx-account",
        json!("invalid"),
        "PROXY_MANAGED",
        "fixture-workspace",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    assert!(official_seen.try_recv().is_err());

    for (input, status, code) in [
        ("expire", StatusCode::UNAUTHORIZED, "chatgpt_unauthorized"),
        ("denied", StatusCode::FORBIDDEN, "chatgpt_forbidden"),
    ] {
        let response = authorized(
            "sx-account",
            json!(input),
            "synthetic-native-old-access",
            "fixture-workspace",
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), status);
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let expected = if input == "expire" {
            "synthetic-expired-body"
        } else {
            "synthetic-denied-body"
        };
        assert!(response.text().await.unwrap().contains(expected));
        assert_eq!(
            records.wait_for(&id).await.error_code.as_deref(),
            Some(code)
        );
        assert!(running.chatgpt_error().is_some());
        let (headers, body, _) = official_seen.recv().await.unwrap();
        assert_eq!(
            headers[header::AUTHORIZATION],
            "Bearer synthetic-native-old-access"
        );
        assert_eq!(body["model"], "gpt-5.5");
        assert!(api_seen.try_recv().is_err());
    }

    let input = json!([
        {"type":"reasoning","encrypted_content":"synthetic-encrypted-reasoning"},
        {"type":"function_call_output","call_id":"fixture-call","output":"synthetic-tool-result"},
    ]);
    let body = json!({"model":"sx-account","input":input,"tools":[{"type":"function","name":"exec_command","parameters":{"type":"object"}}],"stream":true});
    let compressed = zstd::stream::encode_all(body.to_string().as_bytes(), 1).unwrap();
    let response = client
        .post(&url)
        .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
        .bearer_auth("synthetic-native-renewed-access")
        .header("chatgpt-account-id", "fixture-workspace")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "zstd")
        .header("session_id", "fixture-session")
        .header("x-codex-turn-state", "synthetic-prior-turn")
        .header("originator", "codex_cli_rs")
        .header("x-openai-account-routing-override", "NO_CONSTRAINT")
        .header("x-codex-routing-hint", "synthetic-routing-hint")
        .header(header::COOKIE, "synthetic-cookie-secret")
        .header(header::PROXY_AUTHORIZATION, "synthetic-proxy-secret")
        .header("x-arbitrary-secret", "synthetic-header-secret")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["x-codex-turn-state"],
        "synthetic-turn-state"
    );
    let id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.text().await.unwrap(), OFFICIAL_SSE);
    assert_eq!(records.wait_for(&id).await.status, RequestStatus::Completed);
    assert!(running.chatgpt_error().is_none());
    let (headers, forwarded, path) = official_seen.recv().await.unwrap();
    let mut expected = body;
    expected["model"] = "gpt-5.5".into();
    assert_eq!(forwarded, expected);
    assert_eq!(path, "/responses");
    assert_eq!(
        headers[header::AUTHORIZATION],
        "Bearer synthetic-native-renewed-access"
    );
    assert_eq!(headers["chatgpt-account-id"], "fixture-workspace");
    assert_eq!(headers["session_id"], "fixture-session");
    assert_eq!(headers["x-codex-turn-state"], "synthetic-prior-turn");
    assert_eq!(
        headers["x-openai-account-routing-override"],
        "NO_CONSTRAINT"
    );
    assert_eq!(headers["x-codex-routing-hint"], "synthetic-routing-hint");
    for name in [
        LOCAL_TOKEN_HEADER,
        "cookie",
        "proxy-authorization",
        "x-arbitrary-secret",
        "content-encoding",
    ] {
        assert!(!headers.contains_key(name));
    }

    let response = authorized(
        "sx-account",
        json!("changed-account"),
        "synthetic-native-other-access",
        "other-workspace",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        serde_json::from_slice::<Value>(&response.bytes().await.unwrap()).unwrap()["error"]["code"],
        "chatgpt_account_changed"
    );
    assert!(official_seen.try_recv().is_err());
    assert!(running.chatgpt_error().unwrap().contains("变化"));

    let response = authorized(
        "sx-api",
        json!("new-api-session"),
        "synthetic-native-renewed-access",
        "fixture-workspace",
    )
    .header("session_id", "fixture-session")
    .header("x-openai-account-routing-override", "NO_CONSTRAINT")
    .header("x-codex-routing-hint", "synthetic-routing-hint")
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let captured = api_seen.recv().await.unwrap();
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer synthetic-api-only-key")
    );
    assert!(
        captured.chatgpt_account.is_none()
            && captured.account.is_none()
            && captured.local_token.is_none()
            && captured.routing_override.is_none()
            && captured.routing_hint.is_none()
    );
    let response = authorized(
        "sx-api",
        input,
        "synthetic-native-renewed-access",
        "fixture-workspace",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(api_seen.try_recv().is_err());

    let response = client
        .post(format!("{url}/compact"))
        .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
        .bearer_auth("synthetic-native-renewed-access")
        .header("chatgpt-account-id", "fixture-workspace")
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"model":"sx-account","input":[]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        serde_json::from_slice::<Value>(&response.bytes().await.unwrap()).unwrap()["object"],
        "response.compaction"
    );
    assert_eq!(records.wait_for(&id).await.status, RequestStatus::Completed);
    let (_, body, path) = official_seen.recv().await.unwrap();
    assert_eq!(path, "/responses/compact");
    assert_eq!(body["model"], "gpt-5.5");
    let response = client
        .post(format!("{url}/compact"))
        .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"model":"sx-api","input":[]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(api_seen.try_recv().is_err());

    running.stop().await;
    let database = String::from_utf8_lossy(&std::fs::read(&records.0).unwrap()).into_owned();
    for secret in [
        "synthetic-native-old-access",
        "synthetic-native-renewed-access",
        "fixture-workspace",
        "synthetic-encrypted-reasoning",
        "synthetic-cookie-secret",
        "synthetic-compact-content",
        RECORD_TOKEN,
    ] {
        assert!(!database.contains(secret));
    }
    official_task.abort();
    api_task.abort();
}

fn request(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    body: Value,
) -> reqwest::RequestBuilder {
    client
        .post(format!("http://{address}/v1/responses"))
        .bearer_auth(RECORD_TOKEN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
}

async fn recording_router(
    upstream_address: std::net::SocketAddr,
    records: &RequestDatabase,
) -> (RunningRouter, std::net::SocketAddr) {
    recording_router_with_fallback(upstream_address, None, records).await
}

async fn recording_router_with_fallback(
    upstream_address: std::net::SocketAddr,
    fallback: Option<std::net::SocketAddr>,
    records: &RequestDatabase,
) -> (RunningRouter, std::net::SocketAddr) {
    let templates = serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let mut publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-one",
            display_name: "One",
            provider_id: "one",
            upstream_model: "deepseek-flash",
        }],
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut upstreams = HashMap::from([(
        "one".into(),
        Upstream::new(
            &format!("http://{upstream_address}"),
            "synthetic-upstream-key".into(),
        )
        .unwrap(),
    )]);
    if let Some(fallback) = fallback {
        publication
            .routes
            .get_mut("sx-one")
            .unwrap()
            .fallback_provider_id = Some("backup".into());
        upstreams.insert(
            "backup".into(),
            Upstream::new(&format!("http://{fallback}"), "synthetic-backup-key".into()).unwrap(),
        );
    }
    let state = RouterState::new(address, RECORD_TOKEN.into(), publication, upstreams)
        .unwrap()
        .with_request_log(records.store(), "test-generation".into());
    (RunningRouter::start(listener, state).unwrap(), address)
}

#[tokio::test]
async fn explicit_fallback_only_after_connect_failure_uses_own_key_and_records_destination() {
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = unused.local_addr().unwrap();
    drop(unused);
    let (backup, mut seen, _, task) = spawn_mock("/responses", false).await;
    let records = RequestDatabase::new();
    let (running, address) = recording_router_with_fallback(primary, Some(backup), &records).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    // State-bound requests are rejected before considering any candidate.
    let rejected = request(
        &client,
        address,
        json!({"model":"sx-one", "previous_response_id":"private"}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = request(
        &client,
        address,
        json!({"model":"sx-one", "input":"private-fallback-prompt"}),
    )
    .header(header::AUTHORIZATION, "Bearer private-client-key")
    .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
    .header(header::COOKIE, "private-cookie")
    .header("chatgpt-account-id", "private-account")
    .header("x-openai-account-id", "private-account")
    .send()
    .await
    .unwrap();
    let id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("completed"));
    let captured = timeout(Duration::from_secs(3), seen.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(captured.model, "deepseek-flash");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer synthetic-backup-key")
    );
    assert!(
        captured.cookie.is_none()
            && captured.account.is_none()
            && captured.chatgpt_account.is_none()
            && captured.local_token.is_none()
    );
    let record = records.wait_for(&id).await;
    assert_eq!(record.status, RequestStatus::Completed);
    assert_eq!(record.provider_id.as_deref(), Some("backup"));
    assert_eq!(record.fallback_from.as_deref(), Some("one"));
    assert!(seen.try_recv().is_err());
    running.stop().await;
    task.abort();
    let _ = task.await;
    // Exhaustion still means one primary and one backup, with no recursive fallback.
    let (running, address) = recording_router_with_fallback(primary, Some(backup), &records).await;
    let response = request(&client, address, json!({"model":"sx-one"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let record = records
        .wait_for(response.headers()[REQUEST_ID_HEADER].to_str().unwrap())
        .await;
    assert_eq!(record.provider_id.as_deref(), Some("backup"));
    assert_eq!(record.fallback_from.as_deref(), Some("one"));
    assert_eq!(record.status, RequestStatus::Failed);
    assert_eq!(record.error_code.as_deref(), Some("no_eligible_upstream"));
    running.stop().await;
    let database = String::from_utf8_lossy(&std::fs::read(&records.0).unwrap()).into_owned();
    for secret in [
        "synthetic-backup-key",
        "private-client-key",
        "private-cookie",
        "private-account",
        "private-fallback-prompt",
    ] {
        assert!(!database.contains(secret));
    }
}

#[tokio::test]
async fn fallback_never_replays_http_errors_partial_streams_or_ambiguous_sent_requests() {
    let (backup, mut seen, _, task) = spawn_mock("/responses", false).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = listener.local_addr().unwrap();
    let mock = Router::new().route(
        "/responses",
        post(|Json(body): Json<Value>| async move {
            let status = body["input"].as_u64().unwrap() as u16;
            if status == 200 {
                return Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from("data: {\"type\":\"response.created\"}\n\n"))
                    .unwrap();
            }
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::RETRY_AFTER, "37")
                .body(Body::from("{\"error\":{\"message\":\"private-error\"}}"))
                .unwrap()
        }),
    );
    let primary_task = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let records = RequestDatabase::new();
    let (running, address) = recording_router_with_fallback(primary, Some(backup), &records).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for status in [400, 401, 402, 403, 429, 503, 200] {
        let response = request(&client, address, json!({"model":"sx-one", "input":status}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        if status == 429 {
            assert_eq!(response.headers()[header::RETRY_AFTER], "37");
        }
        let id = response.headers()[REQUEST_ID_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        response.bytes().await.unwrap();
        let record = records.wait_for(&id).await;
        assert_eq!(record.provider_id.as_deref(), Some("one"));
        assert!(record.fallback_from.is_none());
        assert!(
            seen.try_recv().is_err(),
            "status {status} must never cause fallback"
        );
    }
    running.stop().await;
    primary_task.abort();
    // Accept real request bytes, then disconnect before responding. It may have
    // been executed upstream, so lack of response headers is not permission to retry.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary = listener.local_addr().unwrap();
    let sent = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut bytes = [0_u8; 4096];
        loop {
            socket.readable().await.unwrap();
            match socket.try_read(&mut bytes) {
                Ok(count) => {
                    assert!(count > 0);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("{error}"),
            }
        }
    });
    let (running, address) = recording_router_with_fallback(primary, Some(backup), &records).await;
    let response = timeout(
        Duration::from_secs(3),
        request(&client, address, json!({"model":"sx-one"})).send(),
    )
    .await
    .unwrap()
    .unwrap();
    sent.await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let record = records
        .wait_for(response.headers()[REQUEST_ID_HEADER].to_str().unwrap())
        .await;
    assert_eq!(record.provider_id.as_deref(), Some("one"));
    assert!(record.fallback_from.is_none());
    assert!(seen.try_recv().is_err());
    running.stop().await;
    task.abort();
}

#[tokio::test]
async fn transport_errors_and_cancellation_before_headers_have_distinct_records() {
    let entered = Arc::new(Notify::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = listener.local_addr().unwrap();
    let mock = Router::new().route(
        "/responses",
        post({
            let entered = entered.clone();
            move |Json(body): Json<Value>| {
                let entered = entered.clone();
                async move {
                    if body["input"] == "wait-for-cancel" {
                        entered.notify_one();
                        std::future::pending::<()>().await;
                    }
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(stream::unfold(0, |step| async move {
                            if step == 0 {
                                Some((
                                    Ok::<_, io::Error>(Bytes::from_static(
                                        b"data: {\"type\":\"response.created\"}\n\n",
                                    )),
                                    1,
                                ))
                            } else {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                                Some((Err(io::Error::other("synthetic stream failure")), 2))
                            }
                        })))
                        .unwrap()
                }
            }
        }),
    );
    let upstream = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let records = RequestDatabase::new();
    let (running, address) = recording_router(upstream_address, &records).await;
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = request(
        &client,
        address,
        json!({"model":"sx-one","input":"read-error"}),
    )
    .send()
    .await
    .unwrap();
    let id = response.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(response.bytes().await.is_err());
    let record = records.wait_for(&id).await;
    assert_eq!(record.status, RequestStatus::Interrupted);
    assert_eq!(record.error_code.as_deref(), Some("upstream_read_error"));
    assert!(record.first_event_ms.is_some());

    let pending = request(
        &client,
        address,
        json!({"model":"sx-one","input":"wait-for-cancel"}),
    );
    let pending = tokio::spawn(async move { pending.send().await });
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    pending.abort();
    let _ = pending.await;
    let cancelled = timeout(Duration::from_secs(3), async {
        loop {
            let rows = records.store().requests(10).unwrap();
            if rows.len() == 2 {
                break rows[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect before headers must also be recorded");
    assert_eq!(cancelled.status, RequestStatus::Cancelled);
    assert!(cancelled.http_status.is_none());
    assert!(cancelled.first_event_ms.is_none());
    running.stop().await;
    upstream.abort();

    // A closed loopback port gives a deterministic connection refusal, not a live request.
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable = unused.local_addr().unwrap();
    drop(unused);
    let (running, address) = recording_router(unavailable, &records).await;
    let response = request(&client, address, json!({"model":"sx-one"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let record = records
        .wait_for(response.headers()[REQUEST_ID_HEADER].to_str().unwrap())
        .await;
    assert_eq!(record.status, RequestStatus::Failed);
    assert_eq!(record.error_code.as_deref(), Some("upstream_unavailable"));
    assert!(record.http_status.is_none());
    running.stop().await;
}

#[derive(Debug)]
struct Captured {
    authorization: Option<String>,
    cookie: Option<String>,
    account: Option<String>,
    chatgpt_account: Option<String>,
    local_token: Option<String>,
    routing_override: Option<String>,
    routing_hint: Option<String>,
    model: String,
}

#[derive(Clone)]
struct MockState {
    captured: mpsc::Sender<Captured>,
    release: Arc<Notify>,
    streaming: bool,
}

async fn mock_upstream(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let model = body["model"].as_str().unwrap().to_owned();
    state
        .captured
        .send(Captured {
            authorization: headers
                .get(header::AUTHORIZATION)
                .map(|v| v.to_str().unwrap().to_owned()),
            cookie: headers
                .get(header::COOKIE)
                .map(|v| v.to_str().unwrap().to_owned()),
            account: headers
                .get("x-openai-account-id")
                .map(|v| v.to_str().unwrap().to_owned()),
            chatgpt_account: headers
                .get("chatgpt-account-id")
                .map(|v| v.to_str().unwrap().to_owned()),
            local_token: headers
                .get(LOCAL_TOKEN_HEADER)
                .map(|v| v.to_str().unwrap().to_owned()),
            routing_override: headers
                .get("x-openai-account-routing-override")
                .map(|v| v.to_str().unwrap().to_owned()),
            routing_hint: headers
                .get("x-codex-routing-hint")
                .map(|v| v.to_str().unwrap().to_owned()),
            model: model.clone(),
        })
        .await
        .unwrap();
    if !state.streaming {
        return Json(json!({ "model": model, "status": "completed", "output": [] }))
            .into_response();
    }
    let body = stream::unfold((0, state.release), |(step, release)| async move {
        match step {
            0 => Some((
                Ok::<_, io::Error>(Bytes::from_static(b"event: response.created\ndata: {}\n\n")),
                (1, release),
            )),
            1 => {
                release.notified().await;
                Some((
                    Ok(Bytes::from_static(
                        b"event: response.completed\ndata: {\"response\":{\"status\":\"completed\"}}\n\n",
                    )),
                    (2, release),
                ))
            }
            _ => None,
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(body))
        .unwrap()
}

async fn spawn_mock(
    path: &'static str,
    streaming: bool,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<Captured>,
    Arc<Notify>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel(2);
    let release = Arc::new(Notify::new());
    let app = Router::new()
        .route(path, post(mock_upstream))
        .with_state(MockState {
            captured: tx,
            release: release.clone(),
            streaming,
        });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, rx, release, task)
}

#[tokio::test]
async fn exact_routes_stream_and_keep_credentials_isolated() {
    assert!(Upstream::new("http://example.com/v1", "key".into()).is_err());
    assert!(Upstream::new("https://user:secret@example.com/v1", "key".into()).is_err());

    let (deepseek_addr, mut deepseek_rx, release, deepseek_task) =
        spawn_mock("/responses", true).await;
    let (openai_addr, mut openai_rx, _, openai_task) = spawn_mock("/v1/responses", false).await;
    let templates = serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-ds-flash",
                display_name: "DeepSeek · Flash",
                provider_id: "deepseek",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "sx-oai-coding",
                display_name: "OpenAI · Coding",
                provider_id: "openai-api",
                upstream_model: "gpt-5.5",
            },
        ],
    )
    .unwrap();
    let providers = HashMap::from([
        (
            "deepseek".into(),
            Upstream::new(
                &format!("http://{deepseek_addr}/"),
                "deepseek-only-key".into(),
            )
            .unwrap(),
        ),
        (
            "openai-api".into(),
            Upstream::new(
                &format!("http://{openai_addr}/v1"),
                "openai-only-key".into(),
            )
            .unwrap(),
        ),
    ]);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = "local-token-only-for-switchx-123456789";
    let state = RouterState::new(address, token.into(), publication, providers).unwrap();
    let router_task = tokio::spawn(async move { serve(listener, state).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{address}");

    let models = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let models: Value = serde_json::from_slice(&models.bytes().await.unwrap()).unwrap();
    assert_eq!(models["data"][0]["id"], "sx-ds-flash");
    assert_eq!(models["data"][1]["id"], "sx-oai-coding");

    let response = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::COOKIE, "private=do-not-forward")
        .header("x-openai-account-id", "do-not-forward")
        .body(json!({ "model": "sx-ds-flash", "input": "hello", "stream": true }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut response = response;
    let first = timeout(Duration::from_secs(3), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("response.created"));
    let captured = deepseek_rx.recv().await.unwrap();
    assert_eq!(captured.model, "deepseek-flash");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer deepseek-only-key")
    );
    assert_eq!(captured.cookie, None);
    assert_eq!(captured.account, None);
    assert_eq!(captured.chatgpt_account, None);
    assert_eq!(captured.local_token, None);
    assert!(openai_rx.try_recv().is_err());
    release.notify_one();
    let second = timeout(Duration::from_secs(3), response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&second).contains("response.completed"));

    let official_auth = "synthetic-chatgpt-access-token";
    let with_official_auth = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(official_auth)
        .header(LOCAL_TOKEN_HEADER, token)
        .header("x-openai-account-id", "synthetic-chatgpt-account")
        .header("chatgpt-account-id", "synthetic-chatgpt-account")
        .header(header::COOKIE, "private=do-not-forward")
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "model": "sx-ds-flash", "input": "hello" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(with_official_auth.status(), StatusCode::OK);
    let captured = deepseek_rx.recv().await.unwrap();
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer deepseek-only-key")
    );
    assert_eq!(captured.account, None);
    assert_eq!(captured.chatgpt_account, None);
    assert_eq!(captured.cookie, None);
    assert_eq!(captured.local_token, None);
    assert!(openai_rx.try_recv().is_err());

    let compressed = zstd::stream::encode_all(
        json!({ "model": "sx-ds-flash", "input": "compressed" })
            .to_string()
            .as_bytes(),
        0,
    )
    .unwrap();
    let response = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(official_auth)
        .header(LOCAL_TOKEN_HEADER, token)
        .header("chatgpt-account-id", "synthetic-chatgpt-account")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured = deepseek_rx.recv().await.unwrap();
    assert_eq!(captured.model, "deepseek-flash");
    assert_eq!(captured.chatgpt_account, None);
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer deepseek-only-key")
    );

    let oversized = zstd::stream::encode_all(
        json!({ "model": "sx-ds-flash", "input": "x".repeat(2 * 1024 * 1024) })
            .to_string()
            .as_bytes(),
        0,
    )
    .unwrap();
    let oversized = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "zstd")
        .body(oversized)
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let unsupported = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "gzip")
        .body("ignored")
        .send()
        .await
        .unwrap();
    assert_eq!(unsupported.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let invalid = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CONTENT_ENCODING, "zstd")
        .body("not zstd")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(deepseek_rx.try_recv().is_err());

    let missing_local_header = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(official_auth)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "model": "sx-ds-flash", "input": "hello" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(missing_local_header.status(), StatusCode::UNAUTHORIZED);
    let wrong_local_header = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(LOCAL_TOKEN_HEADER, "wrong")
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "model": "sx-ds-flash", "input": "hello" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_local_header.status(), StatusCode::UNAUTHORIZED);
    assert!(deepseek_rx.try_recv().is_err());

    let response = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "model": "sx-oai-coding", "input": "hello" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(body["model"], "gpt-5.5");
    let captured = openai_rx.recv().await.unwrap();
    assert_eq!(captured.model, "gpt-5.5");
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer openai-only-key")
    );

    let unknown = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({ "model": "sx-unknown", "input": "hello" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(unknown.text().await.unwrap().contains("unknown_model"));
    assert!(deepseek_rx.try_recv().is_err());
    assert!(openai_rx.try_recv().is_err());

    let unauthorized = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let origin = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(token)
        .header(header::ORIGIN, "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(origin.status(), StatusCode::FORBIDDEN);
    let host = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(token)
        .header(header::HOST, "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(host.status(), StatusCode::FORBIDDEN);
    let continuation = client
        .post(format!("{base}/v1/responses"))
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            json!({ "model": "sx-ds-flash", "previous_response_id": "other-provider" }).to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(continuation.status(), StatusCode::UNPROCESSABLE_ENTITY);
    for input in [
        json!([{"type":"reasoning", "encrypted_content":"private-state"}]),
        json!([{"type":"compaction"}]),
    ] {
        let response = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":"sx-ds-flash", "input": input}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
    assert!(deepseek_rx.try_recv().is_err());

    router_task.abort();
    deepseek_task.abort();
    openai_task.abort();
}

#[tokio::test]
async fn disconnect_and_bounded_shutdown_cancel_upstream_without_replay() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Dropped(Arc<Notify>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }
    let closed = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = listener.local_addr().unwrap();
    let mock = Router::new().route(
        "/custom/responses",
        post({
            let closed = closed.clone();
            let calls = calls.clone();
            move || {
                let guard = Dropped(closed.clone());
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    let stream = stream::unfold((false, guard), |(sent, guard)| async move {
                        if sent {
                            std::future::pending::<()>().await;
                        }
                        Some((
                            Ok::<_, io::Error>(Bytes::from_static(
                                b"data: {\"type\":\"response.created\"}\n\n",
                            )),
                            (true, guard),
                        ))
                    });
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
            }
        }),
    );
    let upstream = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let (backup, mut backup_calls, _, backup_task) = spawn_mock("/responses", false).await;
    let templates = serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let mut publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-one",
            display_name: "One",
            provider_id: "one",
            upstream_model: "deepseek-flash",
        }],
    )
    .unwrap();
    publication
        .routes
        .get_mut("sx-one")
        .unwrap()
        .fallback_provider_id = Some("backup".into());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let token = "synthetic-shutdown-local-token-123456789";
    let records = RequestDatabase::new();
    let state = RouterState::new(
        address,
        token.into(),
        publication,
        HashMap::from([
            (
                "one".into(),
                Upstream::new(
                    &format!("http://{upstream_address}/custom"),
                    "synthetic-key".into(),
                )
                .unwrap(),
            ),
            (
                "backup".into(),
                Upstream::new(&format!("http://{backup}"), "backup-key".into()).unwrap(),
            ),
        ]),
    )
    .unwrap()
    .with_request_log(records.store(), "shutdown-generation".into());
    let running = RunningRouter::start(listener, state).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = || {
        client
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":"sx-one", "input":"synthetic", "stream":true}).to_string())
    };
    let mut first = request().send().await.unwrap();
    assert!(first.chunk().await.unwrap().is_some());
    let first_id = first.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    drop(first);
    timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("upstream should see client cancellation");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let cancelled = records.wait_for(&first_id).await;
    assert_eq!(cancelled.status, RequestStatus::Cancelled);
    assert_eq!(cancelled.error_code.as_deref(), Some("client_disconnected"));
    running.pause();
    assert_eq!(
        request().send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    running.resume();
    let mut second = request().send().await.unwrap();
    assert!(second.chunk().await.unwrap().is_some());
    let second_id = second.headers()[REQUEST_ID_HEADER]
        .to_str()
        .unwrap()
        .to_owned();
    timeout(Duration::from_secs(8), running.stop())
        .await
        .unwrap();
    timeout(Duration::from_secs(3), closed.notified())
        .await
        .expect("shutdown should cancel the upstream");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let ended = timeout(Duration::from_secs(3), second.chunk())
        .await
        .unwrap();
    assert!(ended.is_err() || ended.unwrap().is_none());
    let interrupted = records.wait_for(&second_id).await;
    assert_eq!(interrupted.status, RequestStatus::Interrupted);
    assert_eq!(interrupted.error_code.as_deref(), Some("router_stopping"));
    assert_eq!(records.store().requests(10).unwrap().len(), 2);
    assert!(
        records
            .store()
            .requests(10)
            .unwrap()
            .iter()
            .all(|record| record.fallback_from.is_none())
    );
    assert!(backup_calls.try_recv().is_err());
    let rebound = TcpListener::bind(address).await.unwrap();
    drop(rebound);
    upstream.abort();
    backup_task.abort();
}
