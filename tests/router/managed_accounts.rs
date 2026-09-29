use std::{collections::HashMap, path::PathBuf, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use switchx::{
    accounts::AccountManager,
    app,
    catalog::{Selection, publish},
    chatgpt::{self, Workspace},
    routing::{LOCAL_TOKEN_HEADER, RouterState, RunningRouter, Upstream},
    storage::Store,
};
use tokio::{net::TcpListener, sync::mpsc, time::timeout};

const LOCAL_TOKEN: &str = "synthetic-managed-local-token-123456789";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().join(format!("switchx-managed-test-{nonce:02x?}"));
        std::fs::create_dir_all(path.join("home")).unwrap();
        std::fs::create_dir_all(path.join("data")).unwrap();
        Self(path)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn data(&self) -> PathBuf {
        self.0.join("data")
    }

    fn write_native(&self, auth: &Value) {
        std::fs::write(
            self.home().join("auth.json"),
            serde_json::to_vec_pretty(auth).unwrap(),
        )
        .unwrap();
    }

    fn native(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.home().join("auth.json")).unwrap()).unwrap()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn native_auth(account: &str, revision: &str, last_refresh: &str) -> Value {
    // Synthetic JWTs are parsed for identity/expiry; no real account or signature is used.
    let claims = json!({
        "sub": format!("synthetic-user-{account}"),
        "email": format!("synthetic-{account}@example.invalid"),
        "https://api.openai.com/auth": {
            "chatgpt_account_id": format!("synthetic-workspace-{account}"),
            "chatgpt_plan_type": "plus",
            "user_id": format!("synthetic-user-{account}"),
        },
        "exp": 4_102_444_800u64,
        "jti": format!("synthetic-{account}-{revision}"),
    });
    let token = format!(
        "{}.{}.synthetic-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": token,
            "access_token": token,
            "refresh_token": format!("synthetic-refresh-{account}-{revision}"),
            "account_id": format!("synthetic-workspace-{account}"),
        },
        "last_refresh": last_refresh,
    })
}

fn workspace(account: &str) -> Workspace {
    Workspace {
        account_id: format!("synthetic-workspace-{account}"),
        backend_origin: "https://chatgpt.com".into(),
        routing_override: "NO_CONSTRAINT".into(),
    }
}

#[tokio::test]
async fn shared_codex_home_preserves_each_saved_bundle_and_native_renewal() {
    let directory = TestDirectory::new();
    let home = directory.home();
    let data = directory.data();
    let manager = AccountManager::open(&data).unwrap();
    let config = b"model = \"synthetic-user-model\"\ncli_auth_credentials_store = \"file\"\n";
    std::fs::write(home.join("config.toml"), config).unwrap();
    let original_a = native_auth("a", "original", "2099-01-01T00:00:00.000Z");
    let original_b = native_auth("b", "original", "2099-01-01T00:00:00.000Z");

    directory.write_native(&original_a);
    let account_a = manager.import_current(&home).unwrap();
    manager.activate(&account_a.id, &home).await.unwrap();
    directory.write_native(&original_b);
    let account_b = manager.import_current(&home).unwrap();
    manager.activate(&account_b.id, &home).await.unwrap();
    assert_ne!(account_a.id, account_b.id);
    assert_eq!(manager.list().unwrap().len(), 2);
    assert_eq!(account_a.workspace_id, workspace("a").account_id);
    assert_eq!(account_b.workspace_id, workspace("b").account_id);

    manager.set_default(&account_a.id).unwrap();
    assert_eq!(manager.default_id().unwrap(), Some(account_a.id.clone()));
    assert_eq!(
        manager.active_id(&home).unwrap(),
        Some(account_b.id.clone())
    );
    assert_eq!(directory.native()["tokens"], original_b["tokens"]);
    chatgpt::bind_managed_account(&data, Some("default")).unwrap();
    assert_eq!(
        chatgpt::account_binding(&data).unwrap().as_deref(),
        Some("default")
    );
    assert_eq!(
        chatgpt::managed_account(&data).unwrap(),
        Some(account_a.id.clone())
    );
    manager.set_default(&account_b.id).unwrap();
    assert_eq!(
        chatgpt::managed_account(&data).unwrap(),
        Some(account_b.id.clone())
    );
    assert_eq!(directory.native()["tokens"], original_b["tokens"]);
    chatgpt::bind_managed_account(&data, None).unwrap();
    assert_eq!(chatgpt::account_binding(&data).unwrap(), None);
    assert_eq!(chatgpt::managed_account(&data).unwrap(), None);
    assert_eq!(
        manager
            .credential(&account_a.id, &home)
            .await
            .unwrap()
            .access_token
            .expose(),
        original_a["tokens"]["access_token"].as_str().unwrap(),
    );
    assert_eq!(directory.native()["tokens"], original_b["tokens"]);

    manager.activate(&account_a.id, &home).await.unwrap();
    assert_eq!(directory.native()["tokens"], original_a["tokens"]);
    let renewed_a = native_auth("a", "renewed", "2099-02-01T00:00:00.000Z");
    directory.write_native(&renewed_a);
    manager.sync_current(&home).unwrap();
    directory.write_native(&original_a);
    manager.sync_current(&home).unwrap();
    assert!(
        std::fs::read_to_string(data.join("codex_oauth_auth.json"))
            .unwrap()
            .contains("synthetic-refresh-a-renewed")
    );
    // The stale native file must preserve the saved complete renewal.
    manager.activate(&account_b.id, &home).await.unwrap();
    assert_eq!(directory.native()["tokens"], original_b["tokens"]);
    manager.activate(&account_a.id, &home).await.unwrap();
    assert_eq!(directory.native()["tokens"], renewed_a["tokens"]);
    assert_eq!(
        directory.native()["last_refresh"],
        renewed_a["last_refresh"]
    );
    assert_eq!(std::fs::read(home.join("config.toml")).unwrap(), config);

    // A reopened manager can adopt the active native access token without a network refresh.
    let reopened = AccountManager::open(&data).unwrap();
    assert_eq!(
        reopened
            .credential(&account_a.id, &home)
            .await
            .unwrap()
            .access_token
            .expose(),
        renewed_a["tokens"]["access_token"].as_str().unwrap(),
    );

    let managed_path = data.join("codex_oauth_auth.json");
    let persisted = std::fs::read_to_string(&managed_path).unwrap();
    assert!(persisted.contains("synthetic-refresh-a-renewed"));
    assert!(persisted.contains("synthetic-refresh-b-original"));
    assert!(!persisted.contains("synthetic-refresh-a-original"));
    let marker = std::fs::read_to_string(home.join(".switchx-account.json")).unwrap();
    for secret in [
        renewed_a["tokens"]["access_token"].as_str().unwrap(),
        "synthetic-refresh-a-renewed",
    ] {
        assert!(!marker.contains(secret));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [managed_path, home.join("auth.json")] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

async fn capture(
    State(captured): State<mpsc::Sender<(HeaderMap, Value)>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let input = body["input"].as_str().map(str::to_owned);
    captured.send((headers, body)).await.unwrap();
    match input.as_deref() {
        Some("expire") => StatusCode::UNAUTHORIZED.into_response(),
        Some("denied") => StatusCode::FORBIDDEN.into_response(),
        _ if uri.path().ends_with("/compact") => Json(json!({
            "object":"response.compaction",
            "output":[{"type":"compaction","encrypted_content":"synthetic-compact-state"}],
        }))
        .into_response(),
        _ => Json(json!({"status":"completed","output":[]})).into_response(),
    }
}

async fn mock() -> (
    std::net::SocketAddr,
    mpsc::Receiver<(HeaderMap, Value)>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (captured, seen) = mpsc::channel(8);
    let router = Router::new()
        .route("/responses", post(capture))
        .route("/responses/compact", post(capture))
        .with_state(captured);
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (address, seen, task)
}

#[tokio::test]
async fn managed_router_isolates_bound_accounts_api_keys_and_removed_bindings() {
    let directory = TestDirectory::new();
    let home = directory.home();
    let data = directory.data();
    let manager = AccountManager::open(&data).unwrap();
    let auth_a = native_auth("a", "original", "2099-01-01T00:00:00.000Z");
    let auth_b = native_auth("b", "original", "2099-01-01T00:00:00.000Z");
    directory.write_native(&auth_a);
    let account_a = manager.import_current(&home).unwrap();
    directory.write_native(&auth_b);
    let account_b = manager.import_current(&home).unwrap();
    manager.activate(&account_b.id, &home).await.unwrap();
    manager.set_default(&account_b.id).unwrap();
    chatgpt::bind_managed_account(&data, Some(&account_a.id)).unwrap();
    assert_eq!(
        chatgpt::account_binding(&data).unwrap(),
        Some(account_a.id.clone())
    );
    assert_eq!(
        chatgpt::managed_account(&data).unwrap(),
        Some(account_a.id.clone())
    );

    let (official_address, mut official_seen, official_task) = mock().await;
    let (api_address, mut api_seen, api_task) = mock().await;
    app::save_provider(
        &data,
        None,
        "Synthetic API",
        &format!("http://{api_address}"),
        "gpt-5.5",
        "synthetic-api-key".into(),
    )
    .unwrap();
    let api_provider = Store::open(&data.join("switchx.sqlite"))
        .unwrap()
        .providers()
        .unwrap()
        .into_iter()
        .find(|provider| provider.name == "Synthetic API")
        .unwrap();
    assert!(api_provider.credential_ref.is_none());
    let api_key = app::provider_credential(&data, &api_provider).unwrap();
    let templates =
        serde_json::from_str(include_str!("../fixtures/synthetic-models.json")).unwrap();
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-a",
                display_name: "A",
                provider_id: "account-a",
                upstream_model: "gpt-5.5",
            },
            Selection {
                public_id: "sx-b",
                display_name: "B",
                provider_id: "account-b",
                upstream_model: "gpt-5.5",
            },
            Selection {
                public_id: "sx-api",
                display_name: "API",
                provider_id: &api_provider.id,
                upstream_model: "gpt-5.5",
            },
        ],
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let database_path = data.join("request-metadata.sqlite");
    let state = RouterState::new(
        address,
        LOCAL_TOKEN.into(),
        publication,
        HashMap::from([
            (
                "account-a".into(),
                Upstream::managed_chatgpt_mock(
                    official_address,
                    manager.clone(),
                    account_a.id.clone(),
                    home.clone(),
                    &workspace("a"),
                )
                .unwrap(),
            ),
            (
                "account-b".into(),
                Upstream::managed_chatgpt_mock(
                    official_address,
                    manager.clone(),
                    account_b.id.clone(),
                    home.clone(),
                    &workspace("b"),
                )
                .unwrap(),
            ),
            (
                api_provider.id.clone(),
                Upstream::with_secret(&format!("http://{api_address}"), api_key).unwrap(),
            ),
        ]),
    )
    .unwrap()
    .with_request_log(
        Store::open(&database_path).unwrap(),
        "managed-fixture".into(),
    )
    .with_session_store(Store::open(&database_path).unwrap());
    let running = RunningRouter::start(listener, state).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = |model: &str| {
        client
            .post(format!("http://{address}/v1/responses"))
            .header(LOCAL_TOKEN_HEADER, LOCAL_TOKEN)
            .header(
                "session-id",
                match model {
                    "sx-a" => "00000000-0000-0000-0000-000000000001",
                    "sx-b" => "00000000-0000-0000-0000-000000000002",
                    _ => "00000000-0000-0000-0000-000000000003",
                },
            )
            .bearer_auth("synthetic-untrusted-client-access")
            .header("chatgpt-account-id", "synthetic-workspace-untrusted-client")
            .header("x-openai-account-routing-override", "us")
            .header(header::COOKIE, "synthetic-private-cookie")
            .header(header::PROXY_AUTHORIZATION, "synthetic-private-proxy")
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":model,"input":"synthetic-private-prompt"}).to_string())
    };

    for (model, account, auth) in [("sx-a", "a", &auth_a), ("sx-b", "b", &auth_b)] {
        let response = request(model).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
        let (headers, body) = timeout(Duration::from_secs(2), official_seen.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            headers[header::AUTHORIZATION],
            format!(
                "Bearer {}",
                auth["tokens"]["access_token"].as_str().unwrap()
            )
        );
        assert_eq!(headers["chatgpt-account-id"], workspace(account).account_id);
        assert_eq!(
            headers["x-openai-account-routing-override"],
            "NO_CONSTRAINT"
        );
        assert_eq!(body["model"], "gpt-5.5");
        for name in [LOCAL_TOKEN_HEADER, "cookie", "proxy-authorization"] {
            assert!(!headers.contains_key(name));
        }
        assert_eq!(directory.native()["tokens"], auth_b["tokens"]);
    }

    let response = request("sx-api").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let (headers, _) = timeout(Duration::from_secs(2), api_seen.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(headers[header::AUTHORIZATION], "Bearer synthetic-api-key");
    for name in [
        "chatgpt-account-id",
        "x-openai-account-routing-override",
        LOCAL_TOKEN_HEADER,
        "cookie",
        "proxy-authorization",
    ] {
        assert!(!headers.contains_key(name));
    }

    manager.remove(&account_a.id, &home).await.unwrap();
    assert_eq!(manager.default_id().unwrap(), Some(account_b.id.clone()));
    assert_eq!(manager.active_id(&home).unwrap(), Some(account_b.id));
    let reopened = AccountManager::open(&data).unwrap();
    assert_eq!(reopened.list().unwrap().len(), 1);
    assert!(reopened.credential(&account_a.id, &home).await.is_err());
    assert_eq!(
        chatgpt::account_binding(&data).unwrap(),
        Some(account_a.id.clone())
    );
    assert_eq!(
        chatgpt::managed_account(&data).unwrap(),
        Some(account_a.id.clone())
    );
    let response = request("sx-a").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert!(
        body["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("chatgpt_")
    );
    assert!(official_seen.try_recv().is_err());
    assert!(api_seen.try_recv().is_err());
    assert_eq!(directory.native()["tokens"], auth_b["tokens"]);
    chatgpt::bind_managed_account(&data, None).unwrap();
    assert_eq!(chatgpt::managed_account(&data).unwrap(), None);
    running.stop().await;
    assert_eq!(
        Store::open(&database_path)
            .unwrap()
            .requests(100)
            .unwrap()
            .len(),
        4
    );

    let request_database =
        String::from_utf8_lossy(&std::fs::read(database_path).unwrap()).into_owned();
    let provider_database =
        String::from_utf8_lossy(&std::fs::read(data.join("switchx.sqlite")).unwrap()).into_owned();
    assert!(provider_database.contains("synthetic-api-key"));
    assert!(!request_database.contains("synthetic-api-key"));
    for secret in [
        auth_a["tokens"]["access_token"].as_str().unwrap(),
        auth_b["tokens"]["access_token"].as_str().unwrap(),
        "synthetic-refresh-a-original",
        "synthetic-refresh-b-original",
        "synthetic-untrusted-client-access",
        "synthetic-private-prompt",
        "synthetic-private-cookie",
        "synthetic-private-proxy",
        LOCAL_TOKEN,
    ] {
        assert!(!request_database.contains(secret));
        assert!(!provider_database.contains(secret));
    }
    official_task.abort();
    api_task.abort();
}

#[tokio::test]
async fn session_context_survives_restart_and_blocks_account_changes_before_forwarding() {
    const A: &str = "00000000-0000-0000-0000-000000000001";
    const B: &str = "00000000-0000-0000-0000-000000000002";
    const NEW: &str = "00000000-0000-0000-0000-000000000003";
    const CHILD: &str = "00000000-0000-0000-0000-000000000004";
    let directory = TestDirectory::new();
    let home = directory.home();
    let manager = AccountManager::open(&directory.data()).unwrap();
    directory.write_native(&native_auth("a", "original", "2099-01-01T00:00:00Z"));
    let account_a = manager.import_current(&home).unwrap();
    directory.write_native(&native_auth("b", "original", "2099-01-01T00:00:00Z"));
    let account_b = manager.import_current(&home).unwrap();
    let (official_address, mut official_seen, official_task) = mock().await;
    let (api_address, mut api_seen, api_task) = mock().await;
    let templates =
        serde_json::from_str(include_str!("../fixtures/synthetic-models.json")).unwrap();
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-a",
                display_name: "A",
                provider_id: "account-a",
                upstream_model: "gpt-5.5",
            },
            Selection {
                public_id: "sx-a-two",
                display_name: "A second model",
                provider_id: "account-a",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "sx-b",
                display_name: "B",
                provider_id: "account-b",
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
    let database = directory.data().join("session-test.sqlite");
    let state = |address, rebound: bool| {
        let (id, workspace) = if rebound {
            (account_b.id.clone(), workspace("b"))
        } else {
            (account_a.id.clone(), workspace("a"))
        };
        RouterState::new(
            address,
            LOCAL_TOKEN.into(),
            publication.clone(),
            HashMap::from([
                (
                    "account-a".into(),
                    Upstream::managed_chatgpt_mock(
                        official_address,
                        manager.clone(),
                        id,
                        home.clone(),
                        &workspace,
                    )
                    .unwrap(),
                ),
                (
                    "account-b".into(),
                    Upstream::managed_chatgpt_mock(
                        official_address,
                        manager.clone(),
                        account_b.id.clone(),
                        home.clone(),
                        &self::workspace("b"),
                    )
                    .unwrap(),
                ),
                (
                    "api".into(),
                    Upstream::new(&format!("http://{api_address}"), "synthetic-api-key".into())
                        .unwrap(),
                ),
            ]),
        )
        .unwrap()
        .with_session_store(Store::open(&database).unwrap())
        .with_request_log(Store::open(&database).unwrap(), "session-fixture".into())
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let running = RunningRouter::start(listener, state(address, false)).unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = |address, model: &str, session: Option<&str>, input: Value, compact: bool| {
        let mut request = client
            .post(format!(
                "http://{address}/v1/responses{}",
                if compact { "/compact" } else { "" }
            ))
            .header(LOCAL_TOKEN_HEADER, LOCAL_TOKEN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":model,"input":input}).to_string());
        if let Some(session) = session {
            request = request.header("session-id", session);
        }
        request
    };
    for (model, session, expected_workspace) in [("sx-a", A, "a"), ("sx-b", B, "b")] {
        let response = request(address, model, Some(session), json!("first"), false)
            .header("thread-id", CHILD)
            .header(
                "x-codex-turn-metadata",
                json!({"session_id":session,"thread_id":CHILD}).to_string(),
            )
            .header("x-codex-routing-hint", format!("model={model}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap();
        let (headers, _) = official_seen.recv().await.unwrap();
        assert_eq!(headers["session-id"], session);
        assert_eq!(headers["thread-id"], CHILD);
        assert_eq!(
            headers["chatgpt-account-id"],
            workspace(expected_workspace).account_id
        );
    }
    let opaque = json!([{"type":"reasoning","encrypted_content":"synthetic-prior-state"}]);
    let response = request(address, "sx-a-two", Some(A), opaque.clone(), false)
        .header("session_id", A)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let (headers, body) = official_seen.recv().await.unwrap();
    assert_eq!(headers["session_id"], A);
    assert_eq!(body["model"], "deepseek-flash");

    for model in ["sx-b", "sx-api"] {
        let response = request(address, model, Some(A), json!("switch"), false)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    for response in [
        request(address, "sx-a", Some(NEW), opaque.clone(), false)
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(NEW), json!([]), true)
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(NEW), json!("resume"), false)
            .header("x-codex-turn-state", "synthetic-prior-state")
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(NEW), json!("resume"), false)
            .header("x-codex-routing-hint", "synthetic-prior-hint")
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    // Rejected server-state references must not legitimize a previously
    // unknown root session for a later encrypted continuation.
    for field in ["previous_response_id", "conversation"] {
        let mut body = json!({"model":"sx-a","input":"unsupported-state"});
        body[field] = json!("synthetic-server-state");
        let response = request(address, "sx-a", Some(NEW), json!([]), false)
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        response.bytes().await.unwrap();
        let response = request(address, "sx-a", Some(NEW), opaque.clone(), false)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    for response in [
        request(address, "sx-a", None, json!("missing"), false)
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(A), json!("duplicate"), false)
            .header("session-id", A)
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(A), json!("conflict"), false)
            .header("session_id", B)
            .send()
            .await
            .unwrap(),
        request(address, "sx-a", Some(A), json!("metadata-conflict"), false)
            .header("x-codex-turn-metadata", json!({"session_id":B}).to_string())
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(official_seen.try_recv().is_err());
    assert!(api_seen.try_recv().is_err());

    // One successful account must not clear another account's authentication error.
    let response = request(address, "sx-a", Some(A), json!("expire"), false)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    response.bytes().await.unwrap();
    official_seen.recv().await.unwrap();
    let response = request(address, "sx-b", Some(B), json!("success"), false)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    official_seen.recv().await.unwrap();
    assert!(running.chatgpt_error_for("account-a").is_some());
    assert!(running.chatgpt_error_for("account-b").is_none());
    assert!(running.chatgpt_error().is_some());
    running.stop().await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resumed_address = listener.local_addr().unwrap();
    let resumed = RunningRouter::start(listener, state(resumed_address, false)).unwrap();
    let response = request(resumed_address, "sx-a", Some(A), opaque.clone(), true)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let (_, body) = official_seen.recv().await.unwrap();
    assert_eq!(body["input"], opaque);
    let response = request(resumed_address, "sx-a", None, json!("legacy"), false)
        .header("session_id", A)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    official_seen.recv().await.unwrap();
    resumed.stop().await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rebound_address = listener.local_addr().unwrap();
    let rebound = RunningRouter::start(listener, state(rebound_address, true)).unwrap();
    let response = request(rebound_address, "sx-a", Some(A), json!("rebound"), false)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = request(
        rebound_address,
        "sx-a",
        Some(NEW),
        json!("new-session"),
        false,
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.bytes().await.unwrap();
    let (headers, _) = official_seen.recv().await.unwrap();
    assert_eq!(headers["chatgpt-account-id"], workspace("b").account_id);
    rebound.stop().await;
    let stored = String::from_utf8_lossy(&std::fs::read(database).unwrap()).into_owned();
    for secret in [
        "synthetic-prior-state",
        "synthetic-prior-hint",
        LOCAL_TOKEN,
        "synthetic-refresh-a-original",
        "synthetic-refresh-b-original",
    ] {
        assert!(!stored.contains(secret));
    }
    official_task.abort();
    api_task.abort();
}
