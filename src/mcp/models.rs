//! Credential-free model editing through the same application services as the UI.

use super::*;
use crate::{
    catalog::{InputModalities, MappingSettings},
    request_overrides::RequestOverrides,
};

pub(super) fn save(
    data_dir: &Path,
    store: &Store,
    arguments: &Map<String, Value>,
) -> Result<Value, String> {
    for key in arguments.keys() {
        if ![
            "public_id",
            "provider_id",
            "display_name",
            "upstream_model",
            "context_window",
            "reasoning_levels",
            "default_reasoning",
            "input_modalities",
            "catalog_path",
            "request_overrides",
        ]
        .contains(&key.as_str())
        {
            return Err("包含不支持的模型字段；请按 save_model 的参数定义调用。".into());
        }
    }
    app::ensure_editable(data_dir)?;
    let records = store.models().map_err(|_| "无法读取模型资料")?;
    let public_id = optional_string(arguments, "public_id")?;
    let old = public_id
        .map(|id| {
            records
                .iter()
                .find(|model| model.public_id == id)
                .ok_or("模型不存在；创建时请省略 public_id。")
        })
        .transpose()?;
    let provider_id = optional_string(arguments, "provider_id")?
        .or_else(|| old.map(|model| model.provider_id.as_str()))
        .ok_or("创建模型需要 provider_id。")?;
    if old.is_some_and(|model| model.provider_id != provider_id) {
        return Err("已有模型不能移动到其他连接；请在目标连接中添加模型。".into());
    }
    let provider = app::load_provider(data_dir, provider_id)?;
    let upstream = optional_string(arguments, "upstream_model")?
        .or_else(|| old.map(|model| model.upstream_model.as_str()))
        .ok_or("创建模型需要 upstream_model。")?;
    let auto_name = format!("{upstream}/{}", provider.name);
    let name = optional_string(arguments, "display_name")?
        .or_else(|| old.map(|model| model.display_name.as_str()))
        .unwrap_or(&auto_name);
    let id = match public_id {
        Some(id) => id.to_owned(),
        None => format!("sx-{}", crate::id::new_id()?),
    };
    let catalog_path = optional_string(arguments, "catalog_path")?.unwrap_or("");
    if !catalog_path.is_empty() && !Path::new(catalog_path).is_absolute() {
        return Err("catalog_path 必须是绝对路径。".into());
    }
    let saved_metadata: Option<Value> = old
        .filter(|_| catalog_path.is_empty())
        .map(|model| serde_json::from_str(&model.metadata).map_err(|_| "已保存的模型资料损坏"))
        .transpose()?;
    let context = optional_count(arguments, "context_window", 1, i64::MAX as u64)?
        .or_else(|| {
            saved_metadata
                .as_ref()
                .and_then(|value| value["context_window"].as_u64())
        })
        .map(|n| n.to_string())
        .unwrap_or_default();
    let levels = string_array(arguments, "reasoning_levels")?
        .map(|levels| levels.join(", "))
        .or_else(|| {
            saved_metadata
                .as_ref()
                .and_then(|value| value["supported_reasoning_levels"].as_array())
                .map(|levels| {
                    levels
                        .iter()
                        .filter_map(|level| level["effort"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
        });
    let default = if arguments.contains_key("default_reasoning") {
        Some(optional_string(arguments, "default_reasoning")?.unwrap_or(""))
    } else {
        saved_metadata
            .as_ref()
            .map(|value| value["default_reasoning_level"].as_str().unwrap_or(""))
    };
    let modalities = string_array(arguments, "input_modalities")?
        .map(|types| {
            if !types.contains(&"text")
                || types
                    .iter()
                    .any(|value| !["text", "image", "audio"].contains(value))
            {
                return Err(
                    "input_modalities 必须包含 text，且只能包含 text、image、audio。".to_owned(),
                );
            }
            Ok(InputModalities::from_metadata(
                &json!({"input_modalities": types}),
            ))
        })
        .transpose()?;
    let modalities = modalities.or_else(|| {
        old.filter(|model| model.upstream_model != upstream)
            .and(saved_metadata.as_ref())
            .map(InputModalities::from_metadata)
    });
    let overrides = match arguments.get("request_overrides") {
        None => None,
        Some(Value::Null) => Some(RequestOverrides::default()),
        Some(value) => {
            let overrides: RequestOverrides = serde_json::from_value(value.clone())
                .map_err(|_| "request_overrides 格式无效；只接受 user_agent、headers 和 body。")?;
            overrides.validate()?;
            Some(overrides)
        }
    };
    let input = app::ModelInput {
        request_overrides: overrides.as_ref(),
        provider_id,
        original_id: public_id.unwrap_or(""),
        public_id: &id,
        display_name: name,
        upstream_model: upstream,
        catalog_path,
        settings: Some(MappingSettings {
            input_modalities: modalities,
            context_window: &context,
            reasoning_levels: levels.as_deref(),
            default_reasoning: default,
        }),
    };
    if provider.kind == ProviderKind::Chatgpt {
        // MCP is synchronous; isolate the CLI-backed async lookup from any caller's runtime.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "无法启动官方模型目录读取".to_owned())?
                        .block_on(crate::chatgpt::save_mapping(data_dir, input))
                })
                .join()
                .map_err(|_| "读取官方模型目录失败".to_owned())?
        })?;
    } else {
        app::save_mapping(data_dir, input)?;
    }
    Ok(json!({ "model": saved_model(store, &id)?, "requires_publish": true }))
}

fn string_array<'a>(
    arguments: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<Vec<&'a str>>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let values = value
        .as_array()
        .ok_or_else(|| format!("{key} 必须是字符串数组。"))?;
    let mut result = Vec::new();
    for value in values {
        let value = value
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| format!("{key} 必须是非空字符串数组。"))?;
        if result.contains(&value) {
            return Err(format!("{key} 不能有重复值。"));
        }
        result.push(value);
    }
    Ok(Some(result))
}

pub(super) fn delete(
    data_dir: &Path,
    store: &Store,
    arguments: &Map<String, Value>,
) -> Result<Value, String> {
    let id = required_string(arguments, "public_id")?;
    saved_model(store, id)?;
    app::delete_model(data_dir, id)?;
    Ok(json!({ "deleted": id, "requires_publish": true }))
}

pub(super) fn definitions() -> [Value; 2] {
    [
        described(
            "save_model",
            "添加或编辑模型",
            "创建或修改模型的全部可编辑资料。编辑时传 public_id；创建时省略 public_id 并传 provider_id 和 upstream_model。省略字段保留原值；input_modalities 为 [text] 时明确改成纯文本。request_overrides 整体替换，null 清空；不返回定制内容。启停与备用连接请用对应工具。订阅连接从本机 Codex CLI 读取官方资料。配置托管时禁止修改；保存后需预览并重新发布。",
            json!({
                "public_id": {"type": "string", "description": "编辑已有映射时必填；来自 list_models。"},
                "provider_id": {"type": "string", "description": "创建时必填；来自 list_providers。已有模型的连接不可更改。"},
                "upstream_model": {"type": "string"},
                "display_name": {"type": "string"},
                "context_window": {"type": "integer", "minimum": 1},
                "reasoning_levels": {"type": "array", "uniqueItems": true, "items": {"type": "string", "enum": crate::catalog::REASONING_LEVELS}, "description": "空数组清除思考等级；同时将 default_reasoning 置 null。"},
                "default_reasoning": {"type": ["string", "null"], "description": "必须属于 reasoning_levels；null 清除默认档位。"},
                "input_modalities": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"type": "string", "enum": ["text", "image", "audio"]}, "description": "必须包含 text。省略保留资料；text+image 支持图片，text+audio 支持音频。能力声明须符合上游实际支持情况。"},
                "catalog_path": {"type": "string", "description": "可选的完整模型目录 JSON 的绝对路径。显式输入能力优先于导入资料；订阅连接使用官方目录。"},
                "request_overrides": {"type": ["object", "null"], "additionalProperties": false, "properties": {
                    "user_agent": {"type": "string"}, "headers": {"type": "object", "additionalProperties": {"type": "string"}}, "body": {"type": "object"}
                }, "description": "替换全部请求定制，省略保留，null 清空。禁止认证及会话字段；不填凭据。"}
            }),
            &[],
            Effect::Write,
        ),
        described(
            "delete_model",
            "删除模型映射",
            "删除模型映射；作为备用的模型须先清除备用关系，配置托管时禁止删除。不会删除连接或凭据。",
            json!({"public_id": {"type": "string"}}),
            &["public_id"],
            Effect::Destructive,
        ),
    ]
}
