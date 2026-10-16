//! Read-only views of saved connections and their model mappings.

use std::path::Path;

use super::{AppError, open_store, provider_preset};
use crate::{
    catalog,
    storage::{ModelRecord, ProviderRecord, Store},
    upstream::validate_provider,
};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub providers: Vec<ProviderView>,
    pub models: Vec<ModelView>,
    pub credentials_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelView {
    pub input_modalities: crate::catalog::InputModalities,
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

pub fn load_snapshot(data_dir: &Path, check_credentials: bool) -> Result<Snapshot, AppError> {
    snapshot_from(&open_store(data_dir)?, check_credentials)
}

pub(crate) fn snapshot_from(store: &Store, check_credentials: bool) -> Result<Snapshot, AppError> {
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
            .map(|provider| provider_view(provider, store, check_credentials))
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
            "{} · 上下文 {} · 实际能力待验证",
            crate::catalog::InputModalities::from_metadata(metadata).label(),
            metadata["context_window"]
        )
    } else if model.is_some() {
        "模型资料无效，请编辑或重新导入".into()
    } else {
        "尚未添加模型映射".into()
    };
    ModelView {
        input_modalities: metadata
            .as_ref()
            .map(crate::catalog::InputModalities::from_metadata)
            .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

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
}
