use std::path::Path;

use rusqlite::{Connection, Result, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model_id: String,
    pub credential_ref: Option<String>,
}

pub struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > 2 {
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
        Ok(Self { connection })
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
