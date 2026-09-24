//! Isolated Codex login transport probe with a local mock response.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use switchx::{
    catalog::{Selection, publish},
    routing::LOCAL_TOKEN_HEADER,
};
use tokio::{
    net::TcpListener,
    process::Command,
    sync::mpsc::{self, Sender},
    time::timeout,
};

const CODEX_VERSION: &str = "@openai/codex@0.156.1";
const TOKEN_ENV: &str = "SWITCHX_PROBE_TOKEN";

struct ProbeHome(PathBuf);

impl ProbeHome {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "failed to generate probe directory")?;
        let suffix = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("switchx-chatgpt-probe-{suffix}"));
        let mut builder = fs::DirBuilder::new();
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
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!(
                "Could not remove isolated probe data at {}: {error}",
                self.0.display()
            );
        }
    }
}

#[derive(Clone)]
struct ProbeState {
    host: String,
    local_token: String,
    observed: Sender<ProbeEvent>,
}

#[derive(Debug, PartialEq, Eq)]
enum ProbeEvent {
    Accepted { x_openai_account_id_present: bool },
    Rejected(&'static str),
    UnexpectedPath(&'static str),
}

async fn capture_request(
    State(state): State<ProbeState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.host.as_str()) {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("unexpected Host"));
        return StatusCode::FORBIDDEN.into_response();
    }
    if headers.contains_key(header::ORIGIN) {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("unexpected Origin"));
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut local_values = headers.get_all(LOCAL_TOKEN_HEADER).iter();
    let local_valid = local_values
        .next()
        .filter(|_| local_values.next().is_none())
        .is_some_and(|value| {
            value
                .as_bytes()
                .ct_eq(state.local_token.as_bytes())
                .unwrap_u8()
                == 1
        });
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !local_valid {
        let _ = state.observed.try_send(ProbeEvent::Rejected(
            "missing or invalid local token header",
        ));
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if bearer.is_none_or(|value| value.is_empty() || value == state.local_token) {
        let _ = state.observed.try_send(ProbeEvent::Rejected(
            "missing or invalid Codex bearer header",
        ));
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok());
    if encoding.is_some_and(|value| value.eq_ignore_ascii_case("zstd"))
        || body.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
    {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("zstd-compressed request body"));
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    if encoding.is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("unsupported request content encoding"));
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(body) = serde_json::from_slice::<Value>(&body) else {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("invalid JSON body"));
        return StatusCode::BAD_REQUEST.into_response();
    };
    if body["model"] != "sx-oai-coding" {
        let _ = state
            .observed
            .try_send(ProbeEvent::Rejected("unexpected model ID"));
        return StatusCode::BAD_REQUEST.into_response();
    }
    let _ = state.observed.try_send(ProbeEvent::Accepted {
        x_openai_account_id_present: headers.contains_key("x-openai-account-id"),
    });

    let item = json!({
        "id": "msg_switchx_auth_probe",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "output_text", "text": "hello-switchx", "annotations": []}]
    });
    let item_event = json!({"type": "response.output_item.done", "item": item});
    let complete_event = json!({
        "type": "response.completed",
        "response": {
            "id": "resp_switchx_auth_probe",
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        }
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!(
            "event: response.output_item.done\ndata: {item_event}\n\nevent: response.completed\ndata: {complete_event}\n\n"
        ),
    )
        .into_response()
}

async fn unexpected_request(State(state): State<ProbeState>, uri: Uri) -> StatusCode {
    let path = match uri.path() {
        "/v1/models" => "/v1/models",
        "/v1/responses/compact" => "/v1/responses/compact",
        "/responses" => "/responses",
        _ => "another local path",
    };
    let _ = state.observed.try_send(ProbeEvent::UnexpectedPath(path));
    StatusCode::NOT_FOUND
}

fn command(home: &Path) -> Command {
    let mut command = Command::new("npx");
    command
        .args(["-y", CODEX_VERSION])
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .current_dir(home)
        .kill_on_drop(true);
    command
}

fn synthetic_login(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut child = StdCommand::new("npx")
        .args(["-y", CODEX_VERSION, "login", "--with-api-key"])
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("synthetic login stdin unavailable")?
        .write_all(b"sk-switchx-synthetic-probe\n")?;
    if !child.wait()?.success() {
        return Err("synthetic Codex login failed".into());
    }
    Ok(())
}

async fn run_probe(home: &Path, synthetic: bool) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    if synthetic {
        synthetic_login(home)?;
    } else {
        eprintln!("Complete the official ChatGPT sign-in in the browser.");
        let login = command(home)
            .arg("login")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
        if !login.success() {
            return Err("isolated Codex login did not complete".into());
        }
        let status = command(home).args(["login", "status"]).output().await?;
        let status_text = format!(
            "{}{}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        )
        .to_lowercase();
        if !status.status.success() || !status_text.contains("chatgpt") {
            return Err("isolated Codex login is not a ChatGPT session".into());
        }
    }
    if !fs::symlink_metadata(home.join("auth.json"))?.is_file() {
        return Err("isolated Codex credentials were not stored in the temporary directory".into());
    }

    let templates = serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-oai-coding",
            display_name: "OpenAI · Coding",
            provider_id: "official-probe",
            upstream_model: "gpt-5.5",
        }],
    )?;
    let catalog_path = home.join("catalog.json");
    fs::write(&catalog_path, serde_json::to_vec(&publication.catalog)?)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "failed to generate local token")?;
    let local_token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let (observed_tx, mut observed_rx) = mpsc::channel(16);
    let app = Router::new()
        .route("/v1/responses", post(capture_request))
        .fallback(unexpected_request)
        .with_state(ProbeState {
            host: address.to_string(),
            local_token: local_token.clone(),
            observed: observed_tx,
        });
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let catalog_literal =
        toml_edit::Value::from(catalog_path.to_str().ok_or("invalid path")?).to_string();
    let config = format!(
        "cli_auth_credentials_store = \"file\"\nmodel = \"sx-oai-coding\"\nmodel_provider = \"switchx_official_probe\"\nmodel_catalog_json = {catalog_literal}\n\n[features]\nenable_request_compression = false\n\n[model_providers.switchx_official_probe]\nname = \"OpenAI\"\nbase_url = \"http://{address}/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n\n[model_providers.switchx_official_probe.env_http_headers]\n{LOCAL_TOKEN_HEADER} = \"{TOKEN_ENV}\"\n"
    );
    fs::write(home.join("config.toml"), config)?;
    let output = timeout(
        Duration::from_secs(60),
        command(home)
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--skip-git-repo-check",
                "-s",
                "read-only",
                "-m",
                "sx-oai-coding",
                "Reply exactly hello-switchx.",
            ])
            .env(TOKEN_ENV, &local_token)
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "Codex exec timed out after 60 seconds")??;
    let first_event = timeout(Duration::from_secs(1), observed_rx.recv())
        .await
        .ok()
        .flatten();
    let mut events = first_event.into_iter().collect::<Vec<_>>();
    while let Ok(event) = observed_rx.try_recv() {
        events.push(event);
    }
    server.abort();
    let x_openai_account_id_present = events.iter().find_map(|event| match event {
        ProbeEvent::Accepted {
            x_openai_account_id_present,
        } => Some(*x_openai_account_id_present),
        _ => None,
    });
    let Some(x_openai_account_id_present) = x_openai_account_id_present else {
        let local_result = events.iter().find_map(|event| match event {
            ProbeEvent::Rejected(reason) => Some(format!("local mock rejected request: {reason}")),
            ProbeEvent::UnexpectedPath(path) => {
                Some(format!("Codex requested {path} instead of /v1/responses"))
            }
            _ => None,
        });
        let result = local_result.unwrap_or_else(|| "no request reached the local mock".into());
        let event_types = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .filter_map(|event| event["type"].as_str().map(str::to_owned))
            .take(12)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{result}; Codex exit: {}; event types: [{event_types}]; stderr bytes: {}",
            output.status,
            output.stderr.len()
        )
        .into());
    };
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout).contains("hello-switchx")
    {
        return Err(format!(
            "Codex reached the local mock but did not complete its response; exit: {}",
            output.status
        )
        .into());
    }
    println!(
        "{} reached the local probe with separate local authentication; x-openai-account-id present: {x_openai_account_id_present}. The local mock forwarded no model request.",
        if synthetic {
            "Synthetic API key"
        } else {
            "ChatGPT login"
        }
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let synthetic = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--synthetic") => true,
        Some(_) => {
            return Err("usage: cargo run --example chatgpt_auth_probe -- [--synthetic]".into());
        }
    };
    let home = ProbeHome::new()?;
    tokio::select! {
        result = run_probe(&home.0, synthetic) => result,
        interrupt = tokio::signal::ctrl_c() => {
            interrupt?;
            Err("probe interrupted; isolated data removed".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compressed_request_is_identified_before_json_parsing() {
        let (sender, mut observed) = mpsc::channel(1);
        let state = ProbeState {
            host: "127.0.0.1:18731".into(),
            local_token: "synthetic-local-token".into(),
            observed: sender,
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:18731".parse().unwrap());
        headers.insert(LOCAL_TOKEN_HEADER, "synthetic-local-token".parse().unwrap());
        headers.insert(
            header::AUTHORIZATION,
            "Bearer synthetic-chatgpt".parse().unwrap(),
        );
        headers.insert(header::CONTENT_ENCODING, "zstd".parse().unwrap());
        let response = capture_request(
            State(state),
            headers,
            Bytes::from_static(&[0x28, 0xb5, 0x2f, 0xfd]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            observed.recv().await,
            Some(ProbeEvent::Rejected("zstd-compressed request body"))
        );
    }

    #[tokio::test]
    async fn rejected_local_request_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, mut observed) = mpsc::channel(1);
        let app = Router::new()
            .route("/v1/responses", post(capture_request))
            .with_state(ProbeState {
                host: address.to_string(),
                local_token: "synthetic-local-token".into(),
                observed: sender,
            });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth("synthetic-official-auth")
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"model": "sx-oai-coding"}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            timeout(Duration::from_millis(200), observed.recv())
                .await
                .unwrap(),
            Some(ProbeEvent::Rejected(
                "missing or invalid local token header"
            ))
        );
        server.abort();
    }

    #[tokio::test]
    async fn multiple_requests_finish_without_draining_observations() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, _observed) = mpsc::channel(1);
        let app = Router::new()
            .route("/v1/responses", post(capture_request))
            .with_state(ProbeState {
                host: address.to_string(),
                local_token: "synthetic-local-token".into(),
                observed: sender,
            });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for _ in 0..2 {
            let response = timeout(
                Duration::from_millis(300),
                client
                    .post(format!("http://{address}/v1/responses"))
                    .bearer_auth("synthetic-official-auth")
                    .header(LOCAL_TOKEN_HEADER, "synthetic-local-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json!({"model": "sx-oai-coding"}).to_string())
                    .send(),
            )
            .await
            .expect("a second request must not wait for observation draining")
            .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        server.abort();
    }
}
