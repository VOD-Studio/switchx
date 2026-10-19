use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::clients::ClaudeCapabilities;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Helper,
    ApiKey,
    Bearer,
    Native,
}

impl AuthMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Helper => "SwitchX 凭据 helper",
            Self::ApiKey => "API Key",
            Self::Bearer => "Bearer Token",
            Self::Native => "本机官方登录",
        }
    }
    pub fn index(self) -> i32 {
        match self {
            Self::Helper => 0,
            Self::ApiKey => 1,
            Self::Bearer => 2,
            Self::Native => 3,
        }
    }
    pub fn from_index(index: i32) -> Result<Self, String> {
        match index {
            0 => Ok(Self::Helper),
            1 => Ok(Self::ApiKey),
            2 => Ok(Self::Bearer),
            3 => Ok(Self::Native),
            _ => Err("请选择认证方式".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamProtocol {
    #[default]
    Messages,
    Responses,
}

impl UpstreamProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Messages => "Anthropic Messages",
            Self::Responses => "OpenAI Responses",
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Self::Messages => 0,
            Self::Responses => 1,
        }
    }

    pub fn from_index(index: i32) -> Result<Self, String> {
        match index {
            0 => Ok(Self::Messages),
            1 => Ok(Self::Responses),
            _ => Err("请选择上游协议".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerPolicy {
    Preserve,
    #[default]
    Append,
    Replace,
}

impl PickerPolicy {
    pub fn index(self) -> i32 {
        match self {
            Self::Preserve => 0,
            Self::Append => 1,
            Self::Replace => 2,
        }
    }
    pub fn from_index(index: i32) -> Result<Self, String> {
        match index {
            0 => Ok(Self::Preserve),
            1 => Ok(Self::Append),
            2 => Ok(Self::Replace),
            _ => Err("请选择模型菜单策略".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOption {
    pub model: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub behaves_as: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl ModelOption {
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            label: model.clone(),
            model,
            description: String::new(),
            behaves_as: String::new(),
            enabled: true,
        }
    }
    pub fn picker_value(&self) -> Value {
        let mut value = json!({ "model": self.model });
        for (key, text) in [
            ("label", &self.label),
            ("description", &self.description),
            ("behavesAs", &self.behaves_as),
        ] {
            if !text.is_empty() {
                value[key] = text.as_str().into();
            }
        }
        value
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    #[serde(default)]
    pub sonnet: String,
    #[serde(default)]
    pub opus: String,
    #[serde(default)]
    pub haiku: String,
    #[serde(default)]
    pub fable: String,
    #[serde(default)]
    pub subagent: String,
}

impl Roles {
    pub fn fields(&self) -> [(&'static str, &str); 5] {
        [
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", &self.sonnet),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", &self.opus),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", &self.haiku),
            ("ANTHROPIC_DEFAULT_FABLE_MODEL", &self.fable),
            ("CLAUDE_CODE_SUBAGENT_MODEL", &self.subagent),
        ]
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSettings {
    #[serde(default)]
    pub auth: AuthMode,
    #[serde(default)]
    pub protocol: UpstreamProtocol,
    #[serde(default)]
    pub picker: PickerPolicy,
    #[serde(default)]
    pub models: Vec<ModelOption>,
    #[serde(default)]
    pub default_model: String,
    #[serde(default)]
    pub roles: Roles,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub base_url: String,
    pub settings: ConnectionSettings,
    pub revision: i64,
    pub has_key: bool,
}

pub(crate) fn validate_text(text: &str, limit: usize, label: &str) -> Result<(), String> {
    if text.len() > limit || text.chars().any(char::is_control) {
        return Err(format!("{label}过长或包含控制字符"));
    }
    Ok(())
}

pub fn validate_model(model: &str) -> Result<(), String> {
    validate_text(model, 512, "模型 ID")?;
    if model.is_empty() || model.trim() != model || model.chars().any(char::is_whitespace) {
        return Err("模型 ID 不能为空，也不能包含空白字符".into());
    }
    Ok(())
}

impl ConnectionSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol == UpstreamProtocol::Responses && self.auth != AuthMode::Bearer {
            return Err("Responses 路由需要 Bearer Token；请先选择对应认证方式".into());
        }
        if self.models.len() > 200 {
            return Err("一个连接最多保存 200 个模型".into());
        }
        let mut ids = HashSet::new();
        for model in &self.models {
            validate_model(&model.model)?;
            if !ids.insert(&model.model) {
                return Err("模型 ID 重复，请合并或修改重复项".into());
            }
            validate_text(&model.label, 240, "模型名称")?;
            validate_text(&model.description, 1000, "模型说明")?;
            if !model.behaves_as.is_empty() {
                validate_model(&model.behaves_as)?;
            }
        }
        if self.auth != AuthMode::Native && self.default_model.is_empty() {
            return Err("请设置默认模型".into());
        }
        if !self.default_model.is_empty() {
            validate_model(&self.default_model)?;
        }
        for (_, model) in self.roles.fields() {
            if !model.is_empty() {
                validate_model(model)?;
            }
        }
        Ok(())
    }

    pub fn validate_client(&self, capabilities: ClaudeCapabilities) -> Result<(), String> {
        self.validate()?;
        if self.picker != PickerPolicy::Preserve && !capabilities.model_picker {
            return Err(
                "自定义模型菜单需要 Claude Code 2.1.242 或更高版本；可先选择保留原菜单".into(),
            );
        }
        if self.picker != PickerPolicy::Preserve
            && self
                .models
                .iter()
                .any(|m| m.enabled && !m.behaves_as.is_empty())
            && !capabilities.model_behaves_as
        {
            return Err("behavesAs 需要 Claude Code 2.1.257 或更高版本".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_settings_keep_messages_and_responses_require_bearer() {
        let mut settings: ConnectionSettings = serde_json::from_value(serde_json::json!({
            "default_model":"synthetic-model"
        }))
        .unwrap();
        assert_eq!(settings.protocol, UpstreamProtocol::Messages);
        assert!(settings.validate().is_ok());
        settings.protocol = UpstreamProtocol::Responses;
        for auth in [AuthMode::Helper, AuthMode::ApiKey, AuthMode::Native] {
            settings.auth = auth;
            assert!(settings.validate().is_err());
        }
        settings.auth = AuthMode::Bearer;
        assert!(settings.validate().is_ok());
        let restored: ConnectionSettings =
            serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
        assert_eq!(restored.protocol, UpstreamProtocol::Responses);
    }
    #[test]
    fn menus_preserve_order_labels_and_capability_gates() {
        let mut settings = ConnectionSettings {
            default_model: "custom/one".into(),
            models: vec![
                ModelOption::new("custom/two"),
                ModelOption::new("custom/one"),
            ],
            ..Default::default()
        };
        assert!(
            settings
                .validate_client(ClaudeCapabilities::from_version("2.1.242"))
                .is_ok()
        );
        settings.models[1].behaves_as = "claude-sonnet-4-6".into();
        assert!(
            settings
                .validate_client(ClaudeCapabilities::from_version("2.1.242"))
                .is_err()
        );
        assert!(
            settings
                .validate_client(ClaudeCapabilities::from_version("2.1.257"))
                .is_ok()
        );
        assert_eq!(settings.models[0].picker_value()["model"], "custom/two");
        assert_eq!(
            settings.models[1].picker_value()["behavesAs"],
            "claude-sonnet-4-6"
        );
        settings.models[1].enabled = false;
        assert!(settings.validate().is_ok());
        settings.default_model = "custom/two".into();
        settings.models.push(ModelOption::new("custom/two"));
        assert!(settings.validate().is_err());
    }
}
