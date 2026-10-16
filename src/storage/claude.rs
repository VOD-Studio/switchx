//! Claude connection bindings reuse the existing private provider key storage.
//! Legacy Codex queries exclude these explicitly bound connections.

use rusqlite::{OptionalExtension, Result, Transaction, TransactionBehavior, params};

use crate::{
    claude::{AuthMode, Connection},
    clients::{ClientKind, ClientWorkspace},
    credentials::Secret,
};

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS client_connections (
    provider_id TEXT PRIMARY KEY NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES client_workspaces(id),
    settings TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0)
); CREATE INDEX IF NOT EXISTS client_connections_workspace ON client_connections(workspace_id);";

fn from_row(row: &rusqlite::Row<'_>) -> Result<Connection> {
    let settings: String = row.get(4)?;
    Ok(Connection {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        base_url: row.get(3)?,
        settings: serde_json::from_str(&settings).map_err(|_| rusqlite::Error::InvalidQuery)?,
        revision: row.get(5)?,
        has_key: row.get(6)?,
    })
}

const COLUMNS: &str = "SELECT p.id, c.workspace_id, p.name, p.base_url, c.settings, c.revision,
    COALESCE(json_type(p.settings_config, '$.auth.OPENAI_API_KEY') = 'text' AND length(json_extract(p.settings_config, '$.auth.OPENAI_API_KEY')) > 0, 0)
    FROM providers p JOIN client_connections c ON c.provider_id = p.id
    JOIN client_workspaces w ON w.id = c.workspace_id WHERE w.client = 'claude_code'";

impl super::Store {
    pub fn client_workspace(&self, id: &str) -> Result<Option<ClientWorkspace>> {
        Ok(self
            .client_workspaces()?
            .into_iter()
            .find(|workspace| workspace.id == id))
    }

    pub fn claude_connections(&self, workspace_id: &str) -> Result<Vec<Connection>> {
        self.connection
            .prepare(&format!(
                "{COLUMNS} AND c.workspace_id = ?1 ORDER BY p.name, p.id"
            ))?
            .query_map([workspace_id], from_row)?
            .collect()
    }

    pub fn claude_connection(&self, workspace_id: &str, id: &str) -> Result<Option<Connection>> {
        self.connection
            .query_row(
                &format!("{COLUMNS} AND c.workspace_id = ?1 AND p.id = ?2"),
                params![workspace_id, id],
                from_row,
            )
            .optional()
    }

    /// All cooperating config writers use the same database write lock. Callers
    /// perform network and CLI work before entering this short transaction.
    pub(crate) fn client_edit<T>(
        &self,
        action: impl FnOnce() -> std::result::Result<T, String>,
    ) -> std::result::Result<T, String> {
        let transaction =
            Transaction::new_unchecked(&self.connection, TransactionBehavior::Immediate)
                .map_err(|_| "配置正在被其他操作使用，请稍后重试".to_owned())?;
        let result = action()?;
        transaction
            .commit()
            .map_err(|_| "无法提交客户端资料".to_owned())?;
        Ok(result)
    }

    pub(crate) fn write_claude_connection(
        &self,
        connection: &Connection,
        key: Option<&Secret>,
        expected: Option<i64>,
    ) -> std::result::Result<Connection, String> {
        let workspace = self
            .client_workspace(&connection.workspace_id)
            .map_err(|_| "无法读取工作空间")?
            .ok_or("工作空间不存在")?;
        if workspace.key.client != ClientKind::ClaudeCode {
            return Err("连接不属于 Claude Code 工作空间".into());
        }
        connection.settings.validate()?;
        let old = self
            .claude_connection(&workspace.id, &connection.id)
            .map_err(|_| "无法读取连接")?;
        if old.as_ref().map(|c| c.revision) != expected {
            return Err("连接已被其他操作修改，请重新打开编辑器".into());
        }
        let raw_exists: bool = self
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM providers WHERE id=?1)",
                [&connection.id],
                |row| row.get(0),
            )
            .map_err(|_| "无法检查连接")?;
        if old.is_none() && raw_exists {
            return Err("连接 ID 已属于其他客户端或工作空间".into());
        }
        let saved: Option<String> = self
            .connection
            .query_row(
                "SELECT settings_config FROM providers WHERE id=?1",
                [&connection.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "无法读取凭据状态")?;
        let settings = match key {
            Some(key) => super::settings_with_key(saved.as_deref().unwrap_or("{}"), key)
                .map_err(|_| "无法保存凭据")?,
            None => saved.unwrap_or_else(|| "{}".into()),
        };
        let has_key = super::provider_settings(&settings)
            .map_err(|_| "凭据资料无效")?
            .pointer("/auth/OPENAI_API_KEY")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|key| !key.is_empty());
        if connection.settings.auth != AuthMode::Native && !has_key {
            return Err("请填写 API Key 或 Token；编辑时留空会保留原值".into());
        }
        let configuration =
            serde_json::to_string(&connection.settings).map_err(|_| "无法保存连接设置")?;
        let revision = expected.unwrap_or(0).checked_add(1).ok_or("连接版本无效")?;
        self.connection.execute("INSERT INTO providers(id,name,base_url,model_id,kind,settings_config)
            VALUES(?1,?2,?3,?4,'api_key',?5) ON CONFLICT(id) DO UPDATE SET
            name=excluded.name, base_url=excluded.base_url, model_id=excluded.model_id, settings_config=excluded.settings_config, credential_ref=NULL",
            params![connection.id,connection.name,connection.base_url,connection.settings.default_model,settings]).map_err(|_| "无法保存连接")?;
        self.connection.execute("INSERT INTO client_connections(provider_id,workspace_id,settings,revision) VALUES(?1,?2,?3,?4)
            ON CONFLICT(provider_id) DO UPDATE SET settings=excluded.settings, revision=excluded.revision",
            params![connection.id,workspace.id,configuration,revision]).map_err(|_| "无法绑定客户端连接")?;
        self.connection
            .execute_batch(SYNC_BINDINGS)
            .map_err(|_| "无法保存模型身份")?;
        self.claude_connection(&workspace.id, &connection.id)
            .map_err(|_| "无法读取已保存连接")?
            .ok_or_else(|| "保存后的连接不存在".into())
    }

    pub(crate) fn remove_claude_connection(
        &self,
        workspace: &str,
        id: &str,
        revision: i64,
    ) -> std::result::Result<(), String> {
        let saved = self
            .claude_connection(workspace, id)
            .map_err(|_| "无法读取连接")?
            .ok_or("连接不存在")?;
        if saved.revision != revision {
            return Err("连接已被修改，请刷新后再删除".into());
        }
        self.connection
            .execute("DELETE FROM providers WHERE id=?1", [id])
            .map_err(|_| "无法删除连接")?;
        Ok(())
    }

    pub fn client_cli(&self, workspace_id: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [format!("client_cli:{workspace_id}")],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn save_client_cli(&self, workspace_id: &str, executable: &str) -> Result<()> {
        if self.client_workspace(workspace_id)?.is_none()
            || (!executable.is_empty() && !std::path::Path::new(executable).is_absolute())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.connection.execute("INSERT INTO app_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("client_cli:{workspace_id}"),executable])?;
        Ok(())
    }
}

pub(super) const ROUTE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS claude_model_bindings (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL REFERENCES client_workspaces(id),
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    upstream_model TEXT NOT NULL,
    UNIQUE(workspace_id,provider_id,upstream_model)
);";
pub(super) const SYNC_BINDINGS: &str = "INSERT OR IGNORE INTO claude_model_bindings(id,workspace_id,provider_id,upstream_model)
    SELECT lower(hex(randomblob(16))),workspace_id,provider_id,model FROM (
        SELECT c.workspace_id,c.provider_id,json_extract(m.value,'$.model') AS model
        FROM client_connections c,json_each(c.settings,'$.models') m WHERE json_extract(c.settings,'$.auth') != 'native'
        UNION
        SELECT c.workspace_id,c.provider_id,m.value AS model FROM client_connections c,
        json_each(json_array(json_extract(c.settings,'$.default_model'),json_extract(c.settings,'$.roles.sonnet'),json_extract(c.settings,'$.roles.opus'),json_extract(c.settings,'$.roles.haiku'),json_extract(c.settings,'$.roles.fable'),json_extract(c.settings,'$.roles.subagent'))) m
        WHERE json_extract(c.settings,'$.auth') != 'native'
    ) WHERE typeof(model)='text' AND length(model)>0;";
impl super::Store {
    pub fn claude_model_bindings(
        &self,
        workspace: &str,
    ) -> Result<Vec<crate::claude::publication::Binding>> {
        self.connection.prepare("SELECT id,provider_id,upstream_model FROM claude_model_bindings WHERE workspace_id=?1 ORDER BY id")?
            .query_map([workspace],|row|Ok(crate::claude::publication::Binding{id:row.get(0)?,provider_id:row.get(1)?,upstream_model:row.get(2)?}))?.collect()
    }
    pub fn claude_route_options(
        &self,
        workspace: &str,
    ) -> Result<crate::claude::publication::Options> {
        let text: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [format!("claude-route:{workspace}")],
                |row| row.get(0),
            )
            .optional()?;
        text.map(|text| serde_json::from_str(&text).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    pub(crate) fn save_claude_route_options(
        &self,
        workspace: &str,
        options: &crate::claude::publication::Options,
    ) -> Result<()> {
        let text = serde_json::to_string(options).map_err(|_| rusqlite::Error::InvalidQuery)?;
        self.connection.execute("INSERT INTO app_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("claude-route:{workspace}"),text])?;
        Ok(())
    }
}

impl super::Store {
    pub fn clear_codex_requests(&self) -> Result<usize> {
        self.connection.execute(
            "DELETE FROM request_records WHERE client != 'claude_code'",
            [],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v19_upgrade_assigns_stable_bindings_and_namespaces_old_claude_usage() {
        use crate::claude::{ConnectionSettings, ModelOption};
        let root = std::env::temp_dir().join(format!(
            "switchx-claude-v20-{}",
            crate::id::new_id().unwrap()
        ));
        let data = root.join("data");
        let home = root.join("home");
        let workspace = crate::app::claude::claude_workspace(&data, &home).unwrap();
        let saved = crate::app::claude::save_claude_connection(
            &data,
            &home,
            Connection {
                id: crate::id::new_id().unwrap(),
                workspace_id: workspace.id.clone(),
                name: "Existing Claude".into(),
                base_url: "https://example.test".into(),
                settings: ConnectionSettings {
                    default_model: "same".into(),
                    models: vec![ModelOption::new("same")],
                    ..Default::default()
                },
                revision: 0,
                has_key: false,
            },
            Some(Secret::new("synthetic-v19-key".into())),
            None,
        )
        .unwrap();
        let db = data.join("switchx.sqlite");
        rusqlite::Connection::open(&db).unwrap().execute_batch("DROP TABLE claude_model_bindings; ALTER TABLE request_records DROP COLUMN client; ALTER TABLE request_records DROP COLUMN workspace_id;
            INSERT INTO usage_records(id,source,client,started_at_ms,model,input_tokens,cached_input_tokens,cache_write_tokens,output_tokens,reasoning_tokens) VALUES('claude:msg_old','claude_log','claude_code',1,'same',2,0,0,3,0);
            PRAGMA user_version=19;").unwrap();
        let store = crate::storage::Store::open(&db).unwrap();
        let bindings = store.claude_model_bindings(&workspace.id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].upstream_model, "same");
        assert_eq!(
            store.provider_api_key(&saved.id).unwrap().unwrap().expose(),
            "synthetic-v19-key"
        );
        assert_eq!(
            store
                .usage_page(0, i64::MAX, &Default::default(), 0, 10)
                .unwrap()
                .0[0]
                .response_id
                .as_deref(),
            Some("claude:msg_old")
        );
        drop(store);
        let mut edit = saved.clone();
        edit.name = "Renamed".into();
        crate::app::claude::save_claude_connection(&data, &home, edit, None, Some(saved.revision))
            .unwrap();
        assert_eq!(
            crate::storage::Store::open(&db)
                .unwrap()
                .claude_model_bindings(&workspace.id)
                .unwrap(),
            bindings
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod request_scope_tests {
    use crate::storage::*;
    #[test]
    fn client_and_workspace_filters_run_before_the_history_limit() {
        let path = std::env::temp_dir().join(format!(
            "switchx-request-scope-{}.sqlite",
            crate::id::new_id().unwrap()
        ));
        let store = Store::open(&path).unwrap();
        let make = |id: &str, time, client, workspace| RequestRecord {
            client,
            workspace_id: Some(workspace),
            id: id.into(),
            started_at_ms: time,
            public_model: None,
            provider_id: None,
            upstream_model: None,
            generation: "synthetic".into(),
            http_status: Some(200),
            headers_ms: None,
            first_event_ms: None,
            duration_ms: 1,
            status: RequestStatus::Completed,
            error_code: None,
            fallback_from: None,
            usage: None,
            response_id: None,
        };
        store
            .put_request(&make("codex", 1, UsageClient::Unknown, "codex".into()))
            .unwrap();
        store
            .put_request(&make("claude-a", 2, UsageClient::ClaudeCode, "a".into()))
            .unwrap();
        store
            .put_request(&make("claude-b", 3, UsageClient::ClaudeCode, "b".into()))
            .unwrap();
        assert_eq!(store.codex_requests(1).unwrap()[0].id, "codex");
        assert_eq!(store.claude_requests("a", 1).unwrap()[0].id, "claude-a");
        assert_eq!(store.clear_codex_requests().unwrap(), 1);
        assert_eq!(store.requests(10).unwrap().len(), 2);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }
}
