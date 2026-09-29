use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, TableLike, Value, table, value};

const CONTEXT_WINDOW: u64 = 1_000_000;
const RESERVED: &[&str] = &[
    "model",
    "model_provider",
    "model_providers",
    "model_catalog_json",
    "base_url",
    "wire_api",
    "auth",
    "mcp_servers",
    "mcp",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexOptions {
    pub remote_compaction: bool,
    pub use_common_config: bool,
    pub context_1m: bool,
    pub compact_limit: u64,
    /// A subscription's editable, non-sensitive complete Codex configuration.
    pub config_toml: Option<String>,
}

impl Default for CodexOptions {
    fn default() -> Self {
        Self {
            remote_compaction: false,
            use_common_config: true,
            context_1m: false,
            compact_limit: 900_000,
            config_toml: None,
        }
    }
}

impl CodexOptions {
    pub fn validate(&self) -> Result<(), String> {
        if self.context_1m && !(1..CONTEXT_WINDOW).contains(&self.compact_limit) {
            return Err("压缩阈值须为小于 1000000 的正整数".into());
        }
        if let Some(config) = &self.config_toml {
            validate_subscription(config)?;
        }
        Ok(())
    }

    pub fn from_saved(saved: &str) -> Result<Option<Self>, String> {
        if saved.is_empty() {
            return Ok(None);
        }
        let options: Self = serde_json::from_str(saved).map_err(|_| "供应商 Codex 选项无效")?;
        options.validate()?;
        Ok(Some(options))
    }

    pub fn for_common(common: &str) -> Result<Self, String> {
        let document = validate_common(common)?;
        let mut options = Self::default();
        if document
            .get("model_context_window")
            .and_then(Item::as_integer)
            == Some(CONTEXT_WINDOW as i64)
        {
            options.context_1m = true;
            if let Some(limit) = document
                .get("model_auto_compact_token_limit")
                .and_then(Item::as_integer)
            {
                options.compact_limit =
                    u64::try_from(limit).map_err(|_| "通用配置的压缩阈值无效")?;
            }
        }
        options.validate()?;
        Ok(options)
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace('-', "_");
    matches!(
        key.as_str(),
        "api_key"
            | "token"
            | "secret"
            | "password"
            | "authorization"
            | "auth"
            | "http_headers"
            | "env_http_headers"
            | "experimental_bearer_token"
            | "credential"
            | "credential_ref"
            | "credentials"
    ) || ["_api_key", "_token", "_secret", "_password", "_credentials"]
        .iter()
        .any(|suffix| key.ends_with(suffix))
        || key.contains("secret")
        || key.contains("private_key")
        || key.contains("access_key")
        || key.starts_with("password_")
}

fn validate_item(item: &Item) -> Result<(), String> {
    if let Some(table) = item.as_table_like() {
        for (key, item) in table.iter() {
            if sensitive_key(key)
                || matches!(
                    key,
                    "model_provider" | "model_providers" | "model_catalog_json"
                )
            {
                return Err("通用配置不能包含供应商身份、认证头或密钥；请使用系统凭据存储".into());
            }
            validate_item(item)?;
        }
    } else if let Some(tables) = item.as_array_of_tables() {
        for table in tables.iter() {
            validate_item(&Item::Table(table.clone()))?;
        }
    } else if let Some(array) = item.as_array() {
        for value in array.iter() {
            validate_item(&Item::Value(value.clone()))?;
        }
    }
    Ok(())
}

pub fn validate_common(snippet: &str) -> Result<DocumentMut, String> {
    if snippet.len() > 256 * 1024 {
        return Err("通用配置不能超过 256 KiB".into());
    }
    let document: DocumentMut = snippet
        .parse()
        .map_err(|_| "通用配置不是有效的 TOML，请检查语法")?;
    if document.iter().any(|(key, _)| RESERVED.contains(&key)) {
        return Err(
            "通用配置不能包含模型、供应商、认证或 MCP 配置；这些设置由各自的配置管理".into(),
        );
    }
    validate_item(&Item::Table(document.as_table().clone()))?;
    validate_context(&document, "通用配置")?;
    Ok(document)
}

fn validate_context(document: &DocumentMut, label: &str) -> Result<(), String> {
    for key in ["model_context_window", "model_auto_compact_token_limit"] {
        if let Some(item) = document.get(key)
            && !item.as_integer().is_some_and(|number| number > 0)
        {
            return Err(format!("{label}中的 {key} 须为正整数"));
        }
    }
    if let (Some(window), Some(limit)) = (
        document
            .get("model_context_window")
            .and_then(Item::as_integer),
        document
            .get("model_auto_compact_token_limit")
            .and_then(Item::as_integer),
    ) && limit >= window
    {
        return Err(format!("{label}的压缩阈值须小于上下文窗口"));
    }
    Ok(())
}

fn validate_subscription_item(item: &Item, root: bool) -> Result<(), String> {
    if let Some(table) = item.as_table_like() {
        for (key, item) in table.iter() {
            let normalized = key.to_ascii_lowercase().replace('-', "_");
            if sensitive_key(key)
                || matches!(
                    normalized.as_str(),
                    "model_provider"
                        | "model_providers"
                        | "model_catalog_json"
                        | "base_url"
                        | "chatgpt_base_url"
                        | "wire_api"
                        | "requires_openai_auth"
                        | "env_key"
                        | "env_key_instructions"
                        | "auth_mode"
                        | "auth_type"
                        | "tokens"
                        | "oauth"
                        | "bearer_token_env_var"
                        | "forced_login_method"
                        | "forced_chatgpt_workspace_id"
                )
                || (!root && key == "model")
            {
                return Err("订阅配置不能包含供应商路由、认证头或凭据；请使用账号管理".into());
            }
            validate_subscription_item(item, false)?;
        }
    } else if let Some(tables) = item.as_array_of_tables() {
        for table in tables.iter() {
            validate_subscription_item(&Item::Table(table.clone()), false)?;
        }
    } else if let Some(array) = item.as_array() {
        for value in array.iter() {
            validate_subscription_item(&Item::Value(value.clone()), false)?;
        }
    }
    Ok(())
}

/// Routing and credentials stay managed by SwitchX; ordinary Codex settings are editable.
pub fn validate_subscription(config: &str) -> Result<DocumentMut, String> {
    if config.len() > 256 * 1024 {
        return Err("订阅配置不能超过 256 KiB".into());
    }
    let document: DocumentMut = config
        .parse()
        .map_err(|_| "订阅配置不是有效的 TOML，请检查语法")?;
    validate_subscription_item(&Item::Table(document.as_table().clone()), true)?;
    if let Some(model) = document.get("model")
        && !model.as_str().is_some_and(|model| {
            !model.trim().is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
        })
    {
        return Err("订阅配置中的 model 须为有效的模型 ID".into());
    }
    validate_context(&document, "订阅配置")?;
    Ok(document)
}

pub fn subscription_model(config: &str) -> Result<Option<String>, String> {
    Ok(validate_subscription(config)?
        .get("model")
        .and_then(Item::as_str)
        .map(str::to_owned))
}

/// Text edits are authoritative, including custom windows smaller or larger than 1M.
pub fn subscription_options_from_config(
    options: &CodexOptions,
    config: &str,
) -> Result<CodexOptions, String> {
    let document = validate_subscription(config)?;
    let mut options = options.clone();
    options.config_toml = Some(config.to_owned());
    options.context_1m = document
        .get("model_context_window")
        .and_then(Item::as_integer)
        == Some(CONTEXT_WINDOW as i64);
    if let Some(limit) = document
        .get("model_auto_compact_token_limit")
        .and_then(Item::as_integer)
    {
        options.compact_limit = u64::try_from(limit).map_err(|_| "订阅配置的压缩阈值无效")?;
    }
    options.validate()?;
    Ok(options)
}

pub fn set_subscription_context(
    config: &str,
    context_1m: bool,
    compact_limit: u64,
) -> Result<String, String> {
    let mut document = validate_subscription(config)?;
    if context_1m {
        if !(1..CONTEXT_WINDOW).contains(&compact_limit) {
            return Err("压缩阈值须为小于 1000000 的正整数".into());
        }
        set_context(&mut document, compact_limit);
    } else if document
        .get("model_context_window")
        .and_then(Item::as_integer)
        == Some(CONTEXT_WINDOW as i64)
    {
        document.remove("model_context_window");
        document.remove("model_auto_compact_token_limit");
    }
    Ok(document.to_string())
}

pub fn set_subscription_common(
    config: &str,
    common: &str,
    enabled: bool,
) -> Result<String, String> {
    let mut document = validate_subscription(config)?;
    let common = validate_common(common)?;
    if enabled {
        merge(document.as_table_mut(), common.as_table())?;
    } else {
        crate::config_overlay::Overlay::between(&DocumentMut::new(), &common)?
            .restore(&mut document, &mut Vec::new())?;
    }
    validate_subscription(&document.to_string())?;
    Ok(document.to_string())
}

pub fn subscription_editor_config(
    model: &str,
    options: &CodexOptions,
    common: &str,
) -> Result<String, String> {
    options.validate()?;
    if let Some(config) = &options.config_toml {
        let mut document = validate_subscription(config)?;
        if options.use_common_config {
            let mut shared = validate_common(common)?;
            // Context controls are per-provider: absent raw values also preserve an explicit off.
            shared.remove("model_context_window");
            shared.remove("model_auto_compact_token_limit");
            merge_missing(document.as_table_mut(), shared.as_table());
        }
        validate_subscription(&document.to_string())?;
        return Ok(document.to_string());
    }
    let mut document = DocumentMut::new();
    if options.use_common_config {
        apply_common_to_document(&mut document, common)?;
    }
    if !model.is_empty() {
        document["model"] = value(model);
    }
    if options.context_1m {
        set_context(&mut document, options.compact_limit);
    }
    validate_subscription(&document.to_string())?;
    Ok(document.to_string())
}

fn clean_value(source: &Value) -> Value {
    let mut result = source.clone();
    match &mut result {
        Value::Array(array) => {
            for value in array.iter_mut() {
                *value = clean_value(value);
            }
            array.set_trailing("");
            array.fmt();
        }
        Value::InlineTable(table) => {
            let keys: Vec<_> = table.iter().map(|(key, _)| key.to_owned()).collect();
            for key in keys {
                if sensitive_key(&key)
                    || matches!(
                        key.as_str(),
                        "model_provider" | "model_providers" | "model_catalog_json"
                    )
                {
                    table.remove(&key);
                } else if let Some(value) = table.get_mut(&key) {
                    *value = clean_value(value);
                }
            }
            table.fmt();
        }
        _ => {}
    }
    result.decor_mut().clear();
    result
}

fn clean_table(source: &Table, root: bool) -> Table {
    let mut result = Table::new();
    for (key, item) in source.iter() {
        if sensitive_key(key)
            || (root && RESERVED.contains(&key))
            || matches!(
                key,
                "model_provider" | "model_providers" | "model_catalog_json"
            )
            || (root && key == "web_search" && item.as_str() == Some("disabled"))
        {
            continue;
        }
        let clean = match item {
            Item::Table(table) => Item::Table(clean_table(table, false)),
            Item::ArrayOfTables(tables) => {
                let mut clean = ArrayOfTables::new();
                for table in tables.iter() {
                    clean.push(clean_table(table, false));
                }
                Item::ArrayOfTables(clean)
            }
            Item::Value(value) => Item::Value(clean_value(value)),
            Item::None => continue,
        };
        result.insert(key, clean);
    }
    result
}

pub fn extract_common(current: &str) -> Result<String, String> {
    let document: DocumentMut = current
        .parse()
        .map_err(|_| "当前 Codex 配置不是有效的 TOML")?;
    let mut result = DocumentMut::new();
    *result.as_table_mut() = clean_table(document.as_table(), true);
    let result = result.to_string();
    validate_common(&result)?;
    Ok(result)
}

pub fn extract_from_home(home: &Path) -> Result<String, String> {
    let path = crate::client::config_path(home)?;
    let contents =
        crate::config_transaction::read_config(&path)?.ok_or("当前 Codex 配置尚不存在")?;
    let contents = std::str::from_utf8(&contents).map_err(|_| "当前 Codex 配置不是 UTF-8")?;
    extract_common(contents)
}

fn merge(target: &mut dyn TableLike, source: &dyn TableLike) -> Result<(), String> {
    for (key, incoming) in source.iter() {
        if let Some(existing) = target.get_mut(key) {
            if let (Some(existing), Some(incoming)) =
                (existing.as_table_like_mut(), incoming.as_table_like())
            {
                merge(existing, incoming)?;
                continue;
            }
            if existing.is_table_like() != incoming.is_table_like() {
                return Err("通用配置的表与现有配置值冲突，请检查目标配置".into());
            }
            let mut replacement = incoming.clone();
            if let (Some(old), Some(new)) = (existing.as_value(), replacement.as_value_mut()) {
                *new.decor_mut() = old.decor().clone();
            }
            *existing = replacement;
        } else {
            target.insert(key, incoming.clone());
        }
    }
    Ok(())
}

fn merge_missing(target: &mut dyn TableLike, source: &dyn TableLike) {
    for (key, incoming) in source.iter() {
        if let Some(existing) = target.get_mut(key) {
            if let (Some(existing), Some(incoming)) =
                (existing.as_table_like_mut(), incoming.as_table_like())
            {
                merge_missing(existing, incoming);
            }
        } else {
            target.insert(key, incoming.clone());
        }
    }
}

fn set_context(document: &mut DocumentMut, compact_limit: u64) {
    for (key, number) in [
        ("model_context_window", CONTEXT_WINDOW),
        ("model_auto_compact_token_limit", compact_limit),
    ] {
        let mut replacement = Value::from(number as i64);
        if let Some(old) = document.get(key).and_then(Item::as_value) {
            *replacement.decor_mut() = old.decor().clone();
        }
        document[key] = Item::Value(replacement);
    }
}

pub fn apply_to_document(
    document: &mut DocumentMut,
    options: &CodexOptions,
    common: &str,
    provider_id: &str,
) -> Result<(), String> {
    options.validate()?;
    if options.use_common_config {
        apply_common_to_document(document, common)?;
    }
    if let Some(config) = &options.config_toml {
        let mut subscription = validate_subscription(config)?;
        // The editor's model is upstream metadata; the published public ID stays managed.
        subscription.remove("model");
        document.remove("model_context_window");
        document.remove("model_auto_compact_token_limit");
        merge(document.as_table_mut(), subscription.as_table())?;
    } else if options.context_1m {
        set_context(document, options.compact_limit);
    } else {
        document.remove("model_context_window");
        document.remove("model_auto_compact_token_limit");
    }
    let provider = document
        .get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .and_then(|table| table.get_mut(provider_id))
        .and_then(Item::as_table_mut)
        .ok_or("生成的 Codex 供应商配置缺失")?;
    if options.remote_compaction {
        provider.insert("name", value("OpenAI"));
    } else if provider.get("name").and_then(Item::as_str) == Some("OpenAI") {
        provider.insert("name", value("OpenAI API"));
    }
    Ok(())
}

pub fn apply_common_to_document(document: &mut DocumentMut, common: &str) -> Result<(), String> {
    let shared = validate_common(common)?;
    merge(document.as_table_mut(), shared.as_table())
}

pub fn preview(
    name: &str,
    base_url: &str,
    model: &str,
    id: &str,
    helper: &Path,
    options: &CodexOptions,
    common: &str,
) -> Result<String, String> {
    let provider_id = format!(
        "switchx_direct_{}",
        if id.is_empty() { "new_provider" } else { id }
    );
    let mut document = DocumentMut::new();
    document["model_provider"] = value(&provider_id);
    document["model"] = value(model);
    document["model_providers"] = table();
    document["model_providers"][&provider_id] = table();
    let provider = document["model_providers"][&provider_id]
        .as_table_mut()
        .unwrap();
    provider.insert("name", value(name));
    provider.insert("base_url", value(base_url));
    provider.insert("wire_api", value("responses"));
    provider.insert("supports_websockets", value(false));
    provider.insert("request_max_retries", value(0));
    provider.insert("stream_max_retries", value(0));
    provider.insert("auth", table());
    provider["auth"]["command"] = value(helper.to_str().ok_or("SwitchX 路径不是 UTF-8")?);
    let mut args = Array::new();
    args.push("credential");
    args.push(if id.is_empty() {
        "<保存后生成的凭据引用>"
    } else {
        id
    });
    provider["auth"]["args"] = value(args);
    apply_to_document(&mut document, options, common, &provider_id)?;
    Ok(document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_toml_accepts_general_settings_and_safe_mcp_but_rejects_credentials_and_routes()
    {
        let config = "# provider preferences\nmodel = 'synthetic-official'\napproval_policy = 'never'\nsandbox_mode = 'danger-full-access'\nmodel_reasoning_effort = 'max' # keep effort\nunknown_future_option = true\n[features]\nhooks = true\n[mcp_servers.local]\ncommand = 'synthetic-command'\nargs = ['--local']\nenv = { LANG = 'en_US' }\n";
        assert_eq!(validate_subscription(config).unwrap().to_string(), config);
        assert_eq!(
            subscription_model(config).unwrap().as_deref(),
            Some("synthetic-official")
        );
        for rejected in [
            "model_provider = 'external'",
            "[model_providers.external]\nbase_url = 'https://example.invalid'",
            "model_catalog_json = '/tmp/catalog.json'",
            "profiles = { nested = { model = 'alternate' } }",
            "[profiles.nested]\nwire_api = 'chat'",
            "[mcp_servers.local]\nhttp_headers = { Authorization = 'synthetic-secret' }",
            "[mcp_servers.local.env]\nSERVICE_API_KEY = 'synthetic-secret'",
            "[mcp_servers.local]\nenv = { SERVICE_TOKEN = 'synthetic-secret' }",
            "[[hooks.local]]\nrefresh_token = 'synthetic-secret'",
            "hooks = [{ auth = { command = 'synthetic-secret' } }]",
        ] {
            let error = validate_subscription(rejected).unwrap_err();
            assert!(!error.contains("synthetic-secret"));
        }
        assert!(validate_common(config).is_err());
    }

    #[test]
    fn subscription_editor_controls_preserve_comments_and_manual_custom_windows() {
        let config = "# subscription draft\nmodel = 'synthetic-official'\napproval_policy = 'never' # authored\nmodel_context_window = 128000\nmodel_auto_compact_token_limit = 96000\n[features]\nhooks = false\n";
        let options = subscription_options_from_config(&CodexOptions::default(), config).unwrap();
        assert!(!options.context_1m);
        assert_eq!(options.compact_limit, 96000);
        assert_eq!(options.config_toml.as_deref(), Some(config));
        let common = "approval_policy = 'on-request'\n[features]\nhooks = true\nmemories = true\n[tui]\nnotifications = ['a', 'b']\n";
        let rendered = subscription_editor_config("ignored-model", &options, common).unwrap();
        assert!(rendered.contains("# subscription draft"));
        assert!(rendered.contains("# authored"));
        let document: DocumentMut = rendered.parse().unwrap();
        assert_eq!(document["approval_policy"].as_str(), Some("never"));
        assert_eq!(document["features"]["hooks"].as_bool(), Some(false));
        assert_eq!(document["features"]["memories"].as_bool(), Some(true));
        let rendered = set_subscription_common(&rendered, common, false).unwrap();
        let document: DocumentMut = rendered.parse().unwrap();
        assert_eq!(document["features"]["hooks"].as_bool(), Some(false));
        assert!(document["features"].get("memories").is_none());
        assert!(document.get("tui").is_none());
        assert!(rendered.contains("# authored"));
        assert_eq!(
            set_subscription_context(config, false, 900000).unwrap(),
            config
        );
        let enabled = set_subscription_context(config, true, 850000).unwrap();
        assert!(
            subscription_options_from_config(&options, &enabled)
                .unwrap()
                .context_1m
        );
        let disabled = set_subscription_context(&enabled, false, 850000).unwrap();
        let document: DocumentMut = disabled.parse().unwrap();
        assert!(document.get("model_context_window").is_none());
        assert!(document.get("model_auto_compact_token_limit").is_none());
        assert!(disabled.contains("# subscription draft"));
        let legacy = r#"{"remote_compaction":false,"use_common_config":true,"context_1m":false,"compact_limit":900000}"#;
        assert_eq!(
            CodexOptions::from_saved(legacy)
                .unwrap()
                .unwrap()
                .config_toml,
            None
        );
    }

    #[test]
    fn subscription_config_applies_general_settings_without_taking_over_managed_routing() {
        let mut document: DocumentMut = "model = 'sx-public-official'\nmodel_provider = 'switchx_router'\nmodel_catalog_json = '/tmp/managed-catalog.json'\nmodel_context_window = 1000000\n[model_providers.switchx_router]\nname = 'SwitchX Router'\nbase_url = 'http://127.0.0.1:1234/v1'\nwire_api = 'responses'\n".parse().unwrap();
        let options = subscription_options_from_config(
            &CodexOptions::default(),
            "model = 'synthetic-upstream'\nmodel_reasoning_effort = 'max'\nmodel_context_window = 128000\nmodel_auto_compact_token_limit = 96000\n[features]\nhooks = true\n[mcp_servers.local]\ncommand = 'synthetic-command'\n",
        ).unwrap();
        apply_to_document(
            &mut document,
            &options,
            "approval_policy = 'never'\n",
            "switchx_router",
        )
        .unwrap();
        assert_eq!(document["model"].as_str(), Some("sx-public-official"));
        assert_eq!(document["model_provider"].as_str(), Some("switchx_router"));
        assert_eq!(
            document["model_catalog_json"].as_str(),
            Some("/tmp/managed-catalog.json")
        );
        assert_eq!(
            document["model_providers"]["switchx_router"]["base_url"].as_str(),
            Some("http://127.0.0.1:1234/v1")
        );
        assert_eq!(document["model_context_window"].as_integer(), Some(128000));
        assert_eq!(
            document["model_auto_compact_token_limit"].as_integer(),
            Some(96000)
        );
        assert_eq!(document["features"]["hooks"].as_bool(), Some(true));
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("max"));
        assert_eq!(
            document["mcp_servers"]["local"]["command"].as_str(),
            Some("synthetic-command")
        );
    }

    #[test]
    fn controls_merge_common_and_never_embed_a_key() {
        let options = CodexOptions {
            remote_compaction: true,
            context_1m: true,
            compact_limit: 800_000,
            ..Default::default()
        };
        let rendered = preview(
            "Example",
            "https://example.invalid/v1",
            "model",
            "",
            Path::new("/tmp/switchx"),
            &options,
            "model_auto_compact_token_limit = 700000\n[features]\nmemories = true\n",
        )
        .unwrap();
        let document: DocumentMut = rendered.parse().unwrap();
        assert_eq!(
            document["model_context_window"].as_integer(),
            Some(1_000_000)
        );
        assert_eq!(
            document["model_auto_compact_token_limit"].as_integer(),
            Some(800_000)
        );
        assert_eq!(
            document["model_providers"]["switchx_direct_new_provider"]["name"].as_str(),
            Some("OpenAI")
        );
        assert_eq!(document["features"]["memories"].as_bool(), Some(true));
        assert!(!rendered.contains("OPENAI_API_KEY"));
        let mut document = document;
        apply_to_document(
            &mut document,
            &CodexOptions {
                use_common_config: false,
                ..Default::default()
            },
            "invalid TOML",
            "switchx_direct_new_provider",
        )
        .unwrap();
        assert!(document.get("model_context_window").is_none());
        assert!(document.get("model_auto_compact_token_limit").is_none());
    }

    #[test]
    fn extraction_omits_identity_credentials_mcp_and_source_comments() {
        let shared = extract_common("# private source comment\nmodel = 'private-model'\nmodel_catalog_json = '/private/catalog'\nweb_search = 'disabled'\nmodel_reasoning_effort = 'max' # private note\n[model_providers.custom]\nexperimental_bearer_token = 'secret-marker'\n[mcp_servers.private]\ncommand = 'private-command'\n[features]\nmemories = true\n[env]\nEXAMPLE_API_KEY = 'secret-marker'\nLANG = 'en_US'\n").unwrap();
        assert!(!shared.contains("private"));
        assert!(!shared.contains("secret-marker"));
        assert!(shared.contains("memories = true"));
        assert!(shared.contains("LANG = 'en_US'"));
        assert!(!shared.contains("web_search"));
        assert!(validate_common("[features]\nmemories = true\n").is_ok());
        assert!(validate_common("[env]\nPROVIDER_TOKEN = 'secret-marker'\n").is_err());
        assert!(validate_common("features = { api_key = 'secret-marker' }").is_err());
        assert!(
            validate_common(
                "[shell_environment_policy.set]\nAWS_SECRET_ACCESS_KEY = 'secret-marker'\n"
            )
            .is_err()
        );
        let extracted = extract_common("[shell_environment_policy.set]\nPRIVATE_KEY = 'secret-marker'\nAWS_ACCESS_KEY_ID = 'secret-marker'\nLANG = 'en_US'\n").unwrap();
        assert!(!extracted.contains("secret-marker"));
        assert!(validate_common("model_provider = 'custom'").is_err());
    }

    #[test]
    fn preview_extraction_removes_helper_references_and_env_credentials() {
        let rendered = preview(
            "Synthetic provider",
            "https://example.invalid/v1",
            "synthetic-model",
            "synthetic-provider-reference",
            Path::new("/tmp/synthetic-credential-helper"),
            &CodexOptions::default(),
            "model_reasoning_effort = 'max'\n[env]\nLANG = 'en_US.UTF-8'\n[features]\nhooks = true\n",
        )
        .unwrap();
        assert!(rendered.contains("synthetic-credential-helper"));
        assert!(rendered.contains("synthetic-provider-reference"));
        let mut source: DocumentMut = rendered.parse().unwrap();
        source["credential_ref"] = value("synthetic-standalone-reference");
        source["env"]["GOOGLE_APPLICATION_CREDENTIALS"] = value("synthetic-env-credentials");
        source["env"]["credential_ref"] = value("synthetic-env-reference");

        let shared = extract_common(&source.to_string()).unwrap();
        assert!(!shared.contains("synthetic-"));
        let extracted: DocumentMut = shared.parse().unwrap();
        assert!(extracted.get("model_providers").is_none());
        assert!(extracted.get("auth").is_none());
        assert!(extracted.get("credential_ref").is_none());
        assert!(
            extracted["env"]
                .get("GOOGLE_APPLICATION_CREDENTIALS")
                .is_none()
        );
        assert!(extracted["env"].get("credential_ref").is_none());
        assert_eq!(extracted["env"]["LANG"].as_str(), Some("en_US.UTF-8"));
        assert_eq!(extracted["features"]["hooks"].as_bool(), Some(true));
        assert_eq!(extracted["model_reasoning_effort"].as_str(), Some("max"));
        assert!(validate_common("credential_ref = 'synthetic-reference'").is_err());
        assert!(
            validate_common("[env]\nGOOGLE_APPLICATION_CREDENTIALS = 'synthetic-reference'\n")
                .is_err()
        );
    }

    #[test]
    fn invalid_options_and_common_are_rejected_and_defaults_follow_common() {
        assert!(
            CodexOptions {
                context_1m: true,
                compact_limit: 0,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            CodexOptions {
                context_1m: true,
                compact_limit: 1_000_000,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(validate_common("[broken").is_err());
        assert!(validate_common("model_auto_compact_token_limit = 'invalid'").is_err());
        assert_eq!(
            CodexOptions::for_common(
                "model_context_window = 1000000\nmodel_auto_compact_token_limit = 850000\n"
            )
            .unwrap()
            .compact_limit,
            850_000
        );
        assert_eq!(CodexOptions::from_saved("").unwrap(), None);
        let preview = preview(
            "OpenAI",
            "https://example.invalid/v1",
            "model",
            "",
            Path::new("/tmp/switchx"),
            &CodexOptions::default(),
            "",
        )
        .unwrap();
        assert!(preview.contains("name = \"OpenAI API\""));
    }
}
