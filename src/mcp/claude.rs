//! Explicitly scoped Claude tools. No credentials or arbitrary settings JSON.
use super::*;
use crate::{
    claude::{Connection, ModelOption, PickerPolicy, Roles},
    clients::ClientKind,
};

pub(super) fn workspaces(store: &Store) -> Result<Value, String> {
    let rows = store
        .client_workspaces()
        .map_err(|_| "无法读取客户端工作空间")?;
    Ok(
        json!({"workspaces": rows.into_iter().map(|workspace| json!({
        "id": workspace.id, "client": workspace.key.client, "name": workspace.name,
        "home": workspace.key.home, "config_path": workspace.key.config_path(),
    })).collect::<Vec<_>>()}),
    )
}
fn workspace(store: &Store, id: &str) -> Result<crate::clients::ClientWorkspace, String> {
    let workspace = store
        .client_workspace(id)
        .map_err(|_| "无法读取工作空间")?
        .ok_or("工作空间不存在")?;
    if workspace.key.client != ClientKind::ClaudeCode {
        return Err("目标不是 Claude 工作空间".into());
    }
    Ok(workspace)
}
fn connection(value: Connection) -> Value {
    json!({"id":value.id,"workspace_id":value.workspace_id,"name":value.name,"base_url":value.base_url,
        "revision":value.revision,"credential_configured":value.has_key,"settings":value.settings})
}
pub(super) fn connections(store: &Store, args: &Map<String, Value>) -> Result<Value, String> {
    if args.keys().any(|key| key != "workspace_id") {
        return Err("只接受 workspace_id".into());
    }
    let id = optional_string(args, "workspace_id")?.ok_or("缺少 workspace_id")?;
    workspace(store, id)?;
    Ok(
        json!({"bindings":store.claude_model_bindings(id).map_err(|_|"无法读取模型绑定")?.into_iter().map(|b|json!({"public_id":b.public_id(),"provider_id":b.provider_id,"upstream_model":b.upstream_model})).collect::<Vec<_>>(),"route_options":store.claude_route_options(id).map_err(|_|"无法读取路由设置")?,"connections":store.claude_connections(id).map_err(|_|"无法读取 Claude 连接")?.into_iter().map(connection).collect::<Vec<_>>()}),
    )
}
pub(super) fn save(data: &Path, store: &Store, args: &Map<String, Value>) -> Result<Value, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Edit {
        workspace_id: String,
        provider_id: String,
        revision: i64,
        models: Vec<ModelOption>,
        default_model: String,
        picker: PickerPolicy,
        roles: Roles,
    }
    let edit: Edit = serde_json::from_value(Value::Object(args.clone()))
        .map_err(|_| "参数无效；请提供工作空间、连接、版本和完整模型设置，不接受凭据或任意配置")?;
    let workspace = workspace(store, &edit.workspace_id)?;
    let mut current = store
        .claude_connection(&workspace.id, &edit.provider_id)
        .map_err(|_| "无法读取 Claude 连接")?
        .ok_or("连接不属于此工作空间")?;
    current.settings.models = edit.models;
    current.settings.default_model = edit.default_model;
    current.settings.picker = edit.picker;
    current.settings.roles = edit.roles;
    let saved = app::claude::save_claude_connection(
        data,
        &workspace.key.home,
        current,
        None,
        Some(edit.revision),
    )?;
    Ok(json!({"connection":connection(saved),"applied":false}))
}
pub(super) fn definitions(access: Access) -> Vec<Value> {
    let scope = json!({"type":"string","description":"list_client_workspaces 中 client=claude_code 的工作空间 ID。必须显式指定。"});
    let provider = json!({"type":"string","description":"list_claude_connections 返回的连接 ID。"});
    let preview =
        json!({"preview_id":{"type":"string","description":"本控制通道创建的一次性预览 ID。"}});
    let mut tools = vec![
        described(
            "list_client_workspaces",
            "客户端工作空间",
            "列出已保存的 Codex 和 Claude Code 工作空间；不创建目录，不切换客户端。",
            json!({}),
            &[],
            Effect::Read,
        ),
        described(
            "list_claude_connections",
            "Claude 连接与模型",
            "读取指定 Claude 工作空间的连接、上游协议、认证方式、模型菜单与版本号；不返回凭据。",
            json!({"workspace_id":scope}),
            &["workspace_id"],
            Effect::Read,
        ),
    ];
    tools.push(described("get_claude_status","Claude 工作空间状态","查询指定 Claude 工作空间的配置模式、运行状态与记录故障；不会返回本地令牌。需要正在运行的应用。",json!({"workspace_id":scope}),&["workspace_id"],Effect::Read));
    if access.writes {
        tools.push(described("save_claude_models","保存 Claude 模型列表","完整替换指定连接的模型菜单、默认模型及角色映射。传入读取时的 revision；保存不会应用配置。托管中的工作空间须先恢复。",json!({
        "workspace_id":scope,"provider_id":provider,"revision":{"type":"integer","minimum":1},
        "models":{"type":"array","maxItems":200,"items":{"type":"object","additionalProperties":false,"properties":{
            "model":{"type":"string"},"label":{"type":"string"},"description":{"type":"string"},"behaves_as":{"type":"string"},"enabled":{"type":"boolean"}
        },"required":["model"]}},
        "default_model":{"type":"string"},"picker":{"type":"string","enum":["preserve","append","replace"]},
        "roles":{"type":"object","additionalProperties":false,"properties":{
            "sonnet":{"type":"string"},"opus":{"type":"string"},"haiku":{"type":"string"},"fable":{"type":"string"},"subagent":{"type":"string"}
        }}
    }),&["workspace_id","provider_id","revision","models","default_model","picker","roles"],Effect::Write));
    }
    if access.control {
        tools.extend([
            described("preview_claude_route","预览 Claude 路由与聚合","为指定工作空间准备独立监听、显式模型绑定与配置变更。route 仅发布默认连接，aggregate 发布所有 API 连接；本机官方登录仅支持直连。不会应用客户端配置。",json!({"workspace_id":scope,"mode":{"type":"string","enum":["route","aggregate"]},"provider_id":provider,"default_model":{"type":"string","description":"默认连接中已保存的实际模型 ID；省略沿用连接默认值。"},"port":{"type":"integer","minimum":0,"maximum":65535,"description":"0 自动分配；运行时更改端口需要先停止。"}}),&["workspace_id","mode","provider_id"],Effect::Write),
            described("apply_claude_route","发布 Claude 路由","向用户说明 preview_claude_route 的预览并取得同意后，使用一次性 preview_id 发布；更新后在途请求保留原绑定。SwitchX 需持续运行。停止使用 preview_claude_restore / apply_claude_restore。",preview.clone(),&["preview_id"],Effect::Destructive),

        described("preview_claude_direct","预览 Claude 直连","检查 CLI 能力并预览指定 Claude 工作空间的配置变更；Responses 连接须使用 preview_claude_route。不会应用配置，也不执行外部 helper。",json!({"workspace_id":scope,"provider_id":provider}),&["workspace_id","provider_id"],Effect::Write),
        described("preview_claude_restore","预览 Claude 配置恢复","预览指定 Claude 工作空间的字段恢复与冲突。keep_conflicts=true 表示保留外部值并结束管理，须向用户说明后应用。",json!({"workspace_id":scope,"keep_conflicts":{"type":"boolean","default":false}}),&["workspace_id"],Effect::Write),
        described("apply_claude_direct","应用 Claude 直连","向用户说明 preview_claude_direct 的具体变更并取得同意后，用对应 preview_id 应用。再次核对工作空间、文件、连接与凭据快照。",preview.clone(),&["preview_id"],Effect::Destructive),
        described("apply_claude_restore","恢复 Claude 配置","向用户说明 preview_claude_restore 的内容并取得同意后，用对应 preview_id 恢复；冲突保留外部值。",preview,&["preview_id"],Effect::Destructive),
    ]);
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        claude::{AuthMode, ConnectionSettings, UpstreamProtocol},
        credentials::Secret,
    };
    #[test]
    fn scoped_tools_edit_models_without_credentials_or_codex_retargeting() {
        scoped_edit(UpstreamProtocol::Messages);
    }
    #[test]
    fn responses_model_edits_preserve_protocol_and_require_routing() {
        scoped_edit(UpstreamProtocol::Responses);
    }
    fn scoped_edit(protocol: UpstreamProtocol) {
        let root = std::env::temp_dir().join(format!(
            "switchx-claude-mcp-{}",
            crate::id::new_id().unwrap()
        ));
        let data = root.join("data");
        let home = root.join("claude");
        let workspace = app::claude::claude_workspace(&data, &home).unwrap();
        let c = app::claude::save_claude_connection(
            &data,
            &home,
            Connection {
                id: crate::id::new_id().unwrap(),
                workspace_id: workspace.id.clone(),
                name: "synthetic".into(),
                base_url: "https://example.test".into(),
                settings: ConnectionSettings {
                    auth: if protocol == UpstreamProtocol::Responses {
                        AuthMode::Bearer
                    } else {
                        AuthMode::Helper
                    },
                    protocol,
                    default_model: "manual".into(),
                    ..Default::default()
                },
                revision: 0,
                has_key: false,
            },
            Some(Secret::new("synthetic-mcp-claude-key".into())),
            None,
        )
        .unwrap();
        let store = app::open_store_read_only(&data).unwrap();
        assert_eq!(
            workspaces(&store).unwrap()["workspaces"][0]["client"],
            "claude_code"
        );
        let args = json!({"workspace_id":workspace.id});
        let list = connections(&store, args.as_object().unwrap()).unwrap();
        assert_eq!(list["connections"][0]["credential_configured"], true);
        assert_eq!(
            list["connections"][0]["settings"]["protocol"],
            json!(protocol)
        );
        assert!(!list.to_string().contains("synthetic-mcp-claude-key"));
        assert!(store.providers().unwrap().is_empty());
        let mut edit = json!({"workspace_id":workspace.id,"provider_id":c.id,"revision":c.revision,"models":[{"model":"manual/menu","label":"Custom","enabled":false}],"default_model":"manual/outside","picker":"replace","roles":{"sonnet":"manual/role"}});
        let result = save(&data, &store, edit.as_object().unwrap()).unwrap();
        assert_eq!(
            result["connection"]["settings"]["protocol"],
            json!(protocol)
        );
        assert_eq!(
            result["connection"]["settings"]["models"][0]["enabled"],
            false
        );
        assert_eq!(
            result["connection"]["settings"]["default_model"],
            "manual/outside"
        );
        assert!(!home.join("settings.json").exists());
        assert!(save(&data, &store, edit.as_object().unwrap()).is_err());
        edit["revision"] = result["connection"]["revision"].clone();
        edit["models"][0]["api_key"] = "never-accepted".into();
        assert!(save(&data, &store, edit.as_object().unwrap()).is_err());
        let codex =
            app::remember_client_workspace(&data, ClientKind::Codex, &root.join("codex")).unwrap();
        let args = json!({"workspace_id":codex.id});
        assert!(connections(&store, args.as_object().unwrap()).is_err());
        let preview = app::claude::prepare_claude_direct(
            &data,
            &workspace,
            &c.id,
            &root.join("switchx"),
            crate::clients::ClaudeCapabilities::from_version("2.1.301"),
        );
        if protocol == UpstreamProtocol::Responses {
            assert!(preview.err().unwrap().contains("路由"));
        } else {
            app::claude::apply_claude_change(&data, &home, preview.unwrap()).unwrap();
            edit["models"][0].as_object_mut().unwrap().remove("api_key");
            assert!(
                save(&data, &store, edit.as_object().unwrap())
                    .unwrap_err()
                    .contains("恢复")
            );
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
