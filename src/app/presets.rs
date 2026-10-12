//! Built-in API provider presets and the model templates they seed.

use crate::{
    catalog,
    id::new_id,
    storage::{ModelRecord, ProviderRecord},
};

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
    pub(super) fn metadata(&self) -> Result<serde_json::Value, String> {
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
        icon: include_bytes!("../../assets/providers/deepseek.svg"),
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
        icon: include_bytes!("../../assets/providers/kimi.svg"),
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
        icon: include_bytes!("../../assets/providers/minimax.svg"),
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
        icon: include_bytes!("../../assets/providers/xiaomimimo.svg"),
        monochrome: true,
    },
];

pub fn provider_preset(base_url: &str) -> Option<&'static ProviderPreset> {
    let url = crate::upstream::validate_base_url(base_url).ok()?;
    PROVIDER_PRESETS.iter().find(|preset| {
        let address = url.as_str().trim_end_matches('/');
        address == preset.base_url
            || (preset.id == "deepseek" && address == "https://api.deepseek.com/v1")
    })
}

pub(super) fn new_preset_models(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{load_snapshot, open_store},
        upstream::validate_provider,
    };
    use std::{env, fs};

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
        crate::codex::client::check_catalog(&publication.catalog)
            .await
            .unwrap();
    }
}
