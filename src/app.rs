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
mod usage;

pub(crate) use activity::requests_from;
pub use activity::{
    RequestStats, RequestView, clear_requests, format_duration, load_requests, request_stats,
};
pub use common_config::{
    extract_and_save_common_config, initialize_common_config, load_common_config,
    save_common_config,
};
pub use models::{
    BatchCandidate, BatchInput, BatchModel, ConnectionModel, ModelInput, batch_candidates,
    connection_draft_candidates, delete_model, draft_candidates, load_request_overrides,
    save_connection_models, save_fallback, save_mapping, save_mappings, save_model, select_model,
    select_models, validate_connection_draft,
};
pub(crate) use models::{connection_model_records, save_mapping_from, save_mappings_from};
pub use presets::{ModelPreset, PROVIDER_PRESETS, ProviderPreset, provider_preset};
pub(crate) use providers::resolve_provider_icon_id;
pub use providers::{
    ProviderConfigState, delete_provider, load_provider, load_provider_config,
    load_provider_request_overrides, provider_credential, save_provider,
    save_provider_with_codex_options, save_provider_with_codex_options_and_icon,
    save_provider_with_directory, save_provider_with_mappings,
};
pub(crate) use snapshot::snapshot_from;
pub use snapshot::{ModelView, ProviderView, Snapshot, load_snapshot};
pub use usage::{
    PriceInput, PriceView, USAGE_PAGE, USAGE_ROW_LIMIT, UsageQuery, UsageRow, UsageView,
    delete_price, format_amounts, format_count, format_money, format_tokens, load_prices,
    load_usage, parse_price, price_text, rebuild_usage_logs, save_price, set_usage_log_import,
    sync_usage_logs,
};
pub(crate) use usage::{prices_from, usage_from};

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

/// Rewritten after another process, such as `switchx mcp`, changes saved data.
const EXTERNAL_CHANGE: &str = "external-change";

/// Tells a running app to reload.
pub fn mark_external_change(data_dir: &Path) -> io::Result<()> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("random source unavailable"))?;
    let token: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    replace_private_file(data_dir, EXTERNAL_CHANGE, token.as_bytes())
}

/// Replaces `name` in the data directory atomically with an owner-only file, so
/// an existing file or symlink at that path is never written through.
pub(crate) fn replace_private_file(data_dir: &Path, name: &str, contents: &[u8]) -> io::Result<()> {
    let mut nonce = [0_u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| io::Error::other("random source unavailable"))?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let temporary = data_dir.join(format!(".{name}-{suffix}"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temporary).and_then(|mut file| {
        io::Write::write_all(&mut file, contents)?;
        fs::rename(&temporary, data_dir.join(name))
    });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// The latest external change marker, compared between polls.
pub fn external_change(data_dir: &Path) -> Option<String> {
    let path = data_dir.join(EXTERNAL_CHANGE);
    fs::symlink_metadata(&path)
        .ok()
        .filter(fs::Metadata::is_file)?;
    fs::read_to_string(path).ok()
}

pub fn ensure_editable(data_dir: &Path) -> Result<(), String> {
    if codex::config_managed(data_dir) {
        return Err("SwitchX 正在管理配置，请先恢复原配置再编辑上游或模型".into());
    }
    Ok(())
}

/// Opens an existing current-schema database without creating, migrating, or
/// writing it; a database the app has not yet upgraded is unavailable.
pub(crate) fn open_store_read_only(data_dir: &Path) -> Result<Store, AppError> {
    let regular = |path: &Path, directory: bool| {
        fs::symlink_metadata(path).is_ok_and(|metadata| {
            !metadata.file_type().is_symlink()
                && if directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                }
        })
    };
    if !data_dir.is_absolute() || !regular(data_dir, true) {
        return Err(AppError::DataDirectory);
    }
    let database = data_dir.join("switchx.sqlite");
    if !regular(&database, false) {
        return Err(AppError::Database);
    }
    Store::open_read_only(&database).map_err(|_| AppError::Database)
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
