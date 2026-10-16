//! Client workspace metadata services. None of these operations applies client
//! configuration or reads credentials.

use std::path::Path;

use crate::clients::{ClientKind, ClientWorkspace, WorkspaceKey};

use super::open_store;

pub fn remember_client_workspace(
    data_dir: &Path,
    client: ClientKind,
    home: &Path,
) -> Result<ClientWorkspace, String> {
    let key = WorkspaceKey::new(client, home)?;
    let store = open_store(data_dir).map_err(|error| error.message().to_owned())?;
    let workspace = store
        .remember_client_workspace(&key, client.label())
        .map_err(|_| "无法保存客户端工作空间".to_owned())?;
    store
        .select_client_workspace(client, &workspace.id)
        .map_err(|_| "无法选择客户端工作空间".to_owned())?;
    Ok(workspace)
}

pub fn selected_client_workspace(
    data_dir: &Path,
    client: ClientKind,
) -> Result<Option<ClientWorkspace>, String> {
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .selected_client_workspace(client)
        .map_err(|_| "无法读取客户端工作空间".to_owned())
}

pub fn save_client_page(data_dir: &Path, workspace_id: &str, page: i32) -> Result<(), String> {
    open_store(data_dir)
        .map_err(|error| error.message().to_owned())?
        .save_client_page(workspace_id, page)
        .map_err(|_| "无法保存客户端页面".to_owned())
}
