use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};
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

pub fn cli_path() -> Result<PathBuf, String> {
    let selected = cli_executable();
    let path = if selected.is_absolute() || selected.components().count() > 1 {
        selected
    } else {
        env::var_os("PATH")
            .and_then(|paths| {
                env::split_paths(&paths)
                    .map(|directory| directory.join(&selected))
                    .find(|path| is_executable(path))
            })
            .ok_or("找不到 Codex CLI；可设置 SWITCHX_CODEX_CLI")?
    };
    let path = fs::canonicalize(path).map_err(|_| "无法定位 Codex CLI 的绝对路径")?;
    validate_executable(&path)?;
    Ok(path)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn validate_executable(executable: &Path) -> Result<(), String> {
    if !executable.is_absolute() || !is_executable(executable) {
        return Err("Codex CLI 必须是可执行文件的绝对路径".into());
    }
    Ok(())
}

struct CheckHome(PathBuf);

impl CheckHome {
    fn new() -> Result<Self, String> {
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
        let home = Self(path);
        config_transaction::write_exclusive_atomic(
            &home.0.join("config.toml"),
            b"cli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\nopenai_base_url = \"http://127.0.0.1:1/v1\"\nchatgpt_base_url = \"http://127.0.0.1:1\"\n[analytics]\nenabled = false\n",
        )?;
        Ok(home)
    }
}

impl Drop for CheckHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn isolated_command(executable: &Path, home: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(home)
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_BASE_URL")
        .env_remove("SWITCHX_LOCAL_TOKEN")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

async fn reap(child: &mut Child) -> Result<(), String> {
    if child
        .try_wait()
        .map_err(|_| "无法检查 Codex 子进程状态")?
        .is_none()
    {
        child
            .kill()
            .await
            .map_err(|_| "无法停止 Codex 检查子进程")?;
    }
    child
        .wait()
        .await
        .map_err(|_| "无法回收 Codex 检查子进程")?;
    Ok(())
}

async fn isolated_output(executable: &Path, home: &Path, argument: &str) -> Result<String, String> {
    let mut child = isolated_command(executable, home)
        .arg(argument)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "无法启动 Codex CLI；可设置 SWITCHX_CODEX_CLI")?;
    let result = timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or("Codex 检查输出不可用")?
            .take((MAX_REPLY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "无法读取 Codex 检查输出")?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err("Codex 检查响应超过读取上限".into());
        }
        if !child
            .wait()
            .await
            .map_err(|_| "无法等待 Codex 检查结果")?
            .success()
        {
            return Err("Codex CLI 检查失败".into());
        }
        String::from_utf8(bytes).map_err(|_| "Codex 检查输出不是 UTF-8".into())
    })
    .await
    .map_err(|_| "Codex CLI 检查超时".to_string())
    .and_then(|result| result);
    reap(&mut child).await?;
    result
}

async fn rpc(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let message = format!("{}\n", json!({"id":id,"method":method,"params":params}));
    input
        .write_all(message.as_bytes())
        .await
        .map_err(|_| "Codex 目录服务已断开")?;
    for _ in 0..256 {
        let mut line = Vec::new();
        loop {
            let buffer = output
                .fill_buf()
                .await
                .map_err(|_| "无法读取 Codex 目录服务")?;
            if buffer.is_empty() {
                return Err("Codex 目录服务已退出".into());
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            if line.len() + count > MAX_REPLY_BYTES {
                return Err("Codex 目录服务响应超过读取上限".into());
            }
            line.extend_from_slice(&buffer[..count]);
            output.consume(count);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        let message: Value =
            serde_json::from_slice(&line).map_err(|_| "Codex 目录服务响应格式无效")?;
        if message["id"] == id {
            if message.get("error").is_some() {
                return Err(format!(
                    "Codex 目录检查失败（{method}）；请检查 CLI 版本及模型资料"
                ));
            }
            return message
                .get("result")
                .cloned()
                .ok_or("Codex 目录服务没有返回结果".into());
        }
    }
    Err("Codex 目录服务消息超过读取上限".into())
}

async fn visible_models(executable: &Path, home: &Path) -> Result<BTreeSet<String>, String> {
    let mut child = isolated_command(executable, home)
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "无法启动 Codex 模型目录服务")?;
    let result = timeout(Duration::from_secs(12), async {
        let mut input = child.stdin.take().ok_or("Codex 目录服务输入不可用")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("Codex 目录服务输出不可用")?);
        rpc(&mut input, &mut output, 1, "initialize", json!({"clientInfo":{"name":"switchx_catalog_check","title":"SwitchX","version":env!("CARGO_PKG_VERSION")}})).await?;
        input.write_all(b"{\"method\":\"initialized\",\"params\":{}}\n").await.map_err(|_| "Codex 目录服务已断开")?;
        let mut ids = BTreeSet::new();
        let mut cursors = BTreeSet::new();
        let mut cursor = None::<String>;
        for page in 0..64 {
            let result = rpc(&mut input, &mut output, page + 2, "model/list", json!({"cursor":cursor,"limit":100,"includeHidden":false})).await?;
            for model in result["data"].as_array().ok_or("Codex 模型列表格式无效")? {
                let id = model["id"].as_str().filter(|id| !id.is_empty()).ok_or("Codex 模型列表缺少 ID")?;
                if !ids.insert(id.to_owned()) {
                    return Err("Codex 模型列表包含重复 ID".into());
                }
            }
            match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(ids),
                Some(Value::String(next)) if !next.is_empty() && cursors.insert(next.clone()) => cursor = Some(next.clone()),
                _ => return Err("Codex 模型列表分页无效或重复".into()),
            }
        }
        Err("Codex 模型列表分页超过读取上限".into())
    }).await.map_err(|_| "Codex 模型目录检查超时".to_string()).and_then(|result| result);
    reap(&mut child).await?;
    result
}

pub async fn check_catalog(catalog: &Value) -> Result<String, String> {
    check_catalog_using(catalog, &cli_path()?).await
}

pub async fn check_catalog_using(catalog: &Value, executable: &Path) -> Result<String, String> {
    validate_executable(executable)?;
    let expected: BTreeSet<String> = catalog["models"]
        .as_array()
        .ok_or("待发布模型目录格式无效")?
        .iter()
        .filter(|model| model["visibility"] == "list" && model["supported_in_api"] == true)
        .map(|model| {
            model["slug"]
                .as_str()
                .map(str::to_owned)
                .ok_or("待发布模型缺少 ID".to_string())
        })
        .collect::<Result<_, _>>()?;
    if expected.is_empty() {
        return Err("待发布目录没有可见模型".into());
    }
    let home = CheckHome::new()?;
    let catalog_path = home.0.join("catalog.json");
    config_transaction::write_exclusive_atomic(
        &catalog_path,
        &serde_json::to_vec(catalog).map_err(|_| "无法生成目录")?,
    )?;
    let config = format!(
        "cli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\nmodel_catalog_json = {}\nmodel_provider = \"switchx_schema\"\nopenai_base_url = \"http://127.0.0.1:1/v1\"\nchatgpt_base_url = \"http://127.0.0.1:1\"\n[analytics]\nenabled = false\n[model_providers.switchx_schema]\nname = \"Catalog check\"\nbase_url = \"http://127.0.0.1:1/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = false\n",
        toml_edit::Value::from(catalog_path.to_str().ok_or("目录路径不是 UTF-8")?)
    );
    fs::write(home.0.join("config.toml"), config).map_err(|_| "无法写入隔离 Codex 配置")?;
    let version = isolated_output(executable, &home.0, "--version").await?;
    if !version.starts_with("codex-cli ") || version.trim().len() > 128 {
        return Err("无法识别 Codex CLI 版本".into());
    }
    if visible_models(executable, &home.0).await? != expected {
        return Err("Codex 模型选择器实际加载的模型与待发布目录不一致".into());
    }
    Ok(version.trim().into())
}

fn shell_quote(path: &Path) -> Result<String, String> {
    let path = path.to_str().ok_or("启动路径不是 UTF-8")?;
    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

pub async fn write_codex_launcher(
    state_dir: &Path,
    home: &Path,
    executable: &Path,
) -> Result<PathBuf, String> {
    config_path(home)?;
    validate_executable(executable)?;
    if !state_dir.is_absolute() || !state_dir.is_dir() || !home.is_dir() {
        return Err("启动器需要已存在的绝对配置和数据目录".into());
    }
    let check_home = CheckHome::new()?;
    let help = isolated_output(executable, &check_home.0, "--help").await?;
    if !help.split_whitespace().any(|word| word == "--no-daemon") {
        return Err("当前 Codex CLI 不支持 --no-daemon；请更新 CLI 后启动路由会话".into());
    }
    let script = format!(
        "#!/bin/sh\ncd {} || exit 1\nexec /usr/bin/env -u OPENAI_API_KEY -u CODEX_API_KEY -u CODEX_ACCESS_TOKEN -u OPENAI_BASE_URL -u SWITCHX_LOCAL_TOKEN CODEX_HOME={} {} --no-daemon\n",
        shell_quote(home)?,
        shell_quote(home)?,
        shell_quote(executable)?
    );
    let path = state_dir.join("launch-codex.command");
    let expected = read_config(&path)?;
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(fs::Permissions::from_mode(0o700))
    };
    #[cfg(not(unix))]
    let permissions = None;
    config_transaction::replace(&path, script.as_bytes(), permissions, &expected)?;
    Ok(path)
}

pub fn open_codex_launcher(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err("Codex 启动器不存在或不是绝对路径".into());
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .args(["-a", "Terminal"])
            .arg(path)
            .status()
            .map_err(|_| "无法打开 Terminal；可手动运行 Codex 启动器")?;
        if status.success() {
            Ok(())
        } else {
            Err("Terminal 未打开 Codex 启动器".into())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("此平台暂不支持打开 Codex 启动器".into())
    }
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

    #[cfg(unix)]
    fn mock_cli(home: &Path, name: &str, response: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = home.join(name);
        let script = r#"#!/bin/sh
case "$1" in
    --version) printf '%s\n' 'codex-cli synthetic-test'; exit 0 ;;
    --help) printf '%s\n' 'Usage: codex --no-daemon'; exit 0 ;;
    app-server)
        while IFS= read -r message; do
            id="${message#*\"id\":}"
            id="${id%%,*}"
            case "$message" in
                *'"method":"initialize"'*) printf '{"id":%s,"result":{}}\n' "$id" ;;
                *'"method":"model/list"'*)
                    MODEL_REPLY
                    ;;
            esac
        done
        exit 0 ;;
esac
printf '%s\n' "$#" "$1" "$CODEX_HOME" "$PWD" "${OPENAI_API_KEY-unset}" "${CODEX_API_KEY-unset}" "${CODEX_ACCESS_TOKEN-unset}" "${OPENAI_BASE_URL-unset}" "${SWITCHX_LOCAL_TOKEN-unset}"
"#;
        fs::write(&path, script.replace("MODEL_REPLY", response)).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launcher_preserves_exact_home_and_argv_without_inherited_provider_environment() {
        use std::os::unix::fs::PermissionsExt;
        let root = CheckHome::new().unwrap();
        let home = root.0.join("home ' space $(printf injected)");
        fs::create_dir(&home).unwrap();
        let executable = mock_cli(&root.0, "fake codex ' executable", "");
        let path = write_codex_launcher(&root.0, &home, &executable)
            .await
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let output = Command::new(&path)
            .env("CODEX_HOME", "wrong-home")
            .env("OPENAI_API_KEY", "synthetic-key")
            .env("CODEX_API_KEY", "synthetic-key")
            .env("CODEX_ACCESS_TOKEN", "synthetic-token")
            .env("OPENAI_BASE_URL", "http://127.0.0.1:1")
            .env("SWITCHX_LOCAL_TOKEN", "synthetic-token")
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(
            lines,
            [
                "1",
                "--no-daemon",
                home.to_str().unwrap(),
                home.to_str().unwrap(),
                "unset",
                "unset",
                "unset",
                "unset",
                "unset"
            ]
        );
        assert!(!fs::read_to_string(&path).unwrap().contains("synthetic-key"));
        assert_eq!(
            write_codex_launcher(&root.0, &home, &executable)
                .await
                .unwrap(),
            path
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn catalog_check_uses_paginated_visible_models_and_rejects_stale_ids_or_cursors() {
        let root = CheckHome::new().unwrap();
        let catalog = json!({"models":[
            {"slug":"sx-alpha","visibility":"list","supported_in_api":true},
            {"slug":"sx-beta","visibility":"list","supported_in_api":true}
        ]});
        let good = r#"case "$message" in
            *'"cursor":"page2"'*) printf '{"id":%s,"result":{"data":[{"id":"sx-beta"}],"nextCursor":null}}\n' "$id" ;;
            *) printf '{"id":%s,"result":{"data":[{"id":"sx-alpha"}],"nextCursor":"page2"}}\n' "$id" ;;
        esac"#;
        let executable = mock_cli(&root.0, "good-codex", good);
        assert_eq!(
            check_catalog_using(&catalog, &executable).await.unwrap(),
            "codex-cli synthetic-test"
        );
        let stale = mock_cli(
            &root.0,
            "stale-codex",
            r#"printf '{"id":%s,"result":{"data":[{"id":"official-old"}],"nextCursor":null}}\n' "$id""#,
        );
        assert!(
            check_catalog_using(&catalog, &stale)
                .await
                .unwrap_err()
                .contains("不一致")
        );
        let repeated = mock_cli(
            &root.0,
            "repeated-codex",
            &good.replace("\"nextCursor\":null", "\"nextCursor\":\"page2\""),
        );
        assert!(
            check_catalog_using(&catalog, &repeated)
                .await
                .unwrap_err()
                .contains("分页")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launcher_rejects_a_cli_without_the_no_daemon_flag() {
        let root = CheckHome::new().unwrap();
        let executable = mock_cli(&root.0, "old-codex", "");
        let script = fs::read_to_string(&executable)
            .unwrap()
            .replace("Usage: codex --no-daemon", "Usage: codex");
        fs::write(&executable, script).unwrap();
        assert!(
            write_codex_launcher(&root.0, &root.0, &executable)
                .await
                .unwrap_err()
                .contains("--no-daemon")
        );
        assert!(!root.0.join("launch-codex.command").exists());
    }

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
