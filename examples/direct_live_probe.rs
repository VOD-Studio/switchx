use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::Value;
use switchx::{
    credentials::{CredentialStore, PROVIDER_KEY_SERVICE},
    direct,
    direct_config::{self, PreparedDirectSwitch},
    storage::{ProviderRecord, Store},
};
use tokio::{process::Command, time::timeout};

const ORIGINAL_CONFIG: &str = "# isolated real direct acceptance\nmodel = \"gpt-5.5\" # preserve comment\napproval_policy = \"never\"\n\n[mcp_servers.acceptance_disabled]\ncommand = \"false\"\nenabled = false\n";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let data = PathBuf::from(
        args.next()
            .ok_or("usage: direct_live_probe DATA_DIR PROVIDER_ID")?,
    );
    let id = args
        .next()
        .and_then(|id| id.into_string().ok())
        .ok_or("provider ID is required")?;
    if args.next().is_some() || !data.is_absolute() || !data.join("switchx.sqlite").is_file() {
        return Err("use an absolute existing SwitchX data directory".into());
    }
    let provider = Store::open(&data.join("switchx.sqlite"))?
        .provider(&id)?
        .ok_or("provider was not found")?;
    let helper = std::env::current_exe()?
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("switchx");
    if !direct::helper_is_usable(&helper) {
        return Err("run cargo build --bin switchx first".into());
    }
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| "system randomness is unavailable")?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let root = std::env::temp_dir().join(format!("switchx-direct-live-{suffix}"));
    std::fs::create_dir(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    let home = root.join("codex");
    let state = root.join("state");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&state)?;
    let config = home.join("config.toml");
    std::fs::write(&config, ORIGINAL_CONFIG)?;
    let result = run(&home, &state, &provider, &helper, &suffix).await;
    if state.join("direct-journal.json").exists() {
        let restored = direct_config::restore(&config, &state).map_err(|_| {
            format!(
                "restore failed; isolated files retained at {}",
                root.display()
            )
        })?;
        if !restored.conflicts.is_empty() {
            return Err(format!(
                "restore conflict; isolated files retained at {}",
                root.display()
            )
            .into());
        }
    }
    if std::fs::read_to_string(&config)? != ORIGINAL_CONFIG {
        return Err(format!("config differed after restore; inspect {}", root.display()).into());
    }
    std::fs::remove_dir_all(&root)?;
    println!("Isolated config restored exactly; temporary Codex home removed");
    result
}

async fn run(
    home: &Path,
    state: &Path,
    provider: &ProviderRecord,
    helper: &Path,
    suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let credentials = CredentialStore::new(PROVIDER_KEY_SERVICE)?;
    let secret = credentials.get(
        provider
            .credential_ref
            .as_deref()
            .ok_or("provider has no credential reference")?,
    )?;
    direct::check_models(provider, secret.expose()).await?;
    drop(secret);
    println!("Real upstream /models contains the selected model");
    PreparedDirectSwitch::inspect(&home.join("config.toml"), state, provider, helper)?.apply()?;
    let greeting = format!("SWITCHX_DIRECT_{suffix}");
    let events = request(
        home,
        &provider.model_id,
        &format!("Reply exactly {greeting}. Do not call tools."),
    )
    .await?;
    if !completed_answer(&events, &greeting) {
        return Err("real direct response did not contain the expected answer".into());
    }
    println!(
        "Codex CLI 0.156.1 completed a real direct Responses answer using the keychain helper"
    );
    let marker = format!("SWITCHX_TOOL_{suffix}");
    std::fs::write(home.join("acceptance.txt"), format!("{marker}\n"))?;
    let events = request(
        home,
        &provider.model_id,
        "Read acceptance.txt with a shell tool. Reply exactly with its contents. Do not read other files or modify anything.",
    )
    .await?;
    let read_file = events.iter().any(|event| {
        event["type"] == "item.completed"
            && event["item"]["type"] == "command_execution"
            && event["item"]["exit_code"] == 0
            && event["item"]["command"]
                .as_str()
                .is_some_and(|command| command.contains("acceptance.txt"))
            && event["item"]["aggregated_output"]
                .as_str()
                .is_some_and(|output| output.contains(&marker))
    });
    if !read_file || !completed_answer(&events, &marker) {
        return Err("real file tool and second-round answer were not both observed".into());
    }
    println!("Real file tool completed; its result was used in the second-round answer");
    Ok(())
}

async fn request(
    home: &Path,
    model: &str,
    prompt: &str,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let mut command = Command::new("npx");
    command
        .args([
            "-y",
            "@openai/codex@0.156.1",
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "-m",
            model,
            "-c",
            "model_reasoning_effort=\"low\"",
            prompt,
        ])
        .current_dir(home)
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(180), command.output()).await??;
    if !output.status.success() {
        return Err(format!("Codex CLI exited unsuccessfully: {}", output.status).into());
    }
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn completed_answer(events: &[Value], expected: &str) -> bool {
    events.iter().any(|event| event["type"] == "turn.completed")
        && events.iter().any(|event| {
            event["type"] == "item.completed"
                && event["item"]["type"] == "agent_message"
                && event["item"]["text"]
                    .as_str()
                    .is_some_and(|text| text.trim() == expected)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_a_completed_turn_and_the_exact_agent_answer() {
        let answer = json!({"type":"item.completed","item":{"type":"agent_message","text":"OK\n"}});
        let completed = json!({"type":"turn.completed"});
        assert!(!completed_answer(std::slice::from_ref(&answer), "OK"));
        assert!(completed_answer(&[answer, completed.clone()], "OK"));
        let tool = json!({"type":"item.completed","item":{"type":"command_execution","aggregated_output":"OK"}});
        assert!(!completed_answer(&[tool, completed.clone()], "OK"));
        let wrong =
            json!({"type":"item.completed","item":{"type":"agent_message","text":"not OK"}});
        assert!(!completed_answer(&[wrong, completed], "OK"));
    }
}
