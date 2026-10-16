use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use switchx::{
    app,
    catalog::{self, InputModalities, MappingSettings},
    storage::{ProviderKind, ProviderRecord, Store},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "switchx-modalities-{}",
            switchx::id::new_id().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        let this = Self(path);
        this.store()
            .put_provider(&ProviderRecord {
                id: "api".into(),
                name: "Synthetic".into(),
                base_url: "https://api.deepseek.com".into(),
                model_id: "deepseek-flash".into(),
                kind: ProviderKind::ApiKey,
                credential_ref: None,
                account_binding: None,
                icon_id: None,
            })
            .unwrap();
        this
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

#[test]
fn imported_capabilities_can_be_preserved_replaced_and_published_without_losing_other_metadata() {
    let fixture = Fixture::new();
    let mut template: Value =
        serde_json::from_str(include_str!("fixtures/synthetic-models.json")).unwrap();
    let model_id = template["models"][0]["slug"].as_str().unwrap().to_owned();
    template["models"][0]["input_modalities"] = json!(["text", "image", "audio"]);
    template["models"][0]["supports_image_detail_original"] = true.into();
    template["models"][0]["custom_capability"] = json!({"keep": true});
    let path = fixture.0.join("catalog.json");
    fs::write(&path, template.to_string()).unwrap();
    let save = |original, source: &str, modalities| {
        app::save_mapping(
            &fixture.0,
            app::ModelInput {
                provider_id: "api",
                original_id: original,
                public_id: "sx-test",
                display_name: "My model",
                upstream_model: &model_id,
                catalog_path: source,
                request_overrides: None,
                settings: Some(MappingSettings {
                    input_modalities: modalities,
                    context_window: "",
                    reasoning_levels: None,
                    default_reasoning: None,
                }),
            },
        )
        .unwrap();
    };
    let metadata = || -> Value {
        serde_json::from_str(&fixture.store().models().unwrap()[0].metadata).unwrap()
    };
    save("", path.to_str().unwrap(), None);
    assert_eq!(
        metadata()["input_modalities"],
        json!(["text", "image", "audio"])
    );
    save("sx-test", "", None);
    assert_eq!(metadata()["supports_image_detail_original"], true);
    for modalities in [
        InputModalities::Text,
        InputModalities::TextAudio,
        InputModalities::TextImage,
        InputModalities::TextImageAudio,
    ] {
        save("sx-test", "", Some(modalities));
        let records = fixture.store().models().unwrap();
        let published = catalog::publish_saved(&records).unwrap();
        assert_eq!(
            published.catalog["models"][0]["input_modalities"],
            json!(modalities.types())
        );
        assert_eq!(metadata()["custom_capability"], json!({"keep": true}));
        assert_eq!(
            metadata()["supports_image_detail_original"],
            false,
            "enabling images does not invent original-detail support"
        );
    }
    // Importing again can restore the complete original capability set.
    save("sx-test", path.to_str().unwrap(), None);
    assert_eq!(metadata()["supports_image_detail_original"], true);
    // An explicit choice wins over an imported catalog, including pure text.
    save(
        "sx-test",
        path.to_str().unwrap(),
        Some(InputModalities::Text),
    );
    assert_eq!(metadata()["input_modalities"], json!(["text"]));
}

#[test]
fn batch_choices_override_shared_choices_and_shared_choices_override_templates() {
    for shared in [
        None,
        Some(InputModalities::Text),
        Some(InputModalities::TextImageAudio),
    ] {
        let fixture = Fixture::new();
        app::save_mappings(
            &fixture.0,
            &app::BatchInput {
                provider_id: "api",
                request_overrides: None,
                context_window: "",
                reasoning_levels: "",
                default_reasoning: "",
                input_modalities: shared,
                models: &[
                    app::BatchModel {
                        upstream_model: "deepseek-flash",
                        context_window: "",
                        reasoning_levels: "",
                        input_modalities: None,
                    },
                    app::BatchModel {
                        upstream_model: "custom-text",
                        context_window: "",
                        reasoning_levels: "",
                        input_modalities: Some(InputModalities::Text),
                    },
                    app::BatchModel {
                        upstream_model: "custom-audio",
                        context_window: "",
                        reasoning_levels: "",
                        input_modalities: Some(InputModalities::TextAudio),
                    },
                ],
            },
        )
        .unwrap();
        let records = fixture.store().models().unwrap();
        let types = |id| -> Value {
            serde_json::from_str::<Value>(
                &records
                    .iter()
                    .find(|m| m.upstream_model == id)
                    .unwrap()
                    .metadata,
            )
            .unwrap()["input_modalities"]
                .clone()
        };
        assert_eq!(
            types("deepseek-flash"),
            json!(shared.unwrap_or(InputModalities::TextImage).types())
        );
        assert_eq!(types("custom-text"), json!(["text"]));
        assert_eq!(types("custom-audio"), json!(["text", "audio"]));
        let saved = records
            .iter()
            .find(|m| m.upstream_model == "deepseek-flash")
            .unwrap();
        // The directory path uses the same replacement semantics and survives reopening.
        app::save_connection_models(
            &fixture.0,
            "api",
            &records,
            &[app::ConnectionModel {
                public_id: saved.public_id.clone(),
                display_name: saved.display_name.clone(),
                upstream_model: saved.upstream_model.clone(),
                context_window: "".into(),
                reasoning_levels: "low, high".into(),
                default_reasoning: "high".into(),
                input_modalities: Some(InputModalities::TextAudio),
            }],
            &[],
        )
        .unwrap();
        let fresh = fixture.store().models().unwrap();
        assert_eq!(
            catalog::publish_saved(&fresh).unwrap().catalog["models"][0]["input_modalities"],
            json!(["text", "audio"])
        );
        assert_eq!(
            app::load_snapshot(&fixture.0, false).unwrap().models[0].input_modalities,
            InputModalities::TextAudio
        );
    }
}
