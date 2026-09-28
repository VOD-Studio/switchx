use std::path::Path;

use rusqlite::{Connection, OpenFlags, Result, params};

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
        if version != 5 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(Self { connection })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > 5 {
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
        self.connection.execute(
            "INSERT INTO providers (id, name, base_url, model_id, credential_ref)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                base_url = excluded.base_url,
                model_id = excluded.model_id,
                credential_ref = excluded.credential_ref",
            params![
                provider.id,
                provider.name,
                provider.base_url,
                provider.model_id,
                provider.credential_ref
            ],
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
        self.connection.execute(
            "INSERT INTO published_models (provider_id, public_id, display_name, upstream_model, metadata, enabled, fallback_provider_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(provider_id) DO UPDATE SET
                public_id = excluded.public_id,
                display_name = excluded.display_name,
                upstream_model = excluded.upstream_model,
                metadata = excluded.metadata,
                enabled = excluded.enabled,
                fallback_provider_id = excluded.fallback_provider_id",
            params![model.provider_id, model.public_id, model.display_name, model.upstream_model, model.metadata, model.enabled, model.fallback_provider_id],
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

#[cfg(test)]
mod tests {
    use super::*;

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
            ALTER TABLE request_records DROP COLUMN fallback_from; PRAGMA user_version = 4;",
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
            .execute_batch("DROP TABLE request_records; ALTER TABLE published_models DROP COLUMN fallback_provider_id; PRAGMA user_version = 3;")
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
    fn provider_metadata_survives_reopen_without_secret_columns() {
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
