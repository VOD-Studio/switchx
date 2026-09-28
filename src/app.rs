use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::{
    catalog,
    credentials::{CredentialError, CredentialStore, PROVIDER_KEY_SERVICE, Secret},
    direct::validate_provider,
    storage::{ModelRecord, ProviderRecord, RequestStatus, Store},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppError {
    DataDirectory,
    Database,
    CredentialStore,
    Busy,
    WorkerStopped,
}

impl AppError {
    pub fn code(self) -> &'static str {
        match self {
            Self::DataDirectory => "data_directory",
            Self::Database => "database_unavailable",
            Self::CredentialStore => "credential_store_unavailable",
            Self::Busy => "refresh_busy",
            Self::WorkerStopped => "background_unavailable",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::DataDirectory => "无法访问 SwitchX 本地数据目录",
            Self::Database => "无法读取 SwitchX 本地资料",
            Self::CredentialStore => "无法初始化系统凭据存储",
            Self::Busy => "本地状态检查仍在运行",
            Self::WorkerStopped => "后台状态通道已停止",
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Self::DataDirectory => "检查目录权限后重试。",
            Self::Database => "检查本地数据库文件、权限或空间后重试。",
            Self::CredentialStore => "检查系统凭据服务后重试。",
            Self::Busy => "稍后重试。",
            Self::WorkerStopped => "重新启动 SwitchX 后重试。",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub base_url: String,
    pub model_id: String,
    pub credential_status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub providers: Vec<ProviderView>,
    pub models: Vec<ModelView>,
    pub credentials_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelView {
    pub provider_id: String,
    pub provider_name: String,
    pub upstream_model: String,
    pub public_id: String,
    pub display_name: String,
    pub detail: String,
    pub ready: bool,
    pub enabled: bool,
    pub fallback_provider_id: String,
    pub fallback_label: String,
}

pub struct RequestView {
    pub time: String,
    pub route: String,
    pub timing: String,
    pub detail: String,
    pub status: RequestStatus,
    pub error: String,
    pub fallback: String,
}

pub fn load_requests(data_dir: &Path) -> Result<Vec<RequestView>, AppError> {
    let store = open_store(data_dir)?;
    let providers = store.providers().map_err(|_| AppError::Database)?;
    store
        .requests(100)
        .map_err(|_| AppError::Database)?
        .into_iter()
        .map(|record| {
            let provider = record
                .provider_id
                .as_ref()
                .map(|id| {
                    providers
                        .iter()
                        .find(|provider| &provider.id == id)
                        .map(|provider| provider.name.as_str())
                        .unwrap_or(id)
                })
                .unwrap_or("未选择上游");
            let millis = |value: Option<i64>| {
                value
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "—".into())
            };
            Ok(RequestView {
                time: store
                    .request_time(record.started_at_ms)
                    .map_err(|_| AppError::Database)?,
                route: format!(
                    "{} → {} / {}",
                    record.public_model.as_deref().unwrap_or("未指定模型"),
                    provider,
                    record.upstream_model.as_deref().unwrap_or("—")
                ),
                timing: format!(
                    "总耗时 {} ms · 响应头 {} · 首事件 {} · 上游 HTTP {}",
                    record.duration_ms,
                    millis(record.headers_ms),
                    millis(record.first_event_ms),
                    record
                        .http_status
                        .map(|status| status.to_string())
                        .unwrap_or_else(|| "—".into())
                ),
                detail: format!("请求 {} · 路由版本 {}", record.id, record.generation),
                status: record.status,
                error: record
                    .error_code
                    .map(|code| format!("{} · {code}", request_error_message(&code)))
                    .unwrap_or_default(),
                fallback: record
                    .fallback_from
                    .as_ref()
                    .map(|id| {
                        let primary = providers
                            .iter()
                            .find(|provider| &provider.id == id)
                            .map(|provider| provider.name.as_str())
                            .unwrap_or(id);
                        format!("{primary} 建立连接失败（请求未发送）→ 尝试备用 {provider}")
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn request_error_message(code: &str) -> &'static str {
    match code {
        "client_disconnected" => "完成前客户端断开；可能是用户取消或连接丢失",
        "router_stopping" => "路由停止，未收到完成信号",
        "missing_completion" => "响应结束，但未收到正常完成信号",
        "upstream_timeout" => "上游请求超时",
        "upstream_unavailable" => "无法连接上游",
        "no_eligible_upstream" => "主备上游均无法建立连接",
        "upstream_read_error" => "读取上游时连接中断",
        "upstream_http_error" => "上游返回非成功 HTTP 状态",
        "upstream_response_failed" | "upstream_stream_error" => "上游报告请求失败",
        "upstream_response_incomplete" => "上游报告响应未完成",
        "upstream_response_cancelled" => "上游报告响应已取消",
        "invalid_upstream_event" | "invalid_upstream_response" => "上游响应格式无效",
        "upstream_event_too_large" | "upstream_body_too_large" => "上游响应超过读取上限",
        "unknown_model" => "模型未发布",
        "unsupported_capability" => "请求包含暂不支持的会话状态",
        _ => "本地请求校验或转发失败",
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

pub fn load_snapshot(data_dir: &Path, check_credentials: bool) -> Result<Snapshot, AppError> {
    let store = open_store(data_dir)?;
    let providers = store.providers().map_err(|_| AppError::Database)?;
    let models = store.models().map_err(|_| AppError::Database)?;
    let credentials =
        CredentialStore::new(PROVIDER_KEY_SERVICE).map_err(|_| AppError::CredentialStore)?;
    Ok(Snapshot {
        models: providers
            .iter()
            .map(|provider| {
                let model = models.iter().find(|model| model.provider_id == provider.id);
                let metadata = model.and_then(|model| {
                    serde_json::from_str::<serde_json::Value>(&model.metadata).ok()
                });
                let ready = model.is_some_and(|model| model.upstream_model == provider.model_id)
                    && metadata.as_ref().is_some_and(|metadata| {
                        metadata["slug"] == provider.model_id
                            && catalog::validate_metadata(metadata).is_ok()
                    });
                let detail = if ready {
                    let metadata = metadata.as_ref().unwrap();
                    format!(
                        "上下文 {} · 资料已导入 · 实际能力待验证",
                        metadata["context_window"]
                    )
                } else if model.is_some() {
                    "模型已变化或资料无效，请重新导入".into()
                } else {
                    "待导入模型资料".into()
                };
                ModelView {
                    provider_id: provider.id.clone(),
                    provider_name: provider.name.clone(),
                    upstream_model: provider.model_id.clone(),
                    public_id: model
                        .map(|model| model.public_id.clone())
                        .unwrap_or_else(|| format!("sx-{}", provider.id)),
                    display_name: model
                        .map(|model| model.display_name.clone())
                        .unwrap_or_else(|| format!("{} · {}", provider.name, provider.model_id)),
                    detail,
                    ready,
                    enabled: ready && model.is_some_and(|model| model.enabled),
                    fallback_provider_id: model
                        .and_then(|model| model.fallback_provider_id.clone())
                        .unwrap_or_default(),
                    fallback_label: model
                        .and_then(|model| model.fallback_provider_id.as_ref())
                        .map(|id| {
                            let fallback = providers.iter().find(|provider| &provider.id == id);
                            format!(
                                "备用：{} · 仅连接建立失败时尝试",
                                fallback
                                    .map(|provider| provider.name.as_str())
                                    .unwrap_or(id)
                            )
                        })
                        .unwrap_or_else(|| "备用：未设置".into()),
                }
            })
            .collect(),
        providers: providers
            .into_iter()
            .map(|provider| provider_view(provider, &credentials, check_credentials))
            .collect(),
        credentials_checked: check_credentials,
    })
}

fn provider_view(
    provider: ProviderRecord,
    credentials: &CredentialStore,
    check_credentials: bool,
) -> ProviderView {
    let credential_status = match provider.credential_ref.as_deref() {
        None => "未配置凭据",
        Some(_) if !check_credentials => "凭据未检查",
        Some(reference) => match credentials.get(reference) {
            Ok(_) => "凭据可读取",
            Err(CredentialError::Missing) => "凭据缺失",
            Err(CredentialError::InvalidReference) => "凭据引用无效",
            Err(CredentialError::Unavailable) => "系统凭据不可用",
        },
    };
    ProviderView {
        id: provider.id,
        name: provider.name.clone(),
        endpoint: reqwest::Url::parse(&provider.base_url)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|| "地址无效".into()),
        base_url: validate_provider(&provider.name, &provider.base_url, &provider.model_id)
            .map(|url| url.to_string())
            .unwrap_or_default(),
        model_id: provider.model_id,
        credential_status,
    }
}

pub fn save_provider(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    base_url: &str,
    model_id: &str,
    key: String,
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let url = validate_provider(name, base_url, model_id)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let old = match id {
        Some(id) => Some(
            store
                .provider(id)
                .map_err(|_| "无法读取上游资料")?
                .ok_or("上游不存在，请刷新后重试")?,
        ),
        None => None,
    };
    let id = match &old {
        Some(record) => record.id.clone(),
        None => new_id()?,
    };
    let changing_key = !key.is_empty();
    let reference = if key.is_empty() {
        old.as_ref()
            .and_then(|record| record.credential_ref.clone())
            .ok_or("请输入 API Key")?
    } else {
        id.clone()
    };
    let credentials =
        CredentialStore::new(PROVIDER_KEY_SERVICE).map_err(|_| "系统凭据存储不可用")?;
    let previous_secret = if !key.is_empty()
        && old
            .as_ref()
            .is_some_and(|record| record.credential_ref.as_deref() == Some(id.as_str()))
    {
        match credentials.get(&id) {
            Ok(secret) => Some(secret),
            Err(CredentialError::Missing) => None,
            Err(_) => return Err("无法读取旧 API Key，未修改上游".into()),
        }
    } else {
        None
    };
    if changing_key {
        credentials
            .put(&reference, &Secret::new(key))
            .map_err(|_| "无法保存 API Key 到系统凭据存储")?;
    }
    let record = ProviderRecord {
        id: id.clone(),
        name: name.trim().into(),
        base_url: url.to_string(),
        model_id: model_id.into(),
        credential_ref: Some(reference.clone()),
    };
    if store.put_provider(&record).is_err() {
        if changing_key {
            if let Some(previous_secret) = &previous_secret {
                let _ = credentials.put(&reference, previous_secret);
            } else {
                let _ = credentials.delete(&reference);
            }
        }
        return Err("无法保存上游资料".into());
    }
    Ok(())
}

pub fn delete_provider(data_dir: &Path, id: &str) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let record = store
        .provider(id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    let credentials =
        CredentialStore::new(PROVIDER_KEY_SERVICE).map_err(|_| "系统凭据存储不可用")?;
    if record.credential_ref.as_deref() == Some(id) {
        match credentials.get(id) {
            Ok(_) | Err(CredentialError::Missing) => {}
            Err(_) => return Err("无法检查系统凭据，未删除上游".into()),
        }
    }
    store.delete_provider(id).map_err(|_| "无法删除上游资料")?;
    if let Some(reference) = record.credential_ref.filter(|reference| reference == id) {
        match credentials.delete(&reference) {
            Ok(()) | Err(CredentialError::Missing) => {}
            Err(_) => return Err("上游资料已删除，但系统凭据清理失败".into()),
        }
    }
    Ok(())
}

pub fn save_model(
    data_dir: &Path,
    provider_id: &str,
    public_id: &str,
    display_name: &str,
    catalog_path: &str,
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let provider = store
        .provider(provider_id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let old = models.iter().find(|model| model.provider_id == provider_id);
    if models
        .iter()
        .any(|model| model.public_id == public_id && model.provider_id != provider_id)
    {
        return Err("公开模型 ID 已由另一上游使用".into());
    }
    let metadata = if catalog_path.trim().is_empty() {
        old.filter(|model| model.upstream_model == provider.model_id)
            .ok_or("请输入包含此模型的 Codex 目录 JSON 文件路径")?
            .metadata
            .clone()
    } else {
        catalog::read_template(Path::new(catalog_path.trim()), &provider.model_id)?.to_string()
    };
    let mut model = ModelRecord {
        provider_id: provider_id.into(),
        public_id: public_id.into(),
        display_name: display_name.trim().into(),
        upstream_model: provider.model_id,
        metadata,
        enabled: true,
        fallback_provider_id: None,
    };
    catalog::publish_saved(std::slice::from_ref(&model))?;
    model.enabled = old.is_none_or(|model| model.enabled);
    model.fallback_provider_id = old.and_then(|model| model.fallback_provider_id.clone());
    store
        .put_model(&model)
        .map_err(|_| "无法保存模型资料".into())
}

pub fn select_model(data_dir: &Path, provider_id: &str, enabled: bool) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let mut model = models
        .iter()
        .find(|model| model.provider_id == provider_id)
        .ok_or("请先导入模型资料")?
        .clone();
    if enabled {
        let provider = store
            .provider(provider_id)
            .map_err(|_| "无法读取上游资料")?
            .ok_or("上游不存在")?;
        if model.upstream_model != provider.model_id {
            return Err("上游模型已变化，请重新导入资料".into());
        }
        model.enabled = true;
        catalog::validate_fallback(&model, &models)?;
        let mut validation = model.clone();
        validation.fallback_provider_id = None;
        catalog::publish_saved(&[validation])?;
    }
    model.enabled = enabled;
    store
        .put_model(&model)
        .map_err(|_| "无法保存模型选择".into())
}

pub fn save_fallback(
    data_dir: &Path,
    provider_id: &str,
    fallback_id: Option<&str>,
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let mut model = models
        .iter()
        .find(|model| model.provider_id == provider_id)
        .ok_or("请先导入主上游模型资料")?
        .clone();
    model.fallback_provider_id = fallback_id.map(str::to_owned);
    catalog::validate_fallback(&model, &models)?;
    if let Some(fallback_id) = fallback_id {
        for id in [provider_id, fallback_id] {
            let provider = store
                .provider(id)
                .map_err(|_| "无法读取上游资料")?
                .ok_or("上游不存在")?;
            if provider.model_id != model.upstream_model {
                return Err("主备上游模型已变化，请重新导入资料".into());
            }
            validate_provider(&provider.name, &provider.base_url, &provider.model_id)?;
        }
    }
    store
        .put_model(&model)
        .map_err(|_| "无法保存备用上游".into())
}

pub fn provider_credential(provider: &ProviderRecord) -> Result<Secret, String> {
    let reference = provider
        .credential_ref
        .as_deref()
        .ok_or("上游未配置 API Key")?;
    CredentialStore::new(PROVIDER_KEY_SERVICE)
        .and_then(|store| store.get(reference))
        .map_err(|_| "无法读取上游 API Key".into())
}

fn ensure_editable(data_dir: &Path) -> Result<(), String> {
    if data_dir.join("direct-journal.json").exists()
        || data_dir.join("switch-journal.json").exists()
    {
        return Err("SwitchX 正在管理配置，请先恢复原配置再编辑上游或模型".into());
    }
    Ok(())
}

pub fn load_provider(data_dir: &Path, id: &str) -> Result<ProviderRecord, String> {
    open_store(data_dir)
        .map_err(|error| error.message())?
        .provider(id)
        .map_err(|_| "无法读取上游资料".to_owned())?
        .ok_or_else(|| "上游不存在，请刷新后重试".into())
}

pub(crate) fn new_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "无法生成上游 ID")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reads_metadata_and_redacts_credential_reference() {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let suffix = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = env::temp_dir().join(format!("switchx-app-{suffix}"));
        let store = open_store(&path).unwrap();
        store
            .put_provider(&ProviderRecord {
                id: "probe".into(),
                name: "Test Provider".into(),
                base_url: "https://user:secret@example.invalid/v1?token=private".into(),
                model_id: "test-model".into(),
                credential_ref: Some("../invalid-secret-reference".into()),
            })
            .unwrap();
        drop(store);
        let initial = load_snapshot(&path, false).unwrap();
        assert_eq!(initial.providers[0].credential_status, "凭据未检查");
        let checked = load_snapshot(&path, true).unwrap();
        assert_eq!(checked.providers[0].credential_status, "凭据引用无效");
        assert_eq!(checked.providers[0].endpoint, "https://example.invalid");
        assert!(!format!("{checked:?}").contains("invalid-secret-reference"));
        assert!(!format!("{checked:?}").contains("private"));
        rusqlite::Connection::open(path.join("switchx.sqlite"))
            .unwrap()
            .execute_batch("DROP TABLE providers")
            .unwrap();
        assert_eq!(load_snapshot(&path, false), Err(AppError::Database));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(path.join("switchx.sqlite"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn active_direct_switch_blocks_provider_mutation() {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let path = env::temp_dir().join(format!(
            "switchx-active-provider-{}",
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("direct-journal.json"), "synthetic journal").unwrap();
        assert!(
            save_provider(
                &path,
                None,
                "Mock",
                "https://example.invalid/v1",
                "mock",
                "key".into()
            )
            .unwrap_err()
            .contains("先恢复")
        );
        assert!(
            delete_provider(&path, "any")
                .unwrap_err()
                .contains("先恢复")
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn invalid_import_keeps_saved_model_and_active_route_blocks_edits() {
        let path = env::temp_dir().join(format!("switchx-import-model-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        store
            .put_provider(&ProviderRecord {
                id: "mock".into(),
                name: "Mock".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "deepseek-flash".into(),
                credential_ref: None,
            })
            .unwrap();
        let source = path.join("models.json");
        fs::write(
            &source,
            include_str!("../tests/fixtures/synthetic-models.json"),
        )
        .unwrap();
        save_model(&path, "mock", "sx-mock", "Mock", source.to_str().unwrap()).unwrap();
        let saved = store.models().unwrap();
        assert_eq!(saved.len(), 1);
        assert!(load_snapshot(&path, false).unwrap().models[0].ready);
        store
            .put_provider(&ProviderRecord {
                id: "backup".into(),
                name: "Backup".into(),
                base_url: "https://backup.invalid/v1".into(),
                model_id: "deepseek-flash".into(),
                credential_ref: None,
            })
            .unwrap();
        save_model(
            &path,
            "backup",
            "sx-backup",
            "Backup",
            source.to_str().unwrap(),
        )
        .unwrap();
        select_model(&path, "backup", false).unwrap();
        save_fallback(&path, "mock", Some("backup")).unwrap();
        assert!(save_fallback(&path, "mock", Some("mock")).is_err());
        assert!(save_fallback(&path, "mock", Some("missing")).is_err());
        save_model(&path, "mock", "sx-mock", "Mock", "").unwrap();
        select_model(&path, "mock", true).unwrap();
        let saved = store.models().unwrap();
        assert_eq!(
            saved
                .iter()
                .find(|model| model.provider_id == "mock")
                .unwrap()
                .fallback_provider_id
                .as_deref(),
            Some("backup")
        );
        fs::write(&source, r#"{"models":[{"slug":"deepseek-flash"}]}"#).unwrap();
        assert!(save_model(&path, "mock", "sx-mock", "Broken", source.to_str().unwrap()).is_err());
        assert_eq!(store.models().unwrap(), saved);
        select_model(&path, "mock", false).unwrap();
        assert!(!store.models().unwrap()[0].enabled);
        fs::write(path.join("switch-journal.json"), "synthetic journal").unwrap();
        assert!(
            select_model(&path, "mock", true)
                .unwrap_err()
                .contains("先恢复")
        );
        assert!(save_model(&path, "mock", "sx-changed", "Changed", "").is_err());
        assert!(delete_provider(&path, "mock").is_err());
        assert!(save_fallback(&path, "mock", None).is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }
}
