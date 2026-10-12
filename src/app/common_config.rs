//! The Codex settings shared by every connection that opts in.

use std::path::Path;

use super::{ensure_editable, open_store};
use crate::codex::provider_config;

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
    let path = crate::codex::client::config_path(codex_home)?;
    let Some(original) = crate::codex::route_switch::read_config(&path)? else {
        return saved();
    };
    let contents = std::str::from_utf8(&original).map_err(|_| "当前 Codex 配置不是 UTF-8")?;
    let document: toml_edit::DocumentMut = contents
        .parse()
        .map_err(|_| "当前 Codex 配置不是有效的 TOML")?;
    if crate::codex::route_config::ensure_unmanaged(&document).is_err() {
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
    if crate::codex::route_switch::read_config(&path)? != Some(original) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::new_id;
    use std::{env, fs};

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
        for journal in crate::codex::JOURNALS {
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
        for journal in crate::codex::JOURNALS {
            fs::write(path.join(journal), "synthetic active journal").unwrap();
            assert!(extract_and_save_common_config(&path, saved).is_err());
            assert!(save_common_config(&path, saved).is_err());
            assert_eq!(load_common_config(&path).unwrap(), extracted);
            fs::remove_file(path.join(journal)).unwrap();
        }
        fs::remove_dir_all(path).unwrap();
    }
}
