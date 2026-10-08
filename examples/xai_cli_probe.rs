//! Synthetic Grok OAuth + production publication/recovery + isolated Codex tool execution.
//! `--desktop` opens a disposable UI fixture; it does not make real Grok requests.
use axum::{
    Json, Router,
    extract::{Form, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use switchx::{
    app, client, config_transaction,
    routed::RouteSession,
    storage::{AccountBinding, Store},
    xai,
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, time::timeout};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ORIGINAL: &str =
    "model = \"original\"\napproval_policy = \"never\"\nmodel_reasoning_effort = \"max\"\n";
struct Temp(PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Mock {
    origin: String,
    jwt: String,
    seen: mpsc::Sender<Value>,
    turns: Arc<AtomicUsize>,
}
async fn discovery(State(mock): State<Mock>) -> Json<Value> {
    Json(
        json!({"issuer":mock.origin,"token_endpoint":format!("{}/token",mock.origin),"device_authorization_endpoint":format!("{}/device",mock.origin)}),
    )
}
async fn refresh(
    State(mock): State<Mock>,
    Form(form): Form<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, StatusCode> {
    if form.get("grant_type").map(String::as_str) != Some("refresh_token") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(
        json!({"access_token":mock.jwt,"refresh_token":"synthetic-rotated-refresh","expires_in":3600}),
    ))
}
fn auth(mock: &Mock, headers: &HeaderMap) -> std::result::Result<(), StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {}", mock.jwt).as_str())
        || headers.contains_key(switchx::routing::LOCAL_TOKEN_HEADER)
        || headers.contains_key("chatgpt-account-id")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}
async fn models(
    State(mock): State<Mock>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, StatusCode> {
    auth(&mock, &headers)?;
    Ok(Json(json!({"data":[{"id":"grok-4.5"}]})))
}
async fn responses(
    State(mock): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    auth(&mock, &headers)?;
    if body["model"] != "grok-4.5"
        || body["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["type"] == "namespace" || tool["type"] == "custom")
        })
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let first = mock.turns.fetch_add(1, Ordering::SeqCst) == 0;
    let item = if first {
        let tool = body["tools"]
            .as_array()
            .and_then(|tools| {
                tools.iter().find(|tool| {
                    tool["name"].as_str().is_some_and(|name| {
                        name.ends_with("exec_command") || name.ends_with("shell_command")
                    })
                })
            })
            .ok_or(StatusCode::BAD_REQUEST)?;
        json!({"type":"function_call","name":tool["name"],"call_id":"grok_probe_call","arguments":"{\"cmd\":\"cat probe.txt\",\"command\":\"cat probe.txt\",\"login\":false}"})
    } else {
        json!({"id":"grok_probe_message","type":"message","role":"assistant","content":[{"type":"output_text","text":"SWITCHX_GROK_OK","annotations":[]}]})
    };
    mock.seen
        .send(body)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let reasoning = json!({"id":"grok_probe_reasoning","type":"reasoning","summary":[],"encrypted_content":"synthetic-grok-thinking"});
    let done = json!({"type":"response.output_item.done","item":item});
    let reason_done = json!({"type":"response.output_item.done","item":reasoning});
    let complete = json!({"type":"response.completed","response":{"id":"grok_probe_response","status":"completed","output":[reasoning,item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {reason_done}\n\nevent: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {complete}\n\n"
        ),
    ))
}
fn seed(data: &Path) -> Result<()> {
    let accounts = json!({"version":1,"accounts":{"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","subject":"grok-fixture","label":"Grok 合成账号","refresh_token":"synthetic-refresh","requires_reauth":false},"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb":{"id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","subject":"other-fixture","label":"备用合成账号","refresh_token":"synthetic-other-refresh","requires_reauth":true}},"default_account_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"});
    let path = data.join("xai_oauth_auth.json");
    std::fs::write(&path, accounts.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    xai::save_provider(data, None, "Grok 订阅夹具", AccountBinding::Default)?;
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let temp = Temp(std::env::temp_dir().join(format!("switchx-grok-probe-{}", app::new_id()?)));
    let data = temp.0.join("data");
    let home = temp.0.join("home");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&home)?;
    std::fs::write(home.join("config.toml"), ORIGINAL)?;
    std::fs::write(home.join("probe.txt"), "synthetic Grok tool execution\n")?;
    seed(&data)?;
    let executable = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .ok_or("debug directory missing")?
        .join("switchx");
    if std::env::args().any(|a| a == "--desktop") {
        let bundle_executable = executable
            .parent()
            .unwrap()
            .join("SwitchX.app/Contents/MacOS/switchx");
        let desktop_executable = if bundle_executable.is_file() {
            &bundle_executable
        } else {
            &executable
        };
        let mut child = Command::new(desktop_executable)
            .env("SWITCHX_DATA_DIR", &data)
            .env("CODEX_HOME", &home)
            .kill_on_drop(true)
            .spawn()?;
        println!("Synthetic Grok UI fixture: {}", temp.0.display());
        tokio::select! { result=child.wait()=>{result?;}, _=tokio::signal::ctrl_c()=>{child.kill().await?;} }
        return Ok(());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let origin = format!("http://{address}");
    let jwt = format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(br#"{"sub":"grok-fixture"}"#)
    );
    let (sender, mut seen) = mpsc::channel(8);
    let turns = Arc::new(AtomicUsize::new(0));
    let mock = Mock {
        origin: origin.clone(),
        jwt,
        seen: sender,
        turns: turns.clone(),
    };
    let router = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(refresh))
        .route("/models", get(models))
        .route("/responses", post(responses))
        .with_state(mock);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let store = Store::open(&data.join("switchx.sqlite"))?;
    let mut mapping = store.models()?.remove(0);
    mapping.enabled = true;
    store.put_model(&mapping)?;
    let model = mapping.public_id;
    let mut session = RouteSession::with_xai_probe(&data, &origin, address)?;
    let result: Result<()> = async {
        let preview = session.prepare(&data,&home,0,&model,&executable).await?;
        if preview.contains("synthetic-refresh") { return Err("OAuth leaked into preview".into()); }
        session.apply(&data,&home,0,&model).await?;
        let published: toml_edit::DocumentMut = std::fs::read_to_string(home.join("config.toml"))?.parse()?;
        if published["model_reasoning_effort"].as_str() != Some("high") { return Err("unsupported original reasoning effort was not normalized for Grok".into()); }
        let output = timeout(Duration::from_secs(45),Command::new(client::cli_executable()).args(["--no-daemon","exec","--json","--ephemeral","--skip-git-repo-check","-s","read-only","-m",&model,"Read probe.txt, then reply SWITCHX_GROK_OK."]).env("CODEX_HOME",&home).env_remove("OPENAI_API_KEY").env_remove("CODEX_API_KEY").env_remove("CODEX_ACCESS_TOKEN").env_remove("OPENAI_BASE_URL").env_remove("SWITCHX_LOCAL_TOKEN").env_remove("SWITCHX_DATA_DIR").env("HTTPS_PROXY","http://127.0.0.1:9").env("HTTP_PROXY","http://127.0.0.1:9").env("ALL_PROXY","http://127.0.0.1:9").env("NO_PROXY","127.0.0.1,localhost").current_dir(&home).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).output()).await??;
        let events:Vec<Value> = String::from_utf8_lossy(&output.stdout).lines().filter_map(|line|serde_json::from_str(line).ok()).collect();
        if !output.status.success() || !events.iter().any(|v|v["type"]=="turn.completed") || !events.iter().any(|v|v["item"]["text"]=="SWITCHX_GROK_OK") { return Err(format!("isolated Grok Codex probe failed: {}",String::from_utf8_lossy(&output.stderr)).into()); }
        let first = seen.recv().await.ok_or("first turn missing")?; let second = seen.recv().await.ok_or("tool replay missing")?;
        if first["model"] != "grok-4.5" || !second["input"].to_string().contains("function_call_output") || !second["input"].to_string().contains("synthetic-grok-thinking") { return Err("tool or reasoning replay missing".into()); }
        println!("Isolated Codex Grok OAuth: tool execution + encrypted reasoning replay passed (synthetic upstream).");
        Ok(())
    }.await;
    let restore = session.restore(&data, &home).await;
    session.wait_for_accounts().await?;
    task.abort();
    restore?;
    result?;
    if std::fs::read_to_string(home.join("config.toml"))? != ORIGINAL
        || home.join("auth.json").exists()
        || config_transaction::recovery(&data)?.is_some()
    {
        let restored = std::fs::read_to_string(home.join("config.toml"))?;
        let config_matches = restored == ORIGINAL;
        let whitespace_only = restored.trim() == ORIGINAL.trim();
        eprintln!(
            "Restored config sizes: {} vs {}, whitespace_only={whitespace_only}",
            restored.len(),
            ORIGINAL.len()
        );
        let auth_exists = home.join("auth.json").exists();
        let journal_exists = config_transaction::recovery(&data)?.is_some();
        eprintln!(
            "Recovery checks: original_config={config_matches}, native_auth_exists={auth_exists}, journal_exists={journal_exists}"
        );
        return Err("Grok probe did not restore original configuration".into());
    }
    let count: i64 = rusqlite::Connection::open(data.join("switchx.sqlite"))?.query_row(
        "SELECT count(*) FROM app_settings WHERE key LIKE 'local_token:%'",
        [],
        |row| row.get(0),
    )?;
    if count != 0 {
        return Err("Grok probe left a local router token".into());
    }
    println!("Original config restored; Codex auth unchanged; journal and router token removed.");
    Ok(())
}
