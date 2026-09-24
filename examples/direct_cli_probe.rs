use std::{path::Path, process::Stdio, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use serde_json::{Value, json};
use switchx::{
    app,
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    storage::Store,
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, time::timeout};

#[derive(Clone)]
struct MockState(mpsc::Sender<String>);

async fn models(headers: HeaderMap) -> Result<Json<Value>, StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .is_none_or(|value| value != "Bearer synthetic-direct-key")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(Json(json!({"data": [{"id": "gpt-5.5"}]})))
}

async fn responses(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<([(header::HeaderName, &'static str); 1], String), StatusCode> {
    if headers
        .get(header::AUTHORIZATION)
        .is_none_or(|value| value != "Bearer synthetic-direct-key")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    state
        .0
        .send(request["model"].as_str().unwrap_or("").into())
        .await
        .unwrap();
    let item = json!({"type":"response.output_item.done","item":{"id":"msg_direct_probe","type":"message","role":"assistant","content":[{"type":"output_text","text":"hello-switchx","annotations":[]}]}});
    let complete = json!({"type":"response.completed","response":{"id":"resp_direct_probe","usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    Ok((
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {item}\n\nevent: response.completed\ndata: {complete}\n\n"
        ),
    ))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| "system randomness is unavailable")?;
    let root = std::env::temp_dir().join(format!(
        "switchx-direct-probe-{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ));
    std::fs::create_dir(&root)?;
    let result = run(&root).await;
    std::fs::remove_dir_all(&root)?;
    result
}

async fn run(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = root.join("data");
    let home = root.join("codex");
    std::fs::create_dir(&home)?;
    std::fs::write(
        home.join("config.toml"),
        "# keep\napproval_policy = \"never\"\n",
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (sender, mut seen) = mpsc::channel(2);
    let app = Router::new()
        .route("/v1/models", get(models))
        .route("/v1/responses", post(responses))
        .with_state(MockState(sender));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let outcome = async {
        app::save_provider(
            &data,
            None,
            "Mock direct",
            &format!("http://{address}/v1"),
            "gpt-5.5",
            "synthetic-direct-key".into(),
        )?;
        let provider = Store::open(&data.join("switchx.sqlite"))?
            .providers()?
            .pop()
            .ok_or("provider was not saved")?;
        let helper = std::env::current_exe()?
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("switchx");
        if !helper.is_file() {
            return Err::<String, Box<dyn std::error::Error>>(
                "build the switchx binary before running this probe".into(),
            );
        }
        direct::check_models(&provider, "synthetic-direct-key").await?;
        let prepared =
            PreparedDirectSwitch::inspect(&home.join("config.toml"), &data, &provider, &helper)?;
        prepared.apply()?;
        let mut command = Command::new("npx");
        command
            .args([
                "-y",
                "@openai/codex@0.156.1",
                "exec",
                "--ephemeral",
                "--skip-git-repo-check",
                "-s",
                "read-only",
                "-m",
                "gpt-5.5",
                "Reply exactly hello-switchx.",
            ])
            .current_dir(&home)
            .env("CODEX_HOME", &home)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(Duration::from_secs(40), command.output()).await??;
        let model = timeout(Duration::from_secs(3), seen.recv())
            .await?
            .ok_or("mock did not receive a request")?;
        if model != "gpt-5.5"
            || !output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains("hello-switchx")
        {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            return Err("Codex did not finish the direct mock request".into());
        }
        let restored = direct_config::restore(&home.join("config.toml"), &data)?;
        if !restored.conflicts.is_empty()
            || std::fs::read_to_string(home.join("config.toml"))?
                != "# keep\napproval_policy = \"never\"\n"
        {
            return Err("direct config was not restored".into());
        }
        println!(
            "Codex CLI used the SwitchX credential helper and sent a direct mock Responses request"
        );
        Ok::<String, Box<dyn std::error::Error>>(provider.id)
    }
    .await;
    task.abort();
    if let Ok(id) = &outcome {
        let _ = CredentialStore::new(PROVIDER_KEY_SERVICE).and_then(|store| store.delete(id));
    } else if let Ok(store) = Store::open(&data.join("switchx.sqlite"))
        && let Ok(providers) = store.providers()
    {
        for provider in providers {
            if let Some(reference) = provider.credential_ref {
                let _ = CredentialStore::new(PROVIDER_KEY_SERVICE)
                    .and_then(|store| store.delete(&reference));
            }
        }
    }
    outcome.map(|_| ())
}
