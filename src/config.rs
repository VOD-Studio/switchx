use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
};

use toml_edit::{DocumentMut, Value, table, value};

use crate::catalog::Publication;

pub const LOCAL_TOKEN_ENV: &str = "SWITCHX_LOCAL_TOKEN";
pub(crate) const PROVIDER_ID: &str = "switchx_router";

#[derive(Debug)]
pub struct Preview {
    pub proposed: String,
    pub changed_fields: Vec<String>,
    pub required_environment_variable: Option<&'static str>,
}

pub fn preview_route(
    current: &str,
    publication: &Publication,
    catalog_path: &Path,
    router_address: SocketAddr,
    default_model: &str,
) -> Result<Preview, String> {
    if router_address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err("router address must use 127.0.0.1".into());
    }
    if !catalog_path.is_absolute() {
        return Err("catalog path must be absolute".into());
    }
    if !publication.routes.contains_key(default_model) {
        return Err("default model is not published".into());
    }
    let mut document = current
        .parse::<DocumentMut>()
        .map_err(|_| "invalid Codex TOML")?;
    ensure_unmanaged(&document)?;
    if let Some(providers) = document.as_table().get("model_providers") {
        let providers = providers
            .as_table()
            .ok_or("model_providers is not a table")?;
        if let Some(existing) = providers.get(PROVIDER_ID) {
            if existing.is_table() {
                return Err("switchx_router provider already exists; ownership is unknown".into());
            }
            return Err("switchx_router provider conflicts with an existing value".into());
        }
    }

    let path = catalog_path.to_str().ok_or("catalog path must be UTF-8")?;
    let desired = [
        ("model", default_model),
        ("model_provider", PROVIDER_ID),
        ("model_catalog_json", path),
    ];
    let mut changed_fields = Vec::new();
    for (key, new_value) in desired {
        if document
            .as_table()
            .get(key)
            .is_some_and(|item| item.as_str().is_none())
        {
            return Err(format!("managed Codex field {key} is not a string"));
        }
        if document.as_table().get(key).and_then(|item| item.as_str()) != Some(new_value) {
            changed_fields.push(key.to_owned());
            if let Some(item) = document.as_table_mut().get_mut(key) {
                let old = item
                    .as_value_mut()
                    .ok_or("managed Codex field is not a value")?;
                let mut replacement = Value::from(new_value);
                *replacement.decor_mut() = old.decor().clone();
                *old = replacement;
            } else {
                document.as_table_mut().insert(key, value(new_value));
            }
        }
    }
    if document.as_table().get("model_providers").is_none() {
        document.as_table_mut().insert("model_providers", table());
    }
    let providers = document
        .as_table_mut()
        .get_mut("model_providers")
        .unwrap()
        .as_table_mut()
        .unwrap();
    providers.insert(PROVIDER_ID, table());
    let provider = providers
        .get_mut(PROVIDER_ID)
        .unwrap()
        .as_table_mut()
        .unwrap();
    provider.insert("name", value("SwitchX Router"));
    provider.insert("base_url", value(format!("http://{router_address}/v1")));
    provider.insert("wire_api", value("responses"));
    provider.insert("env_key", value(LOCAL_TOKEN_ENV));
    provider.insert("requires_openai_auth", value(false));
    provider.insert("supports_websockets", value(false));
    provider.insert("request_max_retries", value(0));
    provider.insert("stream_max_retries", value(0));
    changed_fields.push("model_providers.switchx_router".into());

    Ok(Preview {
        proposed: document.to_string(),
        changed_fields,
        required_environment_variable: Some(LOCAL_TOKEN_ENV),
    })
}

pub(crate) fn ensure_unmanaged(document: &DocumentMut) -> Result<(), String> {
    if document
        .as_table()
        .get("model_provider")
        .and_then(|item| item.as_str())
        .is_some_and(|provider| provider == PROVIDER_ID || provider.starts_with("switchx_direct_"))
    {
        return Err("Codex 配置已由 SwitchX 管理，请使用原数据目录恢复后再切换".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Selection, publish};

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

    #[test]
    fn preview_changes_only_owned_codex_fields() {
        let current = include_str!("../tests/fixtures/codex-user-config.toml");
        let preview = preview_route(
            current,
            &publication(),
            Path::new("/tmp/switchx-test/catalog-v1.json"),
            "127.0.0.1:18731".parse().unwrap(),
            "sx-ds-flash",
        )
        .unwrap();
        assert_eq!(
            preview.changed_fields,
            [
                "model",
                "model_provider",
                "model_catalog_json",
                "model_providers.switchx_router"
            ]
        );
        assert_eq!(
            preview.proposed,
            include_str!("../tests/fixtures/routed-user-config.toml")
        );
        assert!(preview.proposed.contains("# user comment about MCP"));
        assert!(preview.proposed.contains("# chosen by user"));
        assert!(
            preview
                .proposed
                .contains("approval_policy = \"on-request\"")
        );
        assert!(preview.proposed.contains("[mcp_servers.sample]"));
        assert!(
            preview
                .proposed
                .contains("[projects.\"/tmp/user-project\"]")
        );
        assert!(preview.proposed.contains("[model_providers.other]"));
        let document: DocumentMut = preview.proposed.parse().unwrap();
        assert_eq!(document["model"].as_str(), Some("sx-ds-flash"));
        assert_eq!(
            document["model_providers"][PROVIDER_ID]["env_key"].as_str(),
            Some(LOCAL_TOKEN_ENV)
        );
        assert_eq!(
            document["model_providers"][PROVIDER_ID]["stream_max_retries"].as_integer(),
            Some(0)
        );
    }

    #[test]
    fn preview_refuses_existing_provider_and_invalid_target() {
        let source = "[model_providers.switchx_router]\nname = \"someone else\"\n";
        assert!(
            preview_route(
                source,
                &publication(),
                Path::new("/tmp/c.json"),
                "127.0.0.1:18731".parse().unwrap(),
                "sx-ds-flash"
            )
            .unwrap_err()
            .contains("ownership")
        );
        assert!(
            preview_route(
                "",
                &publication(),
                Path::new("catalog.json"),
                "127.0.0.1:18731".parse().unwrap(),
                "sx-ds-flash"
            )
            .unwrap_err()
            .contains("absolute")
        );
    }
}
