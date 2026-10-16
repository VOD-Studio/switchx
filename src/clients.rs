//! Writable client identities and configuration workspaces. Usage-only clients
//! remain in `storage::UsageClient`; a provider's authentication is independent.

use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Codex,
    ClaudeCode,
}

impl ClientKind {
    pub const ALL: [Self; 2] = [Self::Codex, Self::ClaudeCode];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude_code",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude_code" => Ok(Self::ClaudeCode),
            _ => Err("不支持的可写客户端".into()),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }

    pub fn config_filename(self) -> &'static str {
        match self {
            Self::Codex => "config.toml",
            Self::ClaudeCode => "settings.json",
        }
    }

    pub fn home_variable(self) -> &'static str {
        match self {
            Self::Codex => "CODEX_HOME",
            Self::ClaudeCode => "CLAUDE_CONFIG_DIR",
        }
    }

    pub fn default_home(self) -> Result<PathBuf, String> {
        if let Some(home) = env::var_os(self.home_variable()) {
            let home = PathBuf::from(home);
            return if home.is_absolute() {
                Ok(home)
            } else {
                Err(format!("{} 必须是绝对路径", self.home_variable()))
            };
        }
        let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
        home.map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .map(|home| {
                home.join(match self {
                    Self::Codex => ".codex",
                    Self::ClaudeCode => ".claude",
                })
            })
            .ok_or_else(|| format!("找不到 {} 配置目录", self.label()))
    }
}

/// Canonicalizes existing ancestors without creating a client's directory.
/// Parent traversal is rejected rather than guessed across unresolved symlinks.
pub fn normalize_home(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err("客户端配置目录必须是有效的绝对路径".into());
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("客户端配置目录不能包含 ..，请选择明确的目录".into());
    }
    let clean: PathBuf = path.components().collect();
    let mut ancestor = clean.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::metadata(ancestor) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err("客户端配置目录不是文件夹".into());
                }
                let mut resolved =
                    fs::canonicalize(ancestor).map_err(|_| "无法解析客户端配置目录".to_owned())?;
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A dangling symlink must not become a second workspace identity.
                if fs::symlink_metadata(ancestor).is_ok() {
                    return Err("客户端配置目录包含失效的符号链接".into());
                }
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or("无法解析客户端配置目录")?
                        .to_owned(),
                );
                ancestor = ancestor.parent().ok_or("无法解析客户端配置目录")?;
            }
            Err(_) => return Err("无法访问客户端配置目录".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceKey {
    pub client: ClientKind,
    pub home: PathBuf,
}

impl WorkspaceKey {
    pub fn new(client: ClientKind, home: &Path) -> Result<Self, String> {
        Ok(Self {
            client,
            home: normalize_home(home)?,
        })
    }

    pub fn config_path(&self) -> PathBuf {
        self.home.join(self.client.config_filename())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientWorkspace {
    pub id: String,
    pub key: WorkspaceKey,
    pub name: String,
    pub last_page: i32,
}

/// Client schema capabilities, not a claim that an upstream supports a model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClaudeCapabilities {
    pub model_picker: bool,
    pub model_behaves_as: bool,
}

impl ClaudeCapabilities {
    pub fn from_version(version: &str) -> Self {
        let Some(version) = version.split_whitespace().next().and_then(parse_version) else {
            return Self::default();
        };
        Self {
            model_picker: version >= (2, 1, 242),
            model_behaves_as: version >= (2, 1, 257),
        }
    }
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_menu_features_have_independent_version_gates() {
        assert_eq!(
            ClaudeCapabilities::from_version("2.1.241"),
            ClaudeCapabilities::default()
        );
        assert_eq!(
            ClaudeCapabilities::from_version("2.1.242 (Claude Code)"),
            ClaudeCapabilities {
                model_picker: true,
                model_behaves_as: false
            }
        );
        assert!(ClaudeCapabilities::from_version("2.1.257").model_behaves_as);
        assert!(ClaudeCapabilities::from_version("3.0.0").model_picker);
        for invalid in [
            "",
            "unknown",
            "2.1",
            "2.1.301.1",
            "2.1.301-beta",
            "token 2.1.301",
        ] {
            assert_eq!(
                ClaudeCapabilities::from_version(invalid),
                ClaudeCapabilities::default()
            );
        }
    }

    #[test]
    fn workspace_identity_keeps_clients_distinct_without_creating_directories() {
        let parent = std::env::temp_dir();
        let home = parent
            .join(format!("switchx-client-{}", crate::id::new_id().unwrap()))
            .join("new");
        let codex = WorkspaceKey::new(ClientKind::Codex, &home).unwrap();
        let claude = WorkspaceKey::new(ClientKind::ClaudeCode, &home).unwrap();
        assert_ne!(codex, claude);
        assert_eq!(codex.home, claude.home);
        assert!(claude.config_path().ends_with("new/settings.json"));
        assert!(!home.exists());
        assert!(normalize_home(Path::new("relative")).is_err());
        assert!(normalize_home(&parent.join("parent/../child")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn aliases_share_identity_and_dangling_links_are_rejected() {
        let root = std::env::temp_dir().join(format!(
            "switchx-client-paths-{}",
            crate::id::new_id().unwrap()
        ));
        fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        assert_eq!(
            normalize_home(&root.join("real/new")).unwrap(),
            normalize_home(&root.join("alias/new")).unwrap()
        );
        std::os::unix::fs::symlink(root.join("absent"), root.join("broken")).unwrap();
        assert!(normalize_home(&root.join("broken/new")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
