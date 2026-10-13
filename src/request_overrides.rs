//! Per-model, local-only customization of the final upstream request.

use std::{collections::BTreeMap, fmt};

use axum::http::{HeaderMap, HeaderName, HeaderValue, header};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const MAX_BYTES: usize = 32 * 1024;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RequestOverrides {
    #[serde(skip_serializing_if = "String::is_empty")]
    user_agent: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Map::is_empty")]
    body: Map<String, Value>,
}

// Custom values may contain private tenant metadata. Never print their contents.
impl fmt::Debug for RequestOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestOverrides")
            .field("configured", &!self.is_empty())
            .finish_non_exhaustive()
    }
}

impl RequestOverrides {
    pub fn parse(user_agent: &str, headers: &str, body: &str) -> Result<Self, String> {
        let header_object = parse_object(headers, "Header")?;
        let mut normalized = BTreeMap::new();
        for (name, value) in header_object {
            let name = name.trim().to_ascii_lowercase();
            let value = value.as_str().ok_or("Header 的值必须是字符串")?;
            if normalized.insert(name, value.to_owned()).is_some() {
                return Err("Header 名称重复（不区分大小写）".into());
            }
        }
        let overrides = Self {
            user_agent: user_agent.trim().into(),
            headers: normalized,
            body: parse_object(body, "Body")?,
        };
        overrides.validate()?;
        Ok(overrides)
    }

    pub fn validate(&self) -> Result<(), String> {
        if serde_json::to_vec(self)
            .map_err(|_| "无法读取请求定制")?
            .len()
            > MAX_BYTES
        {
            return Err("请求定制不能超过 32 KiB".into());
        }
        HeaderValue::from_str(&self.user_agent).map_err(|_| "User-Agent 不能包含换行或控制字符")?;
        let mut seen = std::collections::HashSet::new();
        for (name, value) in &self.headers {
            let name =
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| "Header 名称格式无效")?;
            if !seen.insert(name.clone()) {
                return Err("Header 名称重复（不区分大小写）".into());
            }
            if protected_header(name.as_str()) {
                return Err("Header 不能覆盖认证、会话、网络转发或 HTTP 传输字段".into());
            }
            HeaderValue::from_str(value).map_err(|_| "Header 值不能包含换行或控制字符")?;
        }
        if self.body.keys().any(|key| {
            matches!(
                key.as_str(),
                "model"
                    | "stream"
                    | "input"
                    | "previous_response_id"
                    | "conversation"
                    | "tools"
                    | "tool_choice"
                    | "background"
            )
        }) {
            return Err(
                "Body 不能覆盖 model、stream、input、tools、tool_choice、background 或会话续接字段"
                    .into(),
            );
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.user_agent.is_empty() && self.headers.is_empty() && self.body.is_empty()
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    pub fn headers_json(&self) -> String {
        if self.headers.is_empty() {
            String::new()
        } else {
            serde_json::to_string_pretty(&self.headers).unwrap()
        }
    }

    pub fn body_json(&self) -> String {
        if self.body.is_empty() {
            String::new()
        } else {
            serde_json::to_string_pretty(&self.body).unwrap()
        }
    }

    pub(crate) fn apply(&self, headers: &mut HeaderMap, body: &mut Value) {
        if !self.user_agent.is_empty() {
            headers.insert(
                header::USER_AGENT,
                HeaderValue::from_str(&self.user_agent).expect("validated UA"),
            );
        }
        for (name, value) in &self.headers {
            let mut value = HeaderValue::from_str(value).expect("validated header value");
            value.set_sensitive(true);
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("validated header name"),
                value,
            );
        }
        merge(body, &Value::Object(self.body.clone()));
    }
}

fn parse_object(text: &str, label: &str) -> Result<Map<String, Value>, String> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    if text.len() > MAX_BYTES {
        return Err(format!("{label} 不能超过 32 KiB"));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| {
        // serde's full error can include a user's value; only expose the location.
        format!(
            "{label} JSON 格式错误：第 {} 行，第 {} 列",
            error.line(),
            error.column()
        )
    })?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{label} 必须是 JSON 对象 {{…}}"))
}

fn protected_header(name: &str) -> bool {
    name.starts_with("x-codex-")
        || name.starts_with("x-openai-")
        || name.starts_with("x-forwarded-")
        || name.starts_with("cf-")
        || name.starts_with("sec-")
        || name.starts_with("proxy-")
        || matches!(
            name,
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "te"
                | "trailer"
                | "upgrade"
                | "keep-alive"
                | "accept-encoding"
                | "content-encoding"
                | "content-type"
                | "authorization"
                | "x-api-key"
                | "x-goog-api-key"
                | "cookie"
                | "set-cookie"
                | "chatgpt-account-id"
                | "session-id"
                | "session_id"
                | "thread-id"
                | "x-client-request-id"
                | "originator"
                | "forwarded"
                | "true-client-ip"
                | "fastly-client-ip"
                | "x-azure-clientip"
                | "x-azure-fdid"
                | "x-azure-ref"
                | "x-switchx-token"
                | "x-switchx-local-token"
        )
}

fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            merge(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    } else {
        *target = patch.clone();
    }
}

/// Connect the pure form validation and formatting callbacks used by desktop and previews.
pub fn connect(app: &crate::ui::AppWindow) {
    use slint::ComponentHandle;
    let rules = app.global::<crate::ui::RequestRules>();
    rules.on_validate(|ua, headers, body| {
        RequestOverrides::parse(&ua, &headers, &body)
            .err()
            .unwrap_or_default()
            .into()
    });
    rules.on_format(|text| {
        parse_object(&text, "JSON")
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| text.to_string())
            .into()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_objects_replaces_arrays_and_keeps_explicit_null() {
        let config = RequestOverrides::parse(
            " client/1 ",
            r#"{"User-Agent":"header/2","X-Tenant":"private"}"#,
            r#"{"reasoning":{"effort":"high"},"tags":["new"],"temperature":null}"#,
        )
        .unwrap();
        let mut body = json!({"model":"actual", "stream":true,"reasoning":{"effort":"low","summary":"auto"},"tags":["old"],"temperature":0.5});
        let mut headers = HeaderMap::new();
        config.apply(&mut headers, &mut body);
        assert_eq!(
            body,
            json!({"model":"actual","stream":true,"reasoning":{"effort":"high","summary":"auto"},"tags":["new"],"temperature":null})
        );
        assert_eq!(headers["user-agent"], "header/2");
        assert_eq!(config.user_agent(), "client/1");
        assert!(!format!("{config:?}").contains("private"));
    }

    #[test]
    fn rejects_invalid_or_protected_values_without_echoing_them() {
        for headers in [
            r#"{"Authorization":"private"}"#,
            r#"{"Host":"private"}"#,
            r#"{"X-Codex-Turn-State":"private"}"#,
            r#"{"x-test":1}"#,
            r#"{"x-test":"a\r\nb"}"#,
            r#"{"X-Test":"a","x-test":"b"}"#,
            "[]",
        ] {
            let error = RequestOverrides::parse("", headers, "").unwrap_err();
            assert!(!error.contains("private"));
        }
        for key in ["model", "stream", "input", "previous_response_id", "tools"] {
            assert!(RequestOverrides::parse("", "", &json!({key:"private"}).to_string()).is_err());
        }
        assert!(RequestOverrides::parse("bad\r\nua", "", "").is_err());
        assert!(RequestOverrides::parse("", "", "null").is_err());
        assert_eq!(
            serde_json::to_string(&RequestOverrides::default()).unwrap(),
            "{}"
        );
    }
}
