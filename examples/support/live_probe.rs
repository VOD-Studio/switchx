//! Shared real-CLI answer and file-tool checks for direct and routed acceptance.

use std::{path::Path, process::Stdio, time::Duration};

use serde_json::Value;
use switchx::client;
use tokio::{process::Command, time::timeout};

pub type ProbeResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub async fn answer(home: &Path, model: &str, expected: &str) -> ProbeResult {
    let events = request(
        home,
        model,
        &format!("Reply exactly {expected}. Do not call tools."),
    )
    .await?;
    if !completed_answer(&events, expected) {
        return Err("response did not contain a completed turn and the exact answer".into());
    }
    Ok(())
}

pub async fn file_round_trip(home: &Path, model: &str, marker: &str) -> ProbeResult {
    std::fs::write(home.join("acceptance.txt"), format!("{marker}\n"))?;
    let events = request(
        home,
        model,
        "Read acceptance.txt with a shell tool. Reply exactly with its contents. Do not read other files or modify anything.",
    )
    .await?;
    if !file_tool_completed(&events, marker) || !completed_answer(&events, marker) {
        return Err("file tool and second-round answer were not both observed".into());
    }
    Ok(())
}

async fn request(home: &Path, model: &str, prompt: &str) -> ProbeResult<Vec<Value>> {
    let output = timeout(
        Duration::from_secs(180),
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
                "-c",
                "model_reasoning_effort=\"low\"",
                prompt,
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
    if !output.status.success() {
        // CLI diagnostics may include provider responses; keep them out of probe output.
        return Err(format!("Codex CLI exited unsuccessfully: {}", output.status).into());
    }
    String::from_utf8(output.stdout)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn completed_answer(events: &[Value], expected: &str) -> bool {
    events.iter().any(|event| event["type"] == "turn.completed")
        && !events
            .iter()
            .any(|event| matches!(event["type"].as_str(), Some("turn.failed" | "error")))
        && events.iter().any(|event| {
            event["type"] == "item.completed"
                && event["item"]["type"] == "agent_message"
                && event["item"]["text"]
                    .as_str()
                    .is_some_and(|text| text.trim() == expected)
        })
}

fn file_tool_completed(events: &[Value], marker: &str) -> bool {
    events.iter().any(|event| {
        event["type"] == "item.completed"
            && event["item"]["type"] == "command_execution"
            && event["item"]["exit_code"] == 0
            && event["item"]["command"]
                .as_str()
                .is_some_and(|command| command.contains("acceptance.txt"))
            && event["item"]["aggregated_output"]
                .as_str()
                .is_some_and(|output| output.contains(marker))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_completed_exact_answers_and_successful_file_tools() {
        let answer = json!({"type":"item.completed","item":{"type":"agent_message","text":"OK\n"}});
        let completed = json!({"type":"turn.completed"});
        assert!(!completed_answer(std::slice::from_ref(&answer), "OK"));
        assert!(completed_answer(&[answer.clone(), completed.clone()], "OK"));
        assert!(!completed_answer(
            &[
                answer.clone(),
                completed.clone(),
                json!({"type":"turn.failed"})
            ],
            "OK"
        ));
        assert!(!completed_answer(&[answer, completed.clone()], "different"));
        let mut tool = json!({"type":"item.completed","item":{"type":"command_execution","command":"cat acceptance.txt","exit_code":0,"aggregated_output":"OK"}});
        assert!(file_tool_completed(std::slice::from_ref(&tool), "OK"));
        assert!(!completed_answer(&[tool.clone(), completed], "OK"));
        tool["item"]["exit_code"] = 1.into();
        assert!(!file_tool_completed(std::slice::from_ref(&tool), "OK"));
        tool["item"]["exit_code"] = 0.into();
        tool["item"]["command"] = "cat another.txt".into();
        assert!(!file_tool_completed(&[tool], "OK"));
    }
}
