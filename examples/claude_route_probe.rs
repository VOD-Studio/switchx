//! Isolated Claude CLI -> SwitchX -> synthetic Messages servers. Never calls a real provider.
//! macOS: cargo run --example claude_route_probe -- /absolute/new/probe-directory
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
};
use switchx::{
    app,
    claude::{
        self, AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy,
        publication::{Mode, Options},
        route_session::Sessions,
    },
    credentials::Secret,
    storage::Store,
};
#[derive(Clone)]
struct Mock {
    name: &'static str,
    key: &'static str,
    bearer: bool,
    hits: Arc<Mutex<Vec<String>>>,
}
async fn answer(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    let authorized = if mock.bearer {
        headers.get("authorization").and_then(|v| v.to_str().ok())
            == Some(format!("Bearer {}", mock.key).as_str())
            && !headers.contains_key("x-api-key")
    } else {
        headers.get("x-api-key").and_then(|v| v.to_str().ok()) == Some(mock.key)
            && !headers.contains_key("authorization")
    };
    if !authorized || body["model"] != "same-model" {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    }
    mock.hits.lock().unwrap().push(mock.name.into());
    let reply = format!("SWITCHX_M2_{}", mock.name);
    let message = json!({"id":"msg_synthetic_upstream","type":"message","role":"assistant","model":"same-model","content":[{"type":"text","text":reply}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":3,"output_tokens":7}});
    if body["stream"] != true {
        return Json(message).into_response();
    }
    let mut start = message;
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"]["output_tokens"] = 0.into();
    let events = [
        json!({"type":"message_start","message":start}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":reply}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let body = events
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    ([("content-type", "text/event-stream")], body).into_response()
}
async fn mock(
    name: &'static str,
    key: &'static str,
    bearer: bool,
    hits: Arc<Mutex<Vec<String>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/messages", post(answer))
        .route(
            "/v1/messages/count_tokens",
            post(|| async { Json(json!({"input_tokens":12})) }),
        )
        .with_state(Mock {
            name,
            key,
            bearer,
            hits,
        });
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, task)
}
#[cfg(target_os = "macos")]
async fn cli(cli: &Path, root: &Path, private_home: &Path, expected: &str) -> Result<(), String> {
    let system_home = std::env::var("HOME").map_err(|_| "HOME unavailable")?;
    let quote = |p: &Path| {
        serde_json::to_string(p.to_str().ok_or("invalid probe path")?)
            .map_err(|_| "invalid probe path")
    };
    let policy = format!(
        "(version 1)\n(allow default)\n(deny network*)\n(allow network-outbound (remote ip \"localhost:*\"))\n(deny file-write*)\n(allow file-write* (subpath {}) (subpath \"/private/tmp\") (subpath \"/dev\"))\n(deny file-read* (subpath {}) (literal {}) (subpath {}))\n",
        quote(root)?,
        quote(&PathBuf::from(&system_home).join(".claude"))?,
        quote(&PathBuf::from(&system_home).join(".claude.json"))?,
        quote(&PathBuf::from(&system_home).join("Library/Keychains"))?
    );
    std::fs::write(root.join("probe.sb"), policy).map_err(|_| "cannot write probe policy")?;
    let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
    command
        .arg("-f")
        .arg(root.join("probe.sb"))
        .arg(cli)
        .args([
            "--print",
            "--output-format",
            "json",
            "--setting-sources",
            "user",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--tools",
            "",
            "--",
            "Reply with the synthetic probe marker.",
        ])
        .env_clear()
        .current_dir(root.join("project"))
        .kill_on_drop(true);
    for key in ["PATH", "HOME", "USER", "LOGNAME", "LANG", "SHELL"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("CLAUDE_CONFIG_DIR", private_home)
        .env("TMPDIR", root.join("tmp"))
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .map_err(|_| "isolated Claude CLI timed out")?
        .map_err(|_| "cannot run isolated Claude CLI")?;
    // Never print raw CLI diagnostics: a client could echo its local credential.
    if !output.status.success() {
        let diagnostic = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_lowercase();
        let tags: Vec<_> = [
            "apikeyhelper",
            "api key",
            "authentication",
            "invalid",
            "sandbox",
            "eperm",
            "enoent",
            "unknown option",
            "not found",
            "connection",
            "permission",
            "refused",
            "settings",
            "model",
            "json",
            "loopback",
        ]
        .into_iter()
        .filter(|tag| diagnostic.contains(tag))
        .collect();
        let shape = serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .and_then(|v| v.as_object().map(|v| v.keys().cloned().collect::<Vec<_>>()));
        return Err(format!(
            "isolated Claude CLI exit {:?}; diagnostic categories {tags:?}; JSON keys {shape:?}; raw diagnostics withheld",
            output.status.code()
        ));
    }
    let value: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "CLI output was not JSON")?;
    if value["is_error"] == true || value["result"].as_str() != Some(expected) {
        return Err("isolated CLI did not return the expected synthetic marker".into());
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
async fn cli(_: &Path, _: &Path, _: &Path, _: &str) -> Result<(), String> {
    Err("This probe's network isolation currently requires macOS sandbox-exec".into())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("pass an absolute dedicated probe directory")?;
    if !root.is_absolute() || root.exists() {
        return Err("probe directory must be absolute and not exist yet".into());
    }
    std::fs::create_dir_all(root.join("project"))?;
    std::fs::create_dir_all(root.join("tmp"))?;
    let data = root.join("data");
    let private_home = root.join("claude");
    std::fs::create_dir(&private_home)?;
    let private_bin = root.join("bin");
    std::fs::create_dir(&private_bin)?;
    // Freeze the CLI executable for this probe so an active user session and
    // its installation can continue independently while the probe runs.
    let private_cli = private_bin.join(if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    });
    std::fs::copy(claude::client::executable(None)?, &private_cli)?;
    let info = claude::client::inspect(private_cli.to_str(), &private_home).await?;
    std::fs::write(
        private_home.join(".claude.json"),
        json!({"hasCompletedOnboarding":true,"theme":"dark","lastOnboardingVersion":info.version})
            .to_string(),
    )?;
    let helper = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(if cfg!(windows) {
            "switchx.exe"
        } else {
            "switchx"
        });
    if !helper.is_file() {
        return Err("build the SwitchX binary first".into());
    }
    let workspace = app::claude::claude_workspace(&data, &private_home)?;
    let hits = Arc::new(Mutex::new(Vec::new()));
    let (url_a, task_a) = mock("A", "synthetic-probe-a", false, hits.clone()).await;
    let (url_b, task_b) = mock("B", "synthetic-probe-b", true, hits.clone()).await;
    let save = |name: &str, url: String, auth, key: &str| -> Result<Connection, String> {
        let mut model = ModelOption::new("same-model");
        model.behaves_as = "claude-sonnet-4-6".into();
        app::claude::save_claude_connection(
            &data,
            &private_home,
            Connection {
                id: switchx::id::new_id()?,
                workspace_id: workspace.id.clone(),
                name: name.into(),
                base_url: url,
                settings: ConnectionSettings {
                    auth,
                    picker: PickerPolicy::Replace,
                    models: vec![model],
                    default_model: "same-model".into(),
                    ..Default::default()
                },
                revision: 0,
                has_key: false,
            },
            Some(Secret::new(key.into())),
            None,
        )
    };
    let a = save("Probe A", url_a, AuthMode::ApiKey, "synthetic-probe-a")?;
    let b = save("Probe B", url_b, AuthMode::Bearer, "synthetic-probe-b")?;
    let mut sessions = Sessions::default();
    let outcome=async {
        for (connection,marker) in [(&a,"SWITCHX_M2_A"),(&b,"SWITCHX_M2_B")] {
            let prepared=sessions.prepare(&data,&private_home,Options{mode:Mode::Aggregate,primary:connection.id.clone(),default_model:String::new(),port:0},&helper,info.capabilities).await?;
            sessions.apply(&data,prepared,info.capabilities).await?;
            cli(&info.executable,&root,&private_home,marker).await?;
        }
        let store=Store::open(&data.join("switchx.sqlite")).map_err(|_|"cannot open probe database")?;
        let before=store.usage_page(0,i64::MAX,&Default::default(),0,100).map_err(|_|"cannot read routed usage")?.1;
        let imported=switchx::usage::claude_logs::import(&store,&private_home,&AtomicBool::new(false),|_|{}).map_err(|_|"cannot import probe CLI logs")?;
        let after=store.usage_page(0,i64::MAX,&Default::default(),0,100).map_err(|_|"cannot read imported usage")?.1;
        if before!=after{return Err("CLI logs duplicated router usage".into());}
        Ok::<_,String>(json!({"claude_version":info.version,"routes_seen":hits.lock().unwrap().clone(),"routed_usage_rows":before,"usage_rows_after_log_import":after,"cli_log_files":imported.files,"upstream":"synthetic loopback only"}))
    }.await;
    let cleanup = match app::claude::preview_claude_restore(&data, &private_home, false) {
        Ok(prepared) => sessions
            .restore(&data, &private_home, prepared)
            .await
            .map(|c| c.is_empty()),
        Err(_) => Ok(!claude::config::status(&data, &workspace)?.managed),
    };
    task_a.abort();
    task_b.abort();
    let mut report = outcome?;
    if !cleanup? {
        return Err("probe restoration has conflicts".into());
    }
    report["restored"] = true.into();
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
