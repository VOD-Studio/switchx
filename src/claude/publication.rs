//! Explicit Claude model bindings and immutable publication plans.
use crate::{
    claude::{
        AuthMode, Connection, ConnectionSettings, ModelOption, PickerPolicy, Roles,
        UpstreamProtocol,
    },
    storage::Store,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Route,
    #[default]
    Aggregate,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Aggregate => "aggregate",
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub mode: Mode,
    pub primary: String,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub port: u16,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub id: String,
    pub provider_id: String,
    pub upstream_model: String,
}
impl Binding {
    pub fn public_id(&self) -> String {
        format!("sx-claude-{}", self.id)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct PublishedModel {
    pub public_id: String,
    pub provider_id: String,
    pub provider_name: String,
    pub upstream_model: String,
    pub label: String,
    pub description: String,
    pub behaves_as: String,
    pub menu: bool,
}
#[derive(Debug, Clone)]
pub struct Plan {
    pub options: Options,
    pub connections: Vec<Connection>,
    pub models: Vec<PublishedModel>,
    pub aliases: BTreeMap<String, String>,
    pub settings: ConnectionSettings,
}

pub fn build(store: &Store, workspace: &str, mut options: Options) -> Result<Plan, String> {
    let connections = store
        .claude_connections(workspace)
        .map_err(|_| "无法读取 Claude 连接")?;
    if options.primary.is_empty() {
        options.primary = connections
            .iter()
            .find(|c| c.settings.auth != AuthMode::Native)
            .map(|c| c.id.clone())
            .ok_or("请先添加有凭据的 Messages 或 Responses 连接")?;
    }
    let primary = connections
        .iter()
        .find(|c| c.id == options.primary && c.settings.auth != AuthMode::Native)
        .ok_or("默认连接不存在或使用本机官方登录；官方登录仅支持直连")?;
    if options.default_model.is_empty() {
        options.default_model = primary.settings.default_model.clone();
    }
    if options.default_model != primary.settings.default_model
        && !primary
            .settings
            .models
            .iter()
            .any(|m| m.model == options.default_model)
        && !primary
            .settings
            .roles
            .fields()
            .iter()
            .any(|(_, m)| *m == options.default_model)
    {
        return Err("默认模型不在当前连接的已保存模型或角色中".into());
    }
    let bindings = store
        .claude_model_bindings(workspace)
        .map_err(|_| "无法读取模型绑定")?;
    let selected: Vec<_> = connections
        .iter()
        .filter(|c| {
            c.settings.auth != AuthMode::Native
                && (options.mode == Mode::Aggregate || c.id == primary.id)
        })
        .collect();
    let mut models = Vec::new();
    for c in &selected {
        if !c.has_key {
            return Err(format!("连接「{}」尚未配置凭据", c.name));
        }
        let mut actual: BTreeSet<String> = c
            .settings
            .models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.model.clone())
            .collect();
        if c.id == primary.id {
            actual.insert(options.default_model.clone());
            actual.extend(
                c.settings
                    .roles
                    .fields()
                    .into_iter()
                    .filter(|(_, m)| !m.is_empty())
                    .map(|(_, m)| m.to_owned()),
            );
        }
        // Keep the user's menu order; extra role/default bindings remain hidden.
        let mut ordered: Vec<_> = c
            .settings
            .models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.model.clone())
            .collect();
        for model in actual {
            if !ordered.contains(&model) {
                ordered.push(model);
            }
        }
        for model in ordered {
            let binding = bindings
                .iter()
                .find(|b| b.provider_id == c.id && b.upstream_model == model)
                .ok_or("默认或角色模型不在已保存连接中，请先保存该模型")?;
            let entry = c.settings.models.iter().find(|m| m.model == model);
            models.push(PublishedModel {
                public_id: binding.public_id(),
                provider_id: c.id.clone(),
                provider_name: c.name.clone(),
                upstream_model: model.clone(),
                label: menu_label(format!(
                    "{} · {}",
                    entry
                        .filter(|m| !m.label.is_empty())
                        .map_or(model.as_str(), |m| m.label.as_str()),
                    c.name
                )),
                description: entry.map(|m| m.description.clone()).unwrap_or_default(),
                behaves_as: entry.map(|m| m.behaves_as.clone()).unwrap_or_default(),
                menu: entry.is_some_and(|m| m.enabled),
            });
        }
    }
    if models.len() > 200 {
        return Err("一次最多发布 200 个 Claude 模型，请减少勾选项".into());
    }
    let resolve = |actual: &str| -> Result<String, String> {
        models
            .iter()
            .find(|m| m.provider_id == primary.id && m.upstream_model == actual)
            .map(|m| m.public_id.clone())
            .ok_or_else(|| "默认模型未绑定到默认连接".into())
    };
    let default = resolve(&options.default_model)?;
    let role = |value: &str| {
        if value.is_empty() {
            Ok(default.clone())
        } else {
            resolve(value)
        }
    };
    let roles = Roles {
        sonnet: role(&primary.settings.roles.sonnet)?,
        opus: role(&primary.settings.roles.opus)?,
        haiku: role(&primary.settings.roles.haiku)?,
        fable: role(&primary.settings.roles.fable)?,
        subagent: role(&primary.settings.roles.subagent)?,
    };
    let aliases = BTreeMap::from([
        ("default".into(), default.clone()),
        ("sonnet".into(), roles.sonnet.clone()),
        ("opus".into(), roles.opus.clone()),
        ("haiku".into(), roles.haiku.clone()),
        ("fable".into(), roles.fable.clone()),
    ]);
    let settings = ConnectionSettings {
        auth: AuthMode::Helper,
        protocol: UpstreamProtocol::Messages,
        picker: PickerPolicy::Replace,
        default_model: default,
        roles,
        models: models
            .iter()
            .filter(|m| m.menu)
            .map(|m| ModelOption {
                model: m.public_id.clone(),
                label: m.label.clone(),
                description: m.description.clone(),
                behaves_as: m.behaves_as.clone(),
                enabled: true,
            })
            .collect(),
    };
    settings.validate()?;
    Ok(Plan {
        options,
        connections: selected.into_iter().cloned().collect(),
        models,
        aliases,
        settings,
    })
}

fn menu_label(value: String) -> String {
    let end = value
        .char_indices()
        .map(|(i, ch)| i + ch.len_utf8())
        .take_while(|end| *end <= 240)
        .last()
        .unwrap_or(0);
    value[..end].into()
}
