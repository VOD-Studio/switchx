//! Creates synthetic Claude settings through the real application services.
//! Usage: cargo run --example claude_direct_probe -- /absolute/empty/directory [append|replace|restore]
//! No network requests and no changes to the normal Claude or SwitchX homes.
use std::{env, path::PathBuf};
use switchx::{
    app,
    claude::{AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy},
    clients::ClaudeCapabilities,
    credentials::Secret,
};
fn main() -> Result<(), String> {
    let root = env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("请指定隔离目录的绝对路径")?;
    if !root.is_absolute() {
        return Err("隔离目录必须是绝对路径".into());
    }
    let data = root.join("data");
    let home = root.join("claude");
    let workspace = app::claude::claude_workspace(&data, &home)?;
    let mode = env::args().nth(2).unwrap_or_else(|| "replace".into());
    if mode == "restore" {
        let preview = app::claude::preview_claude_restore(&data, &home, false)?;
        let conflicts = app::claude::apply_claude_change(&data, &home, preview)?;
        if !conflicts.is_empty() {
            return Err(format!("恢复冲突：{conflicts:?}"));
        }
        println!("Isolated settings restored.");
        return Ok(());
    }
    if !["append", "replace"].contains(&mode.as_str()) {
        return Err("模式必须是 append、replace 或 restore".into());
    }
    let mut first = ModelOption::new("switchx-probe/first");
    first.label = "SwitchX First".into();
    first.description = "Synthetic first menu entry".into();
    first.behaves_as = "claude-sonnet-4-6".into();
    let mut second = ModelOption::new("switchx-probe/second");
    second.label = "SwitchX Second".into();
    second.description = "Synthetic second menu entry".into();
    let connection = app::claude::save_claude_connection(
        &data,
        &home,
        Connection {
            id: switchx::id::new_id()?,
            workspace_id: workspace.id.clone(),
            name: format!("Isolated {mode}"),
            base_url: "http://127.0.0.1:9".into(),
            settings: ConnectionSettings {
                auth: AuthMode::ApiKey,
                picker: if mode == "append" {
                    PickerPolicy::Append
                } else {
                    PickerPolicy::Replace
                },
                models: vec![first, second],
                default_model: "switchx-probe/second".into(),
                ..Default::default()
            },
            revision: 0,
            has_key: false,
        },
        Some(Secret::new("synthetic-claude-probe-key".into())),
        None,
    )?;
    let preview = app::claude::prepare_claude_direct(
        &data,
        &workspace,
        &connection.id,
        &root.join("unused-helper"),
        ClaudeCapabilities::from_version("2.1.301"),
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&preview.preview).unwrap()
    );
    app::claude::apply_claude_change(&data, &home, preview)?;
    println!("CLAUDE_CONFIG_DIR={}", home.display());
    Ok(())
}
