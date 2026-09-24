use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Value, table, value};

use crate::{
    config_transaction::{
        HeaderDecor, RestoreResult, read_config, replace, sync_parent, write_exclusive_atomic,
    },
    direct::{helper_is_usable, validate_provider},
    storage::ProviderRecord,
};

const JOURNAL_NAME: &str = "direct-journal.json";
const FIELDS: [&str; 3] = ["model", "model_provider", "model_catalog_json"];

#[derive(Debug, Serialize, Deserialize)]
struct FieldChange {
    name: String,
    before: Option<String>,
    applied: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u8,
    config_path: PathBuf,
    config_existed: bool,
    fields: [FieldChange; 3],
    provider_id: String,
    applied_provider: String,
    applied_provider_header: HeaderDecor,
    applied_parent_header: Option<HeaderDecor>,
    before_providers_table: bool,
}

pub struct PreparedDirectSwitch {
    config_path: PathBuf,
    journal_path: PathBuf,
    original: Option<Vec<u8>>,
    journal: Journal,
    pub proposed: String,
    pub changes: Vec<String>,
}

impl PreparedDirectSwitch {
    pub fn target(&self) -> &Path {
        &self.config_path
    }

    pub fn inspect(
        config_path: &Path,
        state_dir: &Path,
        provider: &ProviderRecord,
        helper_path: &Path,
    ) -> Result<Self, String> {
        if !config_path.is_absolute()
            || config_path
                .file_name()
                .is_none_or(|name| name != "config.toml")
            || !state_dir.is_absolute()
        {
            return Err("Codex config and state paths must be absolute".into());
        }
        if !config_path.parent().is_some_and(Path::is_dir) {
            return Err("Codex 配置目录不存在".into());
        }
        if !helper_is_usable(helper_path) {
            return Err("SwitchX credential helper is unavailable".into());
        }
        validate_provider(&provider.name, &provider.base_url, &provider.model_id)?;
        let reference = provider
            .credential_ref
            .as_deref()
            .ok_or("provider credential is missing")?;
        if reference != provider.id || !valid_id(reference) {
            return Err("provider credential reference is invalid".into());
        }
        let journal_path = state_dir.join(JOURNAL_NAME);
        if journal_path.exists() || state_dir.join("switch-journal.json").exists() {
            return Err("a SwitchX switch is active; restore it first".into());
        }
        let original = read_config(config_path)?;
        let current = original
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|_| "Codex config is not UTF-8")?
            .unwrap_or("");
        let mut document: DocumentMut = current.parse().map_err(|_| "invalid Codex TOML")?;
        let provider_id = format!("switchx_direct_{}", provider.id);
        if document
            .as_table()
            .get("model_providers")
            .and_then(Item::as_table)
            .is_some_and(|table| table.contains_key(&provider_id))
        {
            return Err("SwitchX provider id already exists; ownership is unknown".into());
        }
        let before_providers_table = document.as_table().contains_key("model_providers");
        if document
            .as_table()
            .get("model_catalog_json")
            .and_then(Item::as_value)
            .is_some_and(|value| {
                [value.decor().prefix(), value.decor().suffix()]
                    .into_iter()
                    .flatten()
                    .any(|decor| decor.as_str().is_none_or(|text| !text.trim().is_empty()))
            })
        {
            return Err("model_catalog_json 带有注释；请先手动移走该注释再切换".into());
        }
        let fields = [
            ("model", Some(provider.model_id.clone())),
            ("model_provider", Some(provider_id.clone())),
            ("model_catalog_json", None),
        ]
        .map(|(name, applied)| {
            let before = field(&document, name)?;
            Ok::<_, String>(FieldChange {
                name: name.into(),
                before,
                applied,
            })
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        let fields: [FieldChange; 3] = fields.try_into().expect("three managed fields");
        let mut changes = Vec::new();
        for change in &fields {
            if change.before == change.applied {
                continue;
            }
            changes.push(change.name.clone());
            if let Some(applied) = &change.applied {
                set_string(&mut document, &change.name, applied);
            } else {
                document.as_table_mut().remove(&change.name);
            }
        }
        if !before_providers_table {
            document.as_table_mut().insert("model_providers", table());
        }
        let providers = document["model_providers"]
            .as_table_mut()
            .ok_or("model_providers is not a table")?;
        providers.insert(&provider_id, table());
        let generated = providers[&provider_id].as_table_mut().unwrap();
        generated.insert("name", value(provider.name.as_str()));
        generated.insert("base_url", value(provider.base_url.as_str()));
        generated.insert("wire_api", value("responses"));
        generated.insert("supports_websockets", value(false));
        generated.insert("request_max_retries", value(0));
        generated.insert("stream_max_retries", value(0));
        generated.insert("auth", table());
        let auth = generated["auth"].as_table_mut().unwrap();
        auth.insert(
            "command",
            value(helper_path.to_str().ok_or("helper path is not UTF-8")?),
        );
        let mut args = Array::new();
        args.push("credential");
        args.push(reference);
        auth.insert("args", Item::Value(Value::Array(args)));
        changes.push(format!("model_providers.{provider_id}"));
        let proposed = document.to_string();
        let serialized: DocumentMut = proposed
            .parse()
            .map_err(|_| "generated Codex TOML is invalid")?;
        let generated = &serialized["model_providers"][&provider_id];
        let journal = Journal {
            version: 1,
            config_path: config_path.into(),
            config_existed: original.is_some(),
            fields,
            provider_id,
            applied_provider: generated.to_string(),
            applied_provider_header: HeaderDecor::from_table(generated.as_table().unwrap()),
            applied_parent_header: (!before_providers_table).then(|| {
                HeaderDecor::from_table(serialized["model_providers"].as_table().unwrap())
            }),
            before_providers_table,
        };
        Ok(Self {
            config_path: config_path.into(),
            journal_path,
            original,
            journal,
            proposed,
            changes,
        })
    }

    pub fn apply(self) -> Result<(), String> {
        if read_config(&self.config_path)? != self.original {
            return Err("Codex config changed since inspection".into());
        }
        fs::create_dir_all(self.journal_path.parent().unwrap())
            .map_err(|error| error.to_string())?;
        let journal =
            serde_json::to_vec_pretty(&self.journal).map_err(|error| error.to_string())?;
        write_exclusive_atomic(&self.journal_path, &journal)?;
        let permissions = match &self.original {
            Some(_) => Some(
                self.config_path
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .permissions(),
            ),
            None => None,
        };
        replace(
            &self.config_path,
            self.proposed.as_bytes(),
            permissions,
            &self.original,
        )
    }
}

pub fn restore(config_path: &Path, state_dir: &Path) -> Result<RestoreResult, String> {
    let journal_path = state_dir.join(JOURNAL_NAME);
    let journal: Journal =
        serde_json::from_slice(&fs::read(&journal_path).map_err(|error| error.to_string())?)
            .map_err(|_| "SwitchX direct journal is invalid; inspect it before recovery")?;
    if journal.version != 1
        || journal.config_path != config_path
        || !journal
            .provider_id
            .strip_prefix("switchx_direct_")
            .is_some_and(valid_id)
    {
        return Err("SwitchX direct journal belongs to another config or version".into());
    }
    if journal
        .fields
        .iter()
        .zip(FIELDS)
        .any(|(field, name)| field.name != name)
    {
        return Err("SwitchX direct journal has invalid managed fields".into());
    }
    let original = match read_config(config_path)? {
        Some(config) => config,
        None if !journal.config_existed => {
            fs::remove_file(&journal_path).map_err(|error| error.to_string())?;
            sync_parent(&journal_path)?;
            return Ok(RestoreResult {
                conflicts: Vec::new(),
                changed: false,
            });
        }
        None => return Err("Codex config is missing; inspect before recovery".into()),
    };
    let current = std::str::from_utf8(&original).map_err(|_| "Codex config is not UTF-8")?;
    let mut document: DocumentMut = current.parse().map_err(|_| "invalid Codex TOML")?;
    let mut conflicts = Vec::new();
    let mut changed = false;
    for change in journal.fields {
        let found = field(&document, &change.name)?;
        if found == change.before {
            continue;
        }
        if found != change.applied {
            conflicts.push(change.name);
            continue;
        }
        if let Some(before) = change.before {
            set_string(&mut document, &change.name, &before);
        } else {
            document.as_table_mut().remove(&change.name);
        }
        changed = true;
    }
    if let Some(generated) = document
        .as_table()
        .get("model_providers")
        .and_then(Item::as_table)
        .and_then(|providers| providers.get(&journal.provider_id))
    {
        if generated.to_string() == journal.applied_provider
            && generated.as_table().is_some_and(|table| {
                HeaderDecor::from_table(table) == journal.applied_provider_header
            })
        {
            document["model_providers"]
                .as_table_mut()
                .unwrap()
                .remove(&journal.provider_id);
            changed = true;
        } else {
            conflicts.push(format!("model_providers.{}", journal.provider_id));
        }
    }
    if !journal.before_providers_table
        && document
            .as_table()
            .get("model_providers")
            .and_then(Item::as_table)
            .is_some_and(|table| {
                table.is_empty()
                    && Some(HeaderDecor::from_table(table)) == journal.applied_parent_header
            })
    {
        document.as_table_mut().remove("model_providers");
        changed = true;
    }
    if changed
        && !journal.config_existed
        && document.to_string().trim().is_empty()
        && conflicts.is_empty()
    {
        if read_config(config_path)? != Some(original) {
            return Err("Codex config changed before removal".into());
        }
        fs::remove_file(config_path).map_err(|error| error.to_string())?;
        sync_parent(config_path)?;
    } else if changed {
        let permissions = Some(
            config_path
                .metadata()
                .map_err(|error| error.to_string())?
                .permissions(),
        );
        replace(
            config_path,
            document.to_string().as_bytes(),
            permissions,
            &Some(original),
        )?;
    }
    if conflicts.is_empty() {
        fs::remove_file(&journal_path).map_err(|error| error.to_string())?;
        sync_parent(&journal_path)?;
    }
    Ok(RestoreResult { conflicts, changed })
}

pub fn active_target(state_dir: &Path) -> Result<Option<PathBuf>, String> {
    let path = state_dir.join(JOURNAL_NAME);
    match fs::read(path) {
        Ok(bytes) => {
            let journal: Journal = serde_json::from_slice(&bytes)
                .map_err(|_| "SwitchX direct journal is invalid; inspect it before recovery")?;
            if journal.version != 1 || !journal.config_path.is_absolute() {
                return Err("SwitchX direct journal has an unsupported version or target".into());
            }
            Ok(Some(journal.config_path))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn field(document: &DocumentMut, name: &str) -> Result<Option<String>, String> {
    document
        .as_table()
        .get(name)
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} is not a string"))
        })
        .transpose()
}

fn set_string(document: &mut DocumentMut, name: &str, text: &str) {
    if let Some(existing) = document
        .as_table_mut()
        .get_mut(name)
        .and_then(Item::as_value_mut)
    {
        let mut replacement = Value::from(text);
        *replacement.decor_mut() = existing.decor().clone();
        *existing = replacement;
    } else {
        document.as_table_mut().insert(name, value(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Home(PathBuf);

    impl Home {
        fn new() -> Self {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir().join(format!(
                "switchx-direct-{}",
                nonce
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> PathBuf {
            self.0.join("config.toml")
        }

        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn provider() -> ProviderRecord {
        ProviderRecord {
            id: "test-id".into(),
            name: "Mock Responses".into(),
            base_url: "http://127.0.0.1:12345/v1".into(),
            model_id: "mock-model".into(),
            credential_ref: Some("test-id".into()),
        }
    }

    #[test]
    fn direct_switch_restores_only_owned_fields_and_preserves_comments() {
        let home = Home::new();
        let original = "# original\nmodel = \"old-model\" # keep\nmodel_catalog_json = \"/tmp/old.json\"\n\n[mcp_servers.example]\ncommand = \"echo\"\n";
        fs::write(home.config(), original).unwrap();
        let helper = std::env::current_exe().unwrap();
        let prepared =
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .unwrap();
        assert!(!prepared.proposed.contains("model_catalog_json"));
        assert!(prepared.proposed.contains("auth"));
        assert!(!prepared.proposed.contains("test-secret"));
        prepared.apply().unwrap();
        let active = fs::read_to_string(home.config()).unwrap();
        assert!(active.contains("model_provider = \"switchx_direct_test-id\""));
        assert!(active.contains("# keep"));
        fs::write(
            home.config(),
            format!("{active}\n[projects.\"/tmp/new\"]\ntrust_level = \"trusted\"\n"),
        )
        .unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert!(result.conflicts.is_empty());
        let restored = fs::read_to_string(home.config()).unwrap();
        assert!(
            restored.contains("model_catalog_json = \"/tmp/old.json\""),
            "{restored}"
        );
        assert!(restored.contains("model = \"old-model\" # keep"));
        assert!(restored.contains("[mcp_servers.example]"));
        assert!(restored.contains("[projects.\"/tmp/new\"]"));
        assert!(!restored.contains("switchx_direct_test-id"));
        assert!(!home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn direct_switch_conflict_keeps_external_value_and_journal() {
        let home = Home::new();
        fs::write(home.config(), "model = \"before\"\n").unwrap();
        let helper = std::env::current_exe().unwrap();
        let prepared =
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .unwrap();
        fs::write(home.config(), "model = \"changed before apply\"\n").unwrap();
        assert!(prepared.apply().unwrap_err().contains("changed"));
        assert!(!home.state().join(JOURNAL_NAME).exists());
        let prepared =
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .unwrap();
        prepared.apply().unwrap();
        let active = fs::read_to_string(home.config())
            .unwrap()
            .replace("model = \"mock-model\"", "model = \"external\"");
        fs::write(home.config(), active).unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert_eq!(result.conflicts, ["model"]);
        assert!(
            fs::read_to_string(home.config())
                .unwrap()
                .contains("model = \"external\"")
        );
        assert!(home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn direct_switch_restores_absent_config_to_absence() {
        let home = Home::new();
        let helper = std::env::current_exe().unwrap();
        let prepared =
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .unwrap();
        prepared.apply().unwrap();
        assert!(home.config().exists());
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert!(!home.config().exists());
    }

    #[test]
    fn journal_omits_user_comments_and_rejects_removed_field_comments() {
        let home = Home::new();
        let helper = std::env::current_exe().unwrap();
        fs::write(
            home.config(),
            "model = \"old\" # private-marker\nmodel_catalog_json = \"/tmp/catalog.json\" # catalog note\n",
        )
        .unwrap();
        assert!(
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .err()
                .unwrap()
                .contains("注释")
        );
        fs::write(
            home.config(),
            "model = \"old\" # private-marker\nmodel_catalog_json = \"/tmp/catalog.json\"\n\n[model_providers] # private-parent\n",
        )
        .unwrap();
        let prepared =
            PreparedDirectSwitch::inspect(&home.config(), &home.state(), &provider(), &helper)
                .unwrap();
        prepared.apply().unwrap();
        let journal = fs::read_to_string(home.state().join(JOURNAL_NAME)).unwrap();
        assert!(!journal.contains("private-marker"));
        assert!(!journal.contains("private-parent"));
    }
}
