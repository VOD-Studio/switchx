use std::path::Path;

use rusqlite::{Connection, OpenFlags, Result, params};

use crate::credentials::Secret;

const SCHEMA_VERSION: i64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model_id: String,
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecord {
    pub provider_id: String,
    pub public_id: String,
    pub display_name: String,
    pub upstream_model: String,
    pub metadata: String,
    pub enabled: bool,
    pub fallback_provider_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    Completed,
    Failed,
    Interrupted,
    Cancelled,
}

impl RequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Completed => "正常完成",
            Self::Failed => "请求失败",
            Self::Interrupted => "流中断",
            Self::Cancelled => "用户取消 / 客户端断开",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    pub id: String,
    pub started_at_ms: i64,
    pub public_model: Option<String>,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
    pub generation: String,
    pub http_status: Option<u16>,
    pub headers_ms: Option<i64>,
    pub first_event_ms: Option<i64>,
    pub duration_ms: i64,
    pub status: RequestStatus,
    // Only SwitchX-owned error codes, never upstream messages or response bodies.
    pub error_code: Option<String>,
    // Set only when a connect error before sending caused one explicit fallback.
    pub fallback_from: Option<String>,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    /// Inspect current metadata without creating, migrating or writing the source database.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(Self { connection })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if version == 0 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE providers (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    base_url TEXT NOT NULL,
                    model_id TEXT NOT NULL DEFAULT '',
                    credential_ref TEXT
                 );
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        } else if version == 1 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE providers ADD COLUMN model_id TEXT NOT NULL DEFAULT '';
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        if version < 3 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE published_models (
                    provider_id TEXT PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
                    public_id TEXT UNIQUE NOT NULL,
                    display_name TEXT NOT NULL,
                    upstream_model TEXT NOT NULL,
                    metadata TEXT NOT NULL,
                    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1))
                 );
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
        if version < 4 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE request_records (
                    id TEXT PRIMARY KEY NOT NULL,
                    started_at_ms INTEGER NOT NULL,
                    public_model TEXT,
                    provider_id TEXT,
                    upstream_model TEXT,
                    generation TEXT NOT NULL,
                    http_status INTEGER,
                    headers_ms INTEGER,
                    first_event_ms INTEGER,
                    duration_ms INTEGER NOT NULL,
                    status TEXT NOT NULL CHECK (status IN ('completed', 'failed', 'interrupted', 'cancelled')),
                    error_code TEXT
                 );
                 CREATE INDEX request_records_recent ON request_records(started_at_ms DESC);
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
        }
        if version < 5 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE published_models ADD COLUMN fallback_provider_id TEXT
                    REFERENCES providers(id) ON DELETE SET NULL
                    CHECK (fallback_provider_id != provider_id);
                 ALTER TABLE request_records ADD COLUMN fallback_from TEXT;
                 PRAGMA user_version = 5;
                 COMMIT;",
            )?;
        }
        if version < 6 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE model_mappings (
                    public_id TEXT PRIMARY KEY NOT NULL,
                    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
                    display_name TEXT NOT NULL,
                    upstream_model TEXT NOT NULL,
                    metadata TEXT NOT NULL,
                    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
                    fallback_provider_id TEXT REFERENCES providers(id) ON DELETE SET NULL
                        CHECK (fallback_provider_id != provider_id),
                    UNIQUE (provider_id, upstream_model)
                 );
                 INSERT INTO model_mappings
                    SELECT public_id, provider_id, display_name, upstream_model, metadata,
                           enabled, fallback_provider_id FROM published_models;
                 DROP TABLE published_models;
                 ALTER TABLE model_mappings RENAME TO published_models;
                 PRAGMA user_version = 6;
                 COMMIT;",
            )?;
        }
        if version < 7 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE providers ADD COLUMN codex_options TEXT NOT NULL DEFAULT '';
                 CREATE TABLE app_settings (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                 );
                 PRAGMA user_version = 7;
                 COMMIT;",
            )?;
        }
        if version < 8 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE providers ADD COLUMN settings_config TEXT NOT NULL DEFAULT '{}';
                 PRAGMA user_version = 8;
                 COMMIT;",
            )?;
        }
        Ok(Self { connection })
    }

    pub fn put_request(&self, record: &RequestRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO request_records
             (id, started_at_ms, public_model, provider_id, upstream_model, generation,
              http_status, headers_ms, first_event_ms, duration_ms, status, error_code, fallback_from)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record.id,
                record.started_at_ms,
                record.public_model,
                record.provider_id,
                record.upstream_model,
                record.generation,
                record.http_status,
                record.headers_ms,
                record.first_event_ms,
                record.duration_ms,
                record.status.as_str(),
                record.error_code,
                record.fallback_from
            ],
        )?;
        Ok(())
    }

    pub fn requests(&self, limit: usize) -> Result<Vec<RequestRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, started_at_ms, public_model, provider_id, upstream_model, generation,
                    http_status, headers_ms, first_event_ms, duration_ms, status, error_code, fallback_from
             FROM request_records ORDER BY started_at_ms DESC, rowid DESC LIMIT ?1",
        )?;
        statement
            .query_map([limit.min(1000) as i64], |row| {
                let status: String = row.get(10)?;
                Ok(RequestRecord {
                    id: row.get(0)?,
                    started_at_ms: row.get(1)?,
                    public_model: row.get(2)?,
                    provider_id: row.get(3)?,
                    upstream_model: row.get(4)?,
                    generation: row.get(5)?,
                    http_status: row.get(6)?,
                    headers_ms: row.get(7)?,
                    first_event_ms: row.get(8)?,
                    duration_ms: row.get(9)?,
                    status: match status.as_str() {
                        "completed" => RequestStatus::Completed,
                        "failed" => RequestStatus::Failed,
                        "interrupted" => RequestStatus::Interrupted,
                        "cancelled" => RequestStatus::Cancelled,
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                    error_code: row.get(11)?,
                    fallback_from: row.get(12)?,
                })
            })?
            .collect()
    }

    pub fn request_time(&self, started_at_ms: i64) -> Result<String> {
        self.connection.query_row(
            "SELECT strftime('%Y-%m-%d %H:%M:%S', ?1 / 1000.0, 'unixepoch', 'localtime')",
            [started_at_ms],
            |row| row.get(0),
        )
    }

    pub fn put_provider(&self, provider: &ProviderRecord) -> Result<()> {
        self.put_provider_with_models(provider, &[])
    }

    pub fn put_provider_with_models(
        &self,
        provider: &ProviderRecord,
        models: &[ModelRecord],
    ) -> Result<()> {
        self.write_provider_with_models(provider, models, None, None, false)
    }

    pub fn put_provider_with_models_and_options(
        &self,
        provider: &ProviderRecord,
        models: &[ModelRecord],
        options: &str,
    ) -> Result<()> {
        self.write_provider_with_models(provider, models, Some(options), None, false)
    }

    /// Store CC Switch-style API credentials alongside metadata and model edits.
    /// ProviderRecord remains metadata only; JSON credentials are read separately.
    pub fn put_provider_with_models_options_and_key(
        &self,
        provider: &ProviderRecord,
        models: &[ModelRecord],
        options: Option<&str>,
        key: &Secret,
    ) -> Result<()> {
        self.write_provider_with_models(provider, models, options, Some(key), false)
    }

    /// Edit a saved API provider without reading and rewriting its existing key.
    pub fn update_provider_preserving_key(
        &self,
        provider: &ProviderRecord,
        models: &[ModelRecord],
        options: Option<&str>,
    ) -> Result<()> {
        self.write_provider_with_models(provider, models, options, None, true)
    }

    fn write_provider_with_models(
        &self,
        provider: &ProviderRecord,
        models: &[ModelRecord],
        options: Option<&str>,
        key: Option<&Secret>,
        require_existing_key: bool,
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO providers (id, name, base_url, model_id, credential_ref)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                base_url = excluded.base_url,
                model_id = excluded.model_id,
                credential_ref = CASE WHEN json_valid(providers.settings_config)
                    THEN CASE WHEN json_type(providers.settings_config, '$.auth.OPENAI_API_KEY') = 'text'
                        THEN NULL ELSE excluded.credential_ref END
                    ELSE excluded.credential_ref END",
            params![
                provider.id,
                provider.name,
                provider.base_url,
                provider.model_id,
                provider.credential_ref
            ],
        )?;
        if let Some(options) = options {
            transaction.execute(
                "UPDATE providers SET codex_options = ?1 WHERE id = ?2",
                params![options, provider.id],
            )?;
        }
        if key.is_some() || require_existing_key {
            let settings: String = transaction.query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [&provider.id],
                |row| row.get(0),
            )?;
            if let Some(key) = key {
                let settings = settings_with_key(&settings, key)?;
                transaction.execute(
                    "UPDATE providers SET settings_config = ?1, credential_ref = NULL WHERE id = ?2",
                    params![settings, provider.id],
                )?;
            } else {
                if provider_settings(&settings)?
                    .pointer("/auth/OPENAI_API_KEY")
                    .is_none()
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                transaction.execute(
                    "UPDATE providers SET credential_ref = NULL WHERE id = ?1",
                    [&provider.id],
                )?;
            }
        }
        for model in models {
            if model.provider_id != provider.id {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Self::write_model(&transaction, model)?;
        }
        transaction.commit()
    }

    /// Invalid credential JSON is an error, never permission to use a legacy key.
    pub fn provider_api_key(&self, id: &str) -> Result<Option<Secret>> {
        use rusqlite::OptionalExtension;
        let settings: Option<String> = self
            .connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(settings) = settings else {
            return Ok(None);
        };
        let settings = provider_settings(&settings)?;
        Ok(settings
            .pointer("/auth/OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .map(|key| Secret::new(key.to_owned())))
    }

    /// Validate stored credentials for UI status without returning a secret.
    pub fn has_provider_api_key(&self, id: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let settings: Option<String> = self
            .connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(settings) = settings else {
            return Ok(false);
        };
        Ok(provider_settings(&settings)?
            .pointer("/auth/OPENAI_API_KEY")
            .is_some())
    }

    /// Migrate one old key reference only if neither its reference nor JSON changed.
    pub fn migrate_provider_api_key(
        &self,
        id: &str,
        expected_ref: &str,
        key: &Secret,
    ) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let current: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT settings_config, credential_ref FROM providers WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((settings, reference)) = current else {
            return Ok(false);
        };
        let parsed = provider_settings(&settings)?;
        if parsed.pointer("/auth/OPENAI_API_KEY").is_some()
            || reference.as_deref() != Some(expected_ref)
        {
            return Ok(false);
        }
        let updated = settings_with_key(&settings, key)?;
        Ok(self.connection.execute(
            "UPDATE providers SET settings_config = ?1, credential_ref = NULL
             WHERE id = ?2 AND credential_ref = ?3 AND settings_config = ?4",
            params![updated, id, expected_ref, settings],
        )? != 0)
    }

    pub fn provider_codex_options(&self, id: &str) -> Result<String> {
        self.connection.query_row(
            "SELECT codex_options FROM providers WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
    }

    /// The subscription provider's explicit account binding; absent follows native Codex login.
    /// `default` resolves the managed account default when preparing a new route.
    pub fn chatgpt_account_binding(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        self.connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'chatgpt_account_binding'",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn bind_chatgpt_account(&self, account: Option<&str>) -> Result<()> {
        if let Some(account) = account {
            if account.is_empty()
                || account.len() > 128
                || !account
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            self.connection.execute(
                "INSERT INTO app_settings (key, value) VALUES ('chatgpt_account_binding', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [account],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM app_settings WHERE key = 'chatgpt_account_binding'",
                [],
            )?;
        }
        Ok(())
    }

    pub fn common_codex_config(&self) -> Result<String> {
        use rusqlite::OptionalExtension;
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'common_codex_config'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn common_codex_config_initialized(&self) -> Result<bool> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_settings WHERE key = 'common_codex_config')",
            [],
            |row| row.get(0),
        )
    }

    pub fn initialize_common_codex_config(&self, snippet: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "INSERT INTO app_settings (key, value) VALUES ('common_codex_config', ?1)
             ON CONFLICT(key) DO NOTHING",
            [snippet],
        )? != 0)
    }

    pub fn put_common_codex_config(&self, snippet: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO app_settings (key, value) VALUES ('common_codex_config', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [snippet],
        )?;
        Ok(())
    }

    pub fn providers(&self) -> Result<Vec<ProviderRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, name, base_url, model_id, credential_ref FROM providers ORDER BY name, id",
        )?;
        statement
            .query_map([], |row| {
                Ok(ProviderRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    base_url: row.get(2)?,
                    model_id: row.get(3)?,
                    credential_ref: row.get(4)?,
                })
            })?
            .collect()
    }

    pub fn provider(&self, id: &str) -> Result<Option<ProviderRecord>> {
        use rusqlite::OptionalExtension;
        self.connection
            .query_row(
                "SELECT id, name, base_url, model_id, credential_ref FROM providers WHERE id = ?1",
                [id],
                |row| {
                    Ok(ProviderRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        base_url: row.get(2)?,
                        model_id: row.get(3)?,
                        credential_ref: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    pub fn delete_provider(&self, id: &str) -> Result<bool> {
        Ok(self
            .connection
            .execute("DELETE FROM providers WHERE id = ?1", [id])?
            != 0)
    }

    pub fn put_model(&self, model: &ModelRecord) -> Result<()> {
        Self::write_model(&self.connection, model)
    }

    fn write_model(connection: &Connection, model: &ModelRecord) -> Result<()> {
        let changed = connection.execute(
            "INSERT INTO published_models (provider_id, public_id, display_name, upstream_model, metadata, enabled, fallback_provider_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(public_id) DO UPDATE SET
                display_name = excluded.display_name,
                upstream_model = excluded.upstream_model,
                metadata = excluded.metadata,
                enabled = excluded.enabled,
                fallback_provider_id = excluded.fallback_provider_id
             WHERE published_models.provider_id = excluded.provider_id",
            params![model.provider_id, model.public_id, model.display_name, model.upstream_model, model.metadata, model.enabled, model.fallback_provider_id],
        )?;
        if changed == 0 {
            Err(rusqlite::Error::InvalidQuery)
        } else {
            Ok(())
        }
    }

    pub fn replace_model(&self, original_id: Option<&str>, model: &ModelRecord) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        if original_id != Some(model.public_id.as_str()) {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM published_models WHERE public_id = ?1)",
                [&model.public_id],
                |row| row.get(0),
            )?;
            if exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        if let Some(original_id) = original_id {
            let owner: String = transaction.query_row(
                "SELECT provider_id FROM published_models WHERE public_id = ?1",
                [original_id],
                |row| row.get(0),
            )?;
            if owner != model.provider_id {
                return Err(rusqlite::Error::InvalidQuery);
            }
            if original_id != model.public_id {
                transaction.execute(
                    "DELETE FROM published_models WHERE public_id = ?1",
                    [original_id],
                )?;
            }
        }
        Self::write_model(&transaction, model)?;
        transaction.commit()
    }

    pub fn delete_model(&self, public_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "DELETE FROM published_models WHERE public_id = ?1",
            [public_id],
        )? != 0)
    }

    pub fn models(&self) -> Result<Vec<ModelRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT provider_id, public_id, display_name, upstream_model, metadata, enabled, fallback_provider_id
             FROM published_models ORDER BY public_id",
        )?;
        statement
            .query_map([], |row| {
                Ok(ModelRecord {
                    provider_id: row.get(0)?,
                    public_id: row.get(1)?,
                    display_name: row.get(2)?,
                    upstream_model: row.get(3)?,
                    metadata: row.get(4)?,
                    enabled: row.get(5)?,
                    fallback_provider_id: row.get(6)?,
                })
            })?
            .collect()
    }
}

fn provider_settings(settings: &str) -> Result<serde_json::Value> {
    let settings: serde_json::Value =
        serde_json::from_str(settings).map_err(|_| rusqlite::Error::InvalidQuery)?;
    if !settings.is_object() || settings.get("auth").is_some_and(|auth| !auth.is_object()) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    if let Some(key) = settings.pointer("/auth/OPENAI_API_KEY") {
        validate_api_key(key.as_str().ok_or(rusqlite::Error::InvalidQuery)?)?;
    }
    Ok(settings)
}

fn validate_api_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 65536 || key.chars().any(char::is_control) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

fn settings_with_key(settings: &str, key: &Secret) -> Result<String> {
    validate_api_key(key.expose())?;
    let mut settings = provider_settings(settings)?;
    let auth = settings
        .as_object_mut()
        .ok_or(rusqlite::Error::InvalidQuery)?
        .entry("auth")
        .or_insert_with(|| serde_json::json!({}));
    auth.as_object_mut()
        .ok_or(rusqlite::Error::InvalidQuery)?
        .insert("OPENAI_API_KEY".into(), key.expose().into());
    serde_json::to_string(&settings).map_err(|_| rusqlite::Error::InvalidQuery)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_provider() -> ProviderRecord {
        ProviderRecord {
            id: "synthetic-api".into(),
            name: "Synthetic API".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "synthetic-model".into(),
            credential_ref: Some("synthetic-legacy-ref".into()),
        }
    }

    #[test]
    fn api_keys_persist_as_json_while_metadata_writes_preserve_them() {
        let path = std::env::temp_dir().join(format!(
            "switchx-api-key-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let provider = api_provider();
        let key = Secret::new("synthetic-plaintext-key".into());
        {
            let store = Store::open(&path).unwrap();
            store.put_provider(&provider).unwrap();
            store
                .connection
                .execute(
                    "UPDATE providers SET settings_config = ?1 WHERE id = ?2",
                    params![
                        r#"{"auth":{"keep":"extra-auth"},"config":"extra-config"}"#,
                        provider.id
                    ],
                )
                .unwrap();
            store
                .put_provider_with_models_options_and_key(
                    &provider,
                    &[],
                    Some("keep-options"),
                    &key,
                )
                .unwrap();
            store
                .put_provider(&ProviderRecord {
                    name: "Renamed".into(),
                    ..provider.clone()
                })
                .unwrap();
            store
                .put_provider_with_models_and_options(&provider, &[], "updated-options")
                .unwrap();
            assert!(
                store
                    .provider(&provider.id)
                    .unwrap()
                    .unwrap()
                    .credential_ref
                    .is_none()
            );
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            key.expose()
        );
        assert!(store.has_provider_api_key(&provider.id).unwrap());
        assert!(!store.has_provider_api_key("missing").unwrap());
        assert!(store.provider_api_key("missing").unwrap().is_none());
        assert_eq!(
            store.provider_codex_options(&provider.id).unwrap(),
            "updated-options"
        );
        let settings: String = store
            .connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [&provider.id],
                |row| row.get(0),
            )
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&settings).unwrap();
        assert_eq!(settings["auth"]["OPENAI_API_KEY"], key.expose());
        assert_eq!(settings["auth"]["keep"], "extra-auth");
        assert_eq!(settings["config"], "extra-config");
        assert!(!format!("{:?}", store.providers().unwrap()).contains(key.expose()));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn provider_key_models_and_options_roll_back_in_one_transaction() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let provider = api_provider();
        let old_key = Secret::new("synthetic-old-key".into());
        store
            .put_provider_with_models_options_and_key(&provider, &[], Some("old-options"), &old_key)
            .unwrap();
        let other = ProviderRecord {
            id: "other".into(),
            credential_ref: None,
            ..provider.clone()
        };
        store.put_provider(&other).unwrap();
        let owned = ModelRecord {
            provider_id: other.id,
            public_id: "sx-owned".into(),
            display_name: "Owned".into(),
            upstream_model: "owned-upstream".into(),
            metadata: "{}".into(),
            enabled: false,
            fallback_provider_id: None,
        };
        store.put_model(&owned).unwrap();
        let models = [
            ModelRecord {
                provider_id: provider.id.clone(),
                public_id: "sx-new".into(),
                ..owned.clone()
            },
            ModelRecord {
                provider_id: provider.id.clone(),
                upstream_model: "conflicting-upstream".into(),
                ..owned.clone()
            },
        ];
        assert!(
            store
                .put_provider_with_models_options_and_key(
                    &ProviderRecord {
                        name: "Changed".into(),
                        ..provider.clone()
                    },
                    &models,
                    Some("new-options"),
                    &Secret::new("synthetic-new-key".into()),
                )
                .is_err()
        );
        let stored = store.provider(&provider.id).unwrap().unwrap();
        assert_eq!(stored.name, provider.name);
        assert!(stored.credential_ref.is_none());
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            old_key.expose()
        );
        assert_eq!(
            store.provider_codex_options(&provider.id).unwrap(),
            "old-options"
        );
        assert_eq!(store.models().unwrap(), [owned]);
        assert!(
            store
                .put_provider_with_models_options_and_key(
                    &provider,
                    &[],
                    None,
                    &Secret::new(String::new())
                )
                .is_err()
        );
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            old_key.expose()
        );
    }

    #[test]
    fn legacy_key_migration_does_not_replace_newer_keys_or_references() {
        let path = std::env::temp_dir().join(format!(
            "switchx-api-key-cas-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let provider = api_provider();
        store.put_provider(&provider).unwrap();
        assert!(
            !store
                .migrate_provider_api_key(
                    &provider.id,
                    "wrong-reference",
                    &Secret::new("synthetic-old-key".into())
                )
                .unwrap()
        );
        assert!(store.provider_api_key(&provider.id).unwrap().is_none());
        let other = Store::open(&path).unwrap();
        other
            .put_provider_with_models_options_and_key(
                &provider,
                &[],
                None,
                &Secret::new("synthetic-concurrent-key".into()),
            )
            .unwrap();
        assert!(
            !store
                .migrate_provider_api_key(
                    &provider.id,
                    "synthetic-legacy-ref",
                    &Secret::new("synthetic-old-key".into())
                )
                .unwrap()
        );
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-concurrent-key"
        );
        store
            .update_provider_preserving_key(
                &ProviderRecord {
                    name: "Preserved concurrent key".into(),
                    ..provider.clone()
                },
                &[],
                Some("preserved-options"),
            )
            .unwrap();
        assert_eq!(
            store.provider(&provider.id).unwrap().unwrap().name,
            "Preserved concurrent key"
        );
        assert_eq!(
            store.provider_codex_options(&provider.id).unwrap(),
            "preserved-options"
        );
        assert_eq!(
            store
                .provider_api_key(&provider.id)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-concurrent-key"
        );
        assert!(
            store
                .provider(&provider.id)
                .unwrap()
                .unwrap()
                .credential_ref
                .is_none()
        );

        let second = ProviderRecord {
            id: "second".into(),
            ..provider.clone()
        };
        store.put_provider(&second).unwrap();
        assert!(
            store
                .update_provider_preserving_key(
                    &ProviderRecord {
                        name: "Must roll back".into(),
                        ..second.clone()
                    },
                    &[],
                    Some("must-roll-back"),
                )
                .is_err()
        );
        assert_eq!(store.provider(&second.id).unwrap(), Some(second.clone()));
        assert_eq!(store.provider_codex_options(&second.id).unwrap(), "");
        other
            .put_provider(&ProviderRecord {
                credential_ref: Some("new-reference".into()),
                ..second.clone()
            })
            .unwrap();
        assert!(
            !store
                .migrate_provider_api_key(
                    &second.id,
                    "synthetic-legacy-ref",
                    &Secret::new("synthetic-old-key".into())
                )
                .unwrap()
        );
        assert!(
            store
                .migrate_provider_api_key(
                    &second.id,
                    "new-reference",
                    &Secret::new("synthetic-migrated-key".into())
                )
                .unwrap()
        );
        assert_eq!(
            store
                .provider_api_key(&second.id)
                .unwrap()
                .unwrap()
                .expose(),
            "synthetic-migrated-key"
        );
        assert!(
            store
                .provider(&second.id)
                .unwrap()
                .unwrap()
                .credential_ref
                .is_none()
        );
        assert!(
            !store
                .migrate_provider_api_key(
                    "missing",
                    "new-reference",
                    &Secret::new("synthetic-key".into())
                )
                .unwrap()
        );
        drop(other);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_credential_json_never_falls_back_or_leaks_keys_in_errors() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let provider = api_provider();
        store.put_provider(&provider).unwrap();
        for settings in [
            r#"{"do-not-expose":"synthetic-private-key""#,
            "[]",
            r#"{"auth":null}"#,
            r#"{"auth":{"OPENAI_API_KEY":7}}"#,
            r#"{"auth":{"OPENAI_API_KEY":""}}"#,
        ] {
            store
                .connection
                .execute(
                    "UPDATE providers SET settings_config = ?1 WHERE id = ?2",
                    params![settings, provider.id],
                )
                .unwrap();
            let error = store
                .provider_api_key(&provider.id)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("synthetic-private-key"));
            assert!(store.has_provider_api_key(&provider.id).is_err());
            assert!(
                store
                    .migrate_provider_api_key(
                        &provider.id,
                        "synthetic-legacy-ref",
                        &Secret::new("synthetic-legacy-key".into())
                    )
                    .is_err()
            );
            assert!(
                store
                    .put_provider_with_models_options_and_key(
                        &provider,
                        &[],
                        None,
                        &Secret::new("synthetic-new-key".into())
                    )
                    .is_err()
            );
            assert_eq!(
                store.provider(&provider.id).unwrap(),
                Some(provider.clone())
            );
        }
    }

    #[test]
    fn key_status_and_reader_apply_the_same_byte_and_control_validation() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let provider = api_provider();
        store.put_provider(&provider).unwrap();
        for key in ["é".repeat(32769), "synthetic\nkey".into()] {
            let settings = serde_json::json!({"auth": {"OPENAI_API_KEY": key}}).to_string();
            store
                .connection
                .execute(
                    "UPDATE providers SET settings_config = ?1 WHERE id = ?2",
                    params![settings, provider.id],
                )
                .unwrap();
            assert!(store.provider_api_key(&provider.id).is_err());
            assert!(store.has_provider_api_key(&provider.id).is_err());
        }
    }

    #[test]
    fn v7_migration_keeps_legacy_references_and_initializes_empty_key_json() {
        let path = std::env::temp_dir().join(format!(
            "switchx-v8-migration-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let provider = api_provider();
        let store = Store::open(&path).unwrap();
        store
            .put_provider_with_models_and_options(&provider, &[], "preserved-options")
            .unwrap();
        store.bind_chatgpt_account(Some("default")).unwrap();
        store
            .connection
            .execute_batch(
                "ALTER TABLE providers DROP COLUMN settings_config; PRAGMA user_version = 7;",
            )
            .unwrap();
        drop(store);
        let before = std::fs::read(&path).unwrap();
        assert!(Store::open_read_only(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.provider(&provider.id).unwrap(),
            Some(provider.clone())
        );
        assert_eq!(
            store.provider_codex_options(&provider.id).unwrap(),
            "preserved-options"
        );
        assert_eq!(
            store.chatgpt_account_binding().unwrap().as_deref(),
            Some("default")
        );
        assert!(store.provider_api_key(&provider.id).unwrap().is_none());
        assert!(!store.has_provider_api_key(&provider.id).unwrap());
        let settings: String = store
            .connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id = ?1",
                [&provider.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(settings, "{}");
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        drop(store);
        assert_eq!(
            Store::open_read_only(&path)
                .unwrap()
                .provider(&provider.id)
                .unwrap(),
            Some(provider)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn common_config_initialization_does_not_overwrite_saved_or_explicitly_cleared_values() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        assert!(!store.common_codex_config_initialized().unwrap());
        assert!(
            store
                .initialize_common_codex_config("first snippet")
                .unwrap()
        );
        assert!(store.common_codex_config_initialized().unwrap());
        assert!(
            !store
                .initialize_common_codex_config("next snippet")
                .unwrap()
        );
        assert_eq!(store.common_codex_config().unwrap(), "first snippet");
        store.put_common_codex_config("").unwrap();
        assert!(store.common_codex_config_initialized().unwrap());
        assert!(
            !store
                .initialize_common_codex_config("next snippet")
                .unwrap()
        );
        assert_eq!(store.common_codex_config().unwrap(), "");
    }

    #[test]
    fn provider_and_models_roll_back_together_on_model_conflict() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let provider = ProviderRecord {
            id: "primary".into(),
            name: "Original".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "first-model".into(),
            credential_ref: Some("synthetic-reference".into()),
        };
        store
            .put_provider_with_models_and_options(&provider, &[], "original options")
            .unwrap();
        let other = ProviderRecord {
            id: "other".into(),
            ..provider.clone()
        };
        store.put_provider(&other).unwrap();
        let owned = ModelRecord {
            provider_id: "other".into(),
            public_id: "sx-owned".into(),
            display_name: "Owned model".into(),
            upstream_model: "first-model".into(),
            metadata: "{}".into(),
            enabled: false,
            fallback_provider_id: None,
        };
        store.put_model(&owned).unwrap();
        let added = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-new".into(),
            ..owned.clone()
        };
        let conflicting = ModelRecord {
            provider_id: "primary".into(),
            upstream_model: "second-model".into(),
            ..owned.clone()
        };
        let updated = ProviderRecord {
            name: "Updated".into(),
            ..provider.clone()
        };
        assert!(
            store
                .put_provider_with_models_and_options(
                    &updated,
                    &[added, conflicting],
                    "updated options",
                )
                .is_err()
        );
        assert_eq!(store.provider("primary").unwrap().unwrap(), provider);
        assert_eq!(
            store.provider_codex_options("primary").unwrap(),
            "original options"
        );
        assert_eq!(store.models().unwrap(), [owned]);
    }

    #[test]
    fn provider_options_and_common_config_persist_without_changing_provider_metadata() {
        let path = std::env::temp_dir().join(format!(
            "switchx-codex-options-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let provider = ProviderRecord {
            id: "primary".into(),
            name: "Primary".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "first-model".into(),
            credential_ref: Some("synthetic-reference".into()),
        };
        let options = r#"{"remote_compaction":true,"context_window":1000000}"#;
        let snippet = "[features]\nmemories = true\n";
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.common_codex_config().unwrap(), "");
            assert!(matches!(
                store.provider_codex_options("missing"),
                Err(rusqlite::Error::QueryReturnedNoRows)
            ));
            store
                .put_provider_with_models_and_options(&provider, &[], options)
                .unwrap();
            store.put_common_codex_config(snippet).unwrap();
            assert_eq!(store.provider("primary").unwrap(), Some(provider.clone()));
            assert_eq!(store.provider_codex_options("primary").unwrap(), options);
            store.put_common_codex_config("updated snippet").unwrap();
            assert_eq!(store.common_codex_config().unwrap(), "updated snippet");
            store.put_common_codex_config(snippet).unwrap();
            store.put_provider(&provider).unwrap();
            store.put_provider_with_models(&provider, &[]).unwrap();
            assert_eq!(store.provider_codex_options("primary").unwrap(), options);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.provider_codex_options("primary").unwrap(), options);
        assert_eq!(store.common_codex_config().unwrap(), snippet);
        store.delete_provider("primary").unwrap();
        assert!(matches!(
            store.provider_codex_options("primary"),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
        assert_eq!(store.common_codex_config().unwrap(), snippet);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v6_migration_preserves_provider_model_and_request_data_with_empty_options() {
        let path = std::env::temp_dir().join(format!(
            "switchx-options-migration-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let provider = ProviderRecord {
            id: "primary".into(),
            name: "Primary".into(),
            base_url: "https://example.invalid/v1".into(),
            model_id: "first-model".into(),
            credential_ref: Some("synthetic-reference".into()),
        };
        let model = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-first".into(),
            display_name: "First".into(),
            upstream_model: "first-model".into(),
            metadata: "retained metadata".into(),
            enabled: true,
            fallback_provider_id: None,
        };
        store
            .put_provider_with_models(&provider, std::slice::from_ref(&model))
            .unwrap();
        store
            .connection
            .execute_batch(
                "INSERT INTO request_records (id, started_at_ms, generation, duration_ms, status)
                    VALUES ('v6-request', 1, 'old-generation', 3, 'completed');
                 ALTER TABLE providers DROP COLUMN codex_options;
                 ALTER TABLE providers DROP COLUMN settings_config;
                 DROP TABLE app_settings;
                 PRAGMA user_version = 6;",
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.providers().unwrap(), [provider]);
        assert_eq!(store.models().unwrap(), [model]);
        assert_eq!(store.requests(1).unwrap()[0].id, "v6-request");
        assert_eq!(store.provider_codex_options("primary").unwrap(), "");
        assert_eq!(store.common_codex_config().unwrap(), "");
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v5_migration_preserves_data_and_supports_atomic_independent_mappings() {
        let path = std::env::temp_dir().join(format!(
            "switchx-multi-model-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        for id in ["primary", "backup"] {
            store
                .put_provider(&ProviderRecord {
                    id: id.into(),
                    name: id.into(),
                    base_url: "https://example.invalid/v1".into(),
                    model_id: "first-model".into(),
                    credential_ref: Some(id.into()),
                })
                .unwrap();
        }
        let first = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-first".into(),
            display_name: "First".into(),
            upstream_model: "first-model".into(),
            metadata: "retained metadata".into(),
            enabled: false,
            fallback_provider_id: Some("backup".into()),
        };
        store.put_model(&first).unwrap();
        store.connection.execute_batch(
            "INSERT INTO request_records (id, started_at_ms, generation, duration_ms, status) VALUES ('old-request', 1, 'old-version', 10, 'completed');
             CREATE TABLE old_models (
                provider_id TEXT PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
                public_id TEXT UNIQUE NOT NULL, display_name TEXT NOT NULL, upstream_model TEXT NOT NULL,
                metadata TEXT NOT NULL, enabled INTEGER NOT NULL,
                fallback_provider_id TEXT REFERENCES providers(id) ON DELETE SET NULL CHECK (fallback_provider_id != provider_id));
             INSERT INTO old_models SELECT provider_id, public_id, display_name, upstream_model, metadata, enabled, fallback_provider_id FROM published_models;
             DROP TABLE published_models;
             ALTER TABLE old_models RENAME TO published_models;
             ALTER TABLE providers DROP COLUMN codex_options;
             ALTER TABLE providers DROP COLUMN settings_config;
             DROP TABLE app_settings;
             PRAGMA user_version = 5;"
        ).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.models().unwrap(), std::slice::from_ref(&first));
        assert_eq!(store.requests(1).unwrap()[0].id, "old-request");
        assert_eq!(
            store.providers().unwrap()[1].credential_ref.as_deref(),
            Some("primary")
        );
        let second = ModelRecord {
            public_id: "sx-second".into(),
            upstream_model: "second-model".into(),
            enabled: true,
            fallback_provider_id: None,
            ..first.clone()
        };
        store.put_model(&second).unwrap();
        let before = store.models().unwrap();
        assert!(
            store
                .replace_model(
                    Some("sx-first"),
                    &ModelRecord {
                        public_id: "sx-second".into(),
                        ..first.clone()
                    }
                )
                .is_err()
        );
        assert_eq!(store.models().unwrap(), before);
        let renamed = ModelRecord {
            public_id: "sx-renamed".into(),
            ..first.clone()
        };
        store.replace_model(Some("sx-first"), &renamed).unwrap();
        assert_eq!(store.models().unwrap(), [renamed, second]);
        store.delete_provider("backup").unwrap();
        assert!(
            store
                .models()
                .unwrap()
                .iter()
                .all(|model| model.fallback_provider_id.is_none())
        );
        store.delete_provider("primary").unwrap();
        assert!(store.models().unwrap().is_empty());
        assert_eq!(store.requests(1).unwrap()[0].id, "old-request");
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_only_inspection_cannot_write_create_or_migrate() {
        let path = std::env::temp_dir().join(format!(
            "switchx-read-only-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        assert!(Store::open_read_only(&path).is_err());
        assert!(!path.exists());
        let store = Store::open(&path).unwrap();
        let provider = ProviderRecord {
            id: "read-only".into(),
            name: "Read only".into(),
            base_url: "https://example.invalid".into(),
            model_id: "model".into(),
            credential_ref: None,
        };
        store.put_provider(&provider).unwrap();
        drop(store);
        let original = std::fs::read(&path).unwrap();
        let store = Store::open_read_only(&path).unwrap();
        assert_eq!(store.providers().unwrap(), std::slice::from_ref(&provider));
        assert!(store.put_provider(&provider).is_err());
        assert!(store.delete_provider(&provider.id).is_err());
        drop(store);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 4;")
            .unwrap();
        let old = std::fs::read(&path).unwrap();
        assert!(Store::open_read_only(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), old);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v4_migration_preserves_rows_and_fallback_references_clear_on_deletion() {
        let path = std::env::temp_dir().join(format!(
            "switchx-fallback-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        for id in ["primary", "backup"] {
            store
                .put_provider(&ProviderRecord {
                    id: id.into(),
                    name: id.into(),
                    base_url: "https://example.invalid".into(),
                    model_id: "shared".into(),
                    credential_ref: None,
                })
                .unwrap();
        }
        let mut model = ModelRecord {
            provider_id: "primary".into(),
            public_id: "sx-primary".into(),
            display_name: "Primary".into(),
            upstream_model: "shared".into(),
            metadata: "{}".into(),
            enabled: true,
            fallback_provider_id: None,
        };
        store.put_model(&model).unwrap();
        store
            .connection
            .execute_batch(
                "INSERT INTO request_records (id, started_at_ms, generation, duration_ms, status)
                    VALUES ('v4-request', 1, 'old-generation', 3, 'completed');
                 ALTER TABLE published_models DROP COLUMN fallback_provider_id;
                 ALTER TABLE request_records DROP COLUMN fallback_from;
                 ALTER TABLE providers DROP COLUMN codex_options;
                 ALTER TABLE providers DROP COLUMN settings_config;
                 DROP TABLE app_settings;
                 PRAGMA user_version = 4;",
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.models().unwrap(), [model.clone()]);
        let old_request = store.requests(1).unwrap().remove(0);
        assert_eq!(old_request.id, "v4-request");
        assert_eq!(old_request.status, RequestStatus::Completed);
        assert!(old_request.fallback_from.is_none());
        model.fallback_provider_id = Some("primary".into());
        assert!(store.put_model(&model).is_err());
        model.fallback_provider_id = Some("missing".into());
        assert!(store.put_model(&model).is_err());
        model.fallback_provider_id = Some("backup".into());
        store.put_model(&model).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.models().unwrap(), [model.clone()]);
        store.delete_provider("backup").unwrap();
        model.fallback_provider_id = None;
        assert_eq!(store.models().unwrap(), [model]);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v3_migration_keeps_history_after_reopen_and_provider_deletion() {
        let path = std::env::temp_dir().join(format!(
            "switchx-requests-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute_batch("DROP TABLE request_records; ALTER TABLE published_models DROP COLUMN fallback_provider_id; ALTER TABLE providers DROP COLUMN codex_options; ALTER TABLE providers DROP COLUMN settings_config; DROP TABLE app_settings; PRAGMA user_version = 3;")
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let provider = ProviderRecord {
            id: "one".into(),
            name: "One".into(),
            base_url: "https://example.invalid".into(),
            model_id: "model".into(),
            credential_ref: None,
        };
        store.put_provider(&provider).unwrap();
        let mut record = RequestRecord {
            id: "first".into(),
            started_at_ms: 1_800_000_000_000,
            public_model: Some("sx-one".into()),
            provider_id: Some("one".into()),
            upstream_model: Some("model".into()),
            generation: "test-generation".into(),
            http_status: Some(200),
            headers_ms: Some(10),
            first_event_ms: Some(20),
            duration_ms: 50,
            status: RequestStatus::Completed,
            error_code: None,
            fallback_from: None,
        };
        store.put_request(&record).unwrap();
        record.id = "second".into();
        record.started_at_ms += 10;
        record.status = RequestStatus::Interrupted;
        record.error_code = Some("missing_completion".into());
        store.put_request(&record).unwrap();
        store.delete_provider("one").unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.requests(1).unwrap(), [record]);
        let rows = store.requests(100).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].id, "first");
        assert_eq!(store.request_time(rows[1].started_at_ms).unwrap().len(), 19);
        assert!(store.requests(0).unwrap().is_empty());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn provider_metadata_survives_reopen_without_loading_secret_json() {
        let path = std::env::temp_dir().join(format!(
            "switchx-storage-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let provider = ProviderRecord {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            base_url: "https://api.deepseek.com/".into(),
            model_id: "deepseek-flash".into(),
            credential_ref: Some("deepseek-key-ref".into()),
        };
        {
            let store = Store::open(&path).unwrap();
            store.put_provider(&provider).unwrap();
            store
                .put_provider(&ProviderRecord {
                    name: "DeepSeek Updated".into(),
                    ..provider.clone()
                })
                .unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let rows = store.providers().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].name, "DeepSeek Updated");
            assert_eq!(rows[0].credential_ref.as_deref(), Some("deepseek-key-ref"));
            let schema: String = store
                .connection
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE name = 'providers'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!schema.contains("api_key"));
            assert!(!schema.contains("password"));
            assert!(store.delete_provider("deepseek").unwrap());
            assert!(store.providers().unwrap().is_empty());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn migrates_existing_provider_metadata_without_losing_rows() {
        let path = std::env::temp_dir().join(format!(
            "switchx-storage-migration-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE providers (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, base_url TEXT NOT NULL, credential_ref TEXT);
                 INSERT INTO providers VALUES ('old', 'Old', 'https://example.invalid/v1', 'old-ref');
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);
        let store = Store::open(&path).unwrap();
        let rows = store.providers().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model_id, "");
        assert_eq!(rows[0].credential_ref.as_deref(), Some("old-ref"));
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v2_migration_persists_unique_model_bindings_and_cascades_provider_deletion() {
        let path = std::env::temp_dir().join(format!(
            "switchx-models-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE providers (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, base_url TEXT NOT NULL, model_id TEXT NOT NULL, credential_ref TEXT);
             INSERT INTO providers VALUES ('one', 'One', 'https://example.invalid/v1', 'same-model', NULL);
             INSERT INTO providers VALUES ('two', 'Two', 'https://example.invalid/v1', 'same-model', NULL);
             PRAGMA user_version = 2;",
        ).unwrap();
        drop(connection);
        let store = Store::open(&path).unwrap();
        let model = ModelRecord {
            provider_id: "one".into(),
            public_id: "sx-one".into(),
            display_name: "One".into(),
            upstream_model: "same-model".into(),
            metadata: "{}".into(),
            enabled: true,
            fallback_provider_id: None,
        };
        store.put_model(&model).unwrap();
        assert!(
            store
                .put_model(&ModelRecord {
                    provider_id: "two".into(),
                    ..model.clone()
                })
                .is_err()
        );
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.models().unwrap(), [model]);
        store.delete_provider("one").unwrap();
        assert!(store.models().unwrap().is_empty());
        assert_eq!(store.providers().unwrap().len(), 1);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
