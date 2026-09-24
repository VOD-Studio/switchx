use std::{
    collections::HashMap,
    io::{self, IsTerminal},
    path::PathBuf,
    process::{Command as StdCommand, Stdio},
    time::Duration,
};

use reqwest::Client;
use serde_json::{Value, json};
use switchx::{
    catalog::{Selection, publish},
    config::preview_route,
    routing::{RouterState, Upstream, serve},
};
use tokio::{net::TcpListener, process::Command, time::timeout};
use zeroize::Zeroize;

struct ProbeHome(PathBuf);

impl Drop for ProbeHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct EchoGuard(Option<String>);

impl EchoGuard {
    fn hide() -> Result<Self, Box<dyn std::error::Error>> {
        if !io::stdin().is_terminal() {
            return Ok(Self(None));
        }
        let prior = StdCommand::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !prior.status.success() {
            return Err("could not inspect terminal echo".into());
        }
        let prior = String::from_utf8(prior.stdout)?.trim().to_owned();
        if !StdCommand::new("stty").arg("-echo").status()?.success() {
            return Err("could not disable terminal echo".into());
        }
        Ok(Self(Some(prior)))
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Some(prior) = &self.0 {
            let _ = StdCommand::new("stty").arg(prior).status();
        }
    }
}

fn random_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)?;
    let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    bytes.zeroize();
    Ok(token)
}

fn output_text(response: &Value) -> String {
    response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprint!("DeepSeek test key: ");
    let echo = EchoGuard::hide()?;
    let mut key = String::new();
    io::stdin().read_line(&mut key)?;
    drop(echo);
    eprintln!();
    while key.ends_with('\r') || key.ends_with('\n') {
        key.pop();
    }
    if !key.starts_with("sk-") || key.len() < 20 {
        return Err("invalid test key format".into());
    }

    let client = Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;
    let model_response = client
        .get("https://api.deepseek.com/models")
        .bearer_auth(&key)
        .send()
        .await?;
    let model_status = model_response.status();
    let model_body: Value = serde_json::from_slice(&model_response.bytes().await?)?;
    if !model_status.is_success() {
        let code = model_body["error"]["code"].as_str().unwrap_or("unknown");
        return Err(
            format!("DeepSeek model discovery failed: HTTP {model_status}, code {code}").into(),
        );
    }
    let available: Vec<&str> = model_body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str())
        .collect();
    println!("DeepSeek model discovery: {}", available.join(", "));
    if !available.contains(&"deepseek-flash") {
        return Err("deepseek-flash is not available to this key".into());
    }

    let token = random_token().map_err(|_| "failed to generate local token")?;
    let home_path = std::env::temp_dir().join(format!("switchx-deepseek-probe-{}", &token[..16]));
    std::fs::create_dir(&home_path)?;
    let home = ProbeHome(home_path);
    let templates: Value =
        serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json"))?;
    let publication = publish(
        &templates,
        &[Selection {
            public_id: "sx-ds-flash",
            display_name: "DeepSeek · Flash",
            provider_id: "deepseek",
            upstream_model: "deepseek-flash",
        }],
    )?;
    let catalog_path = home.0.join("catalog.json");
    std::fs::write(
        &catalog_path,
        serde_json::to_vec_pretty(&publication.catalog)?,
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let preview = preview_route("", &publication, &catalog_path, address, "sx-ds-flash")?;
    std::fs::write(home.0.join("config.toml"), preview.proposed)?;
    let upstream = Upstream::new("https://api.deepseek.com/", key)?;
    let state = RouterState::new(
        address,
        token.clone(),
        publication,
        HashMap::from([("deepseek".into(), upstream)]),
    )?;
    let router_task = tokio::spawn(async move { serve(listener, state).await.unwrap() });

    let routed = client
        .post(format!("http://{address}/v1/responses"))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            json!({
                "model": "sx-ds-flash",
                "input": "Reply with exactly SWITCHX_OK.",
                "stream": false,
                "max_output_tokens": 64
            })
            .to_string(),
        )
        .send()
        .await?;
    let routed_status = routed.status();
    let routed_body: Value = serde_json::from_slice(&routed.bytes().await?)?;
    if !routed_status.is_success() {
        let code = routed_body["error"]["code"].as_str().unwrap_or("unknown");
        router_task.abort();
        return Err(format!("routed response failed: HTTP {routed_status}, code {code}").into());
    }
    println!(
        "Routed Responses: HTTP {routed_status}, upstream model {}, response status {}, output {:?}",
        routed_body["model"].as_str().unwrap_or("unknown"),
        routed_body["status"].as_str().unwrap_or("unknown"),
        output_text(&routed_body)
    );
    if routed_body["status"] != "completed"
        || routed_body["model"] != "deepseek-flash"
        || !output_text(&routed_body).contains("SWITCHX_OK")
    {
        router_task.abort();
        return Err("routed response did not meet the expected completion checks".into());
    }

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
            "sx-ds-flash",
            "-c",
            "model_reasoning_effort=\"low\"",
            "Reply with exactly SWITCHX_CODEX_OK. Do not call tools.",
        ])
        .current_dir(&home.0)
        .env("CODEX_HOME", &home.0)
        .env("SWITCHX_LOCAL_TOKEN", &token)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(90), command.output()).await??;
    router_task.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !stdout.contains("SWITCHX_CODEX_OK") {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Codex CLI failed: exit {}, detail {}",
            output.status,
            stderr.chars().take(600).collect::<String>()
        )
        .into());
    }
    println!(
        "Codex CLI 0.156.1: exit {}, output {:?}",
        output.status,
        stdout.trim()
    );
    Ok(())
}
