use std::{collections::HashMap, fs, io::Read, path::Path};

use serde_json::{Value, json};

use crate::storage::ModelRecord;

pub const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBinding {
    pub provider_id: String,
    pub upstream_model: String,
    pub fallback_provider_id: Option<String>,
}

#[derive(Clone, Copy)]
pub struct Selection<'a> {
    pub public_id: &'a str,
    pub display_name: &'a str,
    pub provider_id: &'a str,
    pub upstream_model: &'a str,
}

#[derive(Debug, Clone)]
pub struct Publication {
    pub catalog: Value,
    pub routes: HashMap<String, RouteBinding>,
}

pub fn read_template(path: &Path, model_id: &str) -> Result<Value, String> {
    if !path.is_absolute() {
        return Err("模型目录文件必须使用绝对路径".into());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| "无法读取模型目录文件")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("模型目录必须是普通 JSON 文件".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| "无法打开模型目录文件")?
        .take(MAX_CATALOG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取模型目录文件")?;
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err("模型目录文件超过 2 MiB".into());
    }
    let catalog: Value = serde_json::from_slice(&bytes).map_err(|_| "模型目录不是有效 JSON")?;
    let models = catalog["models"]
        .as_array()
        .ok_or("模型目录缺少 models 数组")?;
    let mut matching = models.iter().filter(|model| model["slug"] == model_id);
    let model = matching.next().ok_or("目录中没有此上游的模型 ID")?;
    if matching.next().is_some() {
        return Err("目录中同一模型 ID 出现多次".into());
    }
    validate_metadata(model)?;
    Ok(model.clone())
}

// Validate the fields needed by the API route. The selected Codex CLI also parses
// the complete catalog in an isolated home before a native route can be applied.
pub fn validate_metadata(model: &Value) -> Result<(), String> {
    for field in ["slug", "display_name"] {
        if model[field]
            .as_str()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(format!("模型资料缺少 {field}"));
        }
    }
    if !matches!(
        model["shell_type"].as_str(),
        Some("unified_exec" | "shell_command" | "local" | "default" | "disabled")
    ) {
        return Err("模型资料的 shell_type 缺失或不受支持".into());
    }
    if model["supported_in_api"] != true || !model["support_verbosity"].is_boolean() {
        return Err("模型资料必须声明 API 支持和 verbosity 能力".into());
    }
    if model["context_window"]
        .as_i64()
        .is_none_or(|value| value <= 0)
        || model["priority"]
            .as_i64()
            .is_none_or(|value| i32::try_from(value).is_err())
        || model["truncation_policy"]["limit"]
            .as_i64()
            .is_none_or(|value| value <= 0)
        || !matches!(
            model["truncation_policy"]["mode"].as_str(),
            Some("tokens" | "bytes")
        )
    {
        return Err("模型资料的上下文、优先级或截断策略无效".into());
    }
    let levels = model["supported_reasoning_levels"]
        .as_array()
        .ok_or("模型资料缺少推理档位列表")?;
    for level in levels {
        if level["effort"].as_str().is_none_or(|effort| {
            effort.is_empty() || effort.len() > 32 || effort.chars().any(char::is_control)
        }) || !level["description"].is_string()
        {
            return Err("模型资料的推理档位无效".into());
        }
    }
    if !model["default_reasoning_level"].is_null()
        && !levels
            .iter()
            .any(|level| level["effort"] == model["default_reasoning_level"])
    {
        return Err("默认推理档位不在支持列表中".into());
    }
    let modalities = model["input_modalities"]
        .as_array()
        .ok_or("模型资料缺少输入类型")?;
    if !modalities.iter().any(|value| value == "text")
        || modalities
            .iter()
            .any(|value| !matches!(value.as_str(), Some("text" | "image" | "audio")))
        || model["experimental_supported_tools"]
            .as_array()
            .is_none_or(|tools| tools.iter().any(|tool| !tool.is_string()))
    {
        return Err("模型资料的输入类型或工具列表无效".into());
    }
    if model["use_responses_lite"] == true {
        return Err("当前路由仅支持标准 Responses，请使用对应的模型资料".into());
    }
    Ok(())
}

pub fn publish_saved(records: &[ModelRecord]) -> Result<Publication, String> {
    let mut publication = Publication {
        catalog: json!({"models": []}),
        routes: HashMap::new(),
    };
    for record in records.iter().filter(|record| record.enabled) {
        validate_fallback(record, records)?;
        let metadata: Value =
            serde_json::from_str(&record.metadata).map_err(|_| "已保存的模型资料损坏")?;
        let selected = publish(
            &json!({"models": [metadata]}),
            &[Selection {
                public_id: &record.public_id,
                display_name: &record.display_name,
                provider_id: &record.provider_id,
                upstream_model: &record.upstream_model,
            }],
        )?;
        for (id, mut binding) in selected.routes {
            binding.fallback_provider_id = record.fallback_provider_id.clone();
            if publication.routes.insert(id.clone(), binding).is_some() {
                return Err(format!("公开模型 ID 重复：{id}"));
            }
        }
        publication.catalog["models"]
            .as_array_mut()
            .unwrap()
            .extend(
                selected.catalog["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned(),
            );
    }
    if publication.routes.is_empty() {
        return Err("请至少选择一个已导入资料的模型".into());
    }
    if serde_json::to_vec(&publication.catalog)
        .map_err(|_| "无法生成模型目录")?
        .len()
        > MAX_CATALOG_BYTES
    {
        return Err("发布目录超过 2 MiB，请减少所选模型".into());
    }
    Ok(publication)
}

pub fn validate_fallback(model: &ModelRecord, records: &[ModelRecord]) -> Result<(), String> {
    let Some(id) = &model.fallback_provider_id else {
        return Ok(());
    };
    if id == &model.provider_id {
        return Err("备用上游不能与主上游相同".into());
    }
    let fallback = records
        .iter()
        .find(|record| &record.provider_id == id)
        .ok_or("备用上游尚未导入模型资料")?;
    if model.upstream_model != fallback.upstream_model {
        return Err("备用上游必须使用相同的实际模型 ID；暂不支持自动换模型".into());
    }
    let comparable = |record: &ModelRecord| -> Result<Value, String> {
        let mut metadata: Value =
            serde_json::from_str(&record.metadata).map_err(|_| "已保存的模型资料损坏")?;
        validate_metadata(&metadata)?;
        if metadata["slug"] != record.upstream_model {
            return Err("模型 ID 与导入资料不一致，请重新导入".into());
        }
        // Compare every capability and instruction field, including unknown fields.
        // Only catalog presentation is allowed to differ.
        for key in ["display_name", "description", "priority", "visibility"] {
            metadata.as_object_mut().unwrap().remove(key);
        }
        Ok(metadata)
    };
    if comparable(model)? != comparable(fallback)? {
        return Err("主备模型的能力或指令模板不同，不能设为备用".into());
    }
    Ok(())
}

pub fn publish(templates: &Value, selections: &[Selection<'_>]) -> Result<Publication, String> {
    let source = templates["models"]
        .as_array()
        .ok_or("catalog templates must contain a models array")?;
    let mut models = Vec::with_capacity(selections.len());
    let mut routes = HashMap::with_capacity(selections.len());

    for selection in selections {
        let id = selection.public_id;
        if id.is_empty()
            || id.len() > 64
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || id.starts_with('-')
            || id.ends_with('-')
        {
            return Err(format!("invalid public model ID: {id}"));
        }
        if selection.display_name.trim().is_empty()
            || selection.display_name.len() > 160
            || selection.display_name.chars().any(char::is_control)
            || selection.provider_id.is_empty()
            || selection.upstream_model.is_empty()
        {
            return Err(format!("incomplete binding for {id}"));
        }
        if routes.contains_key(id) {
            return Err(format!("duplicate public model ID: {id}"));
        }

        let mut matches = source
            .iter()
            .filter(|model| model["slug"].as_str() == Some(selection.upstream_model));
        let mut model = matches
            .next()
            .ok_or_else(|| format!("missing metadata for {}", selection.upstream_model))?
            .clone();
        if matches.next().is_some() {
            return Err(format!(
                "ambiguous metadata for {}",
                selection.upstream_model
            ));
        }
        validate_metadata(&model)?;
        model["slug"] = id.into();
        model["display_name"] = selection.display_name.into();
        model["visibility"] = "list".into();
        models.push(model);
        routes.insert(
            id.to_owned(),
            RouteBinding {
                provider_id: selection.provider_id.to_owned(),
                upstream_model: selection.upstream_model.to_owned(),
                fallback_provider_id: None,
            },
        );
    }

    Ok(Publication {
        catalog: json!({ "models": models }),
        routes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn templates() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap()
    }

    #[test]
    fn fallback_requires_explicit_matching_model_and_complete_capabilities() {
        let primary = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-primary".into(),
            display_name: "Primary".into(),
            upstream_model: "deepseek-flash".into(),
            metadata: templates()["models"][0].to_string(),
            enabled: true,
            fallback_provider_id: Some("backup".into()),
        };
        let mut backup = ModelRecord {
            provider_id: "backup".into(),
            public_id: "sx-backup".into(),
            enabled: false,
            fallback_provider_id: Some("primary".into()),
            ..primary.clone()
        };
        let publication = publish_saved(&[primary.clone(), backup.clone()]).unwrap();
        assert_eq!(publication.routes.len(), 1);
        assert_eq!(
            publication.routes["sx-primary"]
                .fallback_provider_id
                .as_deref(),
            Some("backup")
        );
        assert!(publish_saved(std::slice::from_ref(&primary)).is_err());
        assert!(
            publish_saved(&[ModelRecord {
                fallback_provider_id: Some("primary".into()),
                ..primary.clone()
            }])
            .is_err()
        );
        backup.upstream_model = "different-model".into();
        assert!(publish_saved(&[primary.clone(), backup.clone()]).is_err());
        backup.upstream_model = primary.upstream_model.clone();
        for (key, value) in [
            ("context_window", json!(64000)),
            ("shell_type", json!("shell_command")),
            (
                "model_messages",
                json!({"instructions_template":"incompatible"}),
            ),
            ("unknown_future_capability", json!(true)),
        ] {
            let mut metadata: Value = serde_json::from_str(&primary.metadata).unwrap();
            metadata[key] = value;
            backup.metadata = metadata.to_string();
            assert!(
                publish_saved(&[primary.clone(), backup.clone()]).is_err(),
                "{key}"
            );
        }
        let mut metadata: Value = serde_json::from_str(&primary.metadata).unwrap();
        metadata["description"] = "Another display description".into();
        backup.metadata = metadata.to_string();
        assert!(publish_saved(&[primary, backup]).is_ok());
    }

    #[test]
    fn publishes_distinct_aliases_without_changing_source_capabilities() {
        let selections = [
            Selection {
                public_id: "sx-ds-flash",
                display_name: "DeepSeek · Flash",
                provider_id: "deepseek",
                upstream_model: "deepseek-flash",
            },
            Selection {
                public_id: "sx-oai-coding",
                display_name: "OpenAI · Coding",
                provider_id: "openai-api",
                upstream_model: "gpt-5.5",
            },
        ];
        let publication = publish(&templates(), &selections).unwrap();
        let models = publication.catalog["models"].as_array().unwrap();
        let expected: Value =
            serde_json::from_str(include_str!("../tests/fixtures/published-models.json")).unwrap();
        assert_eq!(publication.catalog, expected);

        assert_eq!(models[0]["slug"], "sx-ds-flash");
        assert_eq!(models[0]["context_window"], 1_048_576);
        assert_eq!(models[1]["slug"], "sx-oai-coding");
        assert_eq!(models[1]["context_window"], 272_000);
        assert_eq!(publication.routes["sx-ds-flash"].provider_id, "deepseek");
        assert_eq!(
            publication.routes["sx-oai-coding"].upstream_model,
            "gpt-5.5"
        );
    }

    #[test]
    fn rejects_ambiguous_or_unknown_routes() {
        let selection = Selection {
            public_id: "sx-known",
            display_name: "Known",
            provider_id: "p",
            upstream_model: "deepseek-flash",
        };
        assert!(
            publish(
                &templates(),
                &[
                    selection,
                    Selection {
                        public_id: "sx-known",
                        display_name: "Again",
                        provider_id: "p",
                        upstream_model: "gpt-5.5"
                    }
                ]
            )
            .unwrap_err()
            .contains("duplicate")
        );
        assert!(
            publish(
                &templates(),
                &[Selection {
                    public_id: "bad/name",
                    ..selection
                }]
            )
            .unwrap_err()
            .contains("invalid")
        );
        assert!(
            publish(
                &templates(),
                &[Selection {
                    upstream_model: "absent",
                    ..selection
                }]
            )
            .unwrap_err()
            .contains("missing")
        );
    }

    #[test]
    fn saved_models_keep_provider_specific_metadata_and_reject_incomplete_templates() {
        let template = templates()["models"][0].clone();
        let first = ModelRecord {
            provider_id: "first".into(),
            public_id: "sx-first".into(),
            display_name: "First".into(),
            upstream_model: "deepseek-flash".into(),
            metadata: template.to_string(),
            enabled: true,
            fallback_provider_id: None,
        };
        let mut second_template = template.clone();
        second_template["context_window"] = 64000.into();
        let mut second = ModelRecord {
            provider_id: "second".into(),
            public_id: "sx-second".into(),
            metadata: second_template.to_string(),
            ..first.clone()
        };
        let publication = publish_saved(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(publication.catalog["models"][0]["context_window"], 1048576);
        assert_eq!(publication.catalog["models"][1]["context_window"], 64000);
        assert_eq!(publication.routes["sx-second"].provider_id, "second");
        second.public_id = first.public_id.clone();
        assert!(
            publish_saved(&[first.clone(), second])
                .unwrap_err()
                .contains("重复")
        );
        let mut invalid = template;
        invalid.as_object_mut().unwrap().remove("shell_type");
        assert!(
            validate_metadata(&invalid)
                .unwrap_err()
                .contains("shell_type")
        );
        assert!(
            publish_saved(&[ModelRecord {
                enabled: false,
                ..first
            }])
            .is_err()
        );
    }
}
