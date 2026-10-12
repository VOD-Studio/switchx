//! Application services behind the desktop UI: local data, upstream
//! connections, model mappings, and the shared Codex configuration.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::{codex, storage::Store};

mod activity;
mod common_config;
mod models;
mod presets;
mod providers;
mod snapshot;

pub use activity::{RequestStats, RequestView, format_duration, load_requests, request_stats};
pub use common_config::{
    extract_and_save_common_config, initialize_common_config, load_common_config,
    save_common_config,
};
pub use models::{
    BatchCandidate, BatchInput, BatchModel, ConnectionModel, ModelInput, batch_candidates,
    delete_model, draft_candidates, save_connection_models, save_fallback, save_mapping,
    save_mappings, save_model, select_model, select_models,
};
pub(crate) use models::{save_mapping_from, save_mappings_from};
pub use presets::{ModelPreset, PROVIDER_PRESETS, ProviderPreset, provider_preset};
pub(crate) use providers::resolve_provider_icon_id;
pub use providers::{
    ProviderConfigState, delete_provider, load_provider, load_provider_config, provider_credential,
    save_provider, save_provider_with_codex_options, save_provider_with_codex_options_and_icon,
    save_provider_with_mappings,
};
pub use snapshot::{ModelView, ProviderView, Snapshot, load_snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    DataDirectory,
    Database,
    Busy,
    WorkerStopped,
}

impl AppError {
    pub fn code(self) -> &'static str {
        match self {
            Self::DataDirectory => "data_directory",
            Self::Database => "database_unavailable",
            Self::Busy => "refresh_busy",
            Self::WorkerStopped => "background_unavailable",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::DataDirectory => "无法访问 SwitchX 本地数据目录",
            Self::Database => "无法读取 SwitchX 本地资料",
            Self::Busy => "本地状态检查仍在运行",
            Self::WorkerStopped => "后台状态通道已停止",
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Self::DataDirectory => "检查目录权限后重试。",
            Self::Database => "检查本地数据库文件、权限或空间后重试。",
            Self::Busy => "稍后重试。",
            Self::WorkerStopped => "重新启动 SwitchX 后重试。",
        }
    }
}

pub fn data_directory() -> Result<PathBuf, AppError> {
    if let Some(path) = env::var_os("SWITCHX_DATA_DIR") {
        let path = PathBuf::from(path);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err(AppError::DataDirectory)
        };
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("Library/Application Support/SwitchX"))
            .ok_or(AppError::DataDirectory)
    }
    #[cfg(target_os = "windows")]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("SwitchX"))
            .ok_or(AppError::DataDirectory)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let base = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|path| path.join(".local/share"))
            })
            .ok_or(AppError::DataDirectory)?;
        Ok(base.join("switchx"))
    }
}

pub fn ensure_editable(data_dir: &Path) -> Result<(), String> {
    if codex::config_managed(data_dir) {
        return Err("SwitchX 正在管理配置，请先恢复原配置再编辑上游或模型".into());
    }
    Ok(())
}

pub(crate) fn open_store(data_dir: &Path) -> Result<Store, AppError> {
    if !data_dir.is_absolute() {
        return Err(AppError::DataDirectory);
    }
    if let Ok(metadata) = fs::symlink_metadata(data_dir)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(AppError::DataDirectory);
    }
    fs::create_dir_all(data_dir).map_err(|_| AppError::DataDirectory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| AppError::DataDirectory)?;
    }
    let database = data_dir.join("switchx.sqlite");
    match fs::symlink_metadata(&database) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AppError::Database);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&database) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(AppError::Database),
            }
        }
        Err(_) => return Err(AppError::Database),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600))
            .map_err(|_| AppError::Database)?;
    }
    Store::open(&database).map_err(|_| AppError::Database)
}
