//! Isolated native-route acceptance with synthetic credentials and two local upstreams.
//! `--desktop` leaves the same fixture open for native UI checks until the app exits.
//! `--desktop-recovery` starts with a journal to check recovery on native Command-Q.
//! `--fallback` verifies one explicit backup after disconnecting the primary.
//! `--desktop-fallback` uses compatible templates for editing the backup in the UI.
//! `--live-probe` checks the real-route probe against these synthetic upstreams.
//! `--models` checks two distinct models on one provider, including a manual catalog.
//! `--desktop-models` opens that fixture for discovery and mapping UI checks.
//! `--http-only` checks production forwarding without a separate CLI keychain reader.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
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
use switchx::{
    app, client, config_transaction,
    credentials::{CredentialStore, ROUTER_TOKEN_SERVICE},
    routed::RouteSession,
    storage::Store,
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, time::timeout};

const ORIGINAL: &str = "model = \"original-model\" # keep\napproval_policy = \"never\"\n\n[mcp_servers.synthetic]\nenabled = false\ncommand = \"false\"\n";

#[derive(Clone)]
struct Mock {
    key: &'static str,
    tool: Arc<AtomicBool>,
    seen: mpsc::Sender<Value>,
    live_probe: bool,
    live_mode: Arc<AtomicU8>,
}

async fn models(State(state): State<Mock>, headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    authorize(&state, &headers)?;
    Ok(Json(
        json!({"data":[{"id":"shared-model"}, {"id":"extra-model"}, {"id":"manual-model"}]}),
    ))
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
    let item = if state.live_probe {
        live_item(&request)?
    } else if state.tool.swap(false, Ordering::SeqCst) {
        json!({"type":"function_call","name":"exec_command","call_id":"call_route_probe","arguments":"{\"cmd\":\"cat probe.txt\",\"login\":false}"})
    } else {
        json!({"id":"msg_route_probe","type":"message","role":"assistant","content":[{"type":"output_text","text":"SWITCHX_ROUTED_OK","annotations":[]}]})
    };
    state
        .seen
        .send(request)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if state.live_probe {
        match state.live_mode.load(Ordering::SeqCst) {
            1 => {
                return Ok(sse_item(
                    json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"unexpected-answer","annotations":[]}]}),
                ));
            }
            2 => std::future::pending::<()>().await,
            _ => {}
        }
    }
    Ok(sse_item(item))
}

fn sse_item(item: Value) -> ([(header::HeaderName, &'static str); 1], String) {
    let done = json!({"type":"response.output_item.done","item":item});
    let complete = json!({"type":"response.completed","response":{"id":"resp_route_probe","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {complete}\n\n"
        ),
    )
}

fn live_item(request: &Value) -> Result<Value, StatusCode> {
    let input = request["input"].to_string();
    if let Some(marker) = ["SWITCHX_TOOL_", "SWITCHX_ANSWER_"]
        .into_iter()
        .find_map(|prefix| {
            let start = input.find(prefix)?;
            Some(
                input[start..]
                    .chars()
                    .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .collect::<String>(),
            )
        })
    {
        return Ok(
            json!({"id":"msg_live_fixture","type":"message","role":"assistant","content":[{"type":"output_text","text":marker,"annotations":[]}]}),
        );
    }
    if input.contains("Read acceptance.txt") {
        return Ok(
            json!({"type":"function_call","name":"exec_command","call_id":"call_live_fixture","arguments":"{\"cmd\":\"cat acceptance.txt\",\"login\":false}"}),
        );
    }
    Err(StatusCode::BAD_REQUEST)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1);
    let desktop = matches!(
        mode.as_deref(),
        Some("--desktop" | "--desktop-recovery" | "--desktop-fallback" | "--desktop-models")
    );
    let multiple = matches!(mode.as_deref(), Some("--models" | "--desktop-models"));
    let fallback = matches!(mode.as_deref(), Some("--fallback" | "--desktop-fallback"));
    let live_probe = mode.as_deref() == Some("--live-probe");
    let http_only = mode.as_deref() == Some("--http-only");
    let live_mode = Arc::new(AtomicU8::new(0));
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
                .with_state(Mock { key, tool: Arc::new(AtomicBool::new(index == 0 || fallback)), seen: sender, live_probe, live_mode: live_mode.clone() });
            tasks.push(tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap(); }));
            app::save_provider(&data, None, name, &format!("http://{address}/v1"), "shared-model", key.into())?;
            let provider = Store::open(&data.join("switchx.sqlite"))?.providers()?.into_iter().find(|provider| provider.name == name).unwrap();
            let mut metadata = templates["models"][if fallback { 0 } else { index }].clone();
            metadata["slug"] = "shared-model".into();
            let source = root.join(format!("model-{index}.json"));
            std::fs::write(&source, json!({"models":[metadata]}).to_string())?;
            app::save_model(&data, &provider.id, public_id, name, source.to_str().unwrap())?;
            if multiple && index == 0 {
                app::save_mapping(&data, app::ModelInput {
                    provider_id: &provider.id, original_id: "", public_id: "sx-mock-alpha-extra",
                    display_name: "Alpha extra model", upstream_model: "extra-model", catalog_path: "",
                    settings: Some(switchx::catalog::MappingSettings {
                        context_window: "256000", reasoning_levels: Some("low, high"), default_reasoning: Some("high"),
                    }),
                })?;
            }
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
        } else if live_probe {
            live_probe_fixture(&data, &helper, &mut seen, &live_mode).await?;
        } else if fallback {
            fallback_probe(&data, &home, &helper, &mut seen, &mut tasks[0]).await?;
        } else {
            headless(&data, &home, &helper, &mut seen, multiple, http_only).await?;
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

async fn live_probe_fixture(
    data: &Path,
    helper: &Path,
    seen: &mut [mpsc::Receiver<Value>],
    live_mode: &AtomicU8,
) -> Result<(), Box<dyn std::error::Error>> {
    let binary = helper.parent().unwrap().join("examples/routed_live_probe");
    check(binary.is_file(), "build --example routed_live_probe first")?;
    let source = Store::open(&data.join("switchx.sqlite"))?;
    let mut models = source.models()?;
    let beta = models
        .iter()
        .find(|model| model.public_id == "sx-mock-beta")
        .ok_or("beta fixture was not saved")?
        .provider_id
        .clone();
    for model in &mut models {
        model.enabled = false;
        if model.public_id == "sx-mock-alpha" {
            model.fallback_provider_id = Some(beta.clone());
        }
        source.put_model(model)?;
    }
    drop(source);
    let original = std::fs::read(data.join("switchx.sqlite"))?;
    for failed in [false, true] {
        live_mode.store(u8::from(failed), Ordering::SeqCst);
        let output = Command::new(&binary)
            .args([data.to_str().unwrap(), "sx-mock-alpha", "sx-mock-beta"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await?;
        check(
            output.status.success() != failed,
            "live probe did not propagate its acceptance result",
        )?;
        verify_live_cleanup(&output.stdout)?;
        println!("{}", String::from_utf8(output.stdout)?);
        check(
            std::fs::read(data.join("switchx.sqlite"))? == original,
            "live probe changed its source database",
        )?;
    }
    for receiver in seen.iter_mut() {
        while receiver.try_recv().is_ok() {}
    }
    #[cfg(unix)]
    {
        live_mode.store(2, Ordering::SeqCst);
        let mut child = Command::new(&binary)
            .args([data.to_str().unwrap(), "sx-mock-alpha"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let pid = child.id().ok_or("live probe child has no PID")?;
        tokio::select! {
            request = timeout(Duration::from_secs(40), seen[0].recv()) => { request?.ok_or("interruption fixture was not called")?; }
            status = child.wait() => {
                status?;
                let output = child.wait_with_output().await?;
                eprintln!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                return Err("interruption fixture exited before sending its request".into());
            }
        }
        let status = Command::new("/bin/kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await?;
        check(
            status.success(),
            "could not interrupt the isolated live probe",
        )?;
        let output = timeout(Duration::from_secs(15), child.wait_with_output()).await??;
        check(
            !output.status.success(),
            "interrupted live probe reported success",
        )?;
        verify_live_cleanup(&output.stdout)?;
        check(
            std::fs::read(data.join("switchx.sqlite"))? == original,
            "interrupted live probe changed its source database",
        )?;
    }
    for provider in Store::open_read_only(&data.join("switchx.sqlite"))?.providers()? {
        let secret = app::provider_credential(data, &provider)?;
        let expected = if provider.name == "Mock Alpha" {
            "synthetic-alpha-key"
        } else {
            "synthetic-beta-key"
        };
        check(
            secret.expose() == expected,
            "live probe changed or removed the source credential",
        )?;
    }
    println!(
        "Live probe passed synthetic success, failed-answer and Ctrl-C cleanup checks; source database and credentials retained."
    );
    Ok(())
}

fn verify_live_cleanup(stdout: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let stdout = std::str::from_utf8(stdout)?;
    let root = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Isolated workspace: "))
        .ok_or("live probe did not report its isolated workspace")?;
    check(
        stdout.contains("Original isolated config restored exactly") && !Path::new(root).exists(),
        "live probe did not restore and remove its temporary workspace",
    )
}

async fn fallback_probe(
    data: &Path,
    home: &Path,
    helper: &Path,
    seen: &mut [mpsc::Receiver<Value>],
    primary_task: &mut tokio::task::JoinHandle<()>,
) -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open(&data.join("switchx.sqlite"))?;
    let providers = store.providers()?;
    let primary = &providers[0];
    let backup = &providers[1];
    app::save_fallback(data, "sx-mock-alpha", Some(&backup.id))?;
    app::select_model(data, "sx-mock-beta", false)?;
    let mut session = RouteSession::default();
    let preview = session
        .prepare(data, home, 0, "sx-mock-alpha", helper)
        .await?;
    check(
        preview.contains("备用") && preview.contains(&backup.name),
        "preview omitted the backup destination",
    )?;
    let mut changed = backup.clone();
    changed.base_url = "http://127.0.0.1:1/v1".into();
    store.put_provider(&changed)?;
    check(
        session.apply(data, home, 0, "sx-mock-alpha").await.is_err(),
        "changed backup bypassed stale preview check",
    )?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL,
        "stale backup preview changed config",
    )?;
    store.put_provider(backup)?;
    session
        .prepare(data, home, 0, "sx-mock-alpha", helper)
        .await?;
    session.apply(data, home, 0, "sx-mock-alpha").await?;
    check(
        app::save_fallback(data, "sx-mock-alpha", None).is_err(),
        "active route allowed candidate edits",
    )?;
    primary_task.abort();
    let _ = primary_task.await;
    run_cli(home, "sx-mock-alpha").await?;
    let first = timeout(Duration::from_secs(3), seen[1].recv())
        .await?
        .ok_or("backup was not called")?;
    let second = timeout(Duration::from_secs(3), seen[1].recv())
        .await?
        .ok_or("backup tool result was not forwarded")?;
    check(
        first["model"] == "shared-model"
            && second["model"] == "shared-model"
            && second["input"].to_string().contains("synthetic-tool-file")
            && seen[0].try_recv().is_err(),
        "fallback did not complete the isolated tool round trip",
    )?;
    let records = store.requests(100)?;
    check(
        records.len() == 2
            && records.iter().all(|record| {
                record.status == switchx::storage::RequestStatus::Completed
                    && record.public_model.as_deref() == Some("sx-mock-alpha")
                    && record.provider_id.as_deref() == Some(backup.id.as_str())
                    && record.fallback_from.as_deref() == Some(primary.id.as_str())
            }),
        "fallback records did not capture both tool rounds and destinations",
    )?;
    session.restore(data, home).await?;
    check(
        std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL
            && config_transaction::recovery(data)?.is_none(),
        "fallback route did not restore original configuration",
    )?;
    println!(
        "Explicit backup completed an isolated CLI file-tool round trip after primary disconnect; both requests recorded, stale backup preview rejected, configuration restored."
    );
    Ok(())
}

async fn headless(
    data: &Path,
    home: &Path,
    helper: &Path,
    seen: &mut [mpsc::Receiver<Value>],
    multiple: bool,
    http_only: bool,
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

    if multiple {
        let extra = store
            .models()?
            .into_iter()
            .find(|model| model.public_id == "sx-mock-alpha-extra")
            .unwrap();
        let mut missing = extra.clone();
        missing.upstream_model = "missing-model".into();
        let mut metadata: Value = serde_json::from_str(&missing.metadata)?;
        metadata["slug"] = "missing-model".into();
        missing.metadata = metadata.to_string();
        store.put_model(&missing)?;
        session
            .prepare(data, home, 0, "sx-mock-alpha", helper)
            .await?;
        check(
            session.apply(data, home, 0, "sx-mock-alpha").await.is_err(),
            "publication checked only the provider default and accepted a missing additional model",
        )?;
        check(
            std::fs::read_to_string(home.join("config.toml"))? == ORIGINAL
                && config_transaction::recovery(data)?.is_none(),
            "missing model changed the target configuration",
        )?;
        store.put_model(&extra)?;
    }

    println!(
        "{}",
        session
            .prepare(data, home, 0, "sx-mock-alpha", helper)
            .await?
    );
    session.apply(data, home, 0, "sx-mock-alpha").await?;
    check(session.is_running(), "route failed to start")?;
    check(
        app::select_model(data, &original_model.public_id, false).is_err(),
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
                && !std::fs::read_to_string(data.join("switch-journal.json"))?.contains(secret),
            "provider credential leaked into the generated config or recovery journal",
        )?;
        check(
            String::from_utf8_lossy(&std::fs::read(data.join("switchx.sqlite"))?).contains(secret),
            "provider credential was not saved in SQLite",
        )?;
    }
    if http_only {
        http_round_trip(data, &session, seen).await?;
    } else {
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
        if multiple {
            run_cli(home, "sx-mock-alpha-extra").await?;
            let request = timeout(Duration::from_secs(3), seen[0].recv())
                .await?
                .ok_or("extra model was not called")?;
            check(
                request["model"] == "extra-model" && seen[1].try_recv().is_err(),
                "second model on one provider used the wrong actual model or credential",
            )?;
            println!(
                "Manual catalog parsed by Codex; two models reached one provider with distinct actual model IDs."
            );
        }
    }
    let records = store.requests(100)?;
    for secret in ["synthetic-alpha-key", "synthetic-beta-key"] {
        check(
            !format!("{records:?}").contains(secret),
            "provider credential leaked into request records",
        )?;
    }
    check(
        records.len()
            == if http_only {
                2
            } else if multiple {
                4
            } else {
                3
            }
            && records.iter().all(|record| {
                record.status == switchx::storage::RequestStatus::Completed
                    && record.http_status == Some(200)
                    && record.first_event_ms.is_some()
                    && record.error_code.is_none()
                    && record.generation.starts_with("catalog-")
            }),
        "route requests did not produce the expected completed metadata records",
    )?;
    for (public_id, expected) in [
        ("sx-mock-alpha", if http_only { 1 } else { 2 }),
        ("sx-mock-beta", 1),
    ] {
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
    if multiple {
        check(
            records.iter().any(|record| {
                record.public_model.as_deref() == Some("sx-mock-alpha-extra")
                    && record.provider_id.as_deref() == Some(original_provider.id.as_str())
                    && record.upstream_model.as_deref() == Some("extra-model")
            }),
            "extra-model request metadata lost its mapping",
        )?;
    }
    println!(
        "Route requests recorded completion, timings, model/provider mappings and catalog generation."
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

async fn http_round_trip(
    data: &Path,
    session: &RouteSession,
    seen: &mut [mpsc::Receiver<Value>],
) -> Result<(), Box<dyn std::error::Error>> {
    let reference = config_transaction::recovery(data)?
        .and_then(|recovery| recovery.local_token_reference)
        .ok_or("synthetic route's local token reference is missing")?;
    // This is only the token that this probe created; no user credentials are read.
    let token = CredentialStore::new(ROUTER_TOKEN_SERVICE)?.get(&reference)?;
    let address = session.address().ok_or("synthetic route is not running")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    for (index, model) in [(0, "sx-mock-alpha"), (1, "sx-mock-beta")] {
        let response = client
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth(token.expose())
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model":model,"input":"synthetic-http-route","stream":true}).to_string())
            .send()
            .await?;
        check(
            response.status() == StatusCode::OK,
            "HTTP route rejected its local synthetic token",
        )?;
        check(
            response.text().await?.contains("response.completed"),
            "HTTP mock response did not complete",
        )?;
        let request = timeout(Duration::from_secs(3), seen[index].recv())
            .await?
            .ok_or("HTTP mock upstream was not called")?;
        check(
            request["model"] == "shared-model" && seen[1 - index].try_recv().is_err(),
            "HTTP public model used the wrong upstream or actual model",
        )?;
    }
    println!("Production RouteSession forwarded both HTTP models with their own SQLite API keys.");
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
            .env_remove("SWITCHX_DATA_DIR")
            .env_remove("SWITCHX_LOCAL_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("OPENAI_BASE_URL")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "127.0.0.1,localhost")
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
