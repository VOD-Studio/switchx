//! Model mappings: single and batch additions, connection directories,
//! publication selection, and fallbacks.

use std::path::Path;

use super::{ModelPreset, ensure_editable, open_store, provider_preset};
use crate::{
    catalog,
    id::new_id,
    storage::{ModelRecord, ProviderKind, ProviderRecord},
    upstream::validate_provider,
};

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
    store.check_model_names(std::slice::from_ref(&model))?;
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
    pub default_reasoning: String,
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
                supported: crate::upstream::validate_model_id(id).is_ok()
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
                default_reasoning: template
                    .as_ref()
                    .and_then(|value| value["default_reasoning_level"].as_str())
                    .unwrap_or_default()
                    .into(),
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
    store.check_model_names(&records)?;
    store
        .add_models(&records)
        .map_err(|_| "无法保存模型映射；原资料未修改")?;
    Ok(records.into_iter().map(|model| model.public_id).collect())
}

/// Editable directory fields; credentials and publication selection are never
/// accepted from the editor. Existing public IDs and selection survive edits.
#[derive(Debug, Clone)]
pub struct ConnectionModel {
    pub public_id: String,
    pub display_name: String,
    pub upstream_model: String,
    pub context_window: String,
    pub reasoning_levels: String,
    pub default_reasoning: String,
}

pub fn save_connection_models(
    data_dir: &Path,
    provider_id: &str,
    expected: &[ModelRecord],
    drafts: &[ConnectionModel],
    templates: &[serde_json::Value],
) -> Result<(), String> {
    ensure_editable(data_dir)?;
    let store = open_store(data_dir).map_err(|error| error.message())?;
    let provider = store
        .provider(provider_id)
        .map_err(|_| "无法读取连接")?
        .ok_or("连接不存在")?;
    let all = store.models().map_err(|_| "无法读取模型目录")?;
    let mut records: Vec<ModelRecord> = Vec::new();
    for draft in drafts {
        let model_id = draft.upstream_model.trim();
        let name = draft.display_name.trim();
        validate_provider(&provider.name, &provider.base_url, model_id)?;
        catalog::validate_display_name(name)?;
        let old = if draft.public_id.is_empty() {
            None
        } else {
            Some(
                expected
                    .iter()
                    .find(|model| {
                        model.public_id == draft.public_id && model.provider_id == provider_id
                    })
                    .ok_or("模型目录已变化，请关闭后重新打开")?,
            )
        };
        if records.iter().any(|model| {
            model.upstream_model == model_id
                || (!draft.public_id.is_empty() && model.public_id == draft.public_id)
        }) {
            return Err(format!("{model_id} 重复，请为每个实际模型保留一个条目"));
        }
        if records.iter().any(|model| model.display_name == name) {
            return Err(format!("模型名称“{name}”重复，请使用不同名称"));
        }
        let source = if let Some(old) = old.filter(|model| model.upstream_model == model_id) {
            Some(serde_json::from_str(&old.metadata).map_err(|_| "已保存的模型资料损坏")?)
        } else {
            model_template(&provider, model_id, templates)?
        };
        if provider.kind == ProviderKind::Chatgpt && source.is_none() {
            return Err(format!("官方目录没有 {model_id}，请获取模型列表后选择"));
        }
        let metadata = catalog::mapping_metadata(
            model_id,
            name,
            &catalog::MappingSettings {
                context_window: &draft.context_window,
                reasoning_levels: Some(&draft.reasoning_levels),
                default_reasoning: Some(&draft.default_reasoning),
            },
            source,
        )
        .map_err(|error| format!("{model_id}：{error}"))?;
        let mut record = ModelRecord {
            provider_id: provider_id.into(),
            public_id: match old {
                Some(model) => model.public_id.clone(),
                None => format!("sx-{}", new_id()?),
            },
            display_name: name.into(),
            upstream_model: model_id.into(),
            metadata: metadata.to_string(),
            enabled: true,
            fallback_provider_id: None,
        };
        catalog::publish_saved(std::slice::from_ref(&record))?;
        record.enabled = old.is_some_and(|model| model.enabled);
        record.fallback_provider_id = old
            .filter(|model| model.upstream_model == model_id)
            .and_then(|model| model.fallback_provider_id.clone());
        records.push(record);
    }
    let updated: Vec<_> = all
        .into_iter()
        .filter(|model| model.provider_id != provider_id)
        .chain(records.clone())
        .collect();
    for model in &updated {
        catalog::validate_fallback(model, &updated)?;
    }
    store
        .replace_provider_models(provider_id, expected, &records)
        .map_err(|error| match error {
            rusqlite::Error::InvalidParameterName(message) => message,
            _ => "模型目录在编辑期间已变化，或保存失败；原目录未修改，请重新打开后重试".into(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{delete_provider, load_snapshot};
    use std::{env, fs};

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
                    display_name: upstream_model,
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
            published.routes["renamed-model"].upstream_model,
            "renamed-model"
        );
        assert_eq!(published.catalog["models"][0]["context_window"], 192000);
        delete_model(&path, "sx-renamed").unwrap();
        assert_eq!(store.models().unwrap().len(), 1);
        assert_eq!(store.models().unwrap()[0].public_id, "sx-second");
        fs::write(path.join(crate::codex::ROUTE_JOURNAL), "synthetic journal").unwrap();
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

        fs::write(path.join(crate::codex::ROUTE_JOURNAL), "synthetic journal").unwrap();
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
        fs::write(path.join(crate::codex::ROUTE_JOURNAL), "synthetic journal").unwrap();
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
            include_str!("../../tests/fixtures/synthetic-models.json"),
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
        fs::write(path.join(crate::codex::ROUTE_JOURNAL), "synthetic journal").unwrap();
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
