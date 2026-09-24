use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::{process::Command, time::timeout};
use toml_edit::{DocumentMut, Item};

use crate::{config_transaction::read_config, direct::validate_provider, direct_config};

pub struct ConfigStatus {
    pub mode: String,
    pub model: String,
    pub provider: String,
    pub direct_active: bool,
    pub config_exists: bool,
}

pub struct ImportCandidate {
    pub name: String,
    pub base_url: String,
    pub model_id: String,
}

pub fn default_home() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("CODEX_HOME") {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("CODEX_HOME 必须是绝对路径".into())
        };
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(".codex"))
        .ok_or_else(|| "找不到 Codex 配置目录".into())
}

pub fn config_path(home: &Path) -> Result<PathBuf, String> {
    if !home.is_absolute() {
        return Err("Codex 配置目录必须是绝对路径".into());
    }
    Ok(home.join("config.toml"))
}

pub fn inspect(home: &Path, state_dir: &Path) -> Result<ConfigStatus, String> {
    let path = config_path(home)?;
    let contents = read_config(&path)?;
    let config_exists = contents.is_some();
    let document: DocumentMut = contents
        .as_deref()
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|_| "Codex 配置不是 UTF-8")?
        .unwrap_or("")
        .parse()
        .map_err(|_| "Codex 配置 TOML 无效")?;
    let model = document
        .as_table()
        .get("model")
        .and_then(Item::as_str)
        .unwrap_or("默认模型")
        .to_owned();
    let provider = document
        .as_table()
        .get("model_provider")
        .and_then(Item::as_str)
        .unwrap_or("openai")
        .to_owned();
    let active_target = direct_config::active_target(state_dir)?;
    let journal_for_target = active_target.as_deref() == Some(path.as_path());
    let direct_active = journal_for_target && provider.starts_with("switchx_direct_");
    let mode = if direct_active {
        "SwitchX 直连已写入 · 新客户端启动后生效"
    } else if journal_for_target {
        "SwitchX 直连 journal 待恢复或处理冲突"
    } else if active_target.is_some() {
        "另一个 Codex 配置目录正在由 SwitchX 管理"
    } else if state_dir.join("switch-journal.json").exists() {
        "SwitchX 路由配置待恢复"
    } else if !config_exists {
        "config.toml 尚不存在 · Codex 默认官方连接"
    } else if provider == "openai" {
        "Codex 官方连接 · 登录由 Codex 管理"
    } else {
        "现有自定义连接 · SwitchX 尚未接管"
    };
    Ok(ConfigStatus {
        mode: mode.into(),
        model,
        provider,
        direct_active,
        config_exists,
    })
}

pub fn import_candidate(home: &Path) -> Result<ImportCandidate, String> {
    let path = config_path(home)?;
    let contents = read_config(&path)?.ok_or("Codex 配置不存在")?;
    let document: DocumentMut = std::str::from_utf8(&contents)
        .map_err(|_| "Codex 配置不是 UTF-8")?
        .parse()
        .map_err(|_| "Codex 配置 TOML 无效")?;
    let provider_id = document
        .as_table()
        .get("model_provider")
        .and_then(Item::as_str)
        .ok_or("当前配置没有自定义上游")?;
    let provider = document
        .as_table()
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|table| table.get(provider_id))
        .and_then(Item::as_table)
        .ok_or("当前上游不是可导入的自定义配置")?;
    if provider
        .get("wire_api")
        .and_then(Item::as_str)
        .is_some_and(|wire_api| wire_api != "responses")
    {
        return Err("当前上游不是 Responses 协议，不能直连导入".into());
    }
    let base_url = provider
        .get("base_url")
        .and_then(Item::as_str)
        .ok_or("当前上游没有地址")?;
    let model_id = document
        .as_table()
        .get("model")
        .and_then(Item::as_str)
        .ok_or("当前配置没有模型 ID")?;
    let name = provider
        .get("name")
        .and_then(Item::as_str)
        .unwrap_or(provider_id);
    let url = validate_provider(name, base_url, model_id)?;
    Ok(ImportCandidate {
        name: name.into(),
        base_url: url.to_string(),
        model_id: model_id.into(),
    })
}

pub async fn login_status(home: &Path) -> Result<String, String> {
    config_path(home)?;
    let executable = env::var_os("SWITCHX_CODEX_CLI")
        .map(PathBuf::from)
        .or_else(|| {
            let bundled = PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex");
            bundled.is_file().then_some(bundled)
        })
        .unwrap_or_else(|| PathBuf::from("codex"));
    let output = timeout(
        Duration::from_secs(12),
        Command::new(executable)
            .arg("login")
            .arg("status")
            .env("CODEX_HOME", home)
            .output(),
    )
    .await
    .map_err(|_| "Codex 登录状态检查超时")?
    .map_err(|_| "无法启动 Codex CLI；可设置 SWITCHX_CODEX_CLI")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let error = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{text}\n{error}").to_ascii_lowercase();
    Ok(classify_login_output(output.status.success(), &combined).into())
}

fn classify_login_output(success: bool, text: &str) -> &'static str {
    if !success {
        "Codex 未报告有效登录；可用官方 Codex 完成登录后重试"
    } else if text.contains("chatgpt") {
        "ChatGPT 订阅登录：Codex CLI 报告已登录"
    } else if text.contains("api key") {
        "OpenAI API Key 登录：Codex CLI 报告已登录（非 ChatGPT 订阅）"
    } else {
        "Codex CLI 报告已登录，无法确定登录类型"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_home_reports_codex_defaults_without_panicking() {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = env::temp_dir().join(format!(
            "switchx-empty-home-{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        std::fs::create_dir(&path).unwrap();
        let status = inspect(&path, &path).unwrap();
        assert_eq!(status.provider, "openai");
        assert_eq!(status.model, "默认模型");
        assert!(!status.config_exists);
        assert!(import_candidate(&path).is_err());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn distinguishes_chatgpt_api_key_and_unknown_login() {
        assert!(classify_login_output(true, "logged in using chatgpt").starts_with("ChatGPT"));
        assert!(
            classify_login_output(true, "logged in using an api key").starts_with("OpenAI API Key")
        );
        assert!(classify_login_output(true, "logged in").contains("无法确定"));
        assert!(classify_login_output(false, "not logged in").contains("未报告有效登录"));
    }

    #[test]
    fn imports_metadata_without_credential_configuration() {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = env::temp_dir().join(format!(
            "switchx-import-{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("config.toml"),
            "model = \"mock-model\"\nmodel_provider = \"previous\"\n[model_providers.previous]\nname = \"Previous\"\nbase_url = \"https://example.invalid/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"SECRET\"\n",
        )
        .unwrap();
        let imported = import_candidate(&path).unwrap();
        assert_eq!(imported.name, "Previous");
        assert_eq!(imported.model_id, "mock-model");
        assert!(
            !format!(
                "{}{}{}",
                imported.name, imported.base_url, imported.model_id
            )
            .contains("SECRET")
        );
        std::fs::remove_dir_all(path).unwrap();
    }
}
