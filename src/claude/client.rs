use crate::clients::ClaudeCapabilities;
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

#[derive(Debug, Clone)]
pub struct CliInfo {
    pub executable: PathBuf,
    pub version: String,
    pub capabilities: ClaudeCapabilities,
}

pub fn executable(saved: Option<&str>) -> Result<PathBuf, String> {
    let explicit = saved
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("SWITCHX_CLAUDE_CLI").map(PathBuf::from));
    if let Some(path) = explicit {
        if !path.is_absolute() || !is_executable(&path) {
            return Err("Claude CLI 路径必须指向可执行程序的绝对路径".into());
        }
        return Ok(path);
    }
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let mut paths: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| {
            env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(name))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        paths.push(PathBuf::from(home).join(".local/bin").join(name));
    }
    paths.extend([
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ]);
    paths
        .into_iter()
        .find(|path| path.is_absolute() && is_executable(path))
        .ok_or_else(|| "未找到 Claude Code；请安装 CLI 或指定程序路径".into())
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

pub async fn inspect(saved: Option<&str>, home: &Path) -> Result<CliInfo, String> {
    let executable = executable(saved)?;
    let mut child = Command::new(&executable)
        .arg("--version")
        .env("CLAUDE_CONFIG_DIR", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "无法启动 Claude Code 版本检查".to_owned())?;
    let output = timeout(Duration::from_secs(8), async {
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .ok_or("无法读取 CLI 版本")?
            .take(4096)
            .read_to_end(&mut output)
            .await
            .map_err(|_| "无法读取 CLI 版本")?;
        if output.len() >= 4096 {
            return Err("CLI 版本输出过长");
        }
        if !child
            .wait()
            .await
            .map_err(|_| "CLI 版本检查失败")?
            .success()
        {
            return Err("CLI 版本检查失败");
        }
        Ok(output)
    })
    .await
    .map_err(|_| "Claude Code 版本检查超时".to_owned())?
    .map_err(str::to_owned)?;
    let text = std::str::from_utf8(&output).map_err(|_| "CLI 版本输出无效")?;
    let version = text.split_whitespace().next().unwrap_or("");
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || p.parse::<u64>().is_err())
    {
        return Err("无法识别 Claude Code 版本".into());
    }
    Ok(CliInfo {
        executable,
        version: version.into(),
        capabilities: ClaudeCapabilities::from_version(version),
    })
}

pub fn helper_command(
    executable: &Path,
    provider: &str,
    data_dir: &Path,
) -> Result<String, String> {
    if !executable.is_absolute() || !data_dir.is_absolute() {
        return Err("凭据程序和数据目录必须是绝对路径".into());
    }
    let executable = executable.to_str().ok_or("凭据程序路径无效")?;
    let data_dir = data_dir.to_str().ok_or("数据目录路径无效")?;
    let quote = |word: &str| -> Result<String, String> {
        if word.chars().any(char::is_control) {
            return Err("凭据命令参数包含控制字符".into());
        }
        if cfg!(windows) {
            if word.contains(['%', '!', '"']) {
                return Err("路径含 cmd 无法安全引用的字符，请选择其他路径或 API Key 认证".into());
            }
            Ok(format!("\"{word}\""))
        } else {
            Ok(format!("'{}'", word.replace('\'', "'\\''")))
        }
    };
    Ok(format!(
        "{} credential {} {}",
        quote(executable)?,
        quote(provider)?,
        quote(data_dir)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn helper_arguments_cannot_execute_shell_substitutions() {
        let command = helper_command(
            Path::new("/opt/My App/switchx"),
            "saved-id",
            Path::new("/data/a'b $(echo secret)"),
        )
        .unwrap();
        assert_eq!(
            command,
            "'/opt/My App/switchx' credential 'saved-id' '/data/a'\\''b $(echo secret)'"
        );
    }
}
