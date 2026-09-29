//! A single Codex config switch. Callers must verify the router before `apply`.

use std::{
    fs::{self, File, OpenOptions, Permissions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, Value, table, value};

use crate::{
    catalog::Publication,
    config::{PROVIDER_ID, Preview, preview_route},
    config_overlay::Overlay,
    direct::helper_is_usable,
};

const JOURNAL_NAME: &str = "switch-journal.json";
const FIELDS: [&str; 3] = ["model", "model_provider", "model_catalog_json"];

#[derive(Debug, Serialize, Deserialize)]
struct FieldChange {
    name: String,
    before: Option<String>,
    applied: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct HeaderDecor {
    prefix: Option<String>,
    suffix: Option<String>,
}

impl HeaderDecor {
    pub(crate) fn from_table(table: &Table) -> Self {
        Self {
            prefix: table
                .decor()
                .prefix()
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            suffix: table
                .decor()
                .suffix()
                .and_then(|value| value.as_str())
                .map(str::to_owned),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u8,
    config_path: PathBuf,
    catalog_path: PathBuf,
    config_existed: bool,
    fields: [FieldChange; 3],
    applied_provider: String,
    applied_provider_header: HeaderDecor,
    applied_parent_header: Option<HeaderDecor>,
    before_providers_table: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_token_reference: Option<String>,
    #[serde(default)]
    overlay: Overlay,
}

pub struct Recovery {
    pub config_path: PathBuf,
    pub local_token_reference: Option<String>,
}

pub struct PreparedSwitch {
    config_path: PathBuf,
    journal_path: PathBuf,
    catalog_path: PathBuf,
    original: Option<Vec<u8>>,
    catalog: Vec<u8>,
    journal: Journal,
    pub preview: Preview,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RestoreResult {
    pub conflicts: Vec<String>,
    pub changed: bool,
}

impl PreparedSwitch {
    pub fn inspect(
        config_path: &Path,
        state_dir: &Path,
        publication: &Publication,
        router_address: SocketAddr,
        default_model: &str,
    ) -> Result<Self, String> {
        if !config_path.is_absolute() || !state_dir.is_absolute() {
            return Err("config and state paths must be absolute".into());
        }
        if config_path
            .file_name()
            .is_none_or(|name| name != "config.toml")
        {
            return Err("config target must be config.toml".into());
        }
        let journal_path = state_dir.join(JOURNAL_NAME);
        if state_dir.join("direct-journal.json").exists() {
            return Err("a SwitchX direct switch is active; restore it first".into());
        }
        if journal_path.exists() {
            return Err("a SwitchX journal already exists; restore or resolve it first".into());
        }
        let original = read_config(config_path)?;
        let current = original
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|_| "Codex config is not UTF-8")?
            .unwrap_or("");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock is invalid")?
            .as_nanos();
        let catalog_path = config_path
            .parent()
            .ok_or("config path has no parent")?
            .join("switchx")
            .join(format!("catalog-{stamp}-{}.json", std::process::id()));
        let preview = preview_route(
            current,
            publication,
            &catalog_path,
            router_address,
            default_model,
        )?;
        let before: DocumentMut = current.parse().map_err(|_| "invalid Codex TOML")?;
        let applied: DocumentMut = preview
            .proposed
            .parse()
            .map_err(|_| "generated Codex TOML is invalid")?;
        let fields = FIELDS.map(|name| FieldChange {
            name: name.into(),
            before: before
                .as_table()
                .get(name)
                .and_then(|item| item.as_str())
                .map(str::to_owned),
            applied: applied[name]
                .as_str()
                .expect("generated string field")
                .into(),
        });
        let journal = Journal {
            version: 1,
            config_path: config_path.into(),
            catalog_path: catalog_path.clone(),
            config_existed: original.is_some(),
            fields,
            applied_provider: applied["model_providers"][PROVIDER_ID].to_string(),
            applied_provider_header: HeaderDecor::from_table(
                applied["model_providers"][PROVIDER_ID].as_table().unwrap(),
            ),
            applied_parent_header: (!before.as_table().contains_key("model_providers"))
                .then(|| HeaderDecor::from_table(applied["model_providers"].as_table().unwrap())),
            before_providers_table: before.as_table().contains_key("model_providers"),
            local_token_reference: None,
            overlay: Overlay::default(),
        };
        let catalog = serde_json::to_vec_pretty(&publication.catalog)
            .map_err(|_| "could not serialize model catalog")?;
        Ok(Self {
            config_path: config_path.into(),
            journal_path,
            catalog_path,
            original,
            catalog,
            journal,
            preview,
        })
    }

    pub fn with_codex_options(
        self,
        options: &crate::provider_config::CodexOptions,
        common: &str,
    ) -> Result<Self, String> {
        self.with_document_update(|document| {
            crate::provider_config::apply_to_document(document, options, common, PROVIDER_ID)
        })
    }

    pub fn with_common_config(self, common: &str) -> Result<Self, String> {
        self.with_document_update(|document| {
            crate::provider_config::apply_common_to_document(document, common)
        })
    }

    fn with_document_update(
        mut self,
        update: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
    ) -> Result<Self, String> {
        let current = self
            .original
            .as_deref()
            .map(std::str::from_utf8)
            .transpose()
            .map_err(|_| "Codex config is not UTF-8")?
            .unwrap_or("");
        let before: DocumentMut = current.parse().map_err(|_| "invalid Codex TOML")?;
        let mut document: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        update(&mut document)?;
        self.preview.proposed = document.to_string();
        let applied: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        let previous_paths = self.journal.overlay.paths();
        self.preview
            .changed_fields
            .retain(|path| !previous_paths.contains(path));
        self.journal.overlay = Overlay::between(&before, &applied)?;
        for path in self.journal.overlay.paths() {
            if !self.preview.changed_fields.contains(&path) {
                self.preview.changed_fields.push(path);
            }
        }
        let provider = &applied["model_providers"][PROVIDER_ID];
        self.journal.applied_provider = provider.to_string();
        self.journal.applied_provider_header =
            HeaderDecor::from_table(provider.as_table().ok_or("invalid generated provider")?);
        Ok(self)
    }

    pub fn catalog_path(&self) -> &Path {
        &self.catalog_path
    }

    pub fn with_chatgpt_auth(mut self, token: &str, reference: &str) -> Result<Self, String> {
        if token.len() != 64
            || !token.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !valid_token_reference(reference)
        {
            return Err("invalid local ChatGPT route credential".into());
        }
        let mut document: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        let provider = document["model_providers"][PROVIDER_ID]
            .as_table_mut()
            .unwrap();
        provider.remove("env_key");
        provider.remove("auth");
        provider.insert("requires_openai_auth", value(true));
        provider.insert("http_headers", table());
        provider["http_headers"][crate::routing::LOCAL_TOKEN_HEADER] = value(token);
        self.preview.proposed = document.to_string();
        self.preview.required_environment_variable = None;
        let serialized: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        let provider = &serialized["model_providers"][PROVIDER_ID];
        self.journal.applied_provider = provider.to_string();
        self.journal.applied_provider_header =
            HeaderDecor::from_table(provider.as_table().unwrap());
        self.journal.local_token_reference = Some(reference.into());
        Ok(self)
    }

    pub fn with_credential_helper(
        mut self,
        helper_path: &Path,
        reference: &str,
    ) -> Result<Self, String> {
        if !helper_is_usable(helper_path) || !valid_token_reference(reference) {
            return Err("SwitchX local token helper is unavailable or invalid".into());
        }
        let state_dir = self
            .journal_path
            .parent()
            .filter(|path| path.is_absolute())
            .ok_or("local token data directory must be absolute")?
            .to_str()
            .ok_or("local token data directory must be UTF-8")?;
        let mut document: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        let provider = document["model_providers"][PROVIDER_ID]
            .as_table_mut()
            .unwrap();
        provider.remove("env_key");
        provider.remove("requires_openai_auth");
        provider.insert("auth", table());
        let auth = provider["auth"].as_table_mut().unwrap();
        auth.insert(
            "command",
            value(helper_path.to_str().ok_or("helper path must be UTF-8")?),
        );
        let mut args = Array::new();
        args.push("local-token");
        args.push(reference);
        args.push(state_dir);
        auth.insert("args", Item::Value(Value::Array(args)));
        self.preview.proposed = document.to_string();
        self.preview.required_environment_variable = None;
        let serialized: DocumentMut = self
            .preview
            .proposed
            .parse()
            .map_err(|_| "invalid generated config")?;
        let provider = &serialized["model_providers"][PROVIDER_ID];
        self.journal.applied_provider = provider.to_string();
        self.journal.applied_provider_header =
            HeaderDecor::from_table(provider.as_table().unwrap());
        self.journal.local_token_reference = Some(reference.into());
        Ok(self)
    }

    pub fn apply(self) -> Result<PathBuf, String> {
        let _lock = lock_config(&self.config_path)?;
        if self
            .journal_path
            .parent()
            .unwrap()
            .join("direct-journal.json")
            .exists()
        {
            return Err("a SwitchX direct switch is active; restore it first".into());
        }
        if read_config(&self.config_path)? != self.original {
            return Err("Codex config changed since inspection".into());
        }
        fs::create_dir_all(self.catalog_path.parent().unwrap()).map_err(io_error)?;
        fs::create_dir_all(self.journal_path.parent().unwrap()).map_err(io_error)?;
        if self.catalog_path.exists() || self.journal_path.exists() {
            return Err("catalog or journal already exists".into());
        }
        // The catalog is immutable. A leftover catalog is safe if a later step fails.
        write_new(&self.catalog_path, &self.catalog)?;
        let journal = serde_json::to_vec_pretty(&self.journal).map_err(io_error)?;
        write_exclusive_atomic(&self.journal_path, &journal)?;
        if read_config(&self.config_path)? != self.original {
            return Err("Codex config changed before commit; journal remains for recovery".into());
        }
        let permissions = match &self.original {
            Some(_) => Some(self.config_path.metadata().map_err(io_error)?.permissions()),
            None => None,
        };
        #[cfg(unix)]
        let permissions = if self
            .preview
            .proposed
            .parse::<DocumentMut>()
            .map_err(|_| "invalid generated config")?["model_providers"][PROVIDER_ID]
            .as_table()
            .and_then(|provider| provider.get("requires_openai_auth"))
            .and_then(Item::as_bool)
            == Some(true)
        {
            use std::os::unix::fs::PermissionsExt;
            Some(Permissions::from_mode(0o600))
        } else {
            permissions
        };
        replace(
            &self.config_path,
            self.preview.proposed.as_bytes(),
            permissions,
            &self.original,
        )?;
        Ok(self.catalog_path)
    }
}

pub fn recovery(state_dir: &Path) -> Result<Option<Recovery>, String> {
    let Some(bytes) = read_config(&state_dir.join(JOURNAL_NAME))? else {
        return Ok(None);
    };
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| "SwitchX route journal is invalid")?;
    if journal.version != 1
        || !journal.config_path.is_absolute()
        || journal
            .config_path
            .file_name()
            .is_none_or(|name| name != "config.toml")
        || journal
            .local_token_reference
            .as_deref()
            .is_some_and(|reference| !valid_token_reference(reference))
    {
        return Err("SwitchX route journal has an invalid version or target".into());
    }
    Ok(Some(Recovery {
        config_path: journal.config_path,
        local_token_reference: journal.local_token_reference,
    }))
}

fn valid_token_reference(reference: &str) -> bool {
    reference
        .strip_prefix("router-")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub fn restore(config_path: &Path, state_dir: &Path) -> Result<RestoreResult, String> {
    restore_impl(config_path, state_dir, false)
}

pub(crate) fn restore_preserving_journal(
    config_path: &Path,
    state_dir: &Path,
) -> Result<RestoreResult, String> {
    restore_impl(config_path, state_dir, true)
}

fn restore_impl(
    config_path: &Path,
    state_dir: &Path,
    keep_journal: bool,
) -> Result<RestoreResult, String> {
    let _lock = lock_config(config_path)?;
    let journal_path = state_dir.join(JOURNAL_NAME);
    let journal: Journal = serde_json::from_slice(&fs::read(&journal_path).map_err(io_error)?)
        .map_err(|_| "SwitchX journal is invalid; inspect it before recovery")?;
    if journal.version != 1 || journal.config_path != config_path {
        return Err("SwitchX journal belongs to another config or version".into());
    }
    if journal
        .fields
        .iter()
        .zip(FIELDS)
        .any(|(field, expected)| field.name != expected)
    {
        return Err("SwitchX journal has invalid managed fields".into());
    }
    let original = match read_config(config_path)? {
        Some(original) => original,
        None if !journal.config_existed => {
            if !keep_journal {
                fs::remove_file(&journal_path).map_err(io_error)?;
                sync_parent(&journal_path)?;
            }
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

    for field in journal.fields {
        if document
            .as_table()
            .get(&field.name)
            .is_some_and(|item| item.as_str().is_none())
        {
            conflicts.push(field.name);
            continue;
        }
        let found = document
            .as_table()
            .get(&field.name)
            .and_then(|item| item.as_str());
        if found == field.before.as_deref() {
            continue;
        }
        if found != Some(field.applied.as_str()) {
            conflicts.push(field.name);
            continue;
        }
        if let Some(before) = field.before {
            let item = document.as_table_mut().get_mut(&field.name).unwrap();
            let old = item.as_value_mut().unwrap();
            let mut replacement = Value::from(before);
            *replacement.decor_mut() = old.decor().clone();
            *old = replacement;
        } else {
            document.as_table_mut().remove(&field.name);
        }
        changed = true;
    }
    if let Some(provider) = document
        .as_table()
        .get("model_providers")
        .and_then(|item| item.as_table())
        .and_then(|providers| providers.get(PROVIDER_ID))
    {
        if provider.to_string() == journal.applied_provider
            && provider.as_table().is_some_and(|table| {
                HeaderDecor::from_table(table) == journal.applied_provider_header
            })
        {
            document["model_providers"]
                .as_table_mut()
                .unwrap()
                .remove(PROVIDER_ID);
            changed = true;
        } else {
            conflicts.push(format!("model_providers.{PROVIDER_ID}"));
        }
    }
    if !journal.before_providers_table
        && document
            .as_table()
            .get("model_providers")
            .and_then(|item| item.as_table())
            .is_some_and(|table| {
                table.is_empty()
                    && Some(HeaderDecor::from_table(table)) == journal.applied_parent_header
            })
    {
        document.as_table_mut().remove("model_providers");
        changed = true;
    }
    changed |= journal.overlay.restore(&mut document, &mut conflicts)?;
    if changed
        && !journal.config_existed
        && document.to_string().trim().is_empty()
        && conflicts.is_empty()
    {
        if read_config(config_path)? != Some(original) {
            return Err("Codex config changed before removal".into());
        }
        fs::remove_file(config_path).map_err(io_error)?;
        sync_parent(config_path)?;
    } else if changed {
        let permissions = Some(config_path.metadata().map_err(io_error)?.permissions());
        replace(
            config_path,
            document.to_string().as_bytes(),
            permissions,
            &Some(original),
        )?;
    }
    if conflicts.is_empty() && !keep_journal {
        fs::remove_file(&journal_path).map_err(io_error)?;
        sync_parent(&journal_path)?;
    }
    Ok(RestoreResult { conflicts, changed })
}

pub(crate) fn read_config(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
            Err("Codex config must be a regular file, not a symlink".into())
        }
        Ok(_) => fs::read(path).map(Some).map_err(io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(error)),
    }
}

pub(crate) fn lock_config(config_path: &Path) -> Result<File, String> {
    let path = config_path.with_file_name(".switchx-config.lock");
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("SwitchX 配置锁必须是普通文件".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(io_error)?;
    file.try_lock()
        .map_err(|_| "另一个 SwitchX 实例正在修改此 Codex 配置，请稍后重试")?;
    Ok(file)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(io_error)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(io_error(error));
    }
    sync_parent(path)
}

pub(crate) fn write_exclusive_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = temporary_path(path)?;
    let result = (|| {
        write_new(&temporary, bytes)?;
        // A hard link publishes a complete journal and fails if another switch owns this path.
        fs::hard_link(&temporary, path).map_err(io_error)?;
        fs::remove_file(&temporary).map_err(io_error)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn replace(
    path: &Path,
    bytes: &[u8],
    permissions: Option<Permissions>,
    expected: &Option<Vec<u8>>,
) -> Result<(), String> {
    let temporary = temporary_path(path)?;
    let result = (|| {
        write_new(&temporary, bytes)?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temporary, permissions).map_err(io_error)?;
        }
        if read_config(path)? != *expected {
            return Err("Codex config changed before atomic replacement".into());
        }
        fs::rename(&temporary, path).map_err(io_error)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> Result<PathBuf, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is invalid")?
        .as_nanos();
    Ok(path.with_file_name(format!(
        ".{}.switchx-{}-{stamp}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    )))
}

pub(crate) fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path.parent().unwrap())
        .and_then(|dir| dir.sync_all())
        .map_err(io_error)?;
    Ok(())
}

fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Selection, publish};

    struct TestHome(PathBuf);

    impl TestHome {
        fn new() -> Self {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).unwrap();
            let name = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = std::env::temp_dir().join(format!("switchx-config-{name}"));
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

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn publication() -> Publication {
        let templates =
            serde_json::from_str(include_str!("../tests/fixtures/synthetic-models.json")).unwrap();
        publish(
            &templates,
            &[Selection {
                public_id: "sx-ds-flash",
                display_name: "DeepSeek · Flash",
                provider_id: "deepseek",
                upstream_model: "deepseek-flash",
            }],
        )
        .unwrap()
    }

    fn inspect(home: &TestHome) -> PreparedSwitch {
        PreparedSwitch::inspect(
            &home.config(),
            &home.state(),
            &publication(),
            "127.0.0.1:18731".parse().unwrap(),
            "sx-ds-flash",
        )
        .unwrap()
    }

    #[test]
    fn applies_and_restores_only_managed_fields() {
        let home = TestHome::new();
        let original = include_str!("../tests/fixtures/codex-user-config.toml");
        fs::write(home.config(), original).unwrap();
        let prepared = inspect(&home);
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
        let catalog_path = prepared.catalog_path().to_path_buf();
        prepared.apply().unwrap();
        assert!(catalog_path.is_file());
        let active = fs::read_to_string(home.config()).unwrap();
        assert!(active.contains("model = \"sx-ds-flash\" # chosen by user"));
        assert!(active.contains("[model_providers.switchx_router]"));
        assert!(home.state().join(JOURNAL_NAME).exists());

        // Another writer may add unrelated Codex settings while the route is active.
        fs::write(home.config(), format!("{active}\nnotify = true\n")).unwrap();
        let restored = restore(&home.config(), &home.state()).unwrap();
        assert_eq!(restored.conflicts, Vec::<String>::new());
        assert!(restored.changed);
        let result = fs::read_to_string(home.config()).unwrap();
        assert!(result.contains("model = \"gpt-example\" # chosen by user"));
        assert!(result.contains("[mcp_servers.sample]"));
        assert!(result.contains("notify = true"));
        assert!(!result.contains("[model_providers.switchx_router]"));
        assert!(!home.state().join(JOURNAL_NAME).exists());
        assert!(catalog_path.is_file());
    }

    #[test]
    fn subscription_config_uses_independent_local_auth_and_preserves_renewed_native_auth() {
        let home = TestHome::new();
        let original = include_str!("../tests/fixtures/codex-user-config.toml");
        fs::write(home.config(), original).unwrap();
        let auth_path = home.0.join("auth.json");
        fs::write(&auth_path, r#"{"synthetic_generation":"before"}"#).unwrap();
        let local_token = "a".repeat(64);
        let reference = format!("router-{}", "b".repeat(32));
        let prepared = inspect(&home)
            .with_chatgpt_auth(&local_token, &reference)
            .unwrap();
        assert!(prepared.preview.required_environment_variable.is_none());
        let proposed: DocumentMut = prepared.preview.proposed.parse().unwrap();
        let provider = &proposed["model_providers"][PROVIDER_ID];
        assert_eq!(provider["requires_openai_auth"].as_bool(), Some(true));
        assert!(provider.as_table().unwrap().get("auth").is_none());
        assert!(provider.as_table().unwrap().get("env_key").is_none());
        assert_eq!(
            provider["http_headers"][crate::routing::LOCAL_TOKEN_HEADER].as_str(),
            Some(local_token.as_str())
        );
        prepared.apply().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [home.config(), home.state().join(JOURNAL_NAME)] {
                assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        let renewed = r#"{"synthetic_generation":"renewed-by-codex"}"#;
        fs::write(&auth_path, renewed).unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
        assert_eq!(fs::read_to_string(auth_path).unwrap(), renewed);
        assert!(!home.state().join(JOURNAL_NAME).exists());
        assert!(
            inspect(&home)
                .with_chatgpt_auth("PROXY_MANAGED", &reference)
                .is_err()
        );
    }

    #[test]
    fn conflict_keeps_external_model_and_journal_until_resolved() {
        let home = TestHome::new();
        fs::write(home.config(), "model = \"first\"\n").unwrap();
        inspect(&home).apply().unwrap();
        let active = fs::read_to_string(home.config()).unwrap();
        fs::write(
            home.config(),
            active.replace("sx-ds-flash", "external-choice"),
        )
        .unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert_eq!(result.conflicts, ["model"]);
        let current = fs::read_to_string(home.config()).unwrap();
        assert!(current.contains("model = \"external-choice\""));
        assert!(!current.contains("switchx_router"));
        assert!(home.state().join(JOURNAL_NAME).exists());
        fs::write(home.config(), current.replace("external-choice", "first")).unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert!(result.conflicts.is_empty());
        assert!(!home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn changed_config_blocks_apply_before_writing() {
        let home = TestHome::new();
        fs::write(home.config(), "model = \"first\"\n").unwrap();
        let prepared = inspect(&home);
        let catalog_path = prepared.catalog_path().to_path_buf();
        fs::write(home.config(), "model = \"changed\"\n").unwrap();
        assert!(prepared.apply().unwrap_err().contains("changed"));
        assert!(!catalog_path.exists());
        assert!(!home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn absent_config_round_trip_removes_generated_provider_table() {
        let home = TestHome::new();
        inspect(&home).apply().unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert!(!home.config().exists());
    }

    #[test]
    fn successful_restore_keeps_journal_until_finalized_including_absent_config() {
        for original in [Some("model = \"original\" # keep\n"), None] {
            let home = TestHome::new();
            if let Some(original) = original {
                fs::write(home.config(), original).unwrap();
            }
            let reference = format!("router-{}", "a".repeat(32));
            inspect(&home)
                .with_credential_helper(&std::env::current_exe().unwrap(), &reference)
                .unwrap()
                .apply()
                .unwrap();
            let journal_path = home.state().join(JOURNAL_NAME);
            let journal = fs::read(&journal_path).unwrap();
            let restored = restore_preserving_journal(&home.config(), &home.state()).unwrap();
            assert!(restored.conflicts.is_empty());
            assert!(restored.changed);
            assert_eq!(
                read_config(&home.config()).unwrap(),
                original.map(|text| text.as_bytes().to_vec())
            );
            assert_eq!(fs::read(&journal_path).unwrap(), journal);
            assert_eq!(
                recovery(&home.state())
                    .unwrap()
                    .unwrap()
                    .local_token_reference
                    .as_deref(),
                Some(reference.as_str())
            );
            let repeated = restore_preserving_journal(&home.config(), &home.state()).unwrap();
            assert!(repeated.conflicts.is_empty());
            assert!(!repeated.changed);
            assert_eq!(fs::read(&journal_path).unwrap(), journal);
            let finalized = restore(&home.config(), &home.state()).unwrap();
            assert!(finalized.conflicts.is_empty());
            assert!(!finalized.changed);
            assert!(!journal_path.exists());
        }
    }

    #[test]
    fn journal_before_commit_recovers_without_changing_config() {
        let home = TestHome::new();
        let original = "model = \"first\"\n";
        fs::write(home.config(), original).unwrap();
        let prepared = inspect(&home);
        fs::create_dir_all(home.state()).unwrap();
        write_exclusive_atomic(
            &home.state().join(JOURNAL_NAME),
            &serde_json::to_vec(&prepared.journal).unwrap(),
        )
        .unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert!(!result.changed);
        assert!(result.conflicts.is_empty());
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
        assert!(!home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn restore_preserves_comments_added_to_managed_tables() {
        let home = TestHome::new();
        inspect(&home).apply().unwrap();
        let active = fs::read_to_string(home.config()).unwrap();
        assert!(active.contains("[model_providers]"));
        fs::write(
            home.config(),
            active.replace("[model_providers]", "[model_providers] # user note"),
        )
        .unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert!(result.conflicts.is_empty());
        let restored = fs::read_to_string(home.config()).unwrap();
        assert!(restored.contains("[model_providers] # user note"));
        assert!(!restored.contains("switchx_router"));

        inspect(&home).apply().unwrap();
        let active = fs::read_to_string(home.config()).unwrap();
        fs::write(
            home.config(),
            active.replace(
                "[model_providers.switchx_router]",
                "[model_providers.switchx_router] # user note",
            ),
        )
        .unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert_eq!(result.conflicts, ["model_providers.switchx_router"]);
        assert!(
            fs::read_to_string(home.config())
                .unwrap()
                .contains("[model_providers.switchx_router] # user note")
        );
        assert!(home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn helper_route_restores_nested_auth_and_records_only_local_token_reference() {
        let home = TestHome::new();
        let original = "model = \"old\" # private field comment\n\n[model_providers] # private parent comment\n";
        fs::write(home.config(), original).unwrap();
        let reference = format!("router-{}", crate::app::new_id().unwrap());
        let helper = std::env::current_exe().unwrap();
        let prepared = inspect(&home)
            .with_credential_helper(&helper, &reference)
            .unwrap();
        assert!(prepared.preview.required_environment_variable.is_none());
        let config: DocumentMut = prepared.preview.proposed.parse().unwrap();
        let provider = config["model_providers"][PROVIDER_ID].as_table().unwrap();
        assert!(!provider.contains_key("env_key"));
        assert!(!provider.contains_key("requires_openai_auth"));
        let args = provider["auth"]["args"].as_array().unwrap();
        assert_eq!(args.len(), 3);
        assert_eq!(args.get(0).unwrap().as_str(), Some("local-token"));
        assert_eq!(args.get(1).unwrap().as_str(), Some(reference.as_str()));
        assert_eq!(args.get(2).unwrap().as_str(), home.state().to_str());
        assert!(Path::new(args.get(2).unwrap().as_str().unwrap()).is_absolute());
        prepared.apply().unwrap();
        let recovery = recovery(&home.state()).unwrap().unwrap();
        assert_eq!(recovery.config_path, home.config());
        assert_eq!(
            recovery.local_token_reference.as_deref(),
            Some(reference.as_str())
        );
        let journal = fs::read_to_string(home.state().join(JOURNAL_NAME)).unwrap();
        assert!(!journal.contains("private"));
        assert!(!journal.contains("api_key"));
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
    }

    #[test]
    fn route_recovery_handles_precommit_without_config_and_preserves_changed_field_types() {
        let home = TestHome::new();
        let prepared = inspect(&home);
        fs::create_dir_all(home.state()).unwrap();
        write_exclusive_atomic(
            &home.state().join(JOURNAL_NAME),
            &serde_json::to_vec(&prepared.journal).unwrap(),
        )
        .unwrap();
        assert!(!restore(&home.config(), &home.state()).unwrap().changed);
        assert!(!home.config().exists());
        inspect(&home).apply().unwrap();
        let current = fs::read_to_string(home.config()).unwrap();
        fs::write(
            home.config(),
            current.replace("model = \"sx-ds-flash\"", "model = 42"),
        )
        .unwrap();
        assert_eq!(
            restore(&home.config(), &home.state()).unwrap().conflicts,
            ["model"]
        );
        assert!(
            fs::read_to_string(home.config())
                .unwrap()
                .contains("model = 42")
        );
    }

    #[test]
    fn switches_share_a_target_lock_and_reject_another_data_directory_owner() {
        let home = TestHome::new();
        let prepared = inspect(&home);
        let lock = lock_config(&home.config()).unwrap();
        assert!(prepared.apply().unwrap_err().contains("另一个"));
        assert!(!home.state().join(JOURNAL_NAME).exists());
        drop(lock);
        let other_state = home.0.join("other-state");
        let other = PreparedSwitch::inspect(
            &home.config(),
            &other_state,
            &publication(),
            "127.0.0.1:18732".parse().unwrap(),
            "sx-ds-flash",
        )
        .unwrap();
        inspect(&home).apply().unwrap();
        assert!(other.apply().unwrap_err().contains("changed"));
        assert!(!other_state.join(JOURNAL_NAME).exists());
        let provider = crate::storage::ProviderRecord {
            id: "fixture".into(),
            name: "Fixture".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "fixture".into(),
            credential_ref: Some("fixture".into()),
        };
        assert!(
            crate::direct_config::PreparedDirectSwitch::inspect(
                &home.config(),
                &other_state,
                &provider,
                &std::env::current_exe().unwrap()
            )
            .is_err()
        );
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
    }

    #[test]
    fn routed_codex_options_preserve_external_leaf_edits_and_retry_conflicts() {
        let home = TestHome::new();
        let original = "model = \"old\"\napproval_policy = \"on-request\" # keep-preference\n\n[features]\nhooks = false\n";
        fs::write(home.config(), original).unwrap();
        let options = crate::provider_config::CodexOptions {
            remote_compaction: true,
            use_common_config: true,
            context_1m: true,
            compact_limit: 900000,
        };
        let common = "approval_policy = \"never\"\n\n[features]\nhooks = true\nmemories = true\n\n[tui]\nnotifications = true\n";
        let reference = format!("router-{}", "b".repeat(32));
        let prepared = inspect(&home)
            .with_codex_options(&options, common)
            .unwrap()
            .with_credential_helper(&std::env::current_exe().unwrap(), &reference)
            .unwrap();
        assert!(
            prepared
                .preview
                .changed_fields
                .contains(&"features.hooks".into())
        );
        let mut active: DocumentMut = prepared.preview.proposed.parse().unwrap();
        assert_eq!(
            active["model_providers"][PROVIDER_ID]["name"].as_str(),
            Some("OpenAI")
        );
        prepared.apply().unwrap();
        active["features"]["memories"] = value(false);
        active["features"]["external"] = value(true);
        active["model_context_window"] = value(777777);
        fs::write(home.config(), active.to_string()).unwrap();
        let result = restore(&home.config(), &home.state()).unwrap();
        assert_eq!(
            result.conflicts,
            ["features.memories", "model_context_window"]
        );
        let text = fs::read_to_string(home.config()).unwrap();
        let mut restored: DocumentMut = text.parse().unwrap();
        assert_eq!(restored["approval_policy"].as_str(), Some("on-request"));
        assert!(text.contains("# keep-preference"));
        assert_eq!(restored["features"]["hooks"].as_bool(), Some(false));
        assert_eq!(restored["features"]["external"].as_bool(), Some(true));
        assert!(restored.as_table().get("tui").is_none());
        assert!(!text.contains("switchx_router"));
        assert!(home.state().join(JOURNAL_NAME).exists());
        restored["features"]
            .as_table_mut()
            .unwrap()
            .remove("memories");
        restored.as_table_mut().remove("model_context_window");
        fs::write(home.config(), restored.to_string()).unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert!(!home.state().join(JOURNAL_NAME).exists());
    }

    #[test]
    fn routed_options_restore_absent_config_and_legacy_journals() {
        let home = TestHome::new();
        let options = crate::provider_config::CodexOptions {
            use_common_config: true,
            context_1m: true,
            ..Default::default()
        };
        inspect(&home)
            .with_codex_options(&options, "[features]\nhooks = true\n")
            .unwrap()
            .apply()
            .unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert!(!home.config().exists());

        let original = "model = \"old\"\n";
        fs::write(home.config(), original).unwrap();
        inspect(&home).apply().unwrap();
        let path = home.state().join(JOURNAL_NAME);
        let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old.as_object_mut().unwrap().remove("overlay");
        fs::write(path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
    }

    #[test]
    fn legacy_routed_common_config_preserves_existing_window_settings() {
        let home = TestHome::new();
        let original = "model_context_window = 262144\nmodel_auto_compact_token_limit = 200000\n";
        fs::write(home.config(), original).unwrap();
        let prepared = inspect(&home)
            .with_common_config("[features]\nhooks = true\n")
            .unwrap();
        let proposed: DocumentMut = prepared.preview.proposed.parse().unwrap();
        assert_eq!(proposed["model_context_window"].as_integer(), Some(262144));
        assert_eq!(
            proposed["model_auto_compact_token_limit"].as_integer(),
            Some(200000)
        );
        assert_eq!(
            proposed["model_providers"][PROVIDER_ID]["name"].as_str(),
            Some("SwitchX Router")
        );
        prepared.apply().unwrap();
        assert!(
            restore(&home.config(), &home.state())
                .unwrap()
                .conflicts
                .is_empty()
        );
        assert_eq!(fs::read_to_string(home.config()).unwrap(), original);
    }
}
