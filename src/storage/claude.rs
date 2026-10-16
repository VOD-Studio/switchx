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
