use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio::{process::Command, time::timeout};
use toml_edit::{DocumentMut, Item};

use crate::{
    config_transaction::{self, read_config},
    direct::validate_provider,
    direct_config,
};

pub struct ConfigStatus {
    pub mode: String,
    pub model: String,
    pub provider: String,
    pub direct_active: bool,
    pub route_managed: bool,
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
    let route = config_transaction::recovery(state_dir)?;
    let route_managed = route.is_some();
    let mode = if direct_active {
        "SwitchX 直连已写入 · 新客户端启动后生效"
    } else if journal_for_target {
        "SwitchX 直连 journal 待恢复或处理冲突"
    } else if active_target.is_some() {
        "另一个 Codex 配置目录正在由 SwitchX 管理"
    } else if route
        .as_ref()
        .is_some_and(|route| route.config_path == path)
    {
        "SwitchX 路由配置已写入 · 请核对本地路由状态"
    } else if route_managed {
        "另一个 Codex 配置目录有路由配置待恢复"
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
        route_managed,
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
    let output = timeout(
        Duration::from_secs(12),
        Command::new(cli_executable())
            .arg("login")
            .arg("status")
            .env("CODEX_HOME", home)
            .kill_on_drop(true)
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

pub fn cli_executable() -> PathBuf {
    env::var_os("SWITCHX_CODEX_CLI")
        .map(PathBuf::from)
        .or_else(|| {
            [
                "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
                "/Applications/ChatGPT.app/Contents/Resources/codex",
            ].into_iter().map(PathBuf::from).find(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("codex"))
}

pub async fn check_catalog(catalog: &serde_json::Value) -> Result<String, String> {
    struct CheckHome(PathBuf);
    impl Drop for CheckHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let path = env::temp_dir().join(format!("switchx-catalog-check-{}", crate::app::new_id()?));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(&path)
        .map_err(|_| "无法创建隔离目录以检查 Codex 兼容性")?;
    let home = CheckHome(path);
    let catalog_path = home.0.join("catalog.json");
    config_transaction::write_exclusive_atomic(
        &catalog_path,
        &serde_json::to_vec(catalog).map_err(|_| "无法生成目录")?,
    )?;
    let config = format!(
        "model_catalog_json = {}\nmodel_provider = \"switchx_schema\"\n[model_providers.switchx_schema]\nname = \"Catalog check\"\nbase_url = \"http://127.0.0.1:1/v1\"\nwire_api = \"responses\"\n",
        toml_edit::Value::from(catalog_path.to_str().ok_or("目录路径不是 UTF-8")?)
    );
    config_transaction::write_exclusive_atomic(&home.0.join("config.toml"), config.as_bytes())?;
    let executable = cli_executable();
    let mut command = Command::new(&executable);
    command
        .current_dir(&home.0)
        .env("CODEX_HOME", &home.0)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_BASE_URL")
        .kill_on_drop(true);
    let version = timeout(Duration::from_secs(5), command.arg("--version").output())
        .await
        .map_err(|_| "Codex 版本检查超时")?
        .map_err(|_| "无法启动 Codex CLI；可设置 SWITCHX_CODEX_CLI")?;
    if !version.status.success() {
        return Err("Codex CLI 版本检查失败".into());
    }
    let version = String::from_utf8(version.stdout).map_err(|_| "无法识别 Codex CLI 版本")?;
    if !version.starts_with("codex-cli ") || version.trim().len() > 128 {
        return Err("无法识别 Codex CLI 版本".into());
    }
    let mut command = Command::new(executable);
    let output = timeout(
        Duration::from_secs(12),
        command
            .args(["debug", "models"])
            .current_dir(&home.0)
            .env("CODEX_HOME", &home.0)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_BASE_URL")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Codex 目录检查超时")?
    .map_err(|_| "无法运行 Codex 目录检查")?;
    if !output.status.success() {
        return Err("目标 Codex CLI 无法解析模型目录；请检查完整模型资料及 CLI 版本".into());
    }
    let loaded: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Codex 目录检查未返回有效结果")?;
    let ids = |value: &serde_json::Value| -> Option<std::collections::BTreeSet<String>> {
        value["models"]
            .as_array()?
            .iter()
            .map(|model| model["slug"].as_str().map(str::to_owned))
            .collect()
    };
    if ids(&loaded).is_none() || ids(&loaded) != ids(catalog) {
        return Err("Codex 实际加载的模型与待发布目录不一致".into());
    }
    Ok(version.trim().into())
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
