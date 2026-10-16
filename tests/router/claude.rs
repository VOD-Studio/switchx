use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use switchx::{
    app,
    claude::{
        self, AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy,
        publication::{Mode, Options},
        route_session::Sessions,
    },
    clients::{ClaudeCapabilities, ClientWorkspace},
    credentials::Secret,
    storage::{RequestStatus, Store, UsageClient, UsageLogFile, UsageRecord, UsageSource},
    usage::TokenUsage,
};
use tokio::sync::Notify;

type SeenRequest = (HeaderMap, String, Value);
#[derive(Clone)]
struct Mock {
    seen: Arc<Mutex<Vec<SeenRequest>>>,
    release: Arc<Notify>,
}
async fn upstream(
    State(mock): State<Mock>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    mock.seen
        .lock()
        .unwrap()
        .push((headers, uri.to_string(), body.clone()));
    if uri.path().ends_with("count_tokens") {
        return Json(json!({"input_tokens":77})).into_response();
    }
    if body["test_case"] == "http_error" {
        return (StatusCode::UNAUTHORIZED, "synthetic-private-error-key").into_response();
    }
    if body["stream"] != true {
        return Json(json!({"id":"same-upstream-id","type":"message","role":"assistant","model":body["model"],"content":[{"type":"text","text":"synthetic reply"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":5,"cache_creation_input_tokens":4,"cache_creation":{"ephemeral_1h_input_tokens":2,"ephemeral_5m_input_tokens":2}}})).into_response();
    }
    let start = format!(
        "event: message_start\ndata: {}\n\n",
        json!({"type":"message_start","message":{"id":"same-upstream-id","type":"message","role":"assistant","model":body["model"],"content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":1,"cache_read_input_tokens":5,"cache_creation_input_tokens":4,"cache_creation":{"ephemeral_1h_input_tokens":2,"ephemeral_5m_input_tokens":2}}}})
    );
    let mut rest = String::from(
        "event: ping\ndata: {\"type\":\"ping\"}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_synthetic\",\"name\":\"echo\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\":\\\"合成\\\"}\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    );
    if body["test_case"] == "stream_error" {
        rest.push_str("event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"synthetic-private-error-key\"}}\n\n");
    } else if body["test_case"] != "truncated" {
        rest.push_str("event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    }
    let wait = body["test_case"] == "held";
    let notify = mock.release;
    let chunks = futures_util::stream::unfold(
        (0, start, rest, notify),
        move |(step, start, rest, notify)| async move {
            if step == 0 {
                return Some((
                    Ok::<_, std::io::Error>(Bytes::from(start.clone())),
                    (1, start, rest, notify),
                ));
            }
            if step == 1 {
                if wait {
                    notify.notified().await;
                }
                return Some((Ok(Bytes::from(rest.clone())), (2, start, rest, notify)));
            }
            None
        },
    );
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(chunks))
        .unwrap()
}
struct Fixture {
    root: PathBuf,
    data: PathBuf,
    home: PathBuf,
    workspace: ClientWorkspace,
    a: Connection,
    b: Connection,
    mock: Mock,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "switchx-claude-route-{}",
            switchx::id::new_id().unwrap()
        ));
        let data = root.join("data");
        let home = root.join("claude");
        let workspace = app::claude::claude_workspace(&data, &home).unwrap();
        let mock = Mock {
            seen: Arc::new(Mutex::new(Vec::new())),
            release: Arc::new(Notify::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/messages", post(upstream))
            .route("/v1/messages/count_tokens", post(upstream))
            .with_state(mock.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let connection = |name: &str, auth, key: &str| {
            app::claude::save_claude_connection(
                &data,
                &home,
                Connection {
                    id: switchx::id::new_id().unwrap(),
                    workspace_id: workspace.id.clone(),
                    name: name.into(),
                    base_url: url.clone(),
                    settings: ConnectionSettings {
                        auth,
                        picker: PickerPolicy::Replace,
                        default_model: "same-model".into(),
                        models: vec![ModelOption::new("same-model")],
                        ..Default::default()
                    },
                    revision: 0,
                    has_key: false,
                },
                Some(Secret::new(key.into())),
                None,
            )
            .unwrap()
        };
        let a = connection("Alpha", AuthMode::ApiKey, "synthetic-alpha-key");
        let b = connection("Beta", AuthMode::Bearer, "synthetic-beta-key");
        Self {
            root,
            data,
            home,
            workspace,
            a,
            b,
            mock,
            task,
        }
    }
    fn options(&self, primary: &str) -> Options {
        Options {
            mode: Mode::Aggregate,
            primary: primary.into(),
            default_model: String::new(),
            port: 0,
        }
    }
    async fn apply(&self, sessions: &mut Sessions, primary: &str) {
        let prepared = sessions
            .prepare(
                &self.data,
                &self.home,
                self.options(primary),
                &self.root.join("switchx"),
                caps(),
            )
            .await
            .unwrap();
        assert!(
            !serde_json::to_string(&prepared.preview)
                .unwrap()
                .contains("synthetic-alpha-key")
        );
        sessions.apply(&self.data, prepared, caps()).await.unwrap();
    }
    fn token(&self) -> Secret {
        let route = claude::config::route_metadata(&self.data, &self.workspace)
            .unwrap()
            .unwrap();
        Store::open_credentials_read_only(&self.data.join("switchx.sqlite"))
            .unwrap()
            .local_token(&route.token_ref)
            .unwrap()
            .unwrap()
    }
    fn id(&self, provider: &str) -> String {
        Store::open_read_only(&self.data.join("switchx.sqlite"))
            .unwrap()
            .claude_model_bindings(&self.workspace.id)
            .unwrap()
            .into_iter()
            .find(|b| b.provider_id == provider && b.upstream_model == "same-model")
            .unwrap()
            .public_id()
    }
    async fn post(
        &self,
        sessions: &Sessions,
        model: &str,
        stream: bool,
        case: &str,
    ) -> reqwest::Response {
        reqwest::Client::new().post(format!("http://{}/v1/messages?beta=true",sessions.address(&self.workspace.id).unwrap())).bearer_auth(self.token().expose()).header("x-api-key",self.token().expose()).header("cookie","synthetic-official-cookie").header("anthropic-version","2023-06-01").header("anthropic-beta","future-capability-2099,test-beta").header("content-type","application/json").body(json!({"model":model,"messages":[{"role":"user","content":"synthetic-private-prompt"}],"stream":stream,"max_tokens":16,"test_case":case,"thinking":{"type":"enabled","budget_tokens":8},"tools":[{"name":"echo","input_schema":{"type":"object"}}]}).to_string()).send().await.unwrap()
    }
    async fn records(&self, count: usize) -> Vec<switchx::storage::RequestRecord> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let rows = Store::open_read_only(&self.data.join("switchx.sqlite"))
                    .unwrap()
                    .requests(100)
                    .unwrap();
                if rows.len() >= count {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn restore(&self, sessions: &mut Sessions, keep: bool) -> Vec<String> {
        let p = app::claude::preview_claude_restore(&self.data, &self.home, keep).unwrap();
        sessions.restore(&self.data, &self.home, p).await.unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn caps() -> ClaudeCapabilities {
    ClaudeCapabilities::from_version("2.1.301")
}

#[tokio::test]
async fn native_routes_are_exact_and_isolate_auth_usage_and_cli_logs() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    let address = sessions.address(&f.workspace.id).unwrap();
    let a = f.id(&f.a.id);
    let b = f.id(&f.b.id);
    assert_ne!(a, b);
    let first = f.post(&sessions, &a, false, "").await.text().await.unwrap();
    let first: Value = serde_json::from_str(&first).unwrap();
    let second = f.post(&sessions, &b, true, "").await.text().await.unwrap();
    assert!(
        second.contains("input_json_delta")
            && second.contains("合成")
            && second.contains("message_stop")
    );
    assert!(!second.contains("same-upstream-id"));
    assert_eq!(first["model"], a);
    let records = f.records(2).await;
    assert!(records.iter().all(|r| r.status == RequestStatus::Completed
        && r.client == UsageClient::ClaudeCode
        && r.workspace_id.as_deref() == Some(&f.workspace.id)));
    assert_ne!(records[0].response_id, records[1].response_id);
    assert!(records.iter().all(|r| r.usage.unwrap().cache_write_1h == 2));
    let seen = f.mock.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0["x-api-key"], "synthetic-alpha-key");
    assert!(!seen[0].0.contains_key("authorization"));
    assert_eq!(seen[1].0["authorization"], "Bearer synthetic-beta-key");
    assert!(!seen[1].0.contains_key("x-api-key"));
    for (headers, query, body) in seen {
        assert!(!headers.contains_key("cookie"));
        assert_eq!(
            headers["anthropic-beta"],
            "future-capability-2099,test-beta"
        );
        assert_eq!(query, "/v1/messages?beta=true");
        assert_eq!(body["model"], "same-model");
        assert_eq!(body["thinking"]["budget_tokens"], 8);
    }
    assert_eq!(
        f.post(&sessions, "unknown-model", false, "").await.status(),
        StatusCode::NOT_FOUND
    );
    let invalid = reqwest::Client::new()
        .get(format!("http://{address}/v1/models"))
        .bearer_auth("invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    let count = reqwest::Client::new()
        .post(format!(
            "http://{address}/v1/messages/count_tokens?beta=true"
        ))
        .bearer_auth(f.token().expose())
        .header("content-type", "application/json")
        .body(json!({"model":b,"messages":[]}).to_string())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let count: Value = serde_json::from_str(&count).unwrap();
    assert_eq!(count["input_tokens"], 77);
    // A later CLI log shares the rewritten response identity, so it cannot double count.
    let store = Store::open_read_only(&f.data.join("switchx.sqlite")).unwrap();
    let before = store
        .usage_page(0, i64::MAX, &Default::default(), 0, 100)
        .unwrap()
        .1;
    drop(store);
    let log = UsageRecord {
        id: format!("claude:{}", first["id"].as_str().unwrap()),
        source: UsageSource::ClaudeLog,
        client: UsageClient::ClaudeCode,
        started_at_ms: 1,
        provider_id: None,
        codex_provider: None,
        public_model: None,
        model: a,
        usage: TokenUsage {
            input: 12,
            output: 7,
            ..Default::default()
        },
        duration_ms: None,
        first_event_ms: None,
        response_id: Some(format!("claude:{}", first["id"].as_str().unwrap())),
        session_id: None,
        reported_cost_micros: None,
        request_count: 1,
    };
    let store = Store::open(&f.data.join("switchx.sqlite")).unwrap();
    store
        .put_imported_usage(
            &[log],
            &UsageLogFile {
                path: "synthetic-claude".into(),
                size: 1,
                modified_ms: 1,
                read_offset: 1,
                state: "{}".into(),
            },
        )
        .unwrap();
    assert_eq!(
        store
            .usage_page(0, i64::MAX, &Default::default(), 0, 100)
            .unwrap()
            .1,
        before
    );
    drop(store);
    assert!(f.restore(&mut sessions, false).await.is_empty());
    assert!(!sessions.is_running(&f.workspace.id));
    assert!(!f.home.join("settings.json").exists());
}
#[tokio::test]
async fn hot_publication_keeps_inflight_destination_and_conflicts_keep_service() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    let address = sessions.address(&f.workspace.id).unwrap();
    let response = f.post(&sessions, "default", true, "held").await;
    f.apply(&mut sessions, &f.b.id).await;
    assert_eq!(sessions.address(&f.workspace.id), Some(address));
    f.mock.release.notify_one();
    assert!(response.text().await.unwrap().contains("message_stop"));
    f.post(&sessions, "default", false, "")
        .await
        .bytes()
        .await
        .unwrap();
    let records = f.records(2).await;
    assert!(
        records
            .iter()
            .any(|r| r.provider_id.as_deref() == Some(&f.a.id))
    );
    assert!(
        records
            .iter()
            .any(|r| r.provider_id.as_deref() == Some(&f.b.id))
    );
    let path = f.home.join("settings.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["model"] = "external-model".into();
    value["unrelated"] = true.into();
    std::fs::write(&path, value.to_string()).unwrap();
    assert!(
        f.restore(&mut sessions, false)
            .await
            .contains(&"model".into())
    );
    assert!(sessions.is_running(&f.workspace.id));
    assert_eq!(
        f.post(&sessions, &f.id(&f.b.id), false, "").await.status(),
        StatusCode::OK
    );
    assert!(f.restore(&mut sessions, true).await.is_empty());
    assert!(!sessions.is_running(&f.workspace.id));
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["model"], "external-model");
    assert_eq!(value["unrelated"], true);
}
#[tokio::test]
async fn failed_and_truncated_streams_never_count_as_completed_and_crashes_restore() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    for (case, stream) in [
        ("truncated", true),
        ("stream_error", true),
        ("http_error", false),
    ] {
        let text = f
            .post(&sessions, "default", stream, case)
            .await
            .text()
            .await
            .unwrap();
        assert!(!text.contains("synthetic-private-error-key"));
    }
    let records = f.records(3).await;
    assert!(records.iter().all(|r| r.status != RequestStatus::Completed));
    assert!(
        records
            .iter()
            .any(|r| r.status == RequestStatus::Interrupted)
    );
    assert!(!format!("{records:?}").contains("synthetic-private-prompt"));
    let metadata = claude::config::route_metadata(&f.data, &f.workspace)
        .unwrap()
        .unwrap();
    drop(sessions);
    let mut restarted = Sessions::default();
    assert!(
        restarted
            .prepare(
                &f.data,
                &f.home,
                f.options(&f.a.id),
                &f.root.join("switchx"),
                caps()
            )
            .await
            .is_err()
    );
    assert!(f.restore(&mut restarted, false).await.is_empty());
    assert!(
        Store::open_read_only(&f.data.join("switchx.sqlite"))
            .unwrap()
            .local_token(&metadata.token_ref)
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn stale_previews_and_second_owners_cannot_publish_or_restore() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    let prepared = sessions
        .prepare(
            &f.data,
            &f.home,
            f.options(&f.a.id),
            &f.root.join("switchx"),
            caps(),
        )
        .await
        .unwrap();
    let mut edit = f.a.clone();
    edit.name = "Changed".into();
    app::claude::save_claude_connection(&f.data, &f.home, edit, None, Some(f.a.revision)).unwrap();
    assert!(sessions.apply(&f.data, prepared, caps()).await.is_err());
    assert!(
        claude::config::route_metadata(&f.data, &f.workspace)
            .unwrap()
            .is_none()
    );
    f.apply(&mut sessions, &f.a.id).await;
    let mut second = Sessions::default();
    let restore = app::claude::preview_claude_restore(&f.data, &f.home, false).unwrap();
    assert!(second.restore(&f.data, &f.home, restore).await.is_err());
    assert!(sessions.is_running(&f.workspace.id));
    let direct = app::claude::prepare_claude_direct(
        &f.data,
        &f.workspace,
        &f.a.id,
        &f.root.join("switchx"),
        caps(),
    );
    assert!(direct.is_err());
    assert!(f.restore(&mut sessions, false).await.is_empty());
}

#[tokio::test]
async fn claude_workspaces_and_codex_keep_independent_listeners_and_credentials() {
    let f = Fixture::new().await;
    let other = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    other.apply(&mut sessions, &other.b.id).await;
    let a = sessions.address(&f.workspace.id).unwrap();
    let b = sessions.address(&other.workspace.id).unwrap();
    assert_ne!(a, b);
    assert_ne!(f.token().expose(), other.token().expose());
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("http://{b}/v1/models"))
            .bearer_auth(f.token().expose())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_address = listener.local_addr().unwrap();
    let mock = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/responses",
                post(|| async {
                    Json(json!({"id":"synthetic-codex-response","status":"completed","output":[]}))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let records = super::RequestDatabase::new();
    let (codex, codex_address) = super::recording_router(mock_address, &records).await;
    assert_eq!(
        super::request(
            &client,
            codex_address,
            json!({"model":"sx-one","input":"synthetic","stream":false})
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    let held = f.post(&sessions, "default", true, "held").await;
    drop(held);
    let rows = f.records(1).await;
    assert_eq!(rows[0].status, RequestStatus::Cancelled);
    assert!(f.restore(&mut sessions, false).await.is_empty());
    assert!(sessions.is_running(&other.workspace.id));
    assert!(codex.is_running());
    assert_eq!(
        other.post(&sessions, "default", false, "").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        super::request(
            &client,
            codex_address,
            json!({"model":"sx-one","input":"synthetic","stream":false})
        )
        .send()
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
    assert!(other.restore(&mut sessions, false).await.is_empty());
    codex.stop().await;
    mock.abort();
}

#[tokio::test]
async fn mode_changes_pin_old_requests_and_exit_conflicts_retain_runtime() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    let held = f.post(&sessions, &f.id(&f.b.id), true, "held").await;
    let mut options = f.options(&f.a.id);
    options.mode = Mode::Route;
    let prepared = sessions
        .prepare(&f.data, &f.home, options, &f.root.join("switchx"), caps())
        .await
        .unwrap();
    sessions.apply(&f.data, prepared, caps()).await.unwrap();
    f.mock.release.notify_one();
    assert!(held.text().await.unwrap().contains("message_stop"));
    assert_eq!(
        f.post(&sessions, &f.id(&f.b.id), false, "").await.status(),
        StatusCode::NOT_FOUND
    );
    let path = f.home.join("settings.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["model"] = "external-exit-model".into();
    std::fs::write(path, value.to_string()).unwrap();
    assert!(sessions.restore_for_exit(&f.data).await.is_err());
    assert!(sessions.is_running(&f.workspace.id));
    assert_eq!(
        f.post(&sessions, "default", false, "").await.status(),
        StatusCode::OK
    );
    assert!(f.restore(&mut sessions, true).await.is_empty());
}

#[tokio::test]
async fn local_admission_is_bounded_and_never_forwards_browser_or_encoded_credentials() {
    let f = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    let address = sessions.address(&f.workspace.id).unwrap();
    let token = f.token();
    let client = reqwest::Client::new();
    let browser = client
        .get(format!("http://{address}/v1/models"))
        .bearer_auth(token.expose())
        .header("origin", "http://example.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(browser.status(), StatusCode::FORBIDDEN);
    let encoded = token
        .expose()
        .bytes()
        .map(|b| format!("%{b:02X}"))
        .collect::<String>();
    let response = client
        .post(format!("http://{address}/v1/messages?x={encoded}"))
        .bearer_auth(token.expose())
        .header("content-type", "application/json")
        .body(json!({"model":"default","messages":[]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(f.mock.seen.lock().unwrap().len(), 0);
    let large = client
        .post(format!("http://{address}/v1/messages"))
        .bearer_auth(token.expose())
        .body(vec![b'x'; 16 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(f.mock.seen.lock().unwrap().len(), 0);
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(f.post(&sessions, "default", true, "held").await);
    }
    assert_eq!(
        f.post(&sessions, "default", false, "").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    f.mock.release.notify_waiters();
    for response in held {
        assert!(response.text().await.unwrap().contains("message_stop"));
    }
    assert!(f.restore(&mut sessions, false).await.is_empty());
}
#[tokio::test]
async fn a_tampered_recovery_reference_cannot_delete_another_workspaces_token() {
    let f = Fixture::new().await;
    let other = Fixture::new().await;
    let mut sessions = Sessions::default();
    f.apply(&mut sessions, &f.a.id).await;
    other.apply(&mut sessions, &other.a.id).await;
    let path = f
        .data
        .join("claude")
        .join(&f.workspace.id)
        .join("active.json");
    let saved = std::fs::read(&path).unwrap();
    let mut journal: Value = serde_json::from_slice(&saved).unwrap();
    journal["route"]["token_ref"] = claude::config::route_metadata(&other.data, &other.workspace)
        .unwrap()
        .unwrap()
        .token_ref
        .into();
    std::fs::write(&path, journal.to_string()).unwrap();
    assert!(app::claude::preview_claude_restore(&f.data, &f.home, false).is_err());
    assert_eq!(
        other.post(&sessions, "default", false, "").await.status(),
        StatusCode::OK
    );
    std::fs::write(&path, saved).unwrap();
    assert!(f.restore(&mut sessions, false).await.is_empty());
    assert!(other.restore(&mut sessions, false).await.is_empty());
}
