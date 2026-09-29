//! Native CLI + production router with synthetic ChatGPT auth and local upstreams only.
//! Never reads the user's Codex home, account, keys or real provider records.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
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
    app,
    catalog::{self, Selection},
    chatgpt, client,
    config_transaction::{self, PreparedSwitch},
    routed::RouteSession,
    routing::{LOCAL_TOKEN_HEADER, RouterState, RunningRouter, Upstream},
    storage::{ModelRecord, ProviderRecord, RequestStatus, Store},
};
use tokio::{
    net::TcpListener,
    process::Command,
    sync::{mpsc, watch},
    time::timeout,
};

const AUTH: &str = include_str!("../tests/fixtures/synthetic-chatgpt-auth.json");
const ORIGINAL: &str = "# isolated synthetic configuration\ncli_auth_credentials_store = \"file\"\nmodel = \"original-synthetic\"\nmodel_provider = \"original\"\n[model_providers.original]\nname = \"Original\"\nbase_url = \"http://127.0.0.1:1/v1\"\nwire_api = \"responses\"\n";

struct ProbeHome(PathBuf);
impl ProbeHome {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("switchx-subscription-probe-{}", app::new_id()?));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for ProbeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Mock {
    official: bool,
    authorization: String,
    seen: mpsc::Sender<Value>,
}

async fn response(
    State(state): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(state.authorization.as_str())
        || headers.contains_key(LOCAL_TOKEN_HEADER)
        || headers.contains_key(header::COOKIE)
        || headers.contains_key(chatgpt::ACCOUNT_HEADER) != state.official
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if state.official
        && headers
            .get(chatgpt::ACCOUNT_HEADER)
            .and_then(|value| value.to_str().ok())
            != Some("synthetic-workspace")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let tool_returned = body["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "function_call_output")
    });
    let item = if tool_returned {
        json!({"id":"msg_subscription_probe","type":"message","role":"assistant","content":[{"type":"output_text","text":"SWITCHX_SUBSCRIPTION_OK","annotations":[]}]})
    } else {
        json!({"type":"function_call","name":"exec_command","call_id":"call_subscription_probe","arguments":"{\"cmd\":\"cat probe.txt\",\"login\":false}"})
    };
    state
        .seen
        .send(body)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let done = json!({"type":"response.output_item.done","item":item});
    let completed = json!({"type":"response.completed","response":{"id":"resp_subscription_probe","status":"completed","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {done}\n\nevent: response.completed\ndata: {completed}\n\n"
        ),
    ))
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
                "Read probe.txt if requested by the model, then reply SWITCHX_SUBSCRIPTION_OK.",
            ])
            .current_dir(home)
            .env("CODEX_HOME", home)
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
    let answered = events.iter().any(|event| {
        event["type"] == "item.completed"
            && event["item"]["type"] == "agent_message"
            && event["item"]["text"] == "SWITCHX_SUBSCRIPTION_OK"
    });
    let completed = events.iter().any(|event| event["type"] == "turn.completed");
    if !output.status.success() || !answered || !completed {
        // The isolated process contains only synthetic credentials; still avoid printing them.
        let errors: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "error")
            .map(|event| event["message"].as_str().unwrap_or("CLI error"))
            .collect();
        return Err(format!("isolated {model} failed: {}", errors.join("; ")).into());
    }
    Ok(())
}

fn check(condition: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = ProbeHome::new()?;
    let home = root.0.join("codex");
    let data = root.0.join("data");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&data)?;
    std::fs::write(home.join("config.toml"), ORIGINAL)?;
    std::fs::write(home.join("auth.json"), AUTH)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            home.join("auth.json"),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    std::fs::write(home.join("probe.txt"), "synthetic-subscription-tool-file")?;
    // New app-server versions discover workspace routing even for account/read(false).
    let discovery = TcpListener::bind("127.0.0.1:0").await?;
    let discovery_address = discovery.local_addr()?;
    let discovery_mock = Router::new().route("/api/codex/accounts/check", get(|| async {
        Json(json!({"accounts":[{"id":"synthetic-workspace","plan_type":"plus","workspace_backend_origin":"https://chatgpt.com","account_routing_override":"NO_CONSTRAINT"}]}))
    })).fallback(|| async { Json(json!({})) });
    let discovery_task = tokio::spawn(async move {
        axum::serve(discovery, discovery_mock).await.unwrap();
    });
    let original = format!("{ORIGINAL}\n");
    // Insert at the root, before the original provider table.
    let original = original.replacen(
        "[model_providers.original]",
        &format!("chatgpt_base_url = \"http://{discovery_address}\"\n[model_providers.original]"),
        1,
    );
    std::fs::write(home.join("config.toml"), &original)?;
    check(
        chatgpt::account(&home, false).await? == chatgpt::AccountStatus::Chatgpt,
        "native CLI did not recognize the synthetic subscription",
    )?;
    let login = chatgpt::Session::start(&home).await?.login().await?;
    let (_cancel, receiver) = watch::channel(true);
    check(
        login.finish(receiver).await.is_err(),
        "native login cancellation was not delivered",
    )?;
    check(
        std::fs::read_to_string(home.join("auth.json"))? == AUTH,
        "native cancellation changed the synthetic prior auth",
    )?;
    println!(
        "Native app-server recognized synthetic ChatGPT auth; login was started and cancelled without opening a browser."
    );

    let native_models = chatgpt::catalog().await?;
    chatgpt::save_connection(&data, &native_models)?;
    let store = Store::open(&data.join("switchx.sqlite"))?;
    let selected = store
        .models()?
        .into_iter()
        .find(|model| model.enabled)
        .ok_or("no selected subscription model")?;
    let binary = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("SwitchX.app/Contents/MacOS/switchx");
    let mut preflight = RouteSession::default();
    let preview = preflight
        .prepare(&data, &home, 0, &selected.public_id, &binary)
        .await?;
    check(
        preview.contains("已选工作区的官方目的地：https://chatgpt.com"),
        "production preflight discarded native workspace routing",
    )?;
    preflight.discard_preview();
    let metadata = native_models
        .first()
        .ok_or("native catalog was empty")?
        .clone();
    let official_model = metadata["slug"]
        .as_str()
        .ok_or("model slug missing")?
        .to_owned();
    let mut api_metadata = metadata.clone();
    api_metadata["slug"] = "synthetic-api-model".into();
    let publication = catalog::publish(
        &json!({"models":[metadata, api_metadata]}),
        &[
            Selection {
                public_id: "sx-subscription",
                display_name: "Synthetic subscription",
                provider_id: "official",
                upstream_model: &official_model,
            },
            Selection {
                public_id: "sx-api",
                display_name: "Synthetic API",
                provider_id: "api",
                upstream_model: "synthetic-api-model",
            },
        ],
    )?;
    let auth: Value = serde_json::from_str(AUTH)?;
    let mut upstreams = HashMap::new();
    let mut tasks = Vec::new();
    let mut seen = Vec::new();
    for (official, id, key) in [
        (
            true,
            "official",
            auth["tokens"]["access_token"].as_str().unwrap(),
        ),
        (false, "api", "synthetic-api-only-key"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (sender, receiver) = mpsc::channel(16);
        seen.push(receiver);
        let app = Router::new()
            .route("/responses", post(response))
            .with_state(Mock {
                official,
                authorization: format!("Bearer {key}"),
                seen: sender,
            });
        tasks.push(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        upstreams.insert(
            id.into(),
            if official {
                Upstream::chatgpt_mock(address)?
            } else {
                Upstream::new(&format!("http://{address}"), key.into())?
            },
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let local_token = format!("{}{}", app::new_id()?, app::new_id()?);
    let reference = format!("router-{}", app::new_id()?);
    let prepared = PreparedSwitch::inspect(
        &home.join("config.toml"),
        &data,
        &publication,
        address,
        "sx-subscription",
    )?
    .with_chatgpt_auth(&local_token, &reference)?;
    let state = RouterState::new(address, local_token, publication, upstreams)?.with_request_log(
        Store::open(&data.join("switchx.sqlite"))?,
        "native-subscription-probe".into(),
    );
    let running = RunningRouter::start(listener, state)?;
    prepared.apply()?;
    let outcome = async {
        for (index, model, expected_upstream) in [(0, "sx-subscription", official_model.as_str()), (1, "sx-api", "synthetic-api-model")] {
            run_cli(&home, model).await?;
            let first = timeout(Duration::from_secs(3), seen[index].recv()).await?.ok_or("first request not captured")?;
            let second = timeout(Duration::from_secs(3), seen[index].recv()).await?.ok_or("tool-result request not captured")?;
            check(first["model"] == expected_upstream && second["model"] == expected_upstream && second["input"].to_string().contains("synthetic-subscription-tool-file"), "explicit route or native tool-result round trip failed")?;
            check(seen[1 - index].try_recv().is_err(), "request reached the other account/provider")?;
        }
        let records = store.requests(100)?;
        check(records.len() == 4 && records.iter().all(|record| record.status == RequestStatus::Completed), "native tool rounds were not recorded as completed")?;
        println!("Native CLI completed subscription and API file-tool round trips through the production router; both auth paths remained isolated and four requests completed.");
        Ok::<_, Box<dyn std::error::Error>>(())
    }.await;
    let restored = config_transaction::restore(&home.join("config.toml"), &data)?;
    check(
        restored.conflicts.is_empty()
            && std::fs::read_to_string(home.join("config.toml"))? == original,
        "original config did not restore exactly",
    )?;
    check(
        std::fs::read_to_string(home.join("auth.json"))? == AUTH,
        "configuration restoration overwrote native auth",
    )?;
    running.stop().await;
    for task in tasks {
        task.abort();
    }
    outcome?;
    if std::env::args().nth(1).as_deref() == Some("--desktop") {
        check(
            binary.is_file(),
            "build the macOS bundle before desktop verification",
        )?;
        let api_id = format!("probe-api-{}", app::new_id()?);
        store.put_provider(&ProviderRecord {
            kind: switchx::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
            id: api_id.clone(),
            name: "Synthetic API".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            model_id: "synthetic-api-model".into(),
            credential_ref: None,
        })?;
        let mut api_metadata = native_models[0].clone();
        api_metadata["slug"] = "synthetic-api-model".into();
        store.put_model(&ModelRecord {
            provider_id: api_id,
            public_id: "sx-api".into(),
            display_name: "synthetic-api-model/Synthetic API".into(),
            upstream_model: "synthetic-api-model".into(),
            metadata: api_metadata.to_string(),
            enabled: true,
            fallback_provider_id: None,
        })?;
        let active_publication = catalog::publish_saved(
            &store
                .models()?
                .into_iter()
                .filter(|model| model.provider_id == chatgpt::PROVIDER_ID)
                .collect::<Vec<_>>(),
        )?;
        PreparedSwitch::inspect(
            &home.join("config.toml"),
            &data,
            &active_publication,
            "127.0.0.1:1".parse()?,
            &selected.public_id,
        )?
        .with_chatgpt_auth(&"a".repeat(64), &format!("router-{}", app::new_id()?))?
        .apply()?;
        println!(
            "Desktop fixture ready at {}; verify recovery and the API-only preview, then quit SwitchX. The synthetic API has no key and cannot be activated.",
            root.0.display()
        );
        let status = Command::new(&binary)
            .env("SWITCHX_DATA_DIR", &data)
            .env("CODEX_HOME", &home)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()?
            .wait()
            .await?;
        check(
            status.success() && config_transaction::recovery(&data)?.is_none(),
            "desktop recovery did not complete",
        )?;
        check(
            std::fs::read_to_string(home.join("config.toml"))? == original
                && std::fs::read_to_string(home.join("auth.json"))? == AUTH,
            "desktop changed original config or native auth",
        )?;
        let enabled = store
            .models()?
            .into_iter()
            .filter(|model| model.enabled)
            .collect::<Vec<_>>();
        check(
            enabled.len() == 1 && enabled[0].public_id == "sx-api",
            "desktop API return did not retain only the selected API model",
        )?;
    }
    discovery_task.abort();
    println!(
        "Isolated config restored, synthetic auth preserved, temporary files and listeners cleaned up. No real account or provider request was used."
    );
    Ok(())
}
