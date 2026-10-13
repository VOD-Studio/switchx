use std::{fs, path::PathBuf};
use switchx::{
    app::{self, ConnectionModel},
    storage::{ProviderKind, ProviderRecord, Store},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "switchx-directory-{}",
            switchx::id::new_id().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> Store {
        Store::open(&self.0.join("switchx.sqlite")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn provider(id: &str) -> ProviderRecord {
    ProviderRecord {
        id: id.into(),
        name: id.into(),
        base_url: "https://example.invalid/v1".into(),
        model_id: "coder-pro".into(),
        kind: ProviderKind::ApiKey,
        credential_ref: None,
        account_binding: None,
        icon_id: None,
    }
}
fn draft(id: &str) -> ConnectionModel {
    ConnectionModel {
        public_id: String::new(),
        display_name: format!("{id} · 示例"),
        upstream_model: id.into(),
        context_window: "128000".into(),
        reasoning_levels: "low, high".into(),
        default_reasoning: "high".into(),
    }
}

#[test]
fn new_api_connection_saves_the_edited_directory_atomically_without_readding_presets() {
    use switchx::codex::provider_config::CodexOptions;
    let fixture = Fixture::new();
    let mut edited = draft("deepseek-flash");
    edited.display_name = " 自定义显示名称 / DeepSeek ".into();
    edited.context_window = "256000".into();
    edited.default_reasoning = "low".into();
    let saved = app::save_provider_with_directory(
        &fixture.0,
        "DeepSeek",
        "https://api.deepseek.com",
        "deepseek-flash",
        "synthetic-key".into(),
        &CodexOptions::default(),
        None,
        &[edited.clone()],
    )
    .unwrap();
    let store = fixture.store();
    let provider = store.providers().unwrap().remove(0);
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1, "removed preset entries must stay removed");
    assert_eq!(models[0].public_id, saved[0]);
    assert_eq!(models[0].provider_id, provider.id);
    assert_eq!(models[0].display_name, edited.display_name.trim());
    assert!(!models[0].enabled, "saving must not join the active route");
    let metadata: serde_json::Value = serde_json::from_str(&models[0].metadata).unwrap();
    assert_eq!(metadata["context_window"], 256000);
    assert_eq!(metadata["default_reasoning_level"], "low");
    assert_eq!(
        metadata["input_modalities"],
        serde_json::json!(["text", "image"])
    );
    assert!(store.provider_api_key(&provider.id).unwrap().is_some());

    edited.display_name = "new name".into();
    edited.default_reasoning = "medium".into();
    assert!(
        app::save_provider_with_directory(
            &fixture.0,
            "Invalid",
            "https://example.invalid/v1",
            "deepseek-flash",
            "synthetic-key".into(),
            &CodexOptions::default(),
            None,
            &[edited.clone()],
        )
        .is_err()
    );
    assert_eq!(store.providers().unwrap(), vec![provider.clone()]);
    assert_eq!(store.models().unwrap(), models);

    edited.default_reasoning = "high".into();
    edited.display_name = models[0].display_name.clone();
    assert!(
        app::save_provider_with_directory(
            &fixture.0,
            "Duplicate",
            "https://example.invalid/v1",
            "deepseek-flash",
            "synthetic-key".into(),
            &CodexOptions::default(),
            None,
            &[edited],
        )
        .is_err()
    );
    assert_eq!(store.providers().unwrap(), vec![provider]);
    assert_eq!(store.models().unwrap(), models);
}

#[test]
fn new_subscription_directory_keeps_only_edited_models_and_official_capabilities() {
    use switchx::{
        catalog, chatgpt, codex::provider_config::CodexOptions, storage::AccountBinding,
    };
    let fixture = Fixture::new();
    let mut template = catalog::mapping_metadata(
        "official-pro",
        "Official",
        &catalog::MappingSettings {
            context_window: "1000000",
            reasoning_levels: Some("low, high"),
            default_reasoning: Some("high"),
        },
        None,
    )
    .unwrap();
    template["custom_capability"] = serde_json::json!({"preserve": true});
    let other = catalog::mapping_metadata(
        "official-mini",
        "Mini",
        &catalog::MappingSettings {
            context_window: "128000",
            reasoning_levels: None,
            default_reasoning: None,
        },
        None,
    )
    .unwrap();
    let templates = [template, other];
    let id = chatgpt::save_subscription_with_directory(
        &fixture.0,
        None,
        "Subscription draft",
        AccountBinding::Native,
        &templates,
        &CodexOptions::default(),
        "",
        None,
        Some(&[draft("official-pro")]),
    )
    .unwrap();
    let store = fixture.store();
    let models = store.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].provider_id, id);
    assert_eq!(models[0].display_name, "official-pro · 示例");
    let metadata: serde_json::Value = serde_json::from_str(&models[0].metadata).unwrap();
    assert_eq!(metadata["custom_capability"]["preserve"], true);
    assert_eq!(metadata["context_window"], 128000);
    assert!(!models[0].enabled);
    assert!(
        chatgpt::save_subscription_with_directory(
            &fixture.0,
            None,
            "Invalid",
            AccountBinding::Native,
            &templates,
            &CodexOptions::default(),
            "",
            None,
            Some(&[draft("unknown")]),
        )
        .is_err()
    );
    assert_eq!(store.providers().unwrap().len(), 1);
    assert_eq!(store.models().unwrap(), models);
    assert!(!fixture.0.join("auth.json").exists());
}

#[test]
fn names_are_global_trimmed_and_renames_keep_identity_and_compatibility() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.put_provider(&provider("one")).unwrap();
    store.put_provider(&provider("two")).unwrap();
    let mut first = draft("coder-pro");
    first.display_name = "  qwen3.8-max/Podlink  ".into();
    app::save_connection_models(&fixture.0, "one", &[], &[first], &[]).unwrap();
    let original = store.models().unwrap();
    assert_eq!(original[0].display_name, "qwen3.8-max/Podlink");
    let mut duplicate = draft("coder-mini");
    duplicate.display_name = "qwen3.8-max/Podlink".into();
    assert!(
        app::save_connection_models(&fixture.0, "two", &[], &[duplicate.clone()], &[])
            .unwrap_err()
            .contains("已被使用")
    );
    assert_eq!(store.models().unwrap(), original);

    let mut renamed = ConnectionModel {
        public_id: original[0].public_id.clone(),
        ..draft("coder-pro")
    };
    renamed.display_name = "通义千问 / Podlink".into();
    app::save_connection_models(&fixture.0, "one", &original, &[renamed.clone()], &[]).unwrap();
    let updated = store.models().unwrap();
    assert_eq!(updated[0].public_id, original[0].public_id);
    assert_eq!(
        store.model_aliases().unwrap(),
        vec![("qwen3.8-max/Podlink".into(), original[0].public_id.clone())]
    );
    app::save_connection_models(&fixture.0, "one", &updated, &[renamed], &[]).unwrap();
    assert_eq!(
        fixture.store().model_aliases().unwrap(),
        store.model_aliases().unwrap()
    );
    assert!(
        app::save_connection_models(&fixture.0, "two", &[], &[duplicate.clone()], &[]).is_err()
    );
    duplicate.display_name = "通义千问 / Podlink".into();
    assert!(app::save_connection_models(&fixture.0, "two", &[], &[duplicate], &[]).is_err());

    let mut a = draft("a");
    let mut b = draft("b");
    a.display_name = "重名".into();
    b.display_name = " 重名 ".into();
    assert!(
        app::save_connection_models(&fixture.0, "two", &[], &[a, b], &[])
            .unwrap_err()
            .contains("重复")
    );
    store.delete_provider("one").unwrap();
    assert!(store.model_aliases().unwrap().is_empty());
}

#[test]
fn legacy_duplicate_names_can_be_resolved_without_ambiguous_aliases() {
    let fixture = Fixture::new();
    let store = fixture.store();
    for id in ["one", "two"] {
        store.put_provider(&provider(id)).unwrap();
        let mut model = draft("coder-pro");
        model.display_name = id.into();
        app::save_connection_models(&fixture.0, id, &[], &[model], &[]).unwrap();
    }
    // Older versions allowed identical display names across connections.
    rusqlite::Connection::open(fixture.0.join("switchx.sqlite"))
        .unwrap()
        .execute(
            "UPDATE published_models SET display_name = 'legacy name'",
            [],
        )
        .unwrap();
    let expected: Vec<_> = store
        .models()
        .unwrap()
        .into_iter()
        .filter(|model| model.provider_id == "one")
        .collect();
    let renamed = ConnectionModel {
        public_id: expected[0].public_id.clone(),
        display_name: "unique name / one".into(),
        ..draft("coder-pro")
    };
    app::save_connection_models(&fixture.0, "one", &expected, &[renamed], &[]).unwrap();
    assert!(store.model_aliases().unwrap().is_empty());
    assert_eq!(
        store
            .models()
            .unwrap()
            .iter()
            .filter(|model| model.display_name == "legacy name")
            .count(),
        1
    );
}

#[test]
fn directory_save_is_atomic_preserves_identity_selection_and_detects_external_edits() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.put_provider(&provider("api")).unwrap();
    app::save_connection_models(
        &fixture.0,
        "api",
        &[],
        &[draft("coder-pro"), draft("coder-mini")],
        &[],
    )
    .unwrap();
    let original = store.models().unwrap();
    assert_eq!(original.len(), 2);
    assert!(original.iter().all(|model| !model.enabled));
    app::select_models(
        &fixture.0,
        &original
            .iter()
            .map(|model| model.public_id.clone())
            .collect::<Vec<_>>(),
        true,
    )
    .unwrap();
    let selected = store.models().unwrap();
    let mut drafts: Vec<_> = selected
        .iter()
        .map(|model| ConnectionModel {
            public_id: model.public_id.clone(),
            ..draft(&model.upstream_model)
        })
        .collect();
    drafts[0].display_name = "自定义菜单名".into();
    drafts[0].context_window = "1000000".into();
    drafts[0].reasoning_levels = "none, high, max".into();
    app::save_connection_models(&fixture.0, "api", &selected, &drafts, &[]).unwrap();
    let edited = store.models().unwrap();
    assert_eq!(edited[0].public_id, selected[0].public_id);
    assert!(edited.iter().all(|model| model.enabled));
    assert_eq!(edited[0].display_name, "自定义菜单名");
    let metadata: serde_json::Value = serde_json::from_str(&edited[0].metadata).unwrap();
    assert_eq!(metadata["context_window"], 1000000);
    assert_eq!(metadata["supported_reasoning_levels"][2]["effort"], "max");

    let mut duplicate = drafts.clone();
    duplicate[1].upstream_model = duplicate[0].upstream_model.clone();
    assert!(app::save_connection_models(&fixture.0, "api", &edited, &duplicate, &[]).is_err());
    assert_eq!(store.models().unwrap(), edited);
    let mut invalid = drafts.clone();
    invalid[1].context_window = "not-a-number".into();
    assert!(app::save_connection_models(&fixture.0, "api", &edited, &invalid, &[]).is_err());
    assert_eq!(store.models().unwrap(), edited);
    app::select_model(&fixture.0, &edited[0].public_id, false).unwrap();
    assert!(app::save_connection_models(&fixture.0, "api", &edited, &drafts, &[]).is_err());
    assert!(!store.models().unwrap()[0].enabled);

    let latest = store.models().unwrap();
    app::save_connection_models(&fixture.0, "api", &latest, &[], &[]).unwrap();
    assert!(store.models().unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn direct_mode_publishes_connection_directory_with_actual_ids_and_restores_it() {
    use switchx::codex::direct_switch::{self, PreparedDirectSwitch};

    let fixture = Fixture::new();
    let store = fixture.store();
    let provider = provider("api");
    store.put_provider(&provider).unwrap();
    app::save_connection_models(
        &fixture.0,
        "api",
        &[],
        &[draft("coder-pro"), draft("coder-mini")],
        &[],
    )
    .unwrap();
    let models = store.models().unwrap();
    let home = fixture.0.join("codex");
    fs::create_dir(&home).unwrap();
    let config = home.join("config.toml");
    let original = "model = \"native-model\"\nmodel_catalog_json = \"/original/catalog.json\"\n";
    fs::write(&config, original).unwrap();
    let prepared = PreparedDirectSwitch::inspect(
        &config,
        &fixture.0,
        &provider,
        std::path::Path::new("/usr/bin/true"),
    )
    .unwrap()
    .with_model_directory(&models)
    .unwrap();
    prepared.validate_model_directory(&models).unwrap();
    assert!(prepared.validate_model_directory(&[]).is_err());
    assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("direct-models-")
    }));
    prepared.apply().unwrap();
    let applied: toml_edit::DocumentMut = fs::read_to_string(&config).unwrap().parse().unwrap();
    let catalog_path = PathBuf::from(applied["model_catalog_json"].as_str().unwrap());
    let catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    let ids: Vec<_> = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["slug"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"coder-pro") && ids.contains(&"coder-mini"));
    assert!(ids.iter().all(|id| !id.starts_with("sx-")));
    assert_eq!(applied["model"].as_str(), Some("coder-pro"));
    let mut external = applied.clone();
    external["model"] = toml_edit::value("externally-chosen-model");
    fs::write(&config, external.to_string()).unwrap();
    let conflict = direct_switch::restore(&config, &fixture.0).unwrap();
    assert_eq!(conflict.conflicts, ["model"]);
    assert!(
        catalog_path.exists(),
        "recovery conflicts must retain the generated directory"
    );
    assert!(fixture.0.join(switchx::codex::DIRECT_JOURNAL).exists());
    let mut resolved: toml_edit::DocumentMut =
        fs::read_to_string(&config).unwrap().parse().unwrap();
    resolved["model"] = toml_edit::value("native-model");
    fs::write(&config, resolved.to_string()).unwrap();
    let restored = direct_switch::restore(&config, &fixture.0).unwrap();
    assert!(restored.conflicts.is_empty());
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
    assert!(!catalog_path.exists());
}

#[test]
fn subscription_directory_keeps_official_capabilities_and_rejects_unknown_models() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let mut provider = provider("subscription");
    provider.kind = ProviderKind::Chatgpt;
    provider.base_url = switchx::chatgpt::BASE_URL.into();
    provider.account_binding = Some(switchx::storage::AccountBinding::Native);
    store.put_provider(&provider).unwrap();
    let mut template = switchx::catalog::mapping_metadata(
        "official-pro",
        "Official",
        &switchx::catalog::MappingSettings {
            context_window: "1000000",
            reasoning_levels: Some("low, high"),
            default_reasoning: Some("high"),
        },
        None,
    )
    .unwrap();
    template["custom_capability"] = serde_json::json!({"preserve": true});
    template["supports_parallel_tool_calls"] = true.into();
    app::save_connection_models(
        &fixture.0,
        "subscription",
        &[],
        &[draft("official-pro")],
        &[template.clone()],
    )
    .unwrap();
    let original = store.models().unwrap();
    let mut edit = draft("official-pro");
    edit.public_id = original[0].public_id.clone();
    edit.reasoning_levels = "low, medium".into();
    edit.default_reasoning.clear();
    app::save_connection_models(
        &fixture.0,
        "subscription",
        &original,
        &[edit],
        &[template.clone()],
    )
    .unwrap();
    let saved = store.models().unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&saved[0].metadata).unwrap();
    assert_eq!(metadata["custom_capability"]["preserve"], true);
    assert_eq!(metadata["supports_parallel_tool_calls"], true);
    assert!(metadata["default_reasoning_level"].is_null());
    assert!(
        app::save_connection_models(
            &fixture.0,
            "subscription",
            &saved,
            &[draft("unknown-official-model")],
            &[template]
        )
        .is_err()
    );
    assert_eq!(store.models().unwrap(), saved);
}

#[test]
fn reasoning_defaults_round_trip_and_invalid_defaults_do_not_partially_save() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store.put_provider(&provider("api")).unwrap();
    app::save_connection_models(&fixture.0, "api", &[], &[draft("coder-pro")], &[]).unwrap();
    let original = store.models().unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&original[0].metadata).unwrap();
    assert_eq!(metadata["default_reasoning_level"], "high");
    let mut changed = ConnectionModel {
        public_id: original[0].public_id.clone(),
        ..draft("coder-pro")
    };
    changed.default_reasoning = "low".into();
    app::save_connection_models(&fixture.0, "api", &original, &[changed.clone()], &[]).unwrap();
    let saved = store.models().unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&saved[0].metadata).unwrap();
    assert_eq!(metadata["default_reasoning_level"], "low");
    changed.reasoning_levels = "high".into();
    assert!(
        app::save_connection_models(&fixture.0, "api", &saved, &[changed.clone()], &[]).is_err()
    );
    assert_eq!(store.models().unwrap(), saved);
    changed.default_reasoning.clear();
    app::save_connection_models(&fixture.0, "api", &saved, &[changed], &[]).unwrap();
    let cleared = store.models().unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&cleared[0].metadata).unwrap();
    assert!(metadata["default_reasoning_level"].is_null());
    assert_eq!(metadata["supported_reasoning_levels"][0]["effort"], "high");
    assert_eq!(cleared[0].public_id, original[0].public_id);
}
