//! Saving, loading, and deleting API upstream connections.

use std::path::Path;

use super::{ensure_editable, open_store, presets::new_preset_models};
use crate::{
    catalog,
    codex::provider_config::{self, CodexOptions},
    credentials::Secret,
    id::new_id,
    storage::{ModelRecord, ProviderRecord},
    upstream::validate_provider,
};

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
    let mut defaults = if old.is_none() {
        new_preset_models(&record, &models)?
    } else {
        Vec::new()
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::load_snapshot;
    use std::{env, fs};

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
            Some(crate::codex::DIRECT_JOURNAL),
            Some(crate::codex::ROUTE_JOURNAL),
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
        fs::write(path.join(crate::codex::DIRECT_JOURNAL), "synthetic journal").unwrap();
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
}
