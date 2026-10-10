use std::{fs, path::PathBuf};
use switchx::{
    app::{self, ConnectionModel},
    direct_config::{self, PreparedDirectSwitch},
    storage::{ProviderKind, ProviderRecord, Store},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("switchx-directory-{}", app::new_id().unwrap()));
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
    }
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
    let conflict = direct_config::restore(&config, &fixture.0).unwrap();
    assert_eq!(conflict.conflicts, ["model"]);
    assert!(
        catalog_path.exists(),
        "recovery conflicts must retain the generated directory"
    );
    assert!(fixture.0.join("direct-journal.json").exists());
    let mut resolved: toml_edit::DocumentMut =
        fs::read_to_string(&config).unwrap().parse().unwrap();
    resolved["model"] = toml_edit::value("native-model");
    fs::write(&config, resolved.to_string()).unwrap();
    let restored = direct_config::restore(&config, &fixture.0).unwrap();
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
