use std::{path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, Result, params};

use crate::credentials::Secret;

const SCHEMA_VERSION: i64 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    ApiKey,
    Chatgpt,
    XaiOAuth,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Chatgpt => "chatgpt",
            Self::XaiOAuth => "xai_oauth",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountBinding {
    Native,
    Default,
    Fixed(String),
}

impl AccountBinding {
    pub fn encode(&self) -> Result<String> {
        match self {
            Self::Native => Ok("native".into()),
            Self::Default => Ok("default".into()),
            Self::Fixed(id) if valid_account_id(id) => Ok(format!("fixed:{id}")),
            Self::Fixed(_) => Err(rusqlite::Error::InvalidQuery),
        }
    }

    pub fn decode(value: &str) -> Result<Self> {
        match value {
            "native" => Ok(Self::Native),
            "default" => Ok(Self::Default),
            _ => value
                .strip_prefix("fixed:")
                .filter(|id| valid_account_id(id))
                .map(|id| Self::Fixed(id.into()))
                .ok_or(rusqlite::Error::InvalidQuery),
        }
    }
}

fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model_id: String,
    // Historical metadata only; provider API keys are read from settings_config.
    pub credential_ref: Option<String>,
    pub kind: ProviderKind,
    pub account_binding: Option<AccountBinding>,
    pub icon_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionAuthContext {
    pub provider_id: String,
    pub account_id: Option<String>,
    pub workspace_id: Option<String>,
    pub backend_origin: Option<String>,
    pub routing_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
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
    /// Old recovery journals must be settled before changing their database schema.
    pub fn needs_recovery_before_migration(path: &Path) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let parent = path.parent().ok_or(rusqlite::Error::InvalidQuery)?;
        Ok(version < SCHEMA_VERSION
            && ["direct-journal.json", "switch-journal.json"]
                .iter()
                .any(|name| parent.join(name).exists()))
    }

    /// Only the stable credential columns are available through this opener.
    pub fn open_credentials_read_only(path: &Path) -> Result<Self> {
        Self::open_compatible(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    }

    /// Recovery may delete its own token without creating or migrating the database.
    pub fn open_recovery(path: &Path) -> Result<Self> {
        Self::open_compatible(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    fn open_compatible(path: &Path, flags: OpenFlags) -> Result<Self> {
        let connection = Connection::open_with_flags(path, flags)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if !matches!(version, 8 | 9 | 10 | SCHEMA_VERSION) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        connection.prepare("SELECT settings_config FROM providers LIMIT 0")?;
        connection.prepare("SELECT key, value FROM app_settings LIMIT 0")?;
        Ok(Self { connection })
    }

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
        if Self::needs_recovery_before_migration(path)? {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
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
        if version < 9 {
            use rusqlite::OptionalExtension;
            let legacy: Option<String> = connection
                .query_row(
                    "SELECT value FROM app_settings WHERE key = 'chatgpt_account_binding'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let binding = match legacy.as_deref() {
                None => AccountBinding::Native,
                Some("default") => AccountBinding::Default,
                Some(id) => AccountBinding::Fixed(id.into()),
            }
            .encode()?;
            let transaction = connection.unchecked_transaction()?;
            transaction.execute_batch(
                "ALTER TABLE providers ADD COLUMN kind TEXT NOT NULL DEFAULT 'api_key'
                    CHECK (kind IN ('api_key', 'chatgpt'));
                 ALTER TABLE providers ADD COLUMN account_binding TEXT
                    CHECK ((kind = 'api_key' AND account_binding IS NULL)
                        OR (kind = 'chatgpt' AND account_binding IS NOT NULL));
                 CREATE TABLE session_bindings (
                    session_id TEXT PRIMARY KEY NOT NULL,
                    context TEXT NOT NULL
                 );",
            )?;
            let migrated = transaction.execute(
                "UPDATE providers SET kind = 'chatgpt', account_binding = ?1 WHERE id = 'switchx-chatgpt'",
                [&binding],
            )?;
            if migrated != 0 {
                transaction.execute(
                    "DELETE FROM app_settings WHERE key = 'chatgpt_account_binding'",
                    [],
                )?;
            }
            transaction.pragma_update(None, "user_version", 9)?;
            transaction.commit()?;
        }
        if version < 10 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE providers ADD COLUMN icon_id TEXT;
                 PRAGMA user_version = 10;
                 COMMIT;",
            )?;
        }
        if version < 11 {
            // Rebuild with foreign keys disabled outside the transaction; keep every mapping,
            // setting and existing credential byte while broadening the provider CHECK.
            connection.execute_batch("PRAGMA foreign_keys = OFF;")?;
            let migration = (|| {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute_batch(
                    "CREATE TABLE providers_v11 (
                        id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, base_url TEXT NOT NULL,
                        model_id TEXT NOT NULL DEFAULT '', credential_ref TEXT,
                        codex_options TEXT NOT NULL DEFAULT '', settings_config TEXT NOT NULL DEFAULT '{}',
                        kind TEXT NOT NULL DEFAULT 'api_key' CHECK (kind IN ('api_key', 'chatgpt', 'xai_oauth')),
                        account_binding TEXT CHECK ((kind = 'api_key' AND account_binding IS NULL)
                            OR (kind = 'chatgpt' AND account_binding IS NOT NULL)
                            OR (kind = 'xai_oauth' AND account_binding IS NOT NULL AND account_binding != 'native')),
                        icon_id TEXT
                    );
                    INSERT INTO providers_v11 SELECT id, name, base_url, model_id, credential_ref,
                        codex_options, settings_config, kind, account_binding, icon_id FROM providers;
                    DROP TABLE providers;
                    ALTER TABLE providers_v11 RENAME TO providers;"
                )?;
                let violations: i64 = transaction.query_row(
                    "SELECT count(*) FROM pragma_foreign_key_check",
                    [],
                    |row| row.get(0),
                )?;
                if violations != 0 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                transaction.pragma_update(None, "user_version", 11)?;
                transaction.commit()
            })();
            connection.execute_batch("PRAGMA foreign_keys = ON;")?;
            migration?;
        }
        Ok(Self { connection })
    }

    /// The first request atomically pins a root session; later requests must match.
    pub fn bind_session(
        &self,
        session_id: &str,
        context: &SessionAuthContext,
        allow_new: bool,
    ) -> Result<bool> {
        use rusqlite::OptionalExtension;
        let context = serde_json::to_string(context).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let transaction = self.connection.unchecked_transaction()?;
        if allow_new {
            transaction.execute(
                "INSERT INTO session_bindings (session_id, context) VALUES (?1, ?2)
                 ON CONFLICT(session_id) DO NOTHING",
                params![session_id, context],
            )?;
        }
        let stored: Option<String> = transaction
            .query_row(
                "SELECT context FROM session_bindings WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        let matches = stored.as_deref() == Some(context.as_str());
        transaction.commit()?;
        Ok(matches)
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
        let binding = match (provider.kind, &provider.account_binding) {
            (ProviderKind::ApiKey, None) => None,
            (ProviderKind::Chatgpt | ProviderKind::XaiOAuth, Some(binding))
                if !(provider.kind == ProviderKind::XaiOAuth
                    && *binding == AccountBinding::Native)
                    && key.is_none()
                    && !require_existing_key
                    && provider.credential_ref.is_none() =>
            {
                Some(binding.encode()?)
            }
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        let transaction = self.connection.unchecked_transaction()?;
        use rusqlite::OptionalExtension;
        let saved_kind: Option<String> = transaction
            .query_row(
                "SELECT kind FROM providers WHERE id = ?1",
                [&provider.id],
                |row| row.get(0),
            )
            .optional()?;
        if saved_kind
            .as_deref()
            .is_some_and(|kind| kind != provider.kind.as_str())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // A legacy binding with no old provider survives until that provider is first created.
        let binding = if provider.id == "switchx-chatgpt" && provider.kind == ProviderKind::Chatgpt
        {
            let pending: Option<String> = transaction
                .query_row(
                    "SELECT value FROM app_settings WHERE key = 'chatgpt_account_binding'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(pending) = pending {
                let binding = if pending == "default" {
                    AccountBinding::Default
                } else {
                    AccountBinding::Fixed(pending)
                }
                .encode()?;
                transaction.execute(
                    "DELETE FROM app_settings WHERE key = 'chatgpt_account_binding'",
                    [],
                )?;
                Some(binding)
            } else {
                binding
            }
        } else {
            binding
        };
        transaction.execute(
            "INSERT INTO providers (id, name, base_url, model_id, credential_ref, kind, account_binding, icon_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                base_url = excluded.base_url,
                model_id = excluded.model_id,
                kind = excluded.kind,
                account_binding = excluded.account_binding,
                icon_id = excluded.icon_id,
                credential_ref = CASE WHEN json_valid(providers.settings_config)
                    THEN CASE WHEN json_type(providers.settings_config, '$.auth.OPENAI_API_KEY') = 'text'
                        THEN NULL ELSE excluded.credential_ref END
                    ELSE excluded.credential_ref END",
            params![
                provider.id,
                provider.name,
                provider.base_url,
                provider.model_id,
                provider.credential_ref,
                provider.kind.as_str(),
                binding,
                provider.icon_id
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

    /// Invalid credential JSON is an error, never a missing credential.
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

    pub fn provider_codex_options(&self, id: &str) -> Result<String> {
        self.connection.query_row(
            "SELECT codex_options FROM providers WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
    }

    pub fn put_local_token(&self, reference: &str, token: &Secret) -> Result<()> {
        let key = local_token_key(reference)?;
        validate_local_token(token.expose())?;
        self.connection.execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, token.expose()],
        )?;
        Ok(())
    }

    pub fn local_token(&self, reference: &str) -> Result<Option<Secret>> {
        use rusqlite::OptionalExtension;
        let key = local_token_key(reference)?;
        let token: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        token
            .map(|token| {
                let token = Secret::new(token);
                validate_local_token(token.expose())?;
                Ok(token)
            })
            .transpose()
    }

    pub fn delete_local_token(&self, reference: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM app_settings WHERE key = ?1",
            [local_token_key(reference)?],
        )?;
        Ok(())
    }

    /// The subscription provider's explicit account binding; absent follows native Codex login.
    /// `default` resolves the managed account default when preparing a new route.
    pub fn chatgpt_account_binding(&self) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        if let Some(provider) = self.provider("switchx-chatgpt")? {
            return Ok(match provider.account_binding {
                Some(AccountBinding::Native) => None,
                Some(AccountBinding::Default) => Some("default".into()),
                Some(AccountBinding::Fixed(id)) => Some(id),
                None => return Err(rusqlite::Error::InvalidQuery),
            });
        }
        self.connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'chatgpt_account_binding'",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn bind_chatgpt_account(&self, account: Option<&str>) -> Result<()> {
        let binding = match account {
            None => AccountBinding::Native,
            Some("default") => AccountBinding::Default,
            Some(id) => AccountBinding::Fixed(id.into()),
        };
        let encoded = binding.encode()?;
        if self.provider("switchx-chatgpt")?.is_some() {
            self.connection.execute(
                "UPDATE providers SET account_binding = ?1 WHERE id = 'switchx-chatgpt' AND kind = 'chatgpt'",
                [encoded],
            )?;
            return Ok(());
        }
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
            "SELECT id, name, base_url, model_id, credential_ref, kind, account_binding, icon_id FROM providers ORDER BY name, id",
        )?;
        statement.query_map([], provider_from_row)?.collect()
    }

    pub fn provider(&self, id: &str) -> Result<Option<ProviderRecord>> {
        use rusqlite::OptionalExtension;
        self.connection
            .query_row(
                "SELECT id, name, base_url, model_id, credential_ref, kind, account_binding, icon_id FROM providers WHERE id = ?1",
                [id],
                provider_from_row,
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

    pub fn set_models_enabled(&self, public_ids: &[String], enabled: bool) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        for public_id in public_ids {
            if transaction.execute(
                "UPDATE published_models SET enabled = ?1 WHERE public_id = ?2",
                params![enabled, public_id],
            )? != 1
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        transaction.commit()
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

    /// Inserts new mappings in one transaction; an existing public ID or
    /// provider/upstream pair rejects the whole batch.
    pub fn add_models(&self, models: &[ModelRecord]) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        for model in models {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM published_models
                 WHERE public_id = ?1 OR (provider_id = ?2 AND upstream_model = ?3))",
                params![model.public_id, model.provider_id, model.upstream_model],
                |row| row.get(0),
            )?;
            if exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Self::write_model(&transaction, model)?;
        }
        transaction.commit()
    }

    /// Replace one connection's directory atomically, without overwriting changes
    /// made since the editor opened (including workbench selections).
    pub fn replace_provider_models(
        &self,
        provider_id: &str,
        expected: &[ModelRecord],
        models: &[ModelRecord],
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let current = self.models()?;
        let current: Vec<_> = current
            .into_iter()
            .filter(|model| model.provider_id == provider_id)
            .collect();
        if current != expected || models.iter().any(|model| model.provider_id != provider_id) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        transaction.execute(
            "DELETE FROM published_models WHERE provider_id = ?1",
            [provider_id],
        )?;
        for model in models {
            // Do not let a draft claim another connection's public ID.
            let exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM published_models WHERE public_id = ?1)",
                [&model.public_id],
                |row| row.get(0),
            )?;
            if exists {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Self::write_model(&transaction, model)?;
        }
        transaction.commit()
    }

    pub fn delete_model(&self, public_id: &str) -> Result<bool> {
        Ok(self.connection.execute(
            "DELETE FROM published_models WHERE public_id = ?1",
            [public_id],
        )? != 0)
    }

    pub fn deselect_subscription_models(&self) -> Result<()> {
        self.connection.execute(
            "UPDATE published_models SET enabled = 0 WHERE provider_id IN
                (SELECT id FROM providers WHERE kind != 'api_key')",
            [],
        )?;
        Ok(())
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

fn provider_from_row(row: &rusqlite::Row<'_>) -> Result<ProviderRecord> {
    let kind: String = row.get(5)?;
    let kind = match kind.as_str() {
        "api_key" => ProviderKind::ApiKey,
        "chatgpt" => ProviderKind::Chatgpt,
        "xai_oauth" => ProviderKind::XaiOAuth,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let binding: Option<String> = row.get(6)?;
    let account_binding = match (kind, binding) {
        (ProviderKind::ApiKey, None) => None,
        (ProviderKind::Chatgpt | ProviderKind::XaiOAuth, Some(binding)) => {
            let binding = AccountBinding::decode(&binding)?;
            if kind == ProviderKind::XaiOAuth && binding == AccountBinding::Native {
                return Err(rusqlite::Error::InvalidQuery);
            }
            Some(binding)
        }
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(ProviderRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        base_url: row.get(2)?,
        model_id: row.get(3)?,
        credential_ref: row.get(4)?,
        kind,
        account_binding,
        icon_id: row.get(7)?,
    })
}

fn local_token_key(reference: &str) -> Result<String> {
    if !reference
        .strip_prefix("router-")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(format!("local_token:{reference}"))
}

fn validate_local_token(token: &str) -> Result<()> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
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

    fn downgrade_to_v8(store: &Store) {
        store
            .connection
            .execute_batch(
                "ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding;
             ALTER TABLE providers DROP COLUMN kind;
             DROP TABLE session_bindings;
             PRAGMA user_version = 8;",
            )
            .unwrap();
    }

    #[test]
    fn v10_migration_preserves_credentials_mappings_fallbacks_and_session_contexts() {
        let path = std::env::temp_dir().join(format!(
            "switchx-v10-xai-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let primary = ProviderRecord {
            credential_ref: None,
            ..api_provider()
        };
        let mut backup = primary.clone();
        backup.id = "backup".into();
        let key = Secret::new("synthetic-v10-key".into());
        store
            .put_provider_with_models_options_and_key(&primary, &[], Some("saved-options"), &key)
            .unwrap();
        store.put_provider(&backup).unwrap();
        let model = ModelRecord {
            provider_id: primary.id.clone(),
            public_id: "v10-model".into(),
            display_name: "Fixture".into(),
            upstream_model: "same-model".into(),
            metadata: "saved-metadata".into(),
            enabled: true,
            fallback_provider_id: Some(backup.id.clone()),
        };
        store.put_model(&model).unwrap();
        store.connection.execute_batch(r#"INSERT INTO session_bindings VALUES ('root', '{"provider_id":"legacy","account_id":null,"workspace_id":null,"backend_origin":null,"routing_override":null}'); PRAGMA foreign_keys = OFF;"#).unwrap();
        // Genuine v10 checks reject xai_oauth until the transactional rebuild.
        store.connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE old_providers (
                id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL, base_url TEXT NOT NULL,
                model_id TEXT NOT NULL DEFAULT '', credential_ref TEXT,
                codex_options TEXT NOT NULL DEFAULT '', settings_config TEXT NOT NULL DEFAULT '{}',
                kind TEXT NOT NULL DEFAULT 'api_key' CHECK (kind IN ('api_key', 'chatgpt')),
                account_binding TEXT CHECK ((kind = 'api_key' AND account_binding IS NULL) OR (kind = 'chatgpt' AND account_binding IS NOT NULL)), icon_id TEXT);
             INSERT INTO old_providers SELECT * FROM providers;
             DROP TABLE providers;
             ALTER TABLE old_providers RENAME TO providers;
             PRAGMA user_version = 10;
             COMMIT;"
        ).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.provider(&primary.id).unwrap(), Some(primary.clone()));
        assert_eq!(store.models().unwrap(), vec![model]);
        assert_eq!(
            store
                .provider_api_key(&primary.id)
                .unwrap()
                .unwrap()
                .expose(),
            key.expose()
        );
        assert_eq!(
            store.provider_codex_options(&primary.id).unwrap(),
            "saved-options"
        );
        let context: String = store
            .connection
            .query_row(
                "SELECT context FROM session_bindings WHERE session_id='root'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            serde_json::from_str::<SessionAuthContext>(&context)
                .unwrap()
                .upstream_model
                .is_none()
        );
        let grok = ProviderRecord {
            id: "grok".into(),
            name: "Grok".into(),
            base_url: crate::xai::BASE_URL.into(),
            model_id: "grok-4.5".into(),
            credential_ref: None,
            kind: ProviderKind::XaiOAuth,
            account_binding: Some(AccountBinding::Default),
            icon_id: None,
        };
        store.put_provider(&grok).unwrap();
        assert_eq!(store.provider("grok").unwrap(), Some(grok));
        store.delete_provider("backup").unwrap();
        assert!(store.models().unwrap()[0].fallback_provider_id.is_none());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v9_icon_migration_and_credentials_readers_preserve_saved_data() {
        let path = std::env::temp_dir().join(format!(
            "switchx-icon-migration-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let provider = ProviderRecord {
            credential_ref: None,
            ..api_provider()
        };
        let key = Secret::new("synthetic-icon-api-key".into());
        let reference = format!("router-{}", "a".repeat(32));
        let token = Secret::new("b".repeat(64));
        let store = Store::open(&path).unwrap();
        store
            .put_provider_with_models_options_and_key(&provider, &[], Some("saved-options"), &key)
            .unwrap();
        store.put_local_token(&reference, &token).unwrap();
        store
            .connection
            .execute_batch("ALTER TABLE providers DROP COLUMN icon_id; PRAGMA user_version = 9;")
            .unwrap();
        drop(store);
        for expected_version in [9, SCHEMA_VERSION] {
            let before = std::fs::read(&path).unwrap();
            let readonly = Store::open_credentials_read_only(&path).unwrap();
            assert_eq!(
                readonly
                    .provider_api_key(&provider.id)
                    .unwrap()
                    .unwrap()
                    .expose(),
                key.expose()
            );
            assert_eq!(
                readonly.local_token(&reference).unwrap().unwrap().expose(),
                token.expose()
            );
            assert_eq!(
                readonly
                    .connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                expected_version
            );
            drop(readonly);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let recovery = Store::open_recovery(&path).unwrap();
            assert_eq!(
                recovery.local_token(&reference).unwrap().unwrap().expose(),
                token.expose()
            );
            drop(recovery);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let store = Store::open(&path).unwrap();
            assert_eq!(
                store.provider(&provider.id).unwrap(),
                Some(provider.clone())
            );
            assert_eq!(
                store.provider_codex_options(&provider.id).unwrap(),
                "saved-options"
            );
            assert_eq!(
                store
                    .provider_api_key(&provider.id)
                    .unwrap()
                    .unwrap()
                    .expose(),
                key.expose()
            );
            assert_eq!(
                store
                    .connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
        }
        let store = Store::open(&path).unwrap();
        let mut custom = provider;
        custom.icon_id = Some("openai".into());
        store.put_provider(&custom).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.providers().unwrap(), [custom.clone()]);
        custom.icon_id = Some("deepseek".into());
        store
            .update_provider_preserving_key(&custom, &[], None)
            .unwrap();
        assert_eq!(store.provider(&custom.id).unwrap(), Some(custom.clone()));
        custom.icon_id = None;
        store.put_provider(&custom).unwrap();
        assert_eq!(store.provider(&custom.id).unwrap(), Some(custom.clone()));
        assert_eq!(
            store
                .provider_api_key(&custom.id)
                .unwrap()
                .unwrap()
                .expose(),
            key.expose()
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn v8_binding_migration_preserves_legacy_models_and_consumes_only_once() {
        for binding in [
            AccountBinding::Native,
            AccountBinding::Default,
            AccountBinding::Fixed("saved-a".into()),
        ] {
            let path = std::env::temp_dir().join(format!(
                "switchx-binding-migration-{}.sqlite",
                crate::app::new_id().unwrap()
            ));
            let store = Store::open(&path).unwrap();
            let provider = ProviderRecord {
                id: "switchx-chatgpt".into(),
                name: "Legacy".into(),
                base_url: crate::chatgpt::BASE_URL.into(),
                model_id: "same-model".into(),
                credential_ref: None,
                kind: ProviderKind::Chatgpt,
                account_binding: Some(AccountBinding::Native),
                icon_id: None,
            };
            let model = ModelRecord {
                provider_id: provider.id.clone(),
                public_id: "sx-chatgpt-same-model".into(),
                display_name: "Legacy model".into(),
                upstream_model: "same-model".into(),
                metadata: "{}".into(),
                enabled: true,
                fallback_provider_id: None,
            };
            store
                .put_provider_with_models(&provider, std::slice::from_ref(&model))
                .unwrap();
            downgrade_to_v8(&store);
            let legacy = match &binding {
                AccountBinding::Native => None,
                AccountBinding::Default => Some("default"),
                AccountBinding::Fixed(id) => Some(id.as_str()),
            };
            if let Some(value) = legacy {
                store
                    .connection
                    .execute(
                        "INSERT INTO app_settings VALUES ('chatgpt_account_binding', ?1)",
                        [value],
                    )
                    .unwrap();
            }
            drop(store);
            let store = Store::open(&path).unwrap();
            assert_eq!(
                store
                    .provider(&provider.id)
                    .unwrap()
                    .unwrap()
                    .account_binding,
                Some(binding.clone())
            );
            assert_eq!(store.models().unwrap(), [model]);
            assert!(!store.connection.query_row("SELECT EXISTS(SELECT 1 FROM app_settings WHERE key='chatgpt_account_binding')", [], |row| row.get::<_,bool>(0)).unwrap());
            drop(store);
            assert_eq!(
                Store::open(&path)
                    .unwrap()
                    .provider(&provider.id)
                    .unwrap()
                    .unwrap()
                    .account_binding,
                Some(binding)
            );
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn pending_legacy_binding_is_consumed_atomically_on_first_old_connection() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        store.bind_chatgpt_account(Some("saved-a")).unwrap();
        let provider = ProviderRecord {
            id: "switchx-chatgpt".into(),
            name: "Legacy".into(),
            base_url: crate::chatgpt::BASE_URL.into(),
            model_id: "same-model".into(),
            credential_ref: None,
            kind: ProviderKind::Chatgpt,
            account_binding: Some(AccountBinding::Native),
            icon_id: None,
        };
        let wrong_model = ModelRecord {
            provider_id: "missing".into(),
            public_id: "sx-test".into(),
            display_name: "Test".into(),
            upstream_model: "same-model".into(),
            metadata: "{}".into(),
            enabled: true,
            fallback_provider_id: None,
        };
        assert!(
            store
                .put_provider_with_models(&provider, &[wrong_model])
                .is_err()
        );
        assert!(store.provider(&provider.id).unwrap().is_none());
        assert_eq!(
            store.chatgpt_account_binding().unwrap().as_deref(),
            Some("saved-a")
        );
        store.put_provider(&provider).unwrap();
        assert_eq!(
            store
                .provider(&provider.id)
                .unwrap()
                .unwrap()
                .account_binding,
            Some(AccountBinding::Fixed("saved-a".into()))
        );
        store.bind_chatgpt_account(None).unwrap();
        store.put_provider(&provider).unwrap();
        assert_eq!(
            store
                .provider(&provider.id)
                .unwrap()
                .unwrap()
                .account_binding,
            Some(AccountBinding::Native)
        );
    }

    #[test]
    fn v8_journals_defer_migration_but_credentials_and_recovery_remain_usable() {
        for journal in ["direct-journal.json", "switch-journal.json"] {
            let directory = std::env::temp_dir().join(format!(
                "switchx-legacy-recovery-{}",
                crate::app::new_id().unwrap()
            ));
            std::fs::create_dir(&directory).unwrap();
            let path = directory.join("switchx.sqlite");
            let store = Store::open(&path).unwrap();
            let provider = api_provider();
            let key = Secret::new("synthetic-api-key".into());
            let reference = format!("router-{}", "a".repeat(32));
            let token = Secret::new("b".repeat(64));
            store
                .put_provider_with_models_options_and_key(&provider, &[], None, &key)
                .unwrap();
            store.put_local_token(&reference, &token).unwrap();
            downgrade_to_v8(&store);
            drop(store);
            std::fs::write(directory.join(journal), b"synthetic recovery marker").unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(Store::needs_recovery_before_migration(&path).unwrap());
            assert!(Store::open(&path).is_err());
            let readonly = Store::open_credentials_read_only(&path).unwrap();
            assert_eq!(
                readonly
                    .provider_api_key(&provider.id)
                    .unwrap()
                    .unwrap()
                    .expose(),
                key.expose()
            );
            assert_eq!(
                readonly.local_token(&reference).unwrap().unwrap().expose(),
                token.expose()
            );
            assert!(readonly.delete_local_token(&reference).is_err());
            drop(readonly);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let recovery = Store::open_recovery(&path).unwrap();
            recovery.delete_local_token(&reference).unwrap();
            assert_eq!(
                recovery
                    .connection
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                8
            );
            drop(recovery);
            std::fs::remove_file(directory.join(journal)).unwrap();
            let store = Store::open(&path).unwrap();
            assert!(store.local_token(&reference).unwrap().is_none());
            assert_eq!(
                store.provider(&provider.id).unwrap().unwrap().kind,
                ProviderKind::ApiKey
            );
            std::fs::remove_dir_all(directory).unwrap();
        }
        let missing = std::env::temp_dir().join(format!(
            "switchx-missing-db-{}",
            crate::app::new_id().unwrap()
        ));
        assert!(Store::open_credentials_read_only(&missing).is_err());
        assert!(Store::open_recovery(&missing).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn invalid_bindings_and_provider_kinds_never_fall_back_to_native() {
        for value in ["", "fixed:", "other", "fixed:bad/value"] {
            assert!(AccountBinding::decode(value).is_err());
        }
        let store = Store::open(Path::new(":memory:")).unwrap();
        let mut provider = api_provider();
        provider.account_binding = Some(AccountBinding::Native);
        assert!(store.put_provider(&provider).is_err());
        provider.account_binding = None;
        provider.kind = ProviderKind::Chatgpt;
        assert!(store.put_provider(&provider).is_err());
        assert!(store.providers().unwrap().is_empty());
    }

    #[test]
    fn persistent_session_context_is_atomic_and_survives_provider_deletion() {
        let path = std::env::temp_dir().join(format!(
            "switchx-session-store-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let a = SessionAuthContext {
            provider_id: "a".into(),
            account_id: Some("saved-a".into()),
            workspace_id: Some("workspace-a".into()),
            backend_origin: Some("https://chatgpt.com".into()),
            routing_override: Some("NO_CONSTRAINT".into()),
            upstream_model: None,
        };
        let mut b = a.clone();
        b.provider_id = "b".into();
        assert!(!store.bind_session("unknown", &a, false).unwrap());
        let (left, right) = std::thread::scope(|scope| {
            let path = &path;
            let a = &a;
            let b = &b;
            let left = scope.spawn(move || {
                Store::open(path)
                    .unwrap()
                    .bind_session("root", a, true)
                    .unwrap()
            });
            let right = scope.spawn(move || {
                Store::open(path)
                    .unwrap()
                    .bind_session("root", b, true)
                    .unwrap()
            });
            (left.join().unwrap(), right.join().unwrap())
        });
        assert_ne!(left, right);
        let winner = if left { &a } else { &b };
        assert!(store.bind_session("root", winner, false).unwrap());
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(store.bind_session("root", winner, false).unwrap());
        assert!(
            !store
                .bind_session("root", if left { &b } else { &a }, true)
                .unwrap()
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn local_tokens_persist_and_delete_only_the_selected_reference() {
        let path = std::env::temp_dir().join(format!(
            "switchx-local-token-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let first = format!("router-{}", "a".repeat(32));
        let second = format!("router-{}", "b".repeat(32));
        let token = Secret::new("a".repeat(64));
        {
            let store = Store::open(&path).unwrap();
            store.put_common_codex_config("keep-config").unwrap();
            store.bind_chatgpt_account(Some("default")).unwrap();
            store.put_local_token(&first, &token).unwrap();
            store
                .put_local_token(&second, &Secret::new("b".repeat(64)))
                .unwrap();
        }
        {
            let store = Store::open_read_only(&path).unwrap();
            assert_eq!(
                store.local_token(&first).unwrap().unwrap().expose(),
                token.expose()
            );
            assert!(!format!("{:?}", store.local_token(&first).unwrap()).contains(token.expose()));
            assert!(store.delete_local_token(&first).is_err());
        }
        let store = Store::open(&path).unwrap();
        store.delete_local_token(&first).unwrap();
        store.delete_local_token(&first).unwrap();
        assert!(store.local_token(&first).unwrap().is_none());
        assert_eq!(
            store.local_token(&second).unwrap().unwrap().expose(),
            "b".repeat(64)
        );
        assert_eq!(store.common_codex_config().unwrap(), "keep-config");
        assert_eq!(
            store.chatgpt_account_binding().unwrap().as_deref(),
            Some("default")
        );
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
    fn local_tokens_reject_invalid_references_and_values_without_exposing_them() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let reference = format!("router-{}", "a".repeat(32));
        let token = Secret::new("a".repeat(64));
        for invalid in [
            "",
            "../outside",
            "router-a",
            &format!("router-{}", "g".repeat(32)),
        ] {
            assert!(store.put_local_token(invalid, &token).is_err());
            assert!(store.local_token(invalid).is_err());
            assert!(store.delete_local_token(invalid).is_err());
        }
        for invalid in [
            String::new(),
            "a".repeat(63),
            "g".repeat(64),
            format!("{}\n", "a".repeat(63)),
        ] {
            assert!(
                store
                    .put_local_token(&reference, &Secret::new(invalid))
                    .is_err()
            );
        }
        assert!(store.local_token(&reference).unwrap().is_none());
        store
            .connection
            .execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)",
                params![
                    format!("local_token:{reference}"),
                    "synthetic-private-invalid-token"
                ],
            )
            .unwrap();
        let error = store.local_token(&reference).unwrap_err().to_string();
        assert!(!error.contains("synthetic-private-invalid-token"));
        store.delete_local_token(&reference).unwrap();
        assert!(store.local_token(&reference).unwrap().is_none());
    }

    fn api_provider() -> ProviderRecord {
        ProviderRecord {
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
    fn provider_edits_preserve_keys_changed_by_another_connection() {
        let path = std::env::temp_dir().join(format!(
            "switchx-api-key-preserve-{}.sqlite",
            crate::app::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let provider = api_provider();
        store
            .put_provider_with_models_options_and_key(
                &provider,
                &[],
                None,
                &Secret::new("synthetic-old-key".into()),
            )
            .unwrap();
        let stale_metadata = store.provider(&provider.id).unwrap().unwrap();
        let other = Store::open(&path).unwrap();
        other
            .put_provider_with_models_options_and_key(
                &provider,
                &[],
                None,
                &Secret::new("synthetic-concurrent-key".into()),
            )
            .unwrap();
        store
            .update_provider_preserving_key(
                &ProviderRecord {
                    name: "Preserved concurrent key".into(),
                    ..stale_metadata
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
        drop(other);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn provider_edits_without_a_saved_key_roll_back_metadata_and_options() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let provider = api_provider();
        store.put_provider(&provider).unwrap();
        assert!(
            store
                .update_provider_preserving_key(
                    &ProviderRecord {
                        name: "Must roll back".into(),
                        ..provider.clone()
                    },
                    &[],
                    Some("must-roll-back"),
                )
                .is_err()
        );
        assert_eq!(
            store.provider(&provider.id).unwrap(),
            Some(provider.clone())
        );
        assert_eq!(store.provider_codex_options(&provider.id).unwrap(), "");
        assert!(store.provider_api_key(&provider.id).unwrap().is_none());
    }

    #[test]
    fn malformed_credential_json_rejects_reads_and_writes_without_leaking_keys() {
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
                "ALTER TABLE providers DROP COLUMN settings_config; ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding; ALTER TABLE providers DROP COLUMN kind; DROP TABLE session_bindings; PRAGMA user_version = 7;",
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
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
                 ALTER TABLE providers DROP COLUMN settings_config; ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding; ALTER TABLE providers DROP COLUMN kind; DROP TABLE session_bindings;
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
                    kind: crate::storage::ProviderKind::ApiKey,
                    account_binding: None,
                    icon_id: None,
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
             ALTER TABLE providers DROP COLUMN settings_config; ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding; ALTER TABLE providers DROP COLUMN kind; DROP TABLE session_bindings;
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
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
                    kind: crate::storage::ProviderKind::ApiKey,
                    account_binding: None,
                    icon_id: None,
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
                 ALTER TABLE providers DROP COLUMN settings_config; ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding; ALTER TABLE providers DROP COLUMN kind; DROP TABLE session_bindings;
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
            .execute_batch("DROP TABLE request_records; ALTER TABLE published_models DROP COLUMN fallback_provider_id; ALTER TABLE providers DROP COLUMN codex_options; ALTER TABLE providers DROP COLUMN settings_config; ALTER TABLE providers DROP COLUMN icon_id; ALTER TABLE providers DROP COLUMN account_binding; ALTER TABLE providers DROP COLUMN kind; DROP TABLE session_bindings; DROP TABLE app_settings; PRAGMA user_version = 3;")
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let provider = ProviderRecord {
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
            kind: crate::storage::ProviderKind::ApiKey,
            account_binding: None,
            icon_id: None,
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
            assert!(!schema.contains("api_key TEXT"));
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
