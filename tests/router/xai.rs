use super::*;
use axum::routing::get;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use switchx::{app, xai};

struct Fixture {
    directory: std::path::PathBuf,
    manager: xai::AccountManager,
    account: String,
    jwt: String,
    upstream: std::net::SocketAddr,
    seen: mpsc::Receiver<(HeaderMap, Value)>,
    reject: Arc<std::sync::atomic::AtomicU16>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
#[derive(Clone)]
struct Mock {
    origin: String,
    jwt: String,
    seen: mpsc::Sender<(HeaderMap, Value)>,
    reject: Arc<std::sync::atomic::AtomicU16>,
}
async fn discovery(State(mock): State<Mock>) -> Json<Value> {
    Json(
        json!({"issuer":mock.origin,"token_endpoint":format!("{}/token",mock.origin),"device_authorization_endpoint":format!("{}/device",mock.origin)}),
    )
}
async fn token(State(mock): State<Mock>) -> Json<Value> {
    Json(json!({"access_token":mock.jwt,"expires_in":3600}))
}
async fn responses(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.seen.send((headers, body.clone())).await.unwrap();
    let status = mock.reject.load(std::sync::atomic::Ordering::SeqCst);
    if status != 0 {
        return (
            StatusCode::from_u16(status).unwrap(),
            [(header::RETRY_AFTER, "15")],
            format!("private upstream token {}", mock.jwt),
        )
            .into_response();
    }
    let call = json!({"type":"function_call","id":"fc1","name":"functions__exec_command","call_id":"call1","arguments":"{\"cmd\":\"echo ok\"}"});
    let output = json!([{"type":"reasoning","id":"reason1","summary":[],"encrypted_content":"fixture-encrypted-reasoning"},call]);
    if body["stream"] == true {
        let done = json!({"type":"response.output_item.done","item":call});
        let complete = json!({"type":"response.completed","response":{"id":"r1","status":"completed","output":output}});
        let bytes = format!(
            "event: response.output_item.done\r\ndata: {done}\r\n\r\nevent: response.completed\ndata: {complete}\n\n"
        );
        let chunks: Vec<Result<Bytes, io::Error>> = bytes
            .as_bytes()
            .chunks(7)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from_stream(stream::iter(chunks)),
        )
            .into_response()
    } else {
        Json(json!({"id":"r2","status":"completed","output":output})).into_response()
    }
}
impl Fixture {
    async fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("switchx-xai-route-{}", app::new_id().unwrap()));
        std::fs::create_dir(&directory).unwrap();
        let account = app::new_id().unwrap();
        let jwt = format!(
            "e30.{}.sig",
            URL_SAFE_NO_PAD.encode(br#"{"sub":"fixture-user"}"#)
        );
        let file = directory.join("xai_oauth_auth.json");
        std::fs::write(&file,json!({"version":1,"accounts":{&account:{"id":account,"subject":"fixture-user","label":"Grok fixture","refresh_token":"fixture-refresh","requires_reauth":false}},"default_account_id":account}).to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream = listener.local_addr().unwrap();
        let origin = format!("http://{upstream}");
        let (sender, seen) = mpsc::channel(8);
        let reject = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let mock = Mock {
            origin: origin.clone(),
            jwt: jwt.clone(),
            seen: sender,
            reject: reject.clone(),
        };
        let router = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/token", post(token))
            .route("/responses", post(responses))
            .with_state(mock);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let manager = xai::AccountManager::open_mock(&directory, &origin).unwrap();
        Self {
            directory,
            manager,
            account,
            jwt,
            upstream,
            seen,
            reject,
            task,
        }
    }
    async fn router(&self) -> (RunningRouter, String) {
        let template: Value =
            serde_json::from_str(include_str!("../fixtures/synthetic-models.json")).unwrap();
        let publication = publish(
            &template,
            &[
                Selection {
                    public_id: "sx-grok",
                    display_name: "Grok",
                    provider_id: "grok",
                    upstream_model: "gpt-5.5",
                },
                Selection {
                    public_id: "sx-grok-other",
                    display_name: "Grok other",
                    provider_id: "grok",
                    upstream_model: "deepseek-flash",
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
        let mut publication = publication;
        publication
            .routes
            .get_mut("sx-grok")
            .unwrap()
            .upstream_model = "grok-4.5".into();
        publication
            .routes
            .get_mut("sx-grok-other")
            .unwrap()
            .upstream_model = "grok-4.7".into();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstreams = HashMap::from([
            (
                "grok".into(),
                Upstream::xai_mock(self.upstream, self.manager.clone(), self.account.clone())
                    .unwrap(),
            ),
            (
                "api".into(),
                Upstream::new(
                    &format!("http://{}", self.upstream),
                    "fixture-api-key".into(),
                )
                .unwrap(),
            ),
        ]);
        let state = RouterState::new(address, RECORD_TOKEN.into(), publication, upstreams)
            .unwrap()
            .with_session_store(Store::open(&self.directory.join("switchx.sqlite")).unwrap());
        (
            RunningRouter::start(listener, state).unwrap(),
            format!("http://{address}"),
        )
    }
}
fn body(stream: bool) -> Value {
    json!({"model":"sx-grok","stream":stream,"input":[{"role":"user","content":"hello"}],"prompt_cache_retention":"24h","tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"exec_command","parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}}]}]})
}
async fn send(
    client: &reqwest::Client,
    base: &str,
    session: &str,
    request: &Value,
) -> reqwest::Response {
    client
        .post(format!("{base}/v1/responses"))
        .bearer_auth("client-openai-credential")
        .header(LOCAL_TOKEN_HEADER, RECORD_TOKEN)
        .header("chatgpt-account-id", "unrelated-openai-account")
        .header("x-openai-account-routing-override", "us")
        .header("session-id", session)
        .header(header::CONTENT_TYPE, "application/json")
        .body(request.to_string())
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn xai_oauth_streaming_tools_and_encrypted_replay_stay_in_their_account_and_model() {
    let mut fixture = Fixture::new().await;
    let (router, base) = fixture.router().await;
    let client = reqwest::Client::new();
    let session = "12345678-1234-1234-1234-123456789012";
    let response = send(&client, &base, session, &body(true)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let output = response.text().await.unwrap();
    assert!(output.contains("\"namespace\":\"functions\""));
    assert!(!output.contains("functions__exec_command"));
    assert!(output.contains("fixture-encrypted-reasoning"));
    let (headers, request) = fixture.seen.recv().await.unwrap();
    assert_eq!(
        headers[header::AUTHORIZATION],
        format!("Bearer {}", fixture.jwt)
    );
    for name in [
        LOCAL_TOKEN_HEADER,
        "chatgpt-account-id",
        "x-openai-account-routing-override",
        "cookie",
    ] {
        assert!(!headers.contains_key(name));
    }
    assert_eq!(request["model"], "grok-4.5");
    assert_eq!(request["tools"][0]["name"], "functions__exec_command");
    assert!(request.get("prompt_cache_retention").is_none());
    let mut replay = body(false);
    replay["input"] = json!([{"type":"reasoning","encrypted_content":"fixture-encrypted-reasoning"},{"type":"function_call","namespace":"functions","name":"exec_command","call_id":"call1","arguments":"{}"},{"type":"function_call_output","call_id":"call1","output":"ok"}]);
    let response = send(&client, &base, session, &replay).await;
    assert_eq!(response.status(), StatusCode::OK);
    let output: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(output["output"][1]["namespace"], "functions");
    let (_, request) = fixture.seen.recv().await.unwrap();
    assert_eq!(request["input"][1]["name"], "functions__exec_command");
    assert_eq!(
        send(
            &client,
            &base,
            "22345678-1234-1234-1234-123456789012",
            &replay
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    replay["model"] = "sx-grok-other".into();
    assert_eq!(
        send(&client, &base, session, &replay).await.status(),
        StatusCode::CONFLICT
    );
    replay["model"] = "sx-api".into();
    assert_eq!(
        send(&client, &base, session, &replay).await.status(),
        StatusCode::CONFLICT
    );
    assert!(fixture.seen.try_recv().is_err());
    router.stop().await;
}
#[tokio::test]
async fn removing_a_bound_xai_account_stops_requests_without_using_client_auth() {
    let mut fixture = Fixture::new().await;
    let (router, base) = fixture.router().await;
    fixture.manager.remove(&fixture.account).await.unwrap();
    let response = send(
        &reqwest::Client::new(),
        &base,
        "32345678-1234-1234-1234-123456789012",
        &body(false),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(fixture.seen.try_recv().is_err());
    assert!(!response.text().await.unwrap().contains(&fixture.jwt));
    router.stop().await;
}

#[tokio::test]
async fn xai_http_errors_are_redacted_preserve_retry_after_and_never_replay_inference() {
    let mut fixture = Fixture::new().await;
    let (router, base) = fixture.router().await;
    let client = reqwest::Client::new();
    for status in [401, 403, 429] {
        fixture
            .reject
            .store(status, std::sync::atomic::Ordering::SeqCst);
        let response = send(
            &client,
            &base,
            "42345678-1234-1234-1234-123456789012",
            &body(false),
        )
        .await;
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()[header::RETRY_AFTER], "15");
        let text = response.text().await.unwrap();
        assert!(!text.contains(&fixture.jwt));
        assert!(!text.contains("private upstream token"));
        fixture.seen.recv().await.unwrap();
        assert!(fixture.seen.try_recv().is_err());
    }
    router.stop().await;
}
