use std::{collections::HashMap, io, sync::Arc, time::Duration};

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
    routing::{LOCAL_TOKEN_HEADER, RouterState, Upstream, serve},
};
use tokio::{
    net::TcpListener,
    sync::{Notify, mpsc},
    time::timeout,
};

#[derive(Debug)]
struct Captured {
    authorization: Option<String>,
    cookie: Option<String>,
    account: Option<String>,
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
            model: model.clone(),
        })
        .await
        .unwrap();
    if !state.streaming {
        return Json(json!({ "model": model, "output": [] })).into_response();
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
                        b"event: response.completed\ndata: {}\n\n",
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
    assert_eq!(captured.cookie, None);
    assert!(openai_rx.try_recv().is_err());

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

    router_task.abort();
    deepseek_task.abort();
    openai_task.abort();
}
