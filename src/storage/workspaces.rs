//! Workspace metadata only: registering or selecting one never writes a client
//! configuration, changes authentication, or mutates the legacy model catalog.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rusqlite::{OptionalExtension, Result, params};

use crate::clients::{ClientKind, ClientWorkspace, WorkspaceKey};

pub(super) const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS client_workspaces (
        id TEXT PRIMARY KEY NOT NULL,
        client TEXT NOT NULL CHECK (client IN ('codex', 'claude_code')),
        config_home TEXT NOT NULL,
        name TEXT NOT NULL,
        last_page INTEGER NOT NULL DEFAULT 0 CHECK (last_page BETWEEN 0 AND 3),
        UNIQUE (client, config_home)
    );
    CREATE TABLE IF NOT EXISTS client_workspace_selections (
        client TEXT PRIMARY KEY NOT NULL CHECK (client IN ('codex', 'claude_code')),
        workspace_id TEXT NOT NULL REFERENCES client_workspaces(id)
    );";

/// SQLite's backup API includes committed WAL contents. Backups contain the
/// same private data as the source and are never exported or logged.
pub(super) fn backup_before_migration(
    source: &rusqlite::Connection,
    path: &Path,
    version: i64,
) -> Result<()> {
    let io_error = |_| rusqlite::Error::InvalidQuery;
    let directory = path
        .parent()
        .ok_or(rusqlite::Error::InvalidQuery)?
        .join("migration-backups");
    if fs::symlink_metadata(&directory).is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink()) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    fs::create_dir_all(&directory).map_err(io_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    let id = crate::id::new_id().map_err(|_| rusqlite::Error::InvalidQuery)?;
    let destination = directory.join(format!("before-clients-v{version}-{id}.sqlite"));
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&destination).map_err(io_error)?;
    let result = (|| {
        let mut target = rusqlite::Connection::open(&destination)?;
        let backup = rusqlite::backup::Backup::new(source, &mut target)?;
        let started = Instant::now();
        loop {
            match backup.step(128)? {
                rusqlite::backup::StepResult::Done => break,
                _ if started.elapsed() >= Duration::from_secs(10) => {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                rusqlite::backup::StepResult::More => {}
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        drop(backup);
        drop(target);
        fs::OpenOptions::new()
            .write(true)
            .open(&destination)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        Ok(())
    })();
    if result.is_err() {
        // Only this attempt's incomplete private file is removed.
        let _ = fs::remove_file(&destination);
    }
    result
}

fn from_row(row: &rusqlite::Row<'_>) -> Result<ClientWorkspace> {
    let client: String = row.get(1)?;
    Ok(ClientWorkspace {
        id: row.get(0)?,
        key: WorkspaceKey {
            client: ClientKind::parse(&client).map_err(|_| rusqlite::Error::InvalidQuery)?,
            home: PathBuf::from(row.get::<_, String>(2)?),
        },
        name: row.get(3)?,
        last_page: row.get(4)?,
    })
}

impl super::Store {
    pub fn client_workspaces(&self) -> Result<Vec<ClientWorkspace>> {
        self.connection
            .prepare("SELECT id, client, config_home, name, last_page FROM client_workspaces ORDER BY client, name, id")?
            .query_map([], from_row)?
            .collect()
    }

    pub fn selected_client_workspace(&self, client: ClientKind) -> Result<Option<ClientWorkspace>> {
        self.connection
            .query_row(
                "SELECT w.id, w.client, w.config_home, w.name, w.last_page
             FROM client_workspaces w JOIN client_workspace_selections s ON s.workspace_id = w.id
             WHERE s.client = ?1 AND w.client = ?1",
                [client.as_str()],
                from_row,
            )
            .optional()
    }

    pub fn remember_client_workspace(
        &self,
        key: &WorkspaceKey,
        name: &str,
    ) -> Result<ClientWorkspace> {
        let key =
            WorkspaceKey::new(key.client, &key.home).map_err(|_| rusqlite::Error::InvalidQuery)?;
        let name = name.trim();
        if name.is_empty() || name.len() > 240 || name.chars().any(char::is_control) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let id = crate::id::new_id().map_err(|_| rusqlite::Error::InvalidQuery)?;
        let home = key.home.to_str().ok_or(rusqlite::Error::InvalidQuery)?;
        self.connection.execute(
            "INSERT INTO client_workspaces (id, client, config_home, name) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(client, config_home) DO NOTHING",
            params![id, key.client.as_str(), home, name],
        )?;
        self.connection.query_row(
            "SELECT id, client, config_home, name, last_page FROM client_workspaces WHERE client = ?1 AND config_home = ?2",
            params![key.client.as_str(), home], from_row,
        )
    }

    pub fn select_client_workspace(&self, client: ClientKind, workspace_id: &str) -> Result<()> {
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM client_workspaces WHERE id = ?1 AND client = ?2)",
            params![workspace_id, client.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute(
            "INSERT INTO client_workspace_selections (client, workspace_id) VALUES (?1, ?2)
             ON CONFLICT(client) DO UPDATE SET workspace_id = excluded.workspace_id",
            params![client.as_str(), workspace_id],
        )?;
        Ok(())
    }

    pub fn save_client_page(&self, workspace_id: &str, page: i32) -> Result<()> {
        if !(0..=3).contains(&page) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let changed = self.connection.execute(
            "UPDATE client_workspaces SET last_page = ?1 WHERE id = ?2",
            params![page, workspace_id],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Store;

    #[test]
    fn workspace_selection_and_navigation_are_isolated_and_persistent() {
        let root = std::env::temp_dir().join(format!(
            "switchx-workspaces-{}",
            crate::id::new_id().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("switchx.sqlite");
        let store = Store::open(&db).unwrap();
        let key = |client| WorkspaceKey::new(client, &root.join("uncreated-home")).unwrap();
        let codex = store
            .remember_client_workspace(&key(ClientKind::Codex), "Codex")
            .unwrap();
        let claude = store
            .remember_client_workspace(&key(ClientKind::ClaudeCode), "Claude")
            .unwrap();
        assert_ne!(codex.id, claude.id);
        assert_eq!(
            store
                .remember_client_workspace(&codex.key, "Other")
                .unwrap(),
            codex
        );
        assert!(
            store
                .select_client_workspace(ClientKind::ClaudeCode, &codex.id)
                .is_err()
        );
        store
            .select_client_workspace(ClientKind::Codex, &codex.id)
            .unwrap();
        store
            .select_client_workspace(ClientKind::ClaudeCode, &claude.id)
            .unwrap();
        store.save_client_page(&codex.id, 3).unwrap();
        store.save_client_page(&claude.id, 1).unwrap();
        assert!(store.save_client_page(&claude.id, 99).is_err());
        assert!(store.save_client_page("missing", 0).is_err());
        drop(store);
        let store = Store::open(&db).unwrap();
        assert_eq!(store.client_workspaces().unwrap().len(), 2);
        assert_eq!(
            store
                .selected_client_workspace(ClientKind::Codex)
                .unwrap()
                .unwrap()
                .last_page,
            3
        );
        assert_eq!(
            store
                .selected_client_workspace(ClientKind::ClaudeCode)
                .unwrap()
                .unwrap()
                .last_page,
            1
        );
        assert!(!root.join("uncreated-home").exists());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn additive_v18_migration_preserves_v17_settings_and_credential_reader() {
        let root =
            std::env::temp_dir().join(format!("switchx-v18-{}", crate::id::new_id().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("switchx.sqlite");
        let store = Store::open(&db).unwrap();
        store.connection.execute_batch("DROP TABLE client_workspace_selections; DROP TABLE client_workspaces;
            INSERT INTO app_settings VALUES ('synthetic-private-setting', 'keep'); PRAGMA user_version = 17;").unwrap();
        drop(store);
        assert!(Store::open_credentials_read_only(&db).is_ok());
        let store = Store::open(&db).unwrap();
        assert!(store.client_workspaces().unwrap().is_empty());
        let value: String = store
            .connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'synthetic-private-setting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "keep");
        assert_eq!(
            store
                .connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            18
        );
        let backups = std::fs::read_dir(root.join("migration-backups"))
            .unwrap()
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(backups.len(), 1);
        let backup = rusqlite::Connection::open_with_flags(
            backups[0].path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            17
        );
        assert_eq!(
            backup
                .query_row(
                    "SELECT value FROM app_settings WHERE key = 'synthetic-private-setting'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "keep"
        );
        drop(backup);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migration_backup_contains_committed_wal_and_has_private_permissions() {
        let root = std::env::temp_dir().join(format!(
            "switchx-wal-backup-{}",
            crate::id::new_id().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("switchx.sqlite");
        let old = Store::open(&db).unwrap();
        old.connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
            DROP TABLE client_workspace_selections; DROP TABLE client_workspaces;
            INSERT INTO app_settings VALUES ('synthetic-wal-value', 'committed'); PRAGMA user_version = 17;").unwrap();
        assert!(root.join("switchx.sqlite-wal").exists());
        let upgraded = Store::open(&db).unwrap();
        let backup_path = std::fs::read_dir(root.join("migration-backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let backup = rusqlite::Connection::open_with_flags(
            &backup_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            17
        );
        assert_eq!(
            backup
                .query_row(
                    "SELECT value FROM app_settings WHERE key='synthetic-wal-value'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "committed"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(root.join("migration-backups"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        drop(backup);
        drop(upgraded);
        drop(old);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_v17_upgrades_preserve_records_and_workspace_registration() {
        let root = std::env::temp_dir().join(format!(
            "switchx-client-upgrades-{}",
            crate::id::new_id().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("switchx.sqlite");
        let old = Store::open(&db).unwrap();
        old.connection.execute_batch("DROP TABLE client_workspace_selections; DROP TABLE client_workspaces;
            INSERT INTO app_settings VALUES ('synthetic-concurrent-value', 'keep'); PRAGMA user_version = 17;").unwrap();
        drop(old);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let db = db.clone();
                let home = root.join("claude");
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let store = Store::open(&db).unwrap();
                    let key = WorkspaceKey::new(ClientKind::ClaudeCode, &home).unwrap();
                    store.remember_client_workspace(&key, "Claude").unwrap().id
                })
            })
            .collect();
        let ids: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
        let store = Store::open(&db).unwrap();
        assert_eq!(store.client_workspaces().unwrap().len(), 1);
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT value FROM app_settings WHERE key='synthetic-concurrent-value'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "keep"
        );
        assert!(!root.join("claude").exists());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
