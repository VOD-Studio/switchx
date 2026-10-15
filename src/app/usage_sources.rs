use super::{AppError, open_store};
use crate::storage::{Store, UsageClient};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSourceView {
    pub client: UsageClient,
    pub path: PathBuf,
    pub enabled: bool,
    pub hint: &'static str,
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

pub(crate) fn usage_sources_from(store: &Store) -> Result<Vec<UsageSourceView>, AppError> {
    let home = absolute_env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
    UsageClient::EXTERNAL
        .into_iter()
        .map(|client| {
            let (default, hint) = match client {
                UsageClient::Gemini => (
                    home.join(".gemini"),
                    "配置目录；读取 tmp/*/chats 下的会话 JSON / JSONL。",
                ),
                UsageClient::OpenCode => (
                    absolute_env("OPENCODE_DB").unwrap_or_else(|| {
                        absolute_env("XDG_DATA_HOME")
                            .unwrap_or_else(|| home.join(".local/share"))
                            .join("opencode/opencode.db")
                    }),
                    "选择 opencode.db；自动读取会话与压缩用量，未完成消息会稍后补齐。",
                ),
                UsageClient::GrokBuild => (
                    home.join(".grok"),
                    "配置目录；读取 sessions 和 archived_sessions，按轮汇总模型调用。",
                ),
                UsageClient::Pi => (
                    absolute_env("PI_CODING_AGENT_SESSION_DIR").unwrap_or_else(|| {
                        absolute_env("PI_CODING_AGENT_DIR")
                            .unwrap_or_else(|| home.join(".pi/agent"))
                            .join("sessions")
                    }),
                    "会话目录；包含助手、工具、压缩与分支摘要的用量，不读取对话正文。",
                ),
                UsageClient::Mcode => (
                    absolute_env("MINIMAX_DATA_DIR")
                        .unwrap_or_else(|| home.join(".minimax"))
                        .join("v2/sqlite/runtime-state.sqlite"),
                    "选择 runtime-state.sqlite；读取模型调用与费用，保留客户端上报的零费用。",
                ),
                _ => unreachable!(),
            };
            let path = store
                .usage_client_setting(client, "path")
                .map_err(|_| AppError::Database)?
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or(default);
            let enabled = store
                .usage_client_setting(client, "enabled")
                .map_err(|_| AppError::Database)?
                .as_deref()
                != Some("off");
            Ok(UsageSourceView {
                client,
                path,
                enabled,
                hint,
            })
        })
        .collect()
}

pub fn save_usage_client_source(
    data_dir: &Path,
    client: &str,
    path: &str,
    enabled: bool,
) -> Result<(), String> {
    let client = UsageClient::parse(client)
        .filter(|c| UsageClient::EXTERNAL.contains(c))
        .ok_or("不支持的会话来源")?;
    let path = path.trim();
    if !path.is_empty() && !Path::new(path).is_absolute() {
        return Err("数据来源须为绝对路径；留空可恢复默认位置".into());
    }
    open_store(data_dir)
        .map_err(|e| e.message().to_owned())?
        .save_usage_client_source(client, path, enabled)
        .map_err(|_| AppError::Database.message().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_source_settings_are_independent_validated_and_persistent() {
        let root = std::env::temp_dir().join(format!(
            "switchx-source-settings-{}",
            crate::id::new_id().unwrap()
        ));
        let path = root.join("session-data");
        save_usage_client_source(&root, "pi", path.to_str().unwrap(), false).unwrap();
        assert!(save_usage_client_source(&root, "pi", "relative", true).is_err());
        assert!(save_usage_client_source(&root, "codex", path.to_str().unwrap(), true).is_err());
        let store = open_store(&root).unwrap();
        let sources = usage_sources_from(&store).unwrap();
        assert_eq!(sources.len(), 5);
        let pi = sources
            .iter()
            .find(|s| s.client == UsageClient::Pi)
            .unwrap();
        assert_eq!(pi.path, path);
        assert!(!pi.enabled);
        assert!(
            sources
                .iter()
                .filter(|s| s.client != UsageClient::Pi)
                .all(|s| s.enabled)
        );
        drop(store);
        save_usage_client_source(&root, "pi", "", true).unwrap();
        let store = open_store(&root).unwrap();
        assert_eq!(
            store
                .usage_client_setting(UsageClient::Pi, "path")
                .unwrap()
                .as_deref(),
            Some("")
        );
        assert!(
            usage_sources_from(&store)
                .unwrap()
                .iter()
                .all(|s| s.enabled)
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
