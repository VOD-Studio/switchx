//! Isolated native-route acceptance with synthetic credentials and two local upstreams.
//! `--desktop` leaves the same fixture open for native UI checks until the app exits.
//! `--desktop-recovery` starts with a journal to check recovery on native Command-Q.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use serde_json::{Value, json};
use switchx::{app, client, config_transaction, routed::RouteSession, storage::Store};
use tokio::{net::TcpListener, process::Command, sync::mpsc, time::timeout};

const ORIGINAL: &str = "model = \"original-model\" # keep\napproval_policy = \"never\"\n\n[mcp_servers.synthetic]\nenabled = false\ncommand = \"false\"\n";

#[derive(Clone)]
struct Mock {
    key: &'static str,
    tool: Arc<AtomicBool>,
    seen: mpsc::Sender<Value>,
}

async fn models(State(state): State<Mock>, headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(json!({"data":[{"id":"shared-model"}]})))
}

fn authorize(state: &Mock, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(format!("Bearer {}", state.key).as_str())
        || headers.contains_key(switchx::routing::LOCAL_TOKEN_HEADER)
        || headers.contains_key("chatgpt-account-id")
        || headers.contains_key("x-openai-account-id")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn responses(
    State(state): State<Mock>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    authorize(&state, &headers)?;
    state
        .seen
        .send(request)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let item = if state.tool.swap(false, Ordering::SeqCst) {
        json!({"type":"function_call","name":"exec_command","call_id":"call_route_probe","arguments":"{\"cmd\":\"cat probe.txt\",\"login\":false}"})
    } else {
        json!({"id":"msg_route_probe","type":"message","role":"assistant","content":[{"type":"output_text","text":"SWITCHX_ROUTED_OK","annotations":[]}]})
    };
    let done = json!({"type":"response.output_item.done","item":item});
    let complete = json!({"type":"response.completed","response":{"id":"resp_route_probe","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {complete}\n\n"
        ),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1);
    let desktop = matches!(mode.as_deref(), Some("--desktop" | "--desktop-recovery"));
    let root = std::env::temp_dir().join(format!(
        "switchx-routed-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir(&root)?;
    let data = root.join("data");
    let home = root.join("codex");
    std::fs::create_dir(&home)?;
    std::fs::write(home.join("config.toml"), ORIGINAL)?;
    std::fs::write(home.join("probe.txt"), "synthetic-tool-file")?;
    let mut tasks = Vec::new();
    let mut seen = Vec::new();
    let outcome = async {
        let templates: Value = serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
        for (index, (name, public_id, key)) in [
            ("Mock Alpha", "sx-mock-alpha", "synthetic-alpha-key"),
            ("Mock Beta", "sx-mock-beta", "synthetic-beta-key"),
        ].into_iter().enumerate() {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let (sender, receiver) = mpsc::channel(16);
            seen.push(receiver);
            let upstream = Router::new().route("/v1/models", get(models)).route("/v1/responses", post(responses))
                .with_state(Mock { key, tool: Arc::new(AtomicBool::new(index == 0)), seen: sender });
            tasks.push(tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap(); }));
            app::save_provider(&data, None, name, &format!("http://{address}/v1"), "shared-model", key.into())?;
            let provider = Store::open(&data.join("switchx.sqlite"))?.providers()?.into_iter().find(|provider| provider.name == name).unwrap();
            let mut metadata = templates["models"][index].clone();
            metadata["slug"] = "shared-model".into();
            let source = root.join(format!("model-{index}.json"));
            std::fs::write(&source, json!({"models":[metadata]}).to_string())?;
            app::save_model(&data, &provider.id, public_id, name, source.to_str().unwrap())?;
        }
        let helper = std::env::current_exe()?.parent().unwrap().parent().unwrap().join("switchx");
        if !helper.is_file() { return Err("build the switchx binary before running this probe".into()); }
        if desktop {
            if mode.as_deref() == Some("--desktop-recovery") {
                let publication = switchx::catalog::publish_saved(&Store::open(&data.join("switchx.sqlite"))?.models()?)?;
                config_transaction::PreparedSwitch::inspect(&home.join("config.toml"), &data, &publication, "127.0.0.1:18731".parse()?, "sx-mock-alpha")?.apply()?;
            }
            println!("Desktop fixture: {}", root.display());
            println!("Catalog files: {} and {}", root.join("model-0.json").display(), root.join("model-1.json").display());
            let binary = std::env::var_os("SWITCHX_DESKTOP_BINARY").map(PathBuf::from).unwrap_or(helper);
            let mut child = Command::new(binary).env("SWITCHX_DATA_DIR", &data).env("CODEX_HOME", &home)
                .env("SWITCHX_CODEX_CLI", client::cli_executable()).current_dir(&home).kill_on_drop(true).spawn()?;
            tokio::select! {
                status = child.wait() => { if !status?.success() { return Err("desktop fixture exited unsuccessfully".into()); } }
                _ = tokio::signal::ctrl_c() => { child.kill().await?; let _ = child.wait().await; }
            }
            check(config_transaction::recovery(&data)?.is_none(), "desktop exited before restoring its route configuration")?;
            check(std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL, "desktop exit changed the original configuration")?;
            println!("Desktop exit left the original configuration restored and no route journal.");
        } else {
            headless(&data, &home, &helper, &mut seen).await?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }.await;
    if let Err(error) = &outcome {
        eprintln!("Acceptance failed: {error}");
    }
    let recovery = async {
        if config_transaction::recovery(&data)?.is_some() {
            RouteSession::default().restore(&data, &home).await?;
        }
        Ok::<_, String>(())
    }
    .await;
    for task in tasks {
        task.abort();
    }
    if let Err(error) = recovery {
        return Err(format!("{error}; fixture retained at {}", root.display()).into());
    }
    if data.join("switchx.sqlite").is_file() {
        for provider in Store::open(&data.join("switchx.sqlite"))?.providers()? {
            app::delete_provider(&data, &provider.id)?;
        }
    }
    std::fs::remove_dir_all(&root)?;
    outcome?;
    println!("Synthetic route fixture, configuration and credentials cleaned up.");
    Ok(())
}

async fn headless(
    data: &Path,
    home: &Path,
    helper: &Path,
    seen: &mut [mpsc::Receiver<Value>],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = RouteSession::default();
    let occupied = TcpListener::bind("127.0.0.1:0").await?;
    check(
        session
            .prepare(
                data,
                home,
                occupied.local_addr()?.port(),
                "sx-mock-alpha",
                helper,
            )
            .await
            .is_err(),
        "occupied port was not rejected",
    )?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL,
        "port conflict changed the config",
    )?;
    drop(occupied);

    let store = Store::open(&data.join("switchx.sqlite"))?;
    let original_model = store.models()?.remove(0);
    let mut invalid_model = original_model.clone();
    let mut invalid_metadata: Value = serde_json::from_str(&invalid_model.metadata)?;
    invalid_metadata["model_messages"]["instructions_template"] = 42.into();
    invalid_model.metadata = invalid_metadata.to_string();
    store.put_model(&invalid_model)?;
    check(
        session
            .prepare(data, home, 0, "sx-mock-alpha", helper)
            .await
            .is_err(),
        "incompatible metadata was not rejected by Codex",
    )?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL,
        "catalog failure changed the config",
    )?;
    store.put_model(&original_model)?;

    session
        .prepare(data, home, 0, "sx-mock-alpha", helper)
        .await?;
    let original_provider = store.providers()?.remove(0);
    let mut changed_provider = original_provider.clone();
    changed_provider.name = "Changed after preview".into();
    store.put_provider(&changed_provider)?;
    check(
        session.apply(data, home, 0, "sx-mock-alpha").await.is_err(),
        "stale preview was accepted",
    )?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL,
        "stale preview changed the config",
    )?;
    store.put_provider(&original_provider)?;

    println!(
        "{}",
        session
            .prepare(data, home, 0, "sx-mock-alpha", helper)
            .await?
    );
    session.apply(data, home, 0, "sx-mock-alpha").await?;
    check(session.is_running(), "route failed to start")?;
    check(
        app::select_model(data, &original_model.provider_id, false).is_err(),
        "active route allowed model edits",
    )?;
    let active = std::fs::read_to_string(home.join("config.toml"))?;
    check(
        active.contains("local-token") && !active.contains("env_key"),
        "generated config did not use the local token helper",
    )?;
    for secret in ["synthetic-alpha-key", "synthetic-beta-key"] {
        check(
            !active.contains(secret)
                && !std::fs::read_to_string(data.join("switch-journal.json"))?.contains(secret)
                && !String::from_utf8_lossy(&std::fs::read(data.join("switchx.sqlite"))?)
                    .contains(secret),
            "provider credential leaked into a metadata file",
        )?;
    }
    run_cli(home, "sx-mock-alpha").await?;
    let first = timeout(Duration::from_secs(3), seen[0].recv())
        .await?
        .ok_or("first upstream was not called")?;
    let second = timeout(Duration::from_secs(3), seen[0].recv())
        .await?
        .ok_or("tool result was not forwarded")?;
    check(
        first["model"] == "shared-model"
            && second["model"] == "shared-model"
            && second["input"].to_string().contains("synthetic-tool-file")
            && seen[1].try_recv().is_err(),
        "first model did not complete an isolated tool round trip",
    )?;
    run_cli(home, "sx-mock-beta").await?;
    let request = timeout(Duration::from_secs(3), seen[1].recv())
        .await?
        .ok_or("second upstream was not called")?;
    check(
        request["model"] == "shared-model" && seen[0].try_recv().is_err(),
        "second model used the wrong upstream",
    )?;
    println!(
        "Both aliases reached their own upstream; the first completed a file-tool round trip."
    );
    let records = store.requests(100)?;
    check(
        records.len() == 3
            && records.iter().all(|record| {
                record.status == switchx::storage::RequestStatus::Completed
                    && record.http_status == Some(200)
                    && record.first_event_ms.is_some()
                    && record.error_code.is_none()
                    && record.generation.starts_with("catalog-")
            }),
        "CLI requests did not produce three completed metadata records",
    )?;
    for (public_id, expected) in [("sx-mock-alpha", 2), ("sx-mock-beta", 1)] {
        let model = store
            .models()?
            .into_iter()
            .find(|model| model.public_id == public_id)
            .unwrap();
        check(
            records
                .iter()
                .filter(|record| {
                    record.public_model.as_deref() == Some(public_id)
                        && record.provider_id.as_deref() == Some(model.provider_id.as_str())
                        && record.upstream_model.as_deref() == Some("shared-model")
                })
                .count()
                == expected,
            "request record used the wrong model or provider",
        )?;
    }
    println!(
        "Three CLI requests recorded completion, timings, model/provider mappings and catalog generation."
    );

    std::fs::write(
        home.join("config.toml"),
        format!(
            "{}\n# external note\n",
            active.replace("model = \"sx-mock-alpha\"", "model = \"external-choice\"")
        ),
    )?;
    check(
        session.restore(data, home).await.is_err()
            && session.is_running()
            && config_transaction::recovery(data)?.is_some(),
        "recovery conflict did not preserve the running route and journal",
    )?;
    let conflicted = std::fs::read_to_string(home.join("config.toml"))?;
    check(
        conflicted.contains("external-choice"),
        "recovery overwrote the external model choice",
    )?;
    std::fs::write(
        home.join("config.toml"),
        conflicted.replace("model = \"external-choice\"", "model = \"original-model\""),
    )?;
    session.restore(data, home).await?;
    check(
        !session.is_running()
            && config_transaction::recovery(data)?.is_none()
            && std::fs::read_to_string(home.join("config.toml"))?
                == format!("{ORIGINAL}\n# external note\n"),
        "route recovery did not restore the config and stop the route",
    )?;
    println!(
        "Port conflicts, invalid catalog, stale preview and recovery conflicts were handled without losing user settings."
    );
    Ok(())
}

fn check(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}

async fn run_cli(home: &Path, model: &str) -> Result<(), Box<dyn std::error::Error>> {
    let output = timeout(
        Duration::from_secs(40),
        Command::new(client::cli_executable())
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--skip-git-repo-check",
                "-s",
                "read-only",
                "-m",
                model,
                "Read probe.txt if requested by the model, then reply SWITCHX_ROUTED_OK.",
            ])
            .current_dir(home)
            .env("CODEX_HOME", home)
            .env_remove("SWITCHX_LOCAL_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_BASE_URL")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let completed = events.iter().any(|event| event["type"] == "turn.completed");
    let answered = events.iter().any(|event| {
        event["type"] == "item.completed"
            && event["item"]["type"] == "agent_message"
            && event["item"]["text"] == "SWITCHX_ROUTED_OK"
    });
    if !output.status.success() || !completed || !answered {
        return Err(format!(
            "isolated Codex did not complete {model}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}
