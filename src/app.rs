use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use crate::{
    catalog,
    credentials::Secret,
    direct::validate_provider,
    provider_config::{self, CodexOptions},
    storage::{ModelRecord, ProviderRecord, RequestStatus, Store},
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderView {
    pub id: String,
    pub name: String,
    pub icon_id: String,
    pub endpoint: String,
    pub base_url: String,
    pub model_id: String,
    pub credential_status: &'static str,
    pub preset_id: &'static str,
    pub kind: crate::storage::ProviderKind,
    pub binding_label: String,
}

pub struct ModelPreset {
    pub model_id: &'static str,
    pub context_window: u64,
    pub reasoning_levels: &'static [&'static str],
    pub default_reasoning: &'static str,
    pub input_modalities: &'static [&'static str],
    pub parallel_tool_calls: bool,
    pub base_instructions: Option<&'static str>,
}

impl ModelPreset {
    fn metadata(&self) -> Result<serde_json::Value, String> {
        let mut metadata = catalog::mapping_metadata(
            self.model_id,
            self.model_id,
            &catalog::MappingSettings {
                context_window: &self.context_window.to_string(),
                reasoning_levels: Some(&self.reasoning_levels.join(", ")),
                default_reasoning: Some(self.default_reasoning),
            },
            None,
        )?;
        metadata["input_modalities"] = serde_json::json!(self.input_modalities);
        metadata["supports_parallel_tool_calls"] = self.parallel_tool_calls.into();
        metadata["supports_reasoning_summaries"] = true.into();
        metadata["effective_context_window_percent"] = 95.into();
        if let Some(instructions) = self.base_instructions {
            metadata["base_instructions"] = instructions.into();
        }
        catalog::validate_metadata(&metadata)?;
        Ok(metadata)
    }
}

pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub model_id: &'static str,
    pub models: &'static [ModelPreset],
    pub website_url: &'static str,
    pub api_key_url: &'static str,
    pub icon: &'static [u8],
    pub monochrome: bool,
}

// Model parameters from the CC Switch da193d4 preset snapshot (2026-09-23).
// Keep SwitchX's shell-command tool profile; model parameters remain editable.
const MIMO_BASE_INSTRUCTIONS: &str = "You are MiMo, an AI assistant developed by Xiaomi. Today's date: {date} {week}. Your knowledge cutoff date is December 2024.";

pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        model_id: "deepseek-flash",
        models: &[
            ModelPreset {
                model_id: "deepseek-flash",
                context_window: 1_048_576,
                reasoning_levels: &["low", "high", "max"],
                default_reasoning: "high",
                input_modalities: &["text", "image"],
                parallel_tool_calls: true,
                base_instructions: None,
            },
            ModelPreset {
                model_id: "deepseek-v4-pro",
                context_window: 1_048_576,
                reasoning_levels: &["low", "high", "max"],
                default_reasoning: "high",
                input_modalities: &["text"],
                parallel_tool_calls: true,
                base_instructions: None,
            },
        ],
        website_url: "https://platform.deepseek.com",
        api_key_url: "https://platform.deepseek.com/api_keys",
        icon: include_bytes!("../assets/providers/deepseek.svg"),
        monochrome: false,
    },
    ProviderPreset {
        id: "kimi",
        name: "Kimi",
        base_url: "https://api.moonshot.cn/v1",
        model_id: "kimi-k3",
        models: &[
            ModelPreset {
                model_id: "kimi-k3",
                context_window: 1_048_576,
                reasoning_levels: &["low", "high", "max"],
                default_reasoning: "high",
                input_modalities: &["text", "image"],
                parallel_tool_calls: true,
                base_instructions: None,
            },
            ModelPreset {
                model_id: "kimi-k2.7-code",
                context_window: 262_144,
                reasoning_levels: &["high"],
                default_reasoning: "high",
                input_modalities: &["text", "image"],
                parallel_tool_calls: true,
                base_instructions: None,
            },
        ],
        website_url: "https://platform.kimi.com",
        api_key_url: "https://platform.kimi.com/console/api-keys",
        icon: include_bytes!("../assets/providers/kimi.svg"),
        monochrome: true,
    },
    ProviderPreset {
        id: "minimax",
        name: "MiniMax",
        base_url: "https://api.minimax.cn/v1",
        model_id: "MiniMax-M3",
        models: &[ModelPreset {
            model_id: "MiniMax-M3",
            context_window: 1_000_000,
            reasoning_levels: &["none", "high"],
            default_reasoning: "high",
            input_modalities: &["text", "image"],
            parallel_tool_calls: true,
            base_instructions: Some(
                "You are Codex, a coding agent based on MiniMax-M3. You and the user share the same workspace and collaborate to achieve the user's goals.",
            ),
        }],
        website_url: "https://platform.minimax.cn",
        api_key_url: "https://platform.minimax.cn/subscribe/token-plan",
        icon: include_bytes!("../assets/providers/minimax.svg"),
        monochrome: false,
    },
    ProviderPreset {
        id: "xiaomimimo",
        name: "小米 MiMo",
        base_url: "https://api.xiaomimimo.com/v1",
        model_id: "mimo-v2.6-pro",
        models: &[
            ModelPreset {
                model_id: "mimo-v2.6-pro",
                context_window: 1_048_576,
                reasoning_levels: &["none", "low", "medium", "high"],
                default_reasoning: "low",
                input_modalities: &["text", "image"],
                parallel_tool_calls: false,
                base_instructions: Some(MIMO_BASE_INSTRUCTIONS),
            },
            ModelPreset {
                model_id: "mimo-v2.6-flash",
                context_window: 1_048_576,
                reasoning_levels: &["none", "low", "medium", "high"],
                default_reasoning: "low",
                input_modalities: &["text", "image"],
                parallel_tool_calls: false,
                base_instructions: Some(MIMO_BASE_INSTRUCTIONS),
            },
            ModelPreset {
                model_id: "mimo-v2.6-pro-ultraspeed",
                context_window: 1_048_576,
                reasoning_levels: &["none", "low", "medium", "high"],
                default_reasoning: "low",
                input_modalities: &["text", "image"],
                parallel_tool_calls: false,
                base_instructions: Some(MIMO_BASE_INSTRUCTIONS),
            },
            ModelPreset {
                model_id: "mimo-v2.5-pro",
                context_window: 1_048_576,
                reasoning_levels: &["none", "low", "medium", "high"],
                default_reasoning: "low",
                input_modalities: &["text"],
                parallel_tool_calls: false,
                base_instructions: Some(MIMO_BASE_INSTRUCTIONS),
            },
            ModelPreset {
                model_id: "mimo-v2.5",
                context_window: 1_048_576,
                reasoning_levels: &["none", "low", "medium", "high"],
                default_reasoning: "low",
                input_modalities: &["text", "image"],
                parallel_tool_calls: false,
                base_instructions: Some(MIMO_BASE_INSTRUCTIONS),
            },
        ],
        website_url: "https://platform.xiaomimimo.com",
        api_key_url: "https://platform.xiaomimimo.com/#/console/api-keys",
        icon: include_bytes!("../assets/providers/xiaomimimo.svg"),
        monochrome: true,
    },
];

pub fn provider_preset(base_url: &str) -> Option<&'static ProviderPreset> {
    let url = crate::direct::validate_base_url(base_url).ok()?;
    PROVIDER_PRESETS.iter().find(|preset| {
        let address = url.as_str().trim_end_matches('/');
        address == preset.base_url
            || (preset.id == "deepseek" && address == "https://api.deepseek.com/v1")
    })
}

fn new_preset_models(
    provider: &ProviderRecord,
    existing: &[ModelRecord],
) -> Result<Vec<ModelRecord>, String> {
    let Some(preset) = provider_preset(&provider.base_url) else {
        return Ok(Vec::new());
    };
    let has_mappings = existing
        .iter()
        .any(|model| model.provider_id == provider.id);
    preset
        .models
        .iter()
        .filter(|preset_model| {
            !existing.iter().any(|model| {
                model.provider_id == provider.id && model.upstream_model == preset_model.model_id
            })
        })
        .map(|model| {
            Ok(ModelRecord {
                provider_id: provider.id.clone(),
                public_id: format!("sx-{}", new_id()?),
                display_name: format!("{}/{}", model.model_id, provider.name),
                upstream_model: model.model_id.into(),
                metadata: model.metadata()?.to_string(),
                enabled: !has_mappings && model.model_id == provider.model_id,
                fallback_provider_id: None,
            })
        })
        .collect()
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
    pub binding_label: String,
    pub upstream_model: String,
    pub public_id: String,
    pub display_name: String,
    pub detail: String,
    pub context_window: String,
    pub reasoning_levels: String,
    pub default_reasoning: String,
    pub saved: bool,
    pub ready: bool,
    pub enabled: bool,
    pub fallback_provider_id: String,
    pub fallback_label: String,
}

pub struct RequestView {
    pub time: String,
    pub route: String,
    pub timing: String,
    pub duration: String,
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
                duration: if record.duration_ms >= 1_000 {
                    format!("{:.1} s", record.duration_ms as f64 / 1_000.0)
                } else {
                    format!("{} ms", record.duration_ms)
                },
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
        "chatgpt_unauthorized" => "官方认证被拒绝；Codex 会尝试续期，仍失败时请恢复并重新登录",
        "chatgpt_forbidden" => "官方账号没有此模型或工作区权限，请检查订阅与模型选择",
        "chatgpt_account_changed" => "工作区已变化，请恢复并重新发布路由",
        "xai_auth_required" | "xai_unauthorized" => "Grok 账号凭据不可用，请重新登录并发布路由",
        "xai_forbidden" => "Grok 账号没有此模型权限或额度，请检查账号",
        "unsupported_xai_tool" | "unsupported_xai_tool_schema" => {
            "Grok 不支持此工具形态，请使用函数工具模型资料"
        }
        "xai_tool_name_collision" => "Grok 工具名称展平后冲突，请修改工具名称",
        "invalid_xai_response" | "xai_response_too_large" => "Grok 响应格式或大小无效",
        "chatgpt_auth_required" | "chatgpt_account_required" => {
            "缺少订阅认证或工作区，请在目标 Codex 中完成 ChatGPT 登录"
        }
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
    let mut views = Vec::new();
    for provider in &providers {
        let mappings: Vec<_> = models
            .iter()
            .filter(|model| model.provider_id == provider.id)
            .collect();
        if mappings.is_empty() {
            views.push(model_view(provider, None, &providers));
        } else {
            views.extend(
                mappings
                    .into_iter()
                    .map(|model| model_view(provider, Some(model), &providers)),
            );
        }
    }
    Ok(Snapshot {
        models: views,
        providers: providers
            .into_iter()
            .map(|provider| provider_view(provider, &store, check_credentials))
            .collect(),
        credentials_checked: check_credentials,
    })
}

fn model_view(
    provider: &ProviderRecord,
    model: Option<&ModelRecord>,
    providers: &[ProviderRecord],
) -> ModelView {
    let metadata =
        model.and_then(|model| serde_json::from_str::<serde_json::Value>(&model.metadata).ok());
    let ready = model.is_some_and(|model| {
        metadata.as_ref().is_some_and(|metadata| {
            metadata["slug"] == model.upstream_model && catalog::validate_metadata(metadata).is_ok()
        })
    });
    let detail = if ready {
        let metadata = metadata.as_ref().unwrap();
        format!(
            "上下文 {} · 资料已保存 · 实际能力待验证",
            metadata["context_window"]
        )
    } else if model.is_some() {
        "模型资料无效，请编辑或重新导入".into()
    } else {
        "尚未添加模型映射".into()
    };
    ModelView {
        provider_id: provider.id.clone(),
        provider_name: provider.name.clone(),
        binding_label: binding_label(provider),
        upstream_model: model
            .map(|model| model.upstream_model.clone())
            .unwrap_or_else(|| provider.model_id.clone()),
        public_id: model
            .map(|model| model.public_id.clone())
            .unwrap_or_else(|| format!("sx-{}", provider.id)),
        display_name: model
            .map(|model| model.display_name.clone())
            .unwrap_or_else(|| format!("{} · {}", provider.name, provider.model_id)),
        detail,
        context_window: metadata
            .as_ref()
            .and_then(|value| value["context_window"].as_i64())
            .map(|value| value.to_string())
            .unwrap_or_default(),
        reasoning_levels: metadata
            .as_ref()
            .and_then(|value| value["supported_reasoning_levels"].as_array())
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| level["effort"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        default_reasoning: metadata
            .as_ref()
            .and_then(|value| value["default_reasoning_level"].as_str())
            .unwrap_or("")
            .into(),
        saved: model.is_some(),
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
}

fn provider_view(provider: ProviderRecord, store: &Store, check_credentials: bool) -> ProviderView {
    let credential_status = if provider.kind == crate::storage::ProviderKind::XaiOAuth {
        "Grok OAuth · 发布时核对账号绑定"
    } else if provider.kind == crate::storage::ProviderKind::Chatgpt {
        "订阅凭据由绑定账号提供"
    } else {
        match store.has_provider_api_key(&provider.id) {
            Ok(true) if !check_credentials => "凭据未检查",
            Ok(true) => "凭据可读取",
            Ok(false) => "未配置 API Key，请重新输入",
            Err(_) => "本地凭据格式无效",
        }
    };
    let preset_id = provider_preset(&provider.base_url)
        .map(|preset| preset.id)
        .unwrap_or("");
    ProviderView {
        kind: provider.kind,
        binding_label: binding_label(&provider),
        id: provider.id,
        name: provider.name.clone(),
        icon_id: provider.icon_id.unwrap_or_default(),
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
        preset_id,
    }
}

fn binding_label(provider: &ProviderRecord) -> String {
    use crate::storage::AccountBinding;
    match &provider.account_binding {
        Some(AccountBinding::Native) => "绑定：原生 Codex 登录".into(),
        Some(AccountBinding::Default) => "绑定：默认保存账号（发布时固定）".into(),
        Some(AccountBinding::Fixed(id)) => format!("绑定：保存账号 {id}"),
        None => String::new(),
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
    save_provider_inner(data_dir, id, name, base_url, model_id, key, None, None, &[]).map(drop)
}

pub fn save_provider_with_codex_options(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    base_url: &str,
    model_id: &str,
    key: String,
    options: &CodexOptions,
) -> Result<(), String> {
    save_provider_with_codex_options_and_icon(
        data_dir, id, name, base_url, model_id, key, options, None,
    )
}

/// None preserves the saved icon; an empty ID clears its custom override.
#[allow(clippy::too_many_arguments)]
pub fn save_provider_with_codex_options_and_icon(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    base_url: &str,
    model_id: &str,
    key: String,
    options: &CodexOptions,
    icon_id: Option<&str>,
) -> Result<(), String> {
    save_provider_with_mappings(
        data_dir,
        id,
        name,
        base_url,
        model_id,
        key,
        options,
        icon_id,
        &[],
    )
    .map(drop)
}

/// Also adds mappings for models picked from discovery in the same transaction.
/// Returns the public IDs of every mapping this save created, presets included.
#[allow(clippy::too_many_arguments)]
pub fn save_provider_with_mappings(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    base_url: &str,
    model_id: &str,
    key: String,
    options: &CodexOptions,
    icon_id: Option<&str>,
    picked: &[String],
) -> Result<Vec<String>, String> {
    options.validate()?;
    save_provider_inner(
        data_dir,
        id,
        name,
        base_url,
        model_id,
        key,
        Some(options),
        icon_id,
        picked,
    )
}

pub(crate) fn resolve_provider_icon_id(
    icon_id: Option<&str>,
    old: Option<&ProviderRecord>,
) -> Result<Option<String>, String> {
    match icon_id {
        None => Ok(old.and_then(|provider| provider.icon_id.clone())),
        Some("") => Ok(None),
        Some(id) => crate::provider_icons::icon(id)
            .map(|icon| Some(icon.id.to_owned()))
            .ok_or_else(|| "请选择有效的上游图标".into()),
    }
}

#[allow(clippy::too_many_arguments)]
fn save_provider_inner(
    data_dir: &Path,
    id: Option<&str>,
    name: &str,
    base_url: &str,
    model_id: &str,
    key: String,
    options: Option<&CodexOptions>,
    icon_id: Option<&str>,
    picked: &[String],
) -> Result<Vec<String>, String> {
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
    if old
        .as_ref()
        .is_some_and(|provider| provider.kind != crate::storage::ProviderKind::ApiKey)
    {
        return Err("请使用订阅连接编辑器修改名称或绑定账号".into());
    }
    let icon_id = resolve_provider_icon_id(icon_id, old.as_ref())?;
    let id = match &old {
        Some(record) => record.id.clone(),
        None => new_id()?,
    };
    let key = if key.is_empty() {
        old.as_ref().ok_or("请输入 API Key")?;
        None
    } else {
        Some(Secret::new(key))
    };
    let record = ProviderRecord {
        kind: crate::storage::ProviderKind::ApiKey,
        account_binding: None,
        id: id.clone(),
        name: name.trim().into(),
        base_url: url.to_string(),
        model_id: model_id.into(),
        credential_ref: None,
        icon_id,
    };
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let mut defaults = new_preset_models(&record, &models)?;
    let picked = picked_models(&record, &models, &defaults, picked)?;
    defaults.extend(picked);
    let options = options
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| "无法序列化 Codex 选项")?;
    let saved = match &key {
        Some(key) => store.put_provider_with_models_options_and_key(
            &record,
            &defaults,
            options.as_deref(),
            key,
        ),
        None => store.update_provider_preserving_key(&record, &defaults, options.as_deref()),
    };
    saved.map_err(|_| {
        if key.is_none() && store.has_provider_api_key(&id).is_ok_and(|exists| !exists) {
            "上游未配置 API Key，请重新输入"
        } else {
            "无法保存上游资料与 API Key；原资料未修改"
        }
    })?;
    Ok(defaults.into_iter().map(|model| model.public_id).collect())
}

// Mappings for models picked in the upstream form, beside any preset models.
// As with presets, only a new upstream's default model joins the selection.
fn picked_models(
    provider: &ProviderRecord,
    existing: &[ModelRecord],
    presets: &[ModelRecord],
    picked: &[String],
) -> Result<Vec<ModelRecord>, String> {
    let selected = existing
        .iter()
        .any(|model| model.provider_id == provider.id)
        || presets.iter().any(|model| model.enabled);
    let mut added: Vec<ModelRecord> = Vec::new();
    for model_id in picked {
        if existing
            .iter()
            .chain(presets)
            .chain(&added)
            .any(|model| model.provider_id == provider.id && &model.upstream_model == model_id)
        {
            continue;
        }
        let display_name = format!("{model_id}/{}", provider.name);
        let metadata = catalog::mapping_metadata(
            model_id,
            &display_name,
            &catalog::MappingSettings {
                context_window: "",
                reasoning_levels: None,
                default_reasoning: None,
            },
            None,
        )
        .map_err(|error| format!("{model_id}：{error}"))?;
        added.push(ModelRecord {
            provider_id: provider.id.clone(),
            public_id: format!("sx-{}", new_id()?),
            display_name,
            upstream_model: model_id.clone(),
            metadata: metadata.to_string(),
            enabled: !selected && model_id == &provider.model_id,
            fallback_provider_id: None,
        });
    }
    Ok(added)
}

pub fn delete_provider(data_dir: &Path, id: &str) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    store
        .provider(id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    store.delete_provider(id).map_err(|_| "无法删除上游资料")?;
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
    let original_id = models
        .iter()
        .find(|model| model.provider_id == provider_id && model.upstream_model == provider.model_id)
        .map(|model| model.public_id.as_str())
        .unwrap_or("");
    save_mapping(
        data_dir,
        ModelInput {
            provider_id,
            original_id,
            public_id,
            display_name,
            upstream_model: &provider.model_id,
            catalog_path,
            settings: None,
        },
    )
}

pub struct ModelInput<'a> {
    pub provider_id: &'a str,
    pub original_id: &'a str,
    pub public_id: &'a str,
    pub display_name: &'a str,
    pub upstream_model: &'a str,
    pub catalog_path: &'a str,
    pub settings: Option<catalog::MappingSettings<'a>>,
}

pub fn save_mapping(data_dir: &Path, input: ModelInput<'_>) -> Result<(), String> {
    save_mapping_from(data_dir, input, None)
}

pub(crate) fn save_mapping_from(
    data_dir: &Path,
    input: ModelInput<'_>,
    source: Option<serde_json::Value>,
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let provider = store
        .provider(input.provider_id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    validate_provider(&provider.name, &provider.base_url, input.upstream_model)?;
    let old = if input.original_id.is_empty() {
        None
    } else {
        Some(
            models
                .iter()
                .find(|model| {
                    model.public_id == input.original_id && model.provider_id == input.provider_id
                })
                .ok_or("原模型映射不存在，请刷新后重试")?,
        )
    };
    if models
        .iter()
        .any(|model| model.public_id == input.public_id && model.public_id != input.original_id)
    {
        return Err("公开模型 ID 已被使用".into());
    }
    if models.iter().any(|model| {
        model.provider_id == input.provider_id
            && model.upstream_model == input.upstream_model
            && model.public_id != input.original_id
    }) {
        return Err("此上游已有该实际模型的映射，请编辑已有条目".into());
    }
    let source = if source.is_some() {
        source
    } else if input.catalog_path.trim().is_empty() {
        old.filter(|model| model.upstream_model == input.upstream_model)
            .map(|model| serde_json::from_str(&model.metadata).map_err(|_| "已保存的模型资料损坏"))
            .transpose()?
    } else {
        Some(catalog::read_template(
            Path::new(input.catalog_path.trim()),
            input.upstream_model,
        )?)
    };
    let metadata = if let Some(settings) = &input.settings {
        catalog::mapping_metadata(
            input.upstream_model,
            input.display_name.trim(),
            settings,
            source,
        )?
    } else {
        source.ok_or("请输入包含此模型的 Codex 目录 JSON 文件路径")?
    };
    let mut model = ModelRecord {
        provider_id: input.provider_id.into(),
        public_id: input.public_id.into(),
        display_name: input.display_name.trim().into(),
        upstream_model: input.upstream_model.into(),
        metadata: metadata.to_string(),
        enabled: true,
        fallback_provider_id: None,
    };
    catalog::publish_saved(std::slice::from_ref(&model))?;
    model.enabled = old.is_none_or(|model| model.enabled);
    model.fallback_provider_id = old
        .filter(|old| old.upstream_model == input.upstream_model)
        .and_then(|model| model.fallback_provider_id.clone());
    let mut updated: Vec<_> = models
        .into_iter()
        .filter(|model| model.public_id != input.original_id)
        .collect();
    updated.push(model.clone());
    for model in &updated {
        catalog::validate_fallback(model, &updated)?;
    }
    store
        .replace_model(
            (!input.original_id.is_empty()).then_some(input.original_id),
            &model,
        )
        .map_err(|_| "无法保存模型资料".into())
}

/// A discovered upstream model and the values a batch addition starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCandidate {
    pub model_id: String,
    pub added: bool,
    pub supported: bool,
    /// Names the preset or official template; empty for the neutral profile.
    pub source: &'static str,
    pub context_window: String,
    pub reasoning_levels: String,
}

pub struct BatchModel<'a> {
    pub upstream_model: &'a str,
    // Empty values inherit the template, then the shared settings.
    pub context_window: &'a str,
    pub reasoning_levels: &'a str,
}

pub struct BatchInput<'a> {
    pub provider_id: &'a str,
    pub models: &'a [BatchModel<'a>],
    pub context_window: &'a str,
    pub reasoning_levels: &'a str,
    pub default_reasoning: &'a str,
}

// Templates come only from an exact preset address or the official CLI catalog,
// never from the model name alone.
fn model_template(
    provider: &ProviderRecord,
    model_id: &str,
    templates: &[serde_json::Value],
) -> Result<Option<serde_json::Value>, String> {
    use crate::storage::ProviderKind;
    match provider.kind {
        ProviderKind::Chatgpt => Ok(templates
            .iter()
            .find(|model| model["slug"] == model_id)
            .cloned()),
        ProviderKind::ApiKey => provider_preset(&provider.base_url)
            .and_then(|preset| {
                preset
                    .models
                    .iter()
                    .find(|model| model.model_id == model_id)
            })
            .map(ModelPreset::metadata)
            .transpose(),
        _ => Ok(None),
    }
}

pub fn batch_candidates(
    data_dir: &Path,
    provider_id: &str,
    model_ids: &[String],
    templates: &[serde_json::Value],
) -> Result<Vec<BatchCandidate>, String> {
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let provider = store
        .provider(provider_id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    candidates_for(&provider, &models, model_ids, templates)
}

/// Candidates for an API upstream that is still being created; preset models
/// carry their source, and none is added yet.
pub fn draft_candidates(base_url: &str, model_ids: &[String]) -> Vec<BatchCandidate> {
    let provider = ProviderRecord {
        kind: crate::storage::ProviderKind::ApiKey,
        account_binding: None,
        id: String::new(),
        name: String::new(),
        base_url: base_url.into(),
        model_id: String::new(),
        credential_ref: None,
        icon_id: None,
    };
    // Preset templates are static and valid, so this cannot fail.
    candidates_for(&provider, &[], model_ids, &[]).unwrap_or_default()
}

fn candidates_for(
    provider: &ProviderRecord,
    models: &[ModelRecord],
    model_ids: &[String],
    templates: &[serde_json::Value],
) -> Result<Vec<BatchCandidate>, String> {
    let chatgpt = provider.kind == crate::storage::ProviderKind::Chatgpt;
    let mut seen = std::collections::HashSet::new();
    model_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| {
            let template = model_template(provider, id, templates)?;
            Ok(BatchCandidate {
                model_id: id.clone(),
                added: models
                    .iter()
                    .any(|model| model.provider_id == provider.id && &model.upstream_model == id),
                supported: crate::direct::validate_model_id(id).is_ok()
                    && (!chatgpt || template.is_some()),
                source: match (&template, chatgpt) {
                    (None, _) => "",
                    (Some(_), true) => "官方目录",
                    (Some(_), false) => "预设参数",
                },
                context_window: template
                    .as_ref()
                    .and_then(|value| value["context_window"].as_i64())
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                reasoning_levels: template
                    .as_ref()
                    .and_then(|value| value["supported_reasoning_levels"].as_array())
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(|level| level["effort"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Returns the new public IDs in input order.
pub fn save_mappings(data_dir: &Path, input: &BatchInput<'_>) -> Result<Vec<String>, String> {
    save_mappings_from(data_dir, input, &[])
}

/// Adds every selected model or none of them. New mappings join the publish
/// selection, matching a single addition.
pub(crate) fn save_mappings_from(
    data_dir: &Path,
    input: &BatchInput<'_>,
    templates: &[serde_json::Value],
) -> Result<Vec<String>, String> {
    ensure_editable(data_dir)?;
    if input.models.is_empty() {
        return Err("请至少选择一个模型".into());
    }
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let provider = store
        .provider(input.provider_id)
        .map_err(|_| "无法读取上游资料")?
        .ok_or("上游不存在")?;
    let existing = store.models().map_err(|_| "无法读取模型资料")?;
    let mut records: Vec<ModelRecord> = Vec::with_capacity(input.models.len());
    for item in input.models {
        let model_id = item.upstream_model;
        validate_provider(&provider.name, &provider.base_url, model_id)
            .map_err(|error| format!("{model_id}：{error}"))?;
        if existing
            .iter()
            .chain(&records)
            .any(|model| model.provider_id == provider.id && model.upstream_model == model_id)
        {
            return Err(format!("{model_id} 已有映射，请刷新列表后重试"));
        }
        let template = model_template(&provider, model_id, templates)?;
        if provider.kind == crate::storage::ProviderKind::Chatgpt && template.is_none() {
            return Err(format!("目标 CLI 内置官方目录没有 {model_id}"));
        }
        let inherit = template.is_some();
        let context_window = if !item.context_window.trim().is_empty() {
            item.context_window
        } else if inherit {
            ""
        } else {
            input.context_window
        };
        let reasoning_levels = if !item.reasoning_levels.trim().is_empty() {
            Some(item.reasoning_levels)
        } else if inherit {
            None
        } else {
            Some(input.reasoning_levels)
        };
        // Changed levels keep the imported default when it still applies.
        let default_reasoning = reasoning_levels
            .map(|text| -> Result<String, String> {
                let allowed = catalog::reasoning_levels(text)
                    .map_err(|error| format!("{model_id}：{error}"))?;
                let imported = template
                    .as_ref()
                    .and_then(|value| value["default_reasoning_level"].as_str())
                    .unwrap_or("");
                Ok([imported, input.default_reasoning]
                    .into_iter()
                    .find(|level| allowed.contains(level))
                    .unwrap_or("")
                    .to_owned())
            })
            .transpose()?;
        let display_name = format!("{model_id}/{}", provider.name);
        let metadata = catalog::mapping_metadata(
            model_id,
            &display_name,
            &catalog::MappingSettings {
                context_window,
                reasoning_levels,
                default_reasoning: default_reasoning.as_deref(),
            },
            template,
        )
        .map_err(|error| format!("{model_id}：{error}"))?;
        records.push(ModelRecord {
            provider_id: provider.id.clone(),
            public_id: format!("sx-{}", new_id()?),
            display_name,
            upstream_model: model_id.into(),
            metadata: metadata.to_string(),
            enabled: true,
            fallback_provider_id: None,
        });
    }
    catalog::publish_saved(&records)?;
    store
        .add_models(&records)
        .map_err(|_| "无法保存模型映射；原资料未修改")?;
    Ok(records.into_iter().map(|model| model.public_id).collect())
}

pub fn select_model(data_dir: &Path, public_id: &str, enabled: bool) -> Result<(), String> {
    select_models(data_dir, &[public_id.into()], enabled)
}

pub fn select_models(data_dir: &Path, public_ids: &[String], enabled: bool) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    for public_id in public_ids {
        let mut model = models
            .iter()
            .find(|model| &model.public_id == public_id)
            .ok_or("请先导入模型资料")?
            .clone();
        if enabled {
            let provider = store
                .provider(&model.provider_id)
                .map_err(|_| "无法读取上游资料")?
                .ok_or("上游不存在")?;
            validate_provider(&provider.name, &provider.base_url, &model.upstream_model)?;
            model.enabled = true;
            catalog::validate_fallback(&model, &models)?;
            model.fallback_provider_id = None;
            catalog::publish_saved(&[model])?;
        }
    }
    store
        .set_models_enabled(public_ids, enabled)
        .map_err(|_| "无法保存模型选择".into())
}

pub fn save_fallback(
    data_dir: &Path,
    public_id: &str,
    fallback_id: Option<&str>,
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let mut model = models
        .iter()
        .find(|model| model.public_id == public_id)
        .ok_or("请先导入主上游模型资料")?
        .clone();
    model.fallback_provider_id = fallback_id.map(str::to_owned);
    catalog::validate_fallback(&model, &models)?;
    if let Some(fallback_id) = fallback_id {
        for id in [model.provider_id.as_str(), fallback_id] {
            let provider = store
                .provider(id)
                .map_err(|_| "无法读取上游资料")?
                .ok_or("上游不存在")?;
            if provider.kind != crate::storage::ProviderKind::ApiKey {
                return Err("订阅账号不参与自动备用切换，请主动选择目标模型".into());
            }
            validate_provider(&provider.name, &provider.base_url, &model.upstream_model)?;
        }
    }
    store
        .put_model(&model)
        .map_err(|_| "无法保存备用上游".into())
}

pub fn delete_model(data_dir: &Path, public_id: &str) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let models = store.models().map_err(|_| "无法读取模型资料")?;
    let model = models
        .iter()
        .find(|model| model.public_id == public_id)
        .ok_or("模型映射不存在")?;
    if models.iter().any(|primary| {
        primary.fallback_provider_id.as_deref() == Some(&model.provider_id)
            && primary.upstream_model == model.upstream_model
    }) {
        return Err("此模型被用作备用，请先清除对应的备用策略".into());
    }
    store
        .delete_model(public_id)
        .map_err(|_| "无法删除模型映射")?;
    Ok(())
}

pub fn provider_credential(data_dir: &Path, provider: &ProviderRecord) -> Result<Secret, String> {
    if provider.kind != crate::storage::ProviderKind::ApiKey {
        return Err("OAuth 上游凭据由账号管理器注入，请使用订阅路由".into());
    }
    let store = open_store(data_dir).map_err(|error| error.message())?;
    store
        .provider_api_key(&provider.id)
        .map_err(|_| "无法读取本地 API Key；请检查数据库后重试".to_owned())?
        .ok_or_else(|| "上游未配置 API Key，请重新输入".into())
}

pub fn ensure_editable(data_dir: &Path) -> Result<(), String> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigState {
    pub options: Option<CodexOptions>,
    pub common: String,
}

pub fn load_provider_config(data_dir: &Path, id: &str) -> Result<ProviderConfigState, String> {
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let options = store
        .provider_codex_options(id)
        .map_err(|_| "无法读取供应商 Codex 选项")?;
    let options = CodexOptions::from_saved(&options)?;
    let common = if options
        .as_ref()
        .is_none_or(|options| options.use_common_config)
    {
        store
            .common_codex_config()
            .map_err(|_| "无法读取 Codex 通用配置")?
    } else {
        String::new()
    };
    if !common.is_empty() {
        provider_config::validate_common(&common)?;
    }
    Ok(ProviderConfigState { options, common })
}

pub fn load_common_config(data_dir: &Path) -> Result<String, String> {
    open_store(data_dir)
        .map_err(|error| error.message())?
        .common_codex_config()
        .map_err(|_| "无法读取 Codex 通用配置".into())
}

pub fn initialize_common_config(data_dir: &Path, codex_home: &Path) -> Result<String, String> {
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let saved = || -> Result<String, String> {
        store
            .common_codex_config()
            .map_err(|_| "无法读取 Codex 通用配置".into())
    };
    if store
        .common_codex_config_initialized()
        .map_err(|_| "无法读取 Codex 通用配置")?
        || ensure_editable(data_dir).is_err()
    {
        return saved();
    }
    let path = crate::client::config_path(codex_home)?;
    let Some(original) = crate::config_transaction::read_config(&path)? else {
        return saved();
    };
    let contents = std::str::from_utf8(&original).map_err(|_| "当前 Codex 配置不是 UTF-8")?;
    let document: toml_edit::DocumentMut = contents
        .parse()
        .map_err(|_| "当前 Codex 配置不是有效的 TOML")?;
    if crate::config::ensure_unmanaged(&document).is_err() {
        return saved();
    }
    let snippet = provider_config::extract_common(contents)?;
    let shared = provider_config::validate_common(&snippet)?;
    if !shared
        .iter()
        .any(|(_, item)| common_config_has_values(item))
    {
        return saved();
    }
    if crate::config_transaction::read_config(&path)? != Some(original) {
        return Err("Codex 配置已变化，请重新提取通用配置".into());
    }
    if ensure_editable(data_dir).is_err() {
        return saved();
    }
    store
        .initialize_common_codex_config(&snippet)
        .map_err(|_| "无法初始化 Codex 通用配置")?;
    saved()
}

fn common_config_has_values(item: &toml_edit::Item) -> bool {
    if let Some(table) = item.as_table_like() {
        table.iter().any(|(_, item)| common_config_has_values(item))
    } else if let Some(tables) = item.as_array_of_tables() {
        tables
            .iter()
            .any(|table| table.iter().any(|(_, item)| common_config_has_values(item)))
    } else {
        item.is_value()
    }
}

pub fn extract_and_save_common_config(data_dir: &Path, form_toml: &str) -> Result<String, String> {
    ensure_editable(data_dir)?;
    let snippet = provider_config::extract_common(form_toml)?;
    save_common_config(data_dir, &snippet)?;
    Ok(snippet)
}

pub fn save_common_config(data_dir: &Path, snippet: &str) -> Result<(), String> {
    ensure_editable(data_dir)?;
    provider_config::validate_common(snippet)?;
    open_store(data_dir)
        .map_err(|error| error.message())?
        .put_common_codex_config(snippet)
        .map_err(|_| "无法保存 Codex 通用配置".into())
}

pub fn new_id() -> Result<String, String> {
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
    fn common_config_initializes_from_selected_home_once_and_preserves_explicit_clear() {
        let path = env::temp_dir().join(format!("switchx-common-init-{}", new_id().unwrap()));
        let data = path.join("data");
        let home = path.join("codex");
        fs::create_dir_all(&home).unwrap();
        let original = "model = 'private-model'\nmodel_provider = 'custom'\nmodel_reasoning_effort = 'high'\n[model_providers.custom]\nbase_url = 'https://example.invalid/v1'\napi_key = 'synthetic-secret'\n[features]\nmemories = true\n[mcp_servers.private]\ncommand = 'private-command'\n[shell_environment_policy.set]\nOPENAI_API_KEY = 'synthetic-secret'\nLANG = 'en_US'\n";
        fs::write(home.join("config.toml"), original).unwrap();
        fs::write(home.join("auth.json"), [0xff]).unwrap();
        let initialized = initialize_common_config(&data, &home).unwrap();
        let document: toml_edit::DocumentMut = initialized.parse().unwrap();
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(document["features"]["memories"].as_bool(), Some(true));
        assert_eq!(
            document["shell_environment_policy"]["set"]["LANG"].as_str(),
            Some("en_US")
        );
        assert!(!initialized.contains("private"));
        assert!(!initialized.contains("synthetic-secret"));
        assert!(
            open_store(&data)
                .unwrap()
                .common_codex_config_initialized()
                .unwrap()
        );
        assert_eq!(
            fs::read(home.join("config.toml")).unwrap(),
            original.as_bytes()
        );
        assert_eq!(fs::read(home.join("auth.json")).unwrap(), [0xff]);
        fs::write(home.join("config.toml"), "model_reasoning_effort = 'max'\n").unwrap();
        assert_eq!(initialize_common_config(&data, &home).unwrap(), initialized);
        save_common_config(&data, "").unwrap();
        assert_eq!(initialize_common_config(&data, &home).unwrap(), "");
        let store = open_store(&data).unwrap();
        assert!(store.common_codex_config_initialized().unwrap());
        assert_eq!(store.common_codex_config().unwrap(), "");
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn common_config_missing_or_empty_sources_can_be_retried() {
        let path = env::temp_dir().join(format!("switchx-common-retry-{}", new_id().unwrap()));
        let data = path.join("data");
        let home = path.join("codex");
        assert_eq!(initialize_common_config(&data, &home).unwrap(), "");
        assert!(
            !open_store(&data)
                .unwrap()
                .common_codex_config_initialized()
                .unwrap()
        );
        fs::create_dir_all(&home).unwrap();
        let original = "model = 'private-model'\nmodel_provider = 'custom'\n[shell_environment_policy.set]\nOPENAI_API_KEY = 'synthetic-secret'\n";
        fs::write(home.join("config.toml"), original).unwrap();
        assert_eq!(initialize_common_config(&data, &home).unwrap(), "");
        assert!(
            !open_store(&data)
                .unwrap()
                .common_codex_config_initialized()
                .unwrap()
        );
        assert_eq!(
            fs::read(home.join("config.toml")).unwrap(),
            original.as_bytes()
        );
        fs::write(home.join("config.toml"), "[features]\nmemories = true\n").unwrap();
        assert!(
            initialize_common_config(&data, &home)
                .unwrap()
                .contains("memories = true")
        );
        assert!(
            open_store(&data)
                .unwrap()
                .common_codex_config_initialized()
                .unwrap()
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn common_config_initialization_skips_managed_sources_and_preserves_parse_failures() {
        let path = env::temp_dir().join(format!("switchx-common-guard-{}", new_id().unwrap()));
        let data = path.join("data");
        let home = path.join("codex");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("config.toml"), "[invalid").unwrap();
        assert!(initialize_common_config(&data, &home).is_err());
        assert!(
            !open_store(&data)
                .unwrap()
                .common_codex_config_initialized()
                .unwrap()
        );
        for provider in ["switchx_router", "switchx_direct_synthetic"] {
            fs::write(
                home.join("config.toml"),
                format!("model_provider = '{provider}'\nmodel_reasoning_effort = 'max'\n"),
            )
            .unwrap();
            assert_eq!(initialize_common_config(&data, &home).unwrap(), "");
            assert!(
                !open_store(&data)
                    .unwrap()
                    .common_codex_config_initialized()
                    .unwrap()
            );
        }
        fs::write(home.join("config.toml"), "model_reasoning_effort = 'max'\n").unwrap();
        for journal in ["direct-journal.json", "switch-journal.json"] {
            fs::write(data.join(journal), "synthetic active journal").unwrap();
            assert_eq!(initialize_common_config(&data, &home).unwrap(), "");
            assert!(
                !open_store(&data)
                    .unwrap()
                    .common_codex_config_initialized()
                    .unwrap()
            );
            fs::remove_file(data.join(journal)).unwrap();
        }
        let saved = "model_reasoning_effort = 'high'\n";
        save_common_config(&data, saved).unwrap();
        fs::write(home.join("config.toml"), "[invalid").unwrap();
        assert_eq!(initialize_common_config(&data, &home).unwrap(), saved);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn common_config_form_extraction_saves_safely_and_respects_active_journals() {
        let path = env::temp_dir().join(format!("switchx-common-form-{}", new_id().unwrap()));
        let saved = "model_reasoning_effort = 'high'\n";
        save_common_config(&path, saved).unwrap();
        assert!(extract_and_save_common_config(&path, "[invalid").is_err());
        assert_eq!(load_common_config(&path).unwrap(), saved);
        let form = "model = 'private-model'\nmodel_provider = 'switchx_direct_new_provider'\nmodel_reasoning_effort = 'max'\n[model_providers.switchx_direct_new_provider.auth]\ncommand = '/private/helper'\n[features]\nmemories = true\n[shell_environment_policy.set]\nPRIVATE_KEY = 'synthetic-secret'\nLANG = 'en_US'\n";
        let extracted = extract_and_save_common_config(&path, form).unwrap();
        assert_eq!(load_common_config(&path).unwrap(), extracted);
        assert!(extracted.contains("model_reasoning_effort = 'max'"));
        assert!(extracted.contains("memories = true"));
        assert!(!extracted.contains("private"));
        assert!(!extracted.contains("synthetic-secret"));
        for journal in ["direct-journal.json", "switch-journal.json"] {
            fs::write(path.join(journal), "synthetic active journal").unwrap();
            assert!(extract_and_save_common_config(&path, saved).is_err());
            assert!(save_common_config(&path, saved).is_err());
            assert_eq!(load_common_config(&path).unwrap(), extracted);
            fs::remove_file(path.join(journal)).unwrap();
        }
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn official_presets_have_valid_defaults_and_match_only_their_endpoints() {
        let mut ids = std::collections::HashSet::new();
        for preset in PROVIDER_PRESETS {
            assert!(ids.insert(preset.id));
            validate_provider(preset.name, preset.base_url, preset.model_id).unwrap();
            assert_eq!(preset.models[0].model_id, preset.model_id);
            let mut model_ids = std::collections::HashSet::new();
            for model in preset.models {
                assert!(model_ids.insert(model.model_id));
                let metadata = model.metadata().unwrap();
                catalog::validate_metadata(&metadata).unwrap();
                assert_eq!(metadata["shell_type"], "shell_command");
                assert!(metadata.get("apply_patch_tool_type").is_none());
            }
            assert_eq!(provider_preset(preset.base_url).unwrap().id, preset.id);
            assert_eq!(
                provider_preset(&format!("{}/", preset.base_url))
                    .unwrap()
                    .id,
                preset.id
            );
            for link in [preset.website_url, preset.api_key_url] {
                let url = reqwest::Url::parse(link).unwrap();
                assert_eq!(url.scheme(), "https");
                assert!(url.host_str().is_some());
                assert!(url.username().is_empty());
                assert!(url.password().is_none());
                assert!(url.query().is_none());
            }
        }
        assert_eq!(
            provider_preset("https://api.deepseek.com/v1/").unwrap().id,
            "deepseek"
        );
        for custom in [
            "http://api.deepseek.com",
            "https://api.deepseek.com.example.invalid",
            "https://api.deepseek.com/proxy",
            "https://api.deepseek.com?key=synthetic",
            "https://user:synthetic@api.deepseek.com",
            "https://example.invalid/v1",
        ] {
            assert!(provider_preset(custom).is_none(), "{custom}");
        }
    }

    #[test]
    fn preset_models_are_saved_and_published_without_overwriting_user_mappings() {
        let path = env::temp_dir().join(format!("switchx-preset-models-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        for preset in PROVIDER_PRESETS {
            let provider = ProviderRecord {
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: preset.id.into(),
                name: preset.name.into(),
                base_url: preset.base_url.into(),
                model_id: preset.model_id.into(),
                credential_ref: None,
            };
            let defaults = new_preset_models(&provider, &store.models().unwrap()).unwrap();
            store
                .put_provider_with_models(&provider, &defaults)
                .unwrap();
        }
        let snapshot = load_snapshot(&path, false).unwrap();
        assert_eq!(snapshot.models.len(), 10);
        assert!(
            snapshot
                .models
                .iter()
                .all(|model| model.saved && model.ready)
        );
        assert_eq!(
            snapshot.models.iter().filter(|model| model.enabled).count(),
            4
        );
        for (model_id, context, levels, default) in [
            ("deepseek-flash", "1048576", "low, high, max", "high"),
            ("deepseek-v4-pro", "1048576", "low, high, max", "high"),
            ("kimi-k3", "1048576", "low, high, max", "high"),
            ("kimi-k2.7-code", "262144", "high", "high"),
            ("MiniMax-M3", "1000000", "none, high", "high"),
            ("mimo-v2.6-pro", "1048576", "none, low, medium, high", "low"),
            (
                "mimo-v2.6-flash",
                "1048576",
                "none, low, medium, high",
                "low",
            ),
            (
                "mimo-v2.6-pro-ultraspeed",
                "1048576",
                "none, low, medium, high",
                "low",
            ),
            ("mimo-v2.5-pro", "1048576", "none, low, medium, high", "low"),
            ("mimo-v2.5", "1048576", "none, low, medium, high", "low"),
        ] {
            let model = snapshot
                .models
                .iter()
                .find(|model| model.upstream_model == model_id)
                .unwrap();
            assert_eq!(model.context_window, context, "{model_id}");
            assert_eq!(model.reasoning_levels, levels, "{model_id}");
            assert_eq!(model.default_reasoning, default, "{model_id}");
            assert_eq!(
                model.display_name,
                format!("{model_id}/{}", model.provider_name)
            );
        }
        let publication = catalog::publish_saved(&store.models().unwrap()).unwrap();
        assert_eq!(publication.routes.len(), 4);
        for entry in publication.catalog["models"].as_array().unwrap() {
            let id = entry["slug"].as_str().unwrap();
            let route = &publication.routes[id];
            let provider = store.provider(&route.provider_id).unwrap().unwrap();
            assert_eq!(route.upstream_model, provider.model_id);
        }

        let mut customized = store
            .models()
            .unwrap()
            .into_iter()
            .find(|model| {
                model.provider_id == "deepseek" && model.upstream_model == "deepseek-flash"
            })
            .unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&customized.metadata).unwrap();
        metadata["context_window"] = 256_000.into();
        metadata["custom_capability"] = serde_json::json!({"retained": true});
        customized.metadata = metadata.to_string();
        customized.display_name = "My model".into();
        customized.enabled = false;
        customized.fallback_provider_id = Some("deepseek-backup".into());
        let backup = ProviderRecord {
            id: "deepseek-backup".into(),
            ..store.provider("deepseek").unwrap().unwrap()
        };
        let backup_model = ModelRecord {
            provider_id: backup.id.clone(),
            public_id: "sx-backup".into(),
            fallback_provider_id: None,
            ..customized.clone()
        };
        store
            .put_provider_with_models(&backup, &[backup_model])
            .unwrap();
        store.put_model(&customized).unwrap();
        catalog::validate_fallback(&customized, &store.models().unwrap()).unwrap();
        let removed = store
            .models()
            .unwrap()
            .into_iter()
            .find(|model| {
                model.provider_id == "deepseek" && model.upstream_model == "deepseek-v4-pro"
            })
            .unwrap();
        store.delete_model(&removed.public_id).unwrap();
        let provider = store.provider("deepseek").unwrap().unwrap();
        let defaults = new_preset_models(&provider, &store.models().unwrap()).unwrap();
        assert_eq!(defaults.len(), 1);
        assert!(!defaults[0].enabled);
        store
            .put_provider_with_models(&provider, &defaults)
            .unwrap();
        assert!(store.models().unwrap().contains(&customized));
        assert_eq!(store.models().unwrap().len(), 11);
        assert!(
            new_preset_models(&provider, &store.models().unwrap())
                .unwrap()
                .is_empty()
        );
        let custom_provider = ProviderRecord {
            base_url: "https://api.deepseek.com.example.invalid/v1".into(),
            ..provider
        };
        assert!(new_preset_models(&custom_provider, &[]).unwrap().is_empty());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a local Codex CLI; only parses a catalog in an isolated CODEX_HOME"]
    async fn preset_catalog_is_accepted_by_local_codex() {
        let mut models = Vec::new();
        for preset in PROVIDER_PRESETS {
            let provider = ProviderRecord {
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: preset.id.into(),
                name: preset.name.into(),
                base_url: preset.base_url.into(),
                model_id: preset.model_id.into(),
                credential_ref: None,
            };
            models.extend(new_preset_models(&provider, &[]).unwrap().into_iter().map(
                |mut model| {
                    model.enabled = true;
                    model
                },
            ));
        }
        let publication = catalog::publish_saved(&models).unwrap();
        assert_eq!(publication.routes.len(), 10);
        crate::client::check_catalog(&publication.catalog)
            .await
            .unwrap();
    }

    #[test]
    fn mappings_can_be_added_edited_selected_and_deleted_independently() {
        let path = env::temp_dir().join(format!("switchx-mapping-edit-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        store
            .put_provider(&ProviderRecord {
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: "mock".into(),
                name: "Mock".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "default-model".into(),
                credential_ref: None,
            })
            .unwrap();
        let save = |original_id, public_id, upstream_model, context_window| {
            save_mapping(
                &path,
                ModelInput {
                    provider_id: "mock",
                    original_id,
                    public_id,
                    display_name: "Mapped model",
                    upstream_model,
                    catalog_path: "",
                    settings: Some(catalog::MappingSettings {
                        context_window,
                        reasoning_levels: Some("low, high"),
                        default_reasoning: Some("high"),
                    }),
                },
            )
        };
        save("", "sx-first", "first-model", "128000").unwrap();
        save("", "sx-second", "second-model", "256000").unwrap();
        let snapshot = load_snapshot(&path, false).unwrap();
        assert_eq!(snapshot.models.len(), 2);
        assert!(
            snapshot
                .models
                .iter()
                .all(|model| model.ready && model.saved)
        );
        assert_eq!(snapshot.models[1].context_window, "256000");
        select_model(&path, "sx-second", false).unwrap();
        assert!(store.models().unwrap()[0].enabled);
        assert!(!store.models().unwrap()[1].enabled);
        let before = store.models().unwrap();
        assert!(save("sx-first", "sx-second", "first-model", "128000").is_err());
        assert!(save("sx-first", "sx-first", "first-model", "0").is_err());
        assert!(save("", "sx-duplicate", "first-model", "128000").is_err());
        assert_eq!(store.models().unwrap(), before);
        save("sx-first", "sx-renamed", "renamed-model", "192000").unwrap();
        let published = catalog::publish_saved(&store.models().unwrap()).unwrap();
        assert_eq!(published.routes.len(), 1);
        assert_eq!(
            published.routes["sx-renamed"].upstream_model,
            "renamed-model"
        );
        assert_eq!(published.catalog["models"][0]["context_window"], 192000);
        delete_model(&path, "sx-renamed").unwrap();
        assert_eq!(store.models().unwrap().len(), 1);
        assert_eq!(store.models().unwrap()[0].public_id, "sx-second");
        fs::write(path.join("switch-journal.json"), "synthetic journal").unwrap();
        assert!(save("", "sx-blocked", "new-model", "").is_err());
        assert!(delete_model(&path, "sx-second").is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn batch_mappings_are_atomic_and_inherit_row_template_then_shared_values() {
        let path = env::temp_dir().join(format!("switchx-batch-models-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        // The exact DeepSeek address supplies preset parameters for known models only.
        store
            .put_provider(&ProviderRecord {
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: "deepseek".into(),
                name: "DeepSeek".into(),
                base_url: "https://api.deepseek.com".into(),
                model_id: "deepseek-flash".into(),
                credential_ref: None,
            })
            .unwrap();
        save_mappings(
            &path,
            &BatchInput {
                provider_id: "deepseek",
                models: &[BatchModel {
                    upstream_model: "deepseek-flash",
                    context_window: "",
                    reasoning_levels: "",
                }],
                context_window: "",
                reasoning_levels: "",
                default_reasoning: "",
            },
        )
        .unwrap();

        let ids = [
            "deepseek-flash",
            "deepseek-v4-pro",
            "custom-chat",
            "deepseek-flash",
            "bad id",
        ]
        .map(String::from);
        let candidates = batch_candidates(&path, "deepseek", &ids, &[]).unwrap();
        assert_eq!(candidates.len(), 4, "duplicate discovery IDs collapse");
        assert!(candidates[0].added);
        assert_eq!(candidates[1].source, "预设参数");
        assert_eq!(candidates[1].context_window, "1048576");
        assert_eq!(candidates[1].reasoning_levels, "low, high, max");
        assert_eq!(candidates[2].source, "");
        assert!(candidates[2].supported && !candidates[2].added);
        assert!(!candidates[3].supported);

        let before = store.models().unwrap();
        let shared = |models| BatchInput {
            provider_id: "deepseek",
            models,
            context_window: "200000",
            reasoning_levels: "low, medium, high",
            default_reasoning: "medium",
        };
        for models in [
            // An existing pair, an invalid row value and an empty batch all write nothing.
            &[
                BatchModel {
                    upstream_model: "custom-chat",
                    context_window: "",
                    reasoning_levels: "",
                },
                BatchModel {
                    upstream_model: "deepseek-flash",
                    context_window: "",
                    reasoning_levels: "",
                },
            ][..],
            &[BatchModel {
                upstream_model: "custom-chat",
                context_window: "0",
                reasoning_levels: "",
            }],
            &[],
        ] {
            assert!(save_mappings(&path, &shared(models)).is_err());
            assert_eq!(store.models().unwrap(), before);
        }

        let added = save_mappings(
            &path,
            &shared(&[
                BatchModel {
                    upstream_model: "deepseek-v4-pro",
                    context_window: "",
                    reasoning_levels: "",
                },
                BatchModel {
                    upstream_model: "custom-chat",
                    context_window: "",
                    reasoning_levels: "",
                },
                BatchModel {
                    upstream_model: "custom-coder",
                    context_window: "64000",
                    reasoning_levels: "high, max",
                },
            ]),
        )
        .unwrap();
        assert_eq!(added.len(), 3);
        let models = store.models().unwrap();
        assert_eq!(models.len(), 4);
        let metadata = |upstream: &str| -> (ModelRecord, serde_json::Value) {
            let model = models
                .iter()
                .find(|model| model.upstream_model == upstream)
                .unwrap()
                .clone();
            let value = serde_json::from_str(&model.metadata).unwrap();
            (model, value)
        };
        let (preset, preset_metadata) = metadata("deepseek-v4-pro");
        assert!(preset.enabled);
        assert_eq!(preset.display_name, "deepseek-v4-pro/DeepSeek");
        assert_eq!(preset_metadata["context_window"], 1_048_576);
        assert_eq!(preset_metadata["default_reasoning_level"], "high");
        let (_, shared_metadata) = metadata("custom-chat");
        assert_eq!(shared_metadata["context_window"], 200_000);
        assert_eq!(
            shared_metadata["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(shared_metadata["default_reasoning_level"], "medium");
        let (_, row_metadata) = metadata("custom-coder");
        assert_eq!(row_metadata["context_window"], 64_000);
        assert_eq!(
            row_metadata["default_reasoning_level"],
            serde_json::Value::Null,
            "a shared default outside the row's levels is not applied"
        );
        let public_ids: std::collections::HashSet<_> =
            models.iter().map(|model| &model.public_id).collect();
        assert_eq!(public_ids.len(), 4);
        assert_eq!(catalog::publish_saved(&models).unwrap().routes.len(), 4);

        fs::write(path.join("switch-journal.json"), "synthetic journal").unwrap();
        let blocked = [BatchModel {
            upstream_model: "custom-blocked",
            context_window: "",
            reasoning_levels: "",
        }];
        assert!(save_mappings(&path, &shared(&blocked)).is_err());
        assert_eq!(store.models().unwrap().len(), 4);
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn picked_models_are_saved_with_a_new_upstream_in_one_transaction() {
        let path = env::temp_dir().join(format!("switchx-picked-models-{}", new_id().unwrap()));
        let options = CodexOptions::default();
        let save = |id: Option<&str>, model_id: &str, picked: &[&str]| {
            save_provider_with_mappings(
                &path,
                id,
                "DeepSeek",
                "https://api.deepseek.com",
                model_id,
                if id.is_some() {
                    String::new()
                } else {
                    "synthetic-key".into()
                },
                &options,
                None,
                &picked.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            )
        };
        assert!(save(None, "custom-chat", &["custom-chat", "bad id"]).is_err());
        assert!(open_store(&path).unwrap().providers().unwrap().is_empty());

        let created = save(
            None,
            "custom-chat",
            &[
                "deepseek-flash",
                "custom-chat",
                "custom-coder",
                "custom-chat",
            ],
        )
        .unwrap();
        let store = open_store(&path).unwrap();
        let models = store.models().unwrap();
        assert_eq!(created.len(), 4, "two presets and two picked models");
        assert_eq!(models.len(), 4);
        let model = |upstream: &str| {
            models
                .iter()
                .find(|model| model.upstream_model == upstream)
                .unwrap()
        };
        let preset: serde_json::Value =
            serde_json::from_str(&model("deepseek-flash").metadata).unwrap();
        assert_eq!(
            preset["context_window"], 1_048_576,
            "presets keep their parameters"
        );
        assert!(
            model("custom-chat").enabled,
            "the picked default joins the selection"
        );
        assert!(!model("custom-coder").enabled && !model("deepseek-flash").enabled);
        assert_eq!(model("custom-coder").display_name, "custom-coder/DeepSeek");
        let picked: serde_json::Value =
            serde_json::from_str(&model("custom-coder").metadata).unwrap();
        assert_eq!(picked["context_window"], 128_000);

        // Saving again keeps every mapping and adds only new picks, unselected.
        let id = store.providers().unwrap()[0].id.clone();
        let created = save(Some(&id), "custom-chat", &["custom-coder", "custom-vision"]).unwrap();
        assert_eq!(created.len(), 1);
        let models = store.models().unwrap();
        assert_eq!(models.len(), 5);
        assert!(
            !models
                .iter()
                .find(|model| model.upstream_model == "custom-vision")
                .unwrap()
                .enabled
        );
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn batch_subscription_mappings_keep_the_official_template() {
        let path = env::temp_dir().join(format!("switchx-batch-official-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        store
            .put_provider(&ProviderRecord {
                kind: crate::storage::ProviderKind::Chatgpt,
                account_binding: Some(crate::storage::AccountBinding::Native),
                icon_id: None,
                id: "plus".into(),
                name: "Plus".into(),
                base_url: crate::chatgpt::BASE_URL.into(),
                model_id: "fixture-official".into(),
                credential_ref: None,
            })
            .unwrap();
        let mut template = catalog::mapping_metadata(
            "fixture-official",
            "Fixture",
            &catalog::MappingSettings {
                context_window: "272000",
                reasoning_levels: Some("low, medium, high"),
                default_reasoning: Some("medium"),
            },
            None,
        )
        .unwrap();
        template["base_instructions"] = "Synthetic official instructions.".into();
        let templates = [template];
        let ids = ["fixture-official".into(), "fixture-missing".into()];
        let candidates = batch_candidates(&path, "plus", &ids, &templates).unwrap();
        assert_eq!(candidates[0].source, "官方目录");
        assert_eq!(candidates[0].context_window, "272000");
        assert!(!candidates[1].supported);

        let input = |upstream_model, reasoning_levels| {
            save_mappings_from(
                &path,
                &BatchInput {
                    provider_id: "plus",
                    models: &[BatchModel {
                        upstream_model,
                        context_window: "",
                        reasoning_levels,
                    }],
                    context_window: "64000",
                    reasoning_levels: "",
                    default_reasoning: "",
                },
                &templates,
            )
        };
        assert!(input("fixture-missing", "").is_err());
        assert_eq!(input("fixture-official", "low, high").unwrap().len(), 1);
        let saved: serde_json::Value =
            serde_json::from_str(&store.models().unwrap()[0].metadata).unwrap();
        assert_eq!(
            saved["base_instructions"],
            "Synthetic official instructions."
        );
        assert_eq!(
            saved["context_window"], 272_000,
            "shared values do not override templates"
        );
        assert_eq!(
            saved["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(saved["default_reasoning_level"], serde_json::Value::Null);
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn bulk_model_selection_is_atomic_and_respects_recovery_lock() {
        let path = env::temp_dir().join(format!("switchx-model-selection-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        store
            .put_provider(&ProviderRecord {
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: "mock".into(),
                name: "Mock".into(),
                base_url: "https://example.invalid/v1".into(),
                model_id: "first-model".into(),
                credential_ref: None,
            })
            .unwrap();
        for id in ["first", "second", "broken"] {
            let public_id = format!("sx-{id}");
            let metadata = catalog::mapping_metadata(
                id,
                id,
                &catalog::MappingSettings {
                    context_window: "128000",
                    reasoning_levels: None,
                    default_reasoning: None,
                },
                None,
            )
            .unwrap();
            store
                .put_model(&ModelRecord {
                    provider_id: "mock".into(),
                    public_id,
                    display_name: id.into(),
                    upstream_model: id.into(),
                    metadata: if id == "broken" {
                        "{}".into()
                    } else {
                        metadata.to_string()
                    },
                    enabled: false,
                    fallback_provider_id: None,
                })
                .unwrap();
        }
        let before = store.models().unwrap();
        for invalid_id in ["sx-broken", "sx-missing"] {
            assert!(select_models(&path, &["sx-first".into(), invalid_id.into()], true).is_err());
            assert_eq!(store.models().unwrap(), before);
        }
        // A row removed between validation and persistence must roll back earlier updates.
        assert!(
            store
                .set_models_enabled(&["sx-first".into(), "sx-missing".into()], true)
                .is_err()
        );
        assert_eq!(store.models().unwrap(), before);

        let ids = ["sx-first".into(), "sx-second".into()];
        select_models(&path, &ids, true).unwrap();
        let mut expected = before;
        for model in &mut expected {
            model.enabled = ids.contains(&model.public_id);
        }
        assert_eq!(store.models().unwrap(), expected);
        select_models(&path, &["sx-second".into()], false).unwrap();
        assert!(
            store
                .models()
                .unwrap()
                .iter()
                .find(|model| model.public_id == "sx-first")
                .unwrap()
                .enabled
        );
        select_models(&path, &ids, false).unwrap();
        assert!(store.models().unwrap().iter().all(|model| !model.enabled));
        fs::write(path.join("switch-journal.json"), "synthetic journal").unwrap();
        let before = store.models().unwrap();
        assert!(
            select_models(&path, &ids, true)
                .unwrap_err()
                .contains("先恢复")
        );
        assert_eq!(store.models().unwrap(), before);
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

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
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
                id: "probe".into(),
                name: "Test Provider".into(),
                base_url: "https://user:secret@example.invalid/v1?token=private".into(),
                model_id: "test-model".into(),
                credential_ref: Some("../invalid-secret-reference".into()),
            })
            .unwrap();
        drop(store);
        let initial = load_snapshot(&path, false).unwrap();
        assert_eq!(
            initial.providers[0].credential_status,
            "未配置 API Key，请重新输入"
        );
        let checked = load_snapshot(&path, true).unwrap();
        assert_eq!(
            checked.providers[0].credential_status,
            "未配置 API Key，请重新输入"
        );
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
    fn provider_icon_changes_preserve_empty_api_key_and_legacy_edits() {
        let path = env::temp_dir().join(format!("switchx-api-icon-{}", new_id().unwrap()));
        let options = CodexOptions::default();
        save_provider_with_codex_options_and_icon(
            &path,
            None,
            "Mock",
            "https://example.invalid/v1",
            "mock",
            "synthetic-icon-key".into(),
            &options,
            Some("deepseek"),
        )
        .unwrap();
        let store = open_store(&path).unwrap();
        let id = store.providers().unwrap()[0].id.clone();
        save_provider(
            &path,
            Some(&id),
            "Renamed",
            "https://example.invalid/v1",
            "mock",
            String::new(),
        )
        .unwrap();
        assert_eq!(
            store.provider(&id).unwrap().unwrap().icon_id.as_deref(),
            Some("deepseek")
        );
        for (icon_id, expected) in [
            (None, Some("deepseek")),
            (Some(" OPENAI "), Some("openai")),
            (Some(""), None),
        ] {
            save_provider_with_codex_options_and_icon(
                &path,
                Some(&id),
                "Renamed",
                "https://example.invalid/v1",
                "mock",
                String::new(),
                &options,
                icon_id,
            )
            .unwrap();
            assert_eq!(
                store.provider(&id).unwrap().unwrap().icon_id.as_deref(),
                expected
            );
            assert_eq!(
                store.provider_api_key(&id).unwrap().unwrap().expose(),
                "synthetic-icon-key"
            );
            assert_eq!(
                load_snapshot(&path, true).unwrap().providers[0].icon_id,
                expected.unwrap_or("")
            );
        }
        let before = store.provider(&id).unwrap().unwrap();
        assert_eq!(
            save_provider_with_codex_options_and_icon(
                &path,
                Some(&id),
                "Must not save",
                "https://example.invalid/v1",
                "mock",
                "must-not-save".into(),
                &options,
                Some("../unknown-icon"),
            )
            .unwrap_err(),
            "请选择有效的上游图标"
        );
        assert_eq!(store.provider(&id).unwrap(), Some(before));
        assert_eq!(
            store.provider_api_key(&id).unwrap().unwrap().expose(),
            "synthetic-icon-key"
        );
        let mut legacy = store.provider(&id).unwrap().unwrap();
        legacy.icon_id = Some("removed-legacy-icon".into());
        store.put_provider(&legacy).unwrap();
        save_provider(
            &path,
            Some(&id),
            "Renamed",
            "https://example.invalid/v1",
            "mock",
            String::new(),
        )
        .unwrap();
        assert_eq!(store.provider(&id).unwrap(), Some(legacy));
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn provider_save_edit_and_delete_use_private_sqlite_without_keychain() {
        let path = env::temp_dir().join(format!("switchx-api-key-{}", new_id().unwrap()));
        let original = "synthetic-sqlite-api-key";
        save_provider(
            &path,
            None,
            "Mock",
            "https://example.invalid/v1",
            "mock",
            original.into(),
        )
        .unwrap();
        let store = open_store(&path).unwrap();
        let provider = store.providers().unwrap().remove(0);
        assert!(provider.credential_ref.is_none());
        assert_eq!(
            provider_credential(&path, &provider).unwrap().expose(),
            original
        );
        let snapshot = load_snapshot(&path, true).unwrap();
        assert_eq!(snapshot.providers[0].credential_status, "凭据可读取");
        assert!(!format!("{snapshot:?}").contains(original));
        save_provider(
            &path,
            Some(&provider.id),
            "Edited",
            "https://example.invalid/v1",
            "new-model",
            String::new(),
        )
        .unwrap();
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            original
        );
        save_provider(
            &path,
            Some(&provider.id),
            "Edited",
            "https://example.invalid/v1",
            "new-model",
            "synthetic-replacement-key".into(),
        )
        .unwrap();
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-replacement-key"
        );
        assert!(
            save_provider(
                &path,
                Some(&provider.id),
                "Edited",
                "https://user:invalid@example.invalid/v1",
                "new-model",
                "must-not-save".into()
            )
            .is_err()
        );
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-replacement-key"
        );
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
        delete_provider(&path, &provider.id).unwrap();
        assert!(store.provider(&provider.id).unwrap().is_none());
        assert!(store.provider_api_key(&provider.id).unwrap().is_none());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn legacy_reference_without_sqlite_key_requires_input_without_changing_credentials() {
        let path =
            env::temp_dir().join(format!("switchx-ignored-legacy-key-{}", new_id().unwrap()));
        let store = open_store(&path).unwrap();
        let provider = ProviderRecord {
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
            id: "synthetic-unused-legacy-key".into(),
            name: "Mock".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "mock".into(),
            credential_ref: Some("synthetic-unused-legacy-key".into()),
        };
        store.put_provider(&provider).unwrap();
        let connection = rusqlite::Connection::open(path.join("switchx.sqlite")).unwrap();
        connection
            .execute_batch("CREATE TRIGGER forbid_credential_changes BEFORE UPDATE ON providers BEGIN SELECT RAISE(ABORT, 'unexpected credential update'); END;")
            .unwrap();
        for journal in [
            None,
            Some("direct-journal.json"),
            Some("switch-journal.json"),
        ] {
            if let Some(journal) = journal {
                fs::write(path.join(journal), "synthetic active switch").unwrap();
            }
            for check_credentials in [false, true] {
                let snapshot = load_snapshot(&path, check_credentials).unwrap();
                assert_eq!(
                    snapshot.providers[0].credential_status,
                    "未配置 API Key，请重新输入"
                );
            }
            assert_eq!(
                provider_credential(&path, &provider).unwrap_err(),
                "上游未配置 API Key，请重新输入"
            );
            assert_eq!(
                store.provider(&provider.id).unwrap(),
                Some(provider.clone())
            );
            assert!(store.provider_api_key(&provider.id).unwrap().is_none());
            if let Some(journal) = journal {
                fs::remove_file(path.join(journal)).unwrap();
            }
        }
        connection
            .execute_batch("DROP TRIGGER forbid_credential_changes")
            .unwrap();
        assert_eq!(
            save_provider(
                &path,
                Some(&provider.id),
                "Must not replace metadata",
                "https://example.invalid/v1",
                "changed-model",
                String::new(),
            )
            .unwrap_err(),
            "上游未配置 API Key，请重新输入"
        );
        assert_eq!(
            store.provider(&provider.id).unwrap(),
            Some(provider.clone())
        );
        assert!(store.provider_api_key(&provider.id).unwrap().is_none());
        delete_provider(&path, &provider.id).unwrap();
        assert!(store.provider(&provider.id).unwrap().is_none());
        drop(connection);
        drop(store);
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
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
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
                kind: crate::storage::ProviderKind::ApiKey,
                account_binding: None,
                icon_id: None,
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
        select_model(&path, "sx-backup", false).unwrap();
        save_fallback(&path, "sx-mock", Some("backup")).unwrap();
        assert!(save_fallback(&path, "sx-mock", Some("mock")).is_err());
        assert!(save_fallback(&path, "sx-mock", Some("missing")).is_err());
        save_model(&path, "mock", "sx-mock", "Mock", "").unwrap();
        select_model(&path, "sx-mock", true).unwrap();
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
        select_model(&path, "sx-mock", false).unwrap();
        assert!(!store.models().unwrap()[0].enabled);
        fs::write(path.join("switch-journal.json"), "synthetic journal").unwrap();
        assert!(
            select_model(&path, "sx-mock", true)
                .unwrap_err()
                .contains("先恢复")
        );
        assert!(save_model(&path, "mock", "sx-changed", "Changed", "").is_err());
        assert!(delete_provider(&path, "mock").is_err());
        assert!(save_fallback(&path, "sx-mock", None).is_err());
        drop(store);
        fs::remove_dir_all(path).unwrap();
    }
}
