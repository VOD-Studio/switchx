//! Complete native publishing workflow with saved A/B, native C, and a separate API key.
//! All authentication is synthetic and every request stays on IPv4 loopback.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Form, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use switchx::{
    accounts::AccountManager,
    app, chatgpt, client, config_transaction,
    routed::RouteSession,
    routing::LOCAL_TOKEN_HEADER,
    storage::{AccountBinding, Store},
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, task::JoinHandle, time::timeout};

type ProbeResult<T> = Result<T, Box<dyn std::error::Error>>;

struct TemporaryHome(PathBuf);
impl TemporaryHome {
    fn new() -> ProbeResult<Self> {
        let path =
            std::env::temp_dir().join(format!("switchx-managed-route-probe-{}", app::new_id()?));
        for directory in ["home", "imports", "data"] {
            std::fs::create_dir_all(path.join(directory))?;
        }
        Ok(Self(path))
    }
}
impl Drop for TemporaryHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct MockTasks(Vec<JoinHandle<()>>);
impl Drop for MockTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

fn synthetic_auth(label: &str) -> Value {
    let claims = json!({"sub":format!("synthetic-user-{label}"),"email":format!("{label}@example.invalid"),
        "https://api.openai.com/auth":{"chatgpt_account_id":format!("synthetic-workspace-{label}"),
            "chatgpt_plan_type":"plus","user_id":format!("synthetic-user-{label}")},"exp":4102444800u64});
    let token = format!(
        "{}.{}.synthetic-signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
        "tokens":{"id_token":token,"access_token":token,"refresh_token":format!("synthetic-refresh-{label}"),
            "account_id":format!("synthetic-workspace-{label}")},"last_refresh":"2099-01-01T00:00:00Z"})
}

async fn refresh(Form(form): Form<HashMap<String, String>>) -> Result<Json<Value>, StatusCode> {
    if form.get("grant_type").map(String::as_str) != Some("refresh_token")
        || form.get("scope").map(String::as_str) != Some("openid profile email")
        || form.get("client_id").is_none_or(String::is_empty)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let label = match form.get("refresh_token").map(String::as_str) {
        Some("synthetic-refresh-a" | "synthetic-refresh-a-renewed") => "a",
        Some("synthetic-refresh-b" | "synthetic-refresh-b-renewed") => "b",
        _ => return Err(StatusCode::UNAUTHORIZED),
    };
    let auth = synthetic_auth(label);
    Ok(Json(json!({
        "access_token":auth["tokens"]["access_token"],
        "id_token":auth["tokens"]["id_token"],
        "refresh_token":format!("synthetic-refresh-{label}-renewed"),
        "expires_in":3600
    })))
}

fn write_auth(home: &Path, value: &Value) -> ProbeResult<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let path = home.join("auth.json");
    std::fs::write(&path, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(bytes)
}

#[derive(Clone)]
struct Mock {
    authorization: String,
    workspace: Option<String>,
    routing: Option<String>,
    model: String,
    seen: mpsc::Sender<(String, Value)>,
}

fn authorize(mock: &Mock, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(mock.authorization.as_str())
        || headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok())
            != mock.workspace.as_deref()
        || headers
            .get("x-openai-account-routing-override")
            .and_then(|value| value.to_str().ok())
            != mock.routing.as_deref()
        || headers.contains_key(LOCAL_TOKEN_HEADER)
        || headers.contains_key(header::COOKIE)
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn models(State(mock): State<Mock>, headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    authorize(&mock, &headers)?;
    Ok(Json(json!({"object":"list","data":[{
        "id":mock.model,"object":"model","owned_by":"synthetic"
    }]})))
}

async fn respond(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    authorize(&mock, &headers)?;
    if body["model"] != mock.model {
        return Err(StatusCode::BAD_REQUEST);
    }
    let session = headers
        .get("session-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let returned = body["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "function_call_output")
    });
    let item = if returned {
        json!({"id":"synthetic-probe-message","type":"message","role":"assistant",
            "content":[{"type":"output_text","text":"MANAGED_ROUTE_OK","annotations":[]}]})
    } else {
        json!({"type":"function_call","name":"exec_command","call_id":"synthetic-probe-call",
            "arguments":"{\"cmd\":\"cat probe.txt\",\"login\":false}"})
    };
    mock.seen
        .send((session, body))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let done = json!({"type":"response.output_item.done","item":item});
    let completed = json!({"type":"response.completed","response":{"id":"synthetic-probe-response","status":"completed",
        "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {completed}\n\n"
        ),
    ))
}

async fn mock(
    authorization: String,
    workspace: Option<String>,
    routing: Option<String>,
    model: &str,
    tasks: &mut MockTasks,
) -> ProbeResult<(SocketAddr, mpsc::Receiver<(String, Value)>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (seen, receiver) = mpsc::channel(16);
    let app = Router::new()
        .route("/models", get(models))
        .route("/v1/models", get(models))
        .route("/responses", post(respond))
        .route("/v1/responses", post(respond))
        .with_state(Mock {
            authorization,
            workspace,
            routing,
            model: model.into(),
            seen,
        });
    tasks.0.push(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    Ok((address, receiver))
}

async fn run_cli(home: &Path, model: &str, resume: Option<&str>) -> ProbeResult<String> {
    let mut command = Command::new(client::cli_executable());
    command.args(["--no-daemon", "exec"]);
    if let Some(id) = resume {
        command.args(["resume", "--json", "--skip-git-repo-check", "-m", model, id]);
    } else {
        command.args([
            "--json",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "-m",
            model,
        ]);
    }
    command
        .arg("Read probe.txt if requested and reply MANAGED_ROUTE_OK.")
        .current_dir(home)
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("SWITCHX_LOCAL_TOKEN")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(45), command.output()).await??;
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    check(
        output.status.success()
            && events.iter().any(|event| event["type"] == "turn.completed")
            && events.iter().any(|event| {
                event["type"] == "item.completed" && event["item"]["text"] == "MANAGED_ROUTE_OK"
            }),
        "isolated native CLI did not complete the synthetic tool response",
    )?;
    events
        .iter()
        .find(|event| event["type"] == "thread.started")
        .and_then(|event| event["thread_id"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "native CLI did not report its thread ID".into())
}

fn check(value: bool, message: &str) -> ProbeResult<()> {
    if value { Ok(()) } else { Err(message.into()) }
}

async fn take(seen: &mut mpsc::Receiver<(String, Value)>) -> ProbeResult<(String, Value)> {
    timeout(Duration::from_secs(3), seen.recv())
        .await?
        .ok_or_else(|| "expected loopback request was not observed".into())
}

async fn take_round(
    seen: &mut mpsc::Receiver<(String, Value)>,
) -> ProbeResult<Vec<(String, Value)>> {
    let mut requests = vec![take(seen).await?];
    while let Ok(request) = seen.try_recv() {
        requests.push(request);
    }
    Ok(requests)
}

fn check_tool_round(
    requests: &[(String, Value)],
    session: Option<&str>,
    model: &str,
) -> ProbeResult<()> {
    check(
        requests.len() == 2
            && requests.iter().all(|(actual_session, request)| {
                session.is_none_or(|expected| actual_session == expected)
                    && request["model"] == model
            })
            && requests[1].1["input"]
                .to_string()
                .contains("synthetic-managed-file-result"),
        "native tool round trip lost account session identity, model mapping, or file result",
    )
}

fn local_token(data: &Path) -> ProbeResult<switchx::credentials::Secret> {
    let reference = config_transaction::recovery(data)?
        .and_then(|recovery| recovery.local_token_reference)
        .ok_or("route recovery has no local credential reference")?;
    Store::open(&data.join("switchx.sqlite"))?
        .local_token(&reference)?
        .ok_or_else(|| "local credential is missing".into())
}

async fn run() -> ProbeResult<()> {
    let temporary = TemporaryHome::new()?;
    let home = temporary.0.join("home");
    let imports = temporary.0.join("imports");
    let data = temporary.0.join("data");
    let mut tasks = MockTasks(Vec::new());
    let auth_a = synthetic_auth("a");
    let auth_b = synthetic_auth("b");
    let auth_c = synthetic_auth("c");
    let manager = AccountManager::open(&data)?;
    write_auth(&imports, &auth_a)?;
    let a = manager.import_current(&imports)?;
    write_auth(&imports, &auth_b)?;
    let b = manager.import_current(&imports)?;
    let original_auth = write_auth(&home, &auth_c)?;
    std::fs::write(home.join("probe.txt"), "synthetic-managed-file-result")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let discovery = listener.local_addr()?;
    let app = Router::new().route("/api/codex/accounts/check", get(|| async {
        Json(json!({"accounts":[
            {"id":"synthetic-workspace-a","plan_type":"plus","workspace_backend_origin":"https://chatgpt.com","account_routing_override":"NO_CONSTRAINT"},
            {"id":"synthetic-workspace-b","plan_type":"plus","workspace_backend_origin":"https://us.chatgpt.com","account_routing_override":"us"},
            {"id":"synthetic-workspace-c","plan_type":"plus","workspace_backend_origin":"https://chatgpt.com","account_routing_override":"NO_CONSTRAINT"},
        ]}))
    }))
    .route("/token", post(refresh))
    .fallback(|| async { Json(json!({})) });
    tasks.0.push(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let original_config = format!(
        "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"http://{discovery}\"\nmodel = \"original-synthetic\"\n[analytics]\nenabled = false\n[features]\nenable_request_compression = false\n"
    );
    std::fs::write(home.join("config.toml"), &original_config)?;
    let metadata = chatgpt::catalog()
        .await?
        .into_iter()
        .next()
        .ok_or("native model catalog is empty")?;
    let upstream_model = metadata["slug"]
        .as_str()
        .ok_or("native model slug missing")?;
    let provider_a = chatgpt::save_subscription(
        &data,
        None,
        "Synthetic A",
        AccountBinding::Fixed(a.id.clone()),
        std::slice::from_ref(&metadata),
    )?;
    let provider_b = chatgpt::save_subscription(
        &data,
        None,
        "Synthetic B",
        AccountBinding::Fixed(b.id.clone()),
        std::slice::from_ref(&metadata),
    )?;
    let (address_a, mut seen_a) = mock(
        format!(
            "Bearer {}",
            auth_a["tokens"]["access_token"].as_str().unwrap()
        ),
        Some(a.workspace_id),
        Some("NO_CONSTRAINT".into()),
        upstream_model,
        &mut tasks,
    )
    .await?;
    let (address_b, mut seen_b) = mock(
        format!(
            "Bearer {}",
            auth_b["tokens"]["access_token"].as_str().unwrap()
        ),
        Some(b.workspace_id),
        Some("us".into()),
        upstream_model,
        &mut tasks,
    )
    .await?;
    let (address_api, mut seen_api) = mock(
        "Bearer synthetic-api-key".into(),
        None,
        None,
        upstream_model,
        &mut tasks,
    )
    .await?;
    app::save_provider(
        &data,
        None,
        "Synthetic API",
        &format!("http://{address_api}"),
        upstream_model,
        "synthetic-api-key".into(),
    )?;
    let store = Store::open(&data.join("switchx.sqlite"))?;
    let api_provider = store
        .providers()?
        .into_iter()
        .find(|provider| provider.name == "Synthetic API")
        .ok_or("API fixture missing")?;
    let catalog_path = temporary.0.join("native-model.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec(&json!({"models":[metadata]}))?,
    )?;
    app::save_model(
        &data,
        &api_provider.id,
        "sx-api-probe",
        "Synthetic API",
        catalog_path
            .to_str()
            .ok_or("invalid temporary catalog path")?,
    )?;
    let api_model = store
        .models()?
        .into_iter()
        .find(|model| model.provider_id == api_provider.id)
        .ok_or("API model fixture missing")?;
    let models = store.models()?;
    let model_a = models
        .iter()
        .find(|model| model.provider_id == provider_a)
        .ok_or("A mapping missing")?
        .public_id
        .clone();
    let model_b = models
        .iter()
        .find(|model| model.provider_id == provider_b)
        .ok_or("B mapping missing")?
        .public_id
        .clone();
    let helper = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("switchx");
    check(
        helper.is_file(),
        "build target/debug/switchx before running this probe",
    )?;
    let responses = HashMap::from([(provider_a, address_a), (provider_b, address_b)]);
    let mut route = RouteSession::for_local_probe(
        &data,
        &format!("http://{discovery}"),
        discovery,
        responses.clone(),
    )?;
    println!("Checking production subscription/API preview and publication.");
    route.prepare(&data, &home, 0, &model_a, &helper).await?;
    check(
        std::fs::read(home.join("auth.json"))? == original_auth
            && std::fs::read_to_string(home.join("config.toml"))? == original_config,
        "route preview changed entry C or its configuration",
    )?;
    route.apply(&data, &home, 0, &model_a).await?;
    check(
        std::fs::read(home.join("auth.json"))? == original_auth,
        "publishing overwrote entry C",
    )?;
    println!("Checking native A tool round trip.");
    let thread_a = run_cli(&home, &model_a, None).await?;
    check_tool_round(
        &take_round(&mut seen_a).await?,
        Some(&thread_a),
        upstream_model,
    )?;
    check(
        seen_b.try_recv().is_err() && seen_api.try_recv().is_err(),
        "A reached another provider",
    )?;
    println!("Checking native B tool round trip.");
    let thread_b = run_cli(&home, &model_b, None).await?;
    check_tool_round(
        &take_round(&mut seen_b).await?,
        Some(&thread_b),
        upstream_model,
    )?;
    check(
        thread_a != thread_b && seen_a.try_recv().is_err() && seen_api.try_recv().is_err(),
        "B did not use its own root and provider",
    )?;
    println!("Checking native API tool round trip.");
    let thread_api = run_cli(&home, &api_model.public_id, None).await?;
    check_tool_round(&take_round(&mut seen_api).await?, None, upstream_model)?;
    check(
        thread_api != thread_a
            && thread_api != thread_b
            && seen_a.try_recv().is_err()
            && seen_b.try_recv().is_err(),
        "API did not use its own root and provider",
    )?;
    println!("Checking native A resume and cross-provider session rejection.");
    check(
        run_cli(&home, &model_a, Some(&thread_a)).await? == thread_a,
        "resume changed A root identity",
    )?;
    let resumed = take_round(&mut seen_a).await?;
    check(
        (1..=2).contains(&resumed.len())
            && resumed.iter().all(|(session, request)| {
                session == &thread_a && request["model"] == upstream_model
            }),
        "resumed request lost its stable session header or mapped model",
    )?;
    if resumed.len() == 2 {
        check_tool_round(&resumed, Some(&thread_a), upstream_model)?;
    }
    check(
        std::fs::read(home.join("auth.json"))? == original_auth,
        "managed requests changed entry C",
    )?;
    let http = reqwest::Client::builder().no_proxy().build()?;
    for model in [&model_b, &api_model.public_id] {
        let token = local_token(&data)?;
        let response = http
            .post(format!("http://{}/v1/responses", route.address().unwrap()))
            .header(LOCAL_TOKEN_HEADER, token.expose())
            .header("session-id", &thread_a)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":model,"input":"switch"}).to_string())
            .send()
            .await?;
        check(
            response.status() == StatusCode::CONFLICT,
            "old A root crossed into B or API",
        )?;
    }
    check(
        seen_a.try_recv().is_err() && seen_b.try_recv().is_err() && seen_api.try_recv().is_err(),
        "rejected cross-provider session reached an upstream",
    )?;
    println!("Checking restore, router restart, and API-only publication.");
    route.restore(&data, &home).await?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == original_config
            && std::fs::read(home.join("auth.json"))? == original_auth,
        "restoring changed the original config or entry C",
    )?;
    route.wait_for_accounts().await?;
    drop(route);
    let mut route =
        RouteSession::for_local_probe(&data, &format!("http://{discovery}"), discovery, responses)?;
    for model in models
        .iter()
        .filter(|model| model.provider_id != api_provider.id)
    {
        app::select_model(&data, &model.public_id, false)?;
    }
    route
        .prepare(&data, &home, 0, &api_model.public_id, &helper)
        .await?;
    route.apply(&data, &home, 0, &api_model.public_id).await?;
    let token = local_token(&data)?;
    let response = http
        .post(format!("http://{}/v1/responses", route.address().unwrap()))
        .bearer_auth(token.expose())
        .header("session-id", &thread_a)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"model":api_model.public_id,"input":"old-root-after-restore"}).to_string())
        .send()
        .await?;
    check(
        response.status() == StatusCode::CONFLICT,
        "API-only republish forgot A root context",
    )?;
    check(
        seen_api.try_recv().is_err(),
        "rejected A root reached API after restore",
    )?;
    let api_only_thread = run_cli(&home, &api_model.public_id, None).await?;
    check(
        api_only_thread != thread_a && api_only_thread != thread_api,
        "new API-only session reused A root",
    )?;
    check_tool_round(&take_round(&mut seen_api).await?, None, upstream_model)?;
    route.restore(&data, &home).await?;
    route.wait_for_accounts().await?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == original_config
            && std::fs::read(home.join("auth.json"))? == original_auth,
        "API-only restore changed entry C",
    )?;
    println!(
        "Production prepare/apply preserved entry C; native A/B/API tool rounds and A resume passed. Persistent session binding rejected A→B/API and survived restore plus API-only republish. All requests used synthetic loopback upstreams."
    );
    Ok(())
}

#[tokio::main]
async fn main() -> ProbeResult<()> {
    run().await
}
