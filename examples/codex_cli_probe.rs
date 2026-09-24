use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, http::header, routing::post};
use serde_json::{Value, json};
use switchx::{
    catalog::{Selection, publish},
    config::preview_route,
    routing::{RouterState, Upstream, serve},
};
use tokio::{net::TcpListener, process::Command, sync::mpsc, time::timeout};

#[derive(Clone)]
struct FakeState {
    seen: mpsc::Sender<Captured>,
    call_tool: Arc<AtomicBool>,
}

struct Captured {
    model: String,
    input: Value,
}

async fn fake_response(
    State(state): State<FakeState>,
    Json(request): Json<Value>,
) -> ([(header::HeaderName, &'static str); 1], String) {
    let model = request["model"].as_str().unwrap_or("missing").to_owned();
    state
        .seen
        .send(Captured {
            model: model.clone(),
            input: request["input"].clone(),
        })
        .await
        .unwrap();
    let item = if state.call_tool.swap(false, Ordering::SeqCst) {
        json!({
            "type": "function_call",
            "name": "exec_command",
            "call_id": "call_switchx_probe",
            "arguments": "{\"cmd\":\"cat probe.txt\",\"login\":false}"
        })
    } else {
        json!({
            "id": "msg_switchx_probe",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "hello-switchx", "annotations": []}]
        })
    };
    let item_event = json!({"type": "response.output_item.done", "item": item});
    let complete_event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_switchx_probe",
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {item_event}\n\nevent: response.completed\ndata: {complete_event}\n\n"
        ),
    )
}

async fn fake_upstream(
    path: &'static str,
    call_tool: bool,
) -> (
    String,
    mpsc::Receiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel(4);
    let app = Router::new()
        .route(path, post(fake_response))
        .with_state(FakeState {
            seen: tx,
            call_tool: Arc::new(AtomicBool::new(call_tool)),
        });
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        format!(
            "http://{address}{}",
            if path == "/responses" { "/" } else { "/v1" }
        ),
        rx,
        task,
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (deepseek_url, mut deepseek_seen, deepseek_task) = fake_upstream("/responses", true).await;
    let (openai_url, mut openai_seen, openai_task) = fake_upstream("/v1/responses", false).await;
    let templates = serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let publication = publish(
        &templates,
        &[
            Selection {
                public_id: "sx-ds-flash",
                display_name: "DeepSeek · Flash",
                provider_id: "deepseek",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "sx-oai-coding",
                display_name: "OpenAI · Coding",
                provider_id: "openai-api",
                upstream_model: "gpt-5.5",
            },
        ],
    )?;
    let home = std::env::temp_dir().join(format!("switchx-cli-probe-{}", std::process::id()));
    std::fs::create_dir(&home)?;
    std::fs::write(home.join("probe.txt"), "hello-switchx")?;
    let catalog_path = home.join("catalog.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec_pretty(&publication.catalog)?,
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = preview_route("", &publication, &catalog_path, address, "sx-ds-flash")?;
    std::fs::write(home.join("config.toml"), config.proposed)?;
    let token = "synthetic-local-token-for-codex-cli-probe";
    let upstreams = HashMap::from([
        (
            "deepseek".into(),
            Upstream::new(&deepseek_url, "deepseek-test-key".into())?,
        ),
        (
            "openai-api".into(),
            Upstream::new(&openai_url, "openai-test-key".into())?,
        ),
    ]);
    let state = RouterState::new(address, token.into(), publication, upstreams)?;
    let router_task = tokio::spawn(async move { serve(listener, state).await.unwrap() });

    let first = run_probe(&home, token, "sx-ds-flash", true, &mut deepseek_seen).await;
    let result = if first.is_ok() {
        run_probe(&home, token, "sx-oai-coding", false, &mut openai_seen).await
    } else {
        first
    };
    router_task.abort();
    deepseek_task.abort();
    openai_task.abort();
    std::fs::remove_dir_all(&home)?;
    result?;
    println!("Codex CLI sent both aliases through the same loopback provider");
    Ok(())
}

async fn run_probe(
    home: &PathBuf,
    token: &str,
    public_model: &str,
    expect_tool: bool,
    seen: &mut mpsc::Receiver<Captured>,
) -> Result<(), Box<dyn std::error::Error>> {
    let prompt = if expect_tool {
        "Read probe.txt with a tool and report its contents."
    } else {
        "Reply exactly hello-switchx."
    };
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
            public_model,
            prompt,
        ])
        .current_dir(home)
        .env("CODEX_HOME", home)
        .env("SWITCHX_LOCAL_TOKEN", token)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(30), command.output()).await??;
    let actual = timeout(Duration::from_secs(3), seen.recv())
        .await?
        .ok_or("upstream did not receive a request")?;
    println!(
        "{public_model} -> {}; CLI exit: {}",
        actual.model, output.status
    );
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout).contains("hello-switchx")
    {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return Err("Codex CLI did not finish the synthetic response".into());
    }
    if expect_tool {
        let second = timeout(Duration::from_secs(3), seen.recv())
            .await?
            .ok_or("Codex did not send a tool result to the upstream")?;
        if !second.input.to_string().contains("hello-switchx") {
            return Err("second request did not include the file tool result".into());
        }
        println!("{public_model} -> tool result forwarded on second request");
    }
    Ok(())
}
